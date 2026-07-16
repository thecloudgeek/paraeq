//! Frequency-response computation and shaping.
//! Oracle: prototype/paraeq/measurement/frequency_response.py
//!
//! Tier 1 (frozen) — `compute_frequency_response`, `fractional_octave_smooth`,
//! `average_measurements` and `normalize_to_reference_band` keep their exact
//! current behaviour and fixtures. `test_fr.rs::averaging_is_in_db_domain` is
//! the contract that stops a future session "unifying" the two averaging paths;
//! the fixture genuinely discriminates (dB mean `a+3.000` vs a linear-domain
//! `a+3.508`, a gap ~5e11× the 1e-12 tolerance).
//!
//! The room additions below are STRICTLY ADDITIVE — new functions, never
//! modified ones. Test tier: 2 + 3. Tier 2 pins `average_measurements_rms` and
//! `sigma_db` against numpy references at 1e-12 and the Alvarez–Mazorra Gaussian
//! against `scipy.ndimage.gaussian_filter1d` at 1e-3 dB (a "matches to within
//! the method's published accuracy" test — the AM recursion is by construction
//! an approximation). Tier 3 pins the power-mean inequality and the null-floor
//! formula.
//!
//! STUB — room-dsp/8, lands in Stage 4.

use crate::{logf::LogGrid, Complex, DspError};
use realfft::RealFftPlanner;

pub fn compute_frequency_response(
    ir: &[f64],
    sample_rate: u32,
    n_fft: Option<usize>,
) -> Result<(Vec<f64>, Vec<f64>), crate::DspError> {
    let n = n_fft.unwrap_or(ir.len());
    if n == 0 {
        return Err(crate::DspError::InvalidInput(
            "FFT length is 0 (empty impulse response, or explicit n_fft=0)".into(),
        ));
    }
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = vec![0.0; n];
    let m = ir.len().min(n);
    input[..m].copy_from_slice(&ir[..m]);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    let freqs: Vec<f64> = (0..spectrum.len())
        .map(|i| i as f64 * sample_rate as f64 / n as f64)
        .collect();
    let mag_db: Vec<f64> = spectrum
        .iter()
        .map(|c| 20.0 * c.norm().max(1e-10).log10())
        .collect();
    Ok((freqs, mag_db))
}

pub fn fractional_octave_smooth(magnitude_db: &[f64], freqs: &[f64], fraction: u32) -> Vec<f64> {
    let ratio = 2f64.powf(1.0 / (2.0 * fraction as f64));
    let linear: Vec<f64> = magnitude_db
        .iter()
        .map(|db| 10f64.powf(db / 20.0))
        .collect();
    let mut out = Vec::with_capacity(linear.len());
    for (i, &f) in freqs.iter().enumerate() {
        if f <= 0.0 {
            out.push(linear[i]);
            continue;
        }
        let (lo, hi) = (f / ratio, f * ratio);
        // freqs is monotonically increasing (rfftfreq): contiguous window.
        let mut sum = 0.0;
        let mut count = 0usize;
        for (j, &fj) in freqs.iter().enumerate() {
            if fj >= lo && fj <= hi {
                sum += linear[j];
                count += 1;
            }
        }
        out.push(sum / count as f64);
    }
    out.iter().map(|v| 20.0 * v.max(1e-10).log10()).collect()
}

pub fn average_measurements(measurements_db: &[Vec<f64>]) -> Result<Vec<f64>, crate::DspError> {
    let n = match measurements_db.first() {
        Some(first) => first.len(),
        None => {
            return Err(crate::DspError::InvalidInput(
                "no measurements to average".into(),
            ))
        }
    };
    for (i, m) in measurements_db.iter().enumerate() {
        if m.len() != n {
            return Err(crate::DspError::InvalidInput(format!(
                "measurement {i} has {} points, expected {n} (all measurements must match)",
                m.len()
            )));
        }
    }
    let mut out = vec![0.0; n];
    for m in measurements_db {
        for (o, v) in out.iter_mut().zip(m) {
            *o += v;
        }
    }
    for o in &mut out {
        *o /= measurements_db.len() as f64;
    }
    Ok(out)
}

pub fn normalize_to_reference_band(
    freqs: &[f64],
    magnitude_db: &[f64],
    low_hz: f64,
    high_hz: f64,
) -> Result<Vec<f64>, crate::DspError> {
    let band: Vec<f64> = freqs
        .iter()
        .zip(magnitude_db)
        .filter(|(f, _)| **f >= low_hz && **f <= high_hz)
        .map(|(_, m)| *m)
        .collect();
    if band.is_empty() {
        return Err(crate::DspError::InvalidInput(format!(
            "no bins in reference band {low_hz}-{high_hz} Hz"
        )));
    }
    let mean = band.iter().sum::<f64>() / band.len() as f64;
    Ok(magnitude_db.iter().map(|m| m - mean).collect())
}

