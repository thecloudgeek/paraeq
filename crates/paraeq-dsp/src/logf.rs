//! Log-frequency grid + resampling — the shared axis FDW, smoothing, splice,
//! targets and authority all operate on.
//! Test tier: 2 + 3. Tier 2 pins `resample_db_to_log_grid` (Prefilter::None)
//! against `np.interp` on the grid-formula axis at 1e-12; Tier 3 pins the
//! grid's closed form and the anti-comb prefilter's reason to exist (aliased
//! vs bounded ripple on a synthetic comb).
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`logf.rs` — new".

use crate::{Complex, DspError};

/// `f_i = f_min · 2^(i/ppo)` for `i ∈ [0, N)`, `N = floor(ppo·log2(f_max/f_min)) + 1`.
///
/// The last bin falls short of `f_max` by less than one bin spacing — the grid is
/// the largest set of `ppo`-spaced points that does not exceed `f_max`. This is
/// deliberate; it is not an off-by-one.
#[derive(Clone, Debug)]
pub struct LogGrid {
    freqs: Vec<f64>,
    f_min: f64,
    f_max: f64,
    points_per_octave: u32,
}

impl LogGrid {
    pub const DEFAULT_PPO: u32 = 96;

    pub fn new(f_min: f64, f_max: f64, points_per_octave: u32) -> Result<Self, DspError> {
        if !f_min.is_finite() || f_min <= 0.0 {
            return Err(DspError::InvalidInput(format!(
                "log grid f_min must be finite and > 0, got {f_min}"
            )));
        }
        if !f_max.is_finite() || f_max <= f_min {
            return Err(DspError::InvalidInput(format!(
                "log grid f_max must be finite and > f_min ({f_min}), got {f_max}"
            )));
        }
        if points_per_octave == 0 {
            return Err(DspError::InvalidInput(
                "log grid points_per_octave must be > 0".into(),
            ));
        }
        let ppo = f64::from(points_per_octave);
        // N = floor(ppo·log2(f_max/f_min)) + 1; per-index pow, matching the
        // oracle's `f_min * 2.0 ** (np.arange(n) / ppo)` term for term.
        let n = (ppo * (f_max / f_min).log2()).floor() as usize + 1;
        let freqs = (0..n).map(|i| f_min * 2f64.powf(i as f64 / ppo)).collect();
        Ok(LogGrid {
            freqs,
            f_min,
            f_max,
            points_per_octave,
        })
    }

    /// 20 Hz .. 20 kHz @ 96 ppo -> 957 points.
    pub fn standard() -> Self {
        Self::new(20.0, 20000.0, Self::DEFAULT_PPO).expect("standard grid params are valid")
    }

    pub fn freqs(&self) -> &[f64] {
        &self.freqs
    }

    pub fn len(&self) -> usize {
        self.freqs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.freqs.is_empty()
    }

    pub fn f_min(&self) -> f64 {
        self.f_min
    }

    pub fn f_max(&self) -> f64 {
        self.f_max
    }

    pub fn points_per_octave(&self) -> u32 {
        self.points_per_octave
    }