// ---------------------------------------------------------------------------
// Room additions (room-dsp/8) — additive. Nothing above this line changes.
// ---------------------------------------------------------------------------

/// Spatially-aligned measurement set. Constructible ONLY via `align_spl`, so a
/// caller cannot power-average unaligned data.
///
/// `#[non_exhaustive]` is what enforces that: it blocks struct-literal
/// construction outside this crate while leaving the fields readable. Skipping
/// alignment does not merely tilt the mean — near positions dominate the power
/// average AND inflate σ(f), corrupting the confidence metric `authority.rs`
/// depends on, which would silently make ParaEQ back off from features that are
/// genuinely correctable. That failure is invisible on a plot, so the compiler
/// prevents it instead.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AlignedSet {
    pub measurements_db: Vec<Vec<f64>>,
    pub offsets_db: Vec<f64>,
    pub reference_band: (f64, f64),
}

/// Remove overall level differences due to different source distances.
/// MANDATORY before any spatial average. Default band: `(200.0, 2000.0)` — above
/// the modal region (so position-dependent modal scatter cannot drive the
/// alignment) and below the directivity/air-absorption region.
///
/// `offset_j = band_mean_j − mean_of_all_band_means`; `m_j −= offset_j`. This
/// removes RELATIVE level differences while preserving the ensemble's absolute
/// level, so the offsets sum to zero. `normalize_to_reference_band` is the
/// existing primitive reused for the band mean — but it ZEROES the band mean
/// rather than aligning to the ensemble mean, so this is a new function, not a
/// rename.
pub fn align_spl(
    measurements_db: &[Vec<f64>],
    freqs: &[f64],
    band: (f64, f64),
) -> Result<AlignedSet, DspError> {
    let _ = (measurements_db, freqs, band);
    unimplemented!("room-dsp/8 — lands in Stage 4")
}

/// Power/RMS spatial average. Room path ONLY.
/// `out_dB[i] = 10·log10( (1/N)·Σ_j 10^(m_j[i]/10) )`
///
/// Null-RESISTANT, not null-immune. With `k` of `N` positions nulled by `d` dB:
/// `P_dB = 10·log10( ((N−k) + k·10^(d/10)) / N )`, which decreases monotonically
/// toward the floor `10·log10((N−k)/N)` but never attains it. For `k = N` the
/// null passes through EXACTLY — a null at every seat is a real feature, not
/// spatial scatter.
pub fn average_measurements_rms(set: &AlignedSet) -> Vec<f64> {
    let _ = set;
    unimplemented!("room-dsp/8 — lands in Stage 4")
}

/// Per-frequency inter-position standard deviation (population, ddof=0), dB.
/// The confidence signal `authority.rs` consumes: low σ ⇒ present at every
/// position ⇒ correctable; high σ ⇒ a one-position interference artifact.
pub fn sigma_db(set: &AlignedSet) -> Vec<f64> {
    let _ = set;
    unimplemented!("room-dsp/8 — lands in Stage 4")
}

/// Coherent (vector) average. ALWAYS `Err` for `n > 1` — a tripwire, kept so the
/// error is discoverable rather than the operation reinvented. It collapses
/// toward the incoherent floor `−10·log10(N)` once position spread approaches a
/// wavelength (−10.94 dB at 1.5 kHz for ±40 cm).
pub fn average_measurements_vector(
    measurements: &[Vec<Complex<f64>>],
) -> Result<Vec<Complex<f64>>, DspError> {
    let _ = measurements;
    unimplemented!("room-dsp/8 — lands in Stage 4")
}

pub enum Smoothing {
    /// Bit-exact legacy path: routes to the existing boxcar
    /// (`fractional_octave_smooth`). Coupler path. Fixtures untouched.
    Fixed(u32),
    /// Constant-Q Gaussian, O(N·K) Alvarez–Mazorra recursive.
    Gaussian {
        fraction: f64,
    },
    /// REW's variable profile: 1/48 oct <100 Hz, 1/6 at 1 kHz, 1/3 >10 kHz,
    /// log-interpolated. Fine in the bass and coarse in the treble — the INVERSE
    /// of psychoacoustic smoothing, and that inversion is the point: it hands the
    /// corrector fine detail exactly where it has authority (modal peaks) and
    /// hides detail where it does not.
    Variable,
    None,
}

pub fn smooth(magnitude_db: &[f64], grid: &LogGrid, mode: Smoothing) -> Result<Vec<f64>, DspError> {
    let _ = (magnitude_db, grid, mode);
    unimplemented!("room-dsp/8 — lands in Stage 4")
}