    /// Octave offset of bin `i` from `f_min`: `i / ppo`. The UNIFORM axis fdw.rs
    /// and fr.rs convolve on — this is why a log grid buys O(N log N).
    pub fn octave_axis(&self) -> Vec<f64> {
        let ppo = f64::from(self.points_per_octave);
        (0..self.freqs.len()).map(|i| i as f64 / ppo).collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Prefilter {
    None,
    /// Average over each bin's 1/`fraction`-octave neighbourhood before
    /// decimating. Default 48 (REW's finest variable-smoothing setting, so it
    /// cannot erase anything downstream smoothing would keep). This is a
    /// lowpass-before-downsample, NOT smoothing.
    AntiComb {
        fraction: u32,
    },
}

/// Complex resample — the ONLY correct input to `fdw::apply_fdw`.
///
/// The caller must derotate (remove the bulk delay) BEFORE calling: at 20 kHz a
/// 50 ms delay winds ~1000 full turns and no log grid could sample it.
pub fn resample_complex_to_log_grid(
    freqs_linear: &[f64],
    spectrum: &[Complex<f64>],
    grid: &LogGrid,
    prefilter: Prefilter,
) -> Result<Vec<Complex<f64>>, DspError> {
    validate_axis(freqs_linear, spectrum.len())?;
    if spectrum
        .iter()
        .any(|c| !c.re.is_finite() || !c.im.is_finite())
    {
        return Err(DspError::InvalidInput(
            "resample: spectrum contains non-finite values".into(),
        ));
    }
    validate_prefilter(prefilter)?;
    // Real and imaginary parts interpolate/average independently; a complex
    // mean IS the mean of the parts, so this is exact, not an approximation.
    let re: Vec<f64> = spectrum.iter().map(|c| c.re).collect();
    let im: Vec<f64> = spectrum.iter().map(|c| c.im).collect();
    let re_out = resample_scalar(freqs_linear, &re, grid, prefilter);
    let im_out = resample_scalar(freqs_linear, &im, grid, prefilter);
    Ok(re_out
        .into_iter()
        .zip(im_out)
        .map(|(r, i)| Complex::new(r, i))
        .collect())
}

/// Magnitude resample — for display and dB-domain operations only. Never feed
/// this to `fdw::apply_fdw`; smoothing `|H|` is a different, wrong operation.
pub fn resample_db_to_log_grid(
    freqs_linear: &[f64],
    magnitude_db: &[f64],
    grid: &LogGrid,
    prefilter: Prefilter,
) -> Result<Vec<f64>, DspError> {
    validate_axis(freqs_linear, magnitude_db.len())?;
    if magnitude_db.iter().any(|v| !v.is_finite()) {
        return Err(DspError::InvalidInput(
            "resample: magnitude contains non-finite values".into(),
        ));
    }
    validate_prefilter(prefilter)?;
    Ok(resample_scalar(freqs_linear, magnitude_db, grid, prefilter))
}

fn validate_axis(freqs: &[f64], data_len: usize) -> Result<(), DspError> {
    if freqs.is_empty() {
        return Err(DspError::InvalidInput("resample: empty input axis".into()));
    }
    if freqs.len() != data_len {
        return Err(DspError::InvalidInput(format!(
            "resample: {} freqs vs {} data points",
            freqs.len(),
            data_len
        )));
    }
    if freqs.iter().any(|v| !v.is_finite()) {
        return Err(DspError::InvalidInput(
            "resample: freqs contain non-finite values".into(),
        ));
    }
    if freqs.windows(2).any(|w| w[1] <= w[0]) {
        return Err(DspError::InvalidInput(
            "resample: freqs must be strictly increasing".into(),
        ));
    }
    Ok(())
}

fn validate_prefilter(prefilter: Prefilter) -> Result<(), DspError> {
    match prefilter {
        Prefilter::AntiComb { fraction: 0 } => Err(DspError::InvalidInput(
            "AntiComb fraction must be > 0".into(),
        )),
        _ => Ok(()),
    }
}

/// `np.interp` semantics: linear interpolation in f, clamped to the end values
/// outside the input range. Bit-identical to numpy's compiled_interp formula
/// (`slope·(x − x_j) + y_j`) on finite, strictly-increasing input.
fn np_interp(x: f64, xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len();
    if x <= xs[0] {
        return ys[0];
    }
    if x >= xs[n - 1] {
        return ys[n - 1];
    }
    // largest j with xs[j] <= x (x is strictly inside, so j ∈ [0, n-2])
    let j = xs.partition_point(|&v| v <= x) - 1;
    let slope = (ys[j + 1] - ys[j]) / (xs[j + 1] - xs[j]);
    slope * (x - xs[j]) + ys[j]
}

fn resample_scalar(freqs: &[f64], data: &[f64], grid: &LogGrid, prefilter: Prefilter) -> Vec<f64> {
    match prefilter {
        Prefilter::None => grid
            .freqs
            .iter()
            .map(|&f| np_interp(f, freqs, data))
            .collect(),
        Prefilter::AntiComb { fraction } => {
            let half_oct = 1.0 / (2.0 * f64::from(fraction));
            let lo_k = 2f64.powf(-half_oct);
            let hi_k = 2f64.powf(half_oct);
            grid.freqs
                .iter()
                .map(|&f| {
                    // inclusive neighbourhood [f·2^(-1/2k), f·2^(+1/2k)]
                    let i0 = freqs.partition_point(|&v| v < f * lo_k);
                    let i1 = freqs.partition_point(|&v| v <= f * hi_k);
                    if i1 - i0 >= 2 {
                        data[i0..i1].iter().sum::<f64>() / (i1 - i0) as f64
                    } else {
                        // The log grid out-resolves the linear axis here (low
                        // frequencies): the neighbourhood holds at most one
                        // point, so averaging degenerates — plain interpolation
                        // is the correct limit, never an empty-average NaN.
                        np_interp(f, freqs, data)
                    }
                })
                .collect()
        }
    }
}
