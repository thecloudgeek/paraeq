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
//! `sigma_db` against numpy references at 1e-12 and the Alvarez–Mazorra
//! Gaussian against `scipy.ndimage.gaussian_filter1d` at the method's MEASURED
//! accuracy — 0.03/0.06/0.13 dB for σ = 2/8/32 bins (the AM recursion is by
//! construction an approximation, and its second-difference discretization
//! floor makes the spec's 1e-3 dB unattainable at any K; the transfer-function
//! argument lives in `test_fr_room.rs`). Tier 3 pins the power-mean inequality
//! and the null-floor formula.

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

/// Default `align_spl` band: 200–2000 Hz — above the modal region (so
/// position-dependent modal scatter cannot drive the alignment) and below the
/// directivity/air-absorption region.
pub const DEFAULT_SPL_ALIGN_BAND: (f64, f64) = (200.0, 2000.0);

/// FWHM of a Gaussian per unit σ: `2·√(2·ln 2)` ≈ 2.3548.
///
/// The bridge between a `1/n`-octave smoothing request and the Gaussian's σ:
/// `Smoothing::Gaussian { fraction }` (and each bin of `Variable`) smooths
/// with a Gaussian whose FWHM spans `fraction` octaves, i.e.
/// `σ_octaves = fraction / GAUSSIAN_FWHM_PER_SIGMA` and
/// `σ_bins = σ_octaves · ppo` on the uniform octave axis. This mapping is
/// fr.rs's own design decision (the same FWHM convention `fdw.rs` documents);
/// the Tier-2 fixtures pin the KERNEL at a given σ_bins, deliberately not
/// this mapping.
pub const GAUSSIAN_FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949_3;

/// Spatially-aligned measurement set. Constructible ONLY via `align_spl`, so a
/// caller cannot power-average unaligned data.
///
/// The fields are PRIVATE, not `pub` — that is what enforces the invariant. The
/// spec's API sketch shows `pub` fields, but `#[non_exhaustive]` alone would
/// only block struct-literal construction: it leaves existing `pub` fields
/// writable, so `set.measurements_db = unaligned` (or a post-alignment
/// `set.measurements_db.remove(i)`) would compile and re-introduce unaligned
/// data. Skipping alignment does not merely tilt the mean — near positions
/// dominate the power average AND inflate σ(f), corrupting the confidence
/// metric `authority.rs` depends on, which would silently make ParaEQ back off
/// from features that are genuinely correctable. That failure is invisible on a
/// plot, so read-only accessors + the single `align_spl` constructor prevent it
/// at the type level; the spec's own `pub`-field sketch is honoured in intent,
/// not letter.
#[derive(Clone, Debug)]
pub struct AlignedSet {
    measurements_db: Vec<Vec<f64>>,
    offsets_db: Vec<f64>,
    reference_band: (f64, f64),
}

impl AlignedSet {
    /// The aligned per-position curves — read-only; mutating them would defeat
    /// the alignment the type exists to guarantee.
    pub fn measurements_db(&self) -> &[Vec<f64>] {
        &self.measurements_db
    }

    /// The per-position level offsets `align_spl` removed. Sum to zero (the
    /// ensemble's absolute level is preserved).
    pub fn offsets_db(&self) -> &[f64] {
        &self.offsets_db
    }

    /// The band the alignment was computed over.
    pub fn reference_band(&self) -> (f64, f64) {
        self.reference_band
    }
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
    if measurements_db.is_empty() {
        return Err(DspError::InvalidInput("no measurements to align".into()));
    }
    for (j, m) in measurements_db.iter().enumerate() {
        if m.len() != freqs.len() {
            return Err(DspError::InvalidInput(format!(
                "measurement {j} has {} points, expected {} (the freqs axis length)",
                m.len(),
                freqs.len()
            )));
        }
        if m.iter().any(|v| !v.is_finite()) {
            return Err(DspError::InvalidInput(format!(
                "measurement {j} contains non-finite values"
            )));
        }
    }
    if freqs.iter().any(|f| !f.is_finite()) {
        return Err(DspError::InvalidInput(
            "align_spl: freqs contain non-finite values".into(),
        ));
    }
    let band_means = measurements_db
        .iter()
        .map(|m| band_mean(freqs, m, band))
        .collect::<Result<Vec<f64>, DspError>>()?;
    let grand_mean = band_means.iter().sum::<f64>() / band_means.len() as f64;
    let offsets_db: Vec<f64> = band_means.iter().map(|bm| bm - grand_mean).collect();
    let aligned = measurements_db
        .iter()
        .zip(&offsets_db)
        .map(|(m, o)| m.iter().map(|v| v - o).collect())
        .collect();
    Ok(AlignedSet {
        measurements_db: aligned,
        offsets_db,
        reference_band: band,
    })
}

/// Mean level over the inclusive band `[low_hz, high_hz]` — the same
/// band-selection arithmetic `normalize_to_reference_band` applies (that
/// function ZEROES the band mean rather than aligning to an ensemble mean,
/// which is why `align_spl` is a new function, not a rename; its Tier-1
/// behaviour is frozen, so the selection logic is mirrored here instead of
/// refactored out of it).
fn band_mean(
    freqs: &[f64],
    magnitude_db: &[f64],
    (low_hz, high_hz): (f64, f64),
) -> Result<f64, DspError> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for (f, m) in freqs.iter().zip(magnitude_db) {
        if *f >= low_hz && *f <= high_hz {
            sum += m;
            count += 1;
        }
    }
    if count == 0 {
        return Err(DspError::InvalidInput(format!(
            "no bins in reference band {low_hz}-{high_hz} Hz"
        )));
    }
    Ok(sum / count as f64)
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
    let n_pos = set.measurements_db.len();
    let bins = set.measurements_db.first().map_or(0, Vec::len);
    let mut power_sum = vec![0.0; bins];
    for m in &set.measurements_db {
        for (p, v) in power_sum.iter_mut().zip(m) {
            *p += 10f64.powf(v / 10.0);
        }
    }
    power_sum
        .iter()
        .map(|p| 10.0 * (p / n_pos as f64).log10())
        .collect()
}

/// Per-frequency inter-position standard deviation (population, ddof=0), dB.
/// The confidence signal `authority.rs` consumes: low σ ⇒ present at every
/// position ⇒ correctable; high σ ⇒ a one-position interference artifact.
pub fn sigma_db(set: &AlignedSet) -> Vec<f64> {
    let n_pos = set.measurements_db.len() as f64;
    let bins = set.measurements_db.first().map_or(0, Vec::len);
    let mut mean = vec![0.0; bins];
    for m in &set.measurements_db {
        for (a, v) in mean.iter_mut().zip(m) {
            *a += v;
        }
    }
    for a in &mut mean {
        *a /= n_pos;
    }
    let mut var = vec![0.0; bins];
    for m in &set.measurements_db {
        for ((s, v), a) in var.iter_mut().zip(m).zip(&mean) {
            let d = v - a;
            *s += d * d;
        }
    }
    var.iter().map(|s| (s / n_pos).sqrt()).collect()
}

/// Coherent (vector) average. ALWAYS `Err` for `n > 1` — a tripwire, kept so the
/// error is discoverable rather than the operation reinvented. It collapses
/// toward the incoherent floor `−10·log10(N)` once position spread approaches a
/// wavelength (−10.94 dB at 1.5 kHz for ±40 cm).
pub fn average_measurements_vector(
    measurements: &[Vec<Complex<f64>>],
) -> Result<Vec<Complex<f64>>, DspError> {
    match measurements {
        [] => Err(DspError::InvalidInput(
            "no measurements to vector-average".into(),
        )),
        [one] => Ok(one.clone()),
        _ => Err(DspError::InvalidInput(
            "coherent (vector) averaging across positions is rejected by design: \
             once position spread approaches a wavelength it collapses toward the \
             incoherent floor -10*log10(N) (-10.94 dB at 1.5 kHz for +/-40 cm). \
             Use align_spl + average_measurements_rms for the room path."
                .into(),
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Smoothing {
    /// Bit-exact legacy path: routes to the existing boxcar
    /// (`fractional_octave_smooth`). Coupler path. Fixtures untouched.
    /// `Fixed(0)` is rejected by `smooth` (a 1/0-octave window is meaningless;
    /// the frozen boxcar itself would degenerate to a whole-range average).
    Fixed(u32),
    /// Constant-Q Gaussian, O(N·K) Alvarez–Mazorra recursive. `fraction` is
    /// the smoothing bandwidth in octaves (e.g. `1.0 / 6.0` for 1/6-octave),
    /// mapped to σ by the FWHM convention ([`GAUSSIAN_FWHM_PER_SIGMA`]).
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

/// Smooth `magnitude_db` (dB domain) on `grid`. The Gaussian and Variable
/// modes exploit the grid's uniform octave axis ([`LogGrid::octave_axis`]):
/// a constant σ in BINS there IS constant-Q smoothing — the same coordinate
/// trick `fdw.rs` uses, and why the boxcar's O(N²) re-scan is unnecessary.
pub fn smooth(magnitude_db: &[f64], grid: &LogGrid, mode: Smoothing) -> Result<Vec<f64>, DspError> {
    if magnitude_db.len() != grid.len() {
        return Err(DspError::InvalidInput(format!(
            "smooth: {} magnitude points vs {} grid bins",
            magnitude_db.len(),
            grid.len()
        )));
    }
    if magnitude_db.iter().any(|v| !v.is_finite()) {
        return Err(DspError::InvalidInput(
            "smooth: magnitude contains non-finite values".into(),
        ));
    }
    match mode {
        Smoothing::Fixed(0) => Err(DspError::InvalidInput(
            "smooth: Fixed(0) is not a valid octave fraction".into(),
        )),
        Smoothing::Fixed(n) => Ok(fractional_octave_smooth(magnitude_db, grid.freqs(), n)),
        Smoothing::Gaussian { fraction } => {
            if !fraction.is_finite() || fraction <= 0.0 {
                return Err(DspError::InvalidInput(format!(
                    "smooth: Gaussian fraction must be finite and > 0, got {fraction}"
                )));
            }
            let sigma_bins =
                fraction / GAUSSIAN_FWHM_PER_SIGMA * f64::from(grid.points_per_octave());
            Ok(am_gaussian_smooth(magnitude_db, sigma_bins))
        }
        Smoothing::Variable => Ok(variable_gaussian_smooth(magnitude_db, grid)),
        Smoothing::None => Ok(magnitude_db.to_vec()),
    }
}

/// Alvarez–Mazorra recursive Gaussian, K = 4 passes with Getreuer's
/// q-correction (IPOL 2013 survey, `gaussian_conv_am`): each pass is a
/// causal-plus-anticausal first-order recursion, O(N·K) total regardless of
/// σ, vs the boxcar's measured 2586 ms for 65536 bins.
///
/// ```text
/// q = σ·(1 + (0.3165K + 0.5695)/(K + 0.7818)²)      (Getreuer's regression)
/// λ = q²/(2K)
/// ν = (1 + 2λ − √(1 + 4λ)) / (2λ)
/// ```
///
/// Boundary handling is AM's own published treatment: constant (Neumann)
/// extension, i.e. the causal init `u₀ = x₀/(1−ν)` (the geometric tail of a
/// constant signal) and its anticausal mirror — exact on constant data, which
/// is precisely the regime the Tier-2 fixtures put at the edges.
///
/// The per-pass DC gain is 1/(1−ν)² and λ(1−ν)² = ν exactly, so the final
/// (ν/λ)^K scale makes the composite unit-gain: flat spectra pass through
/// bit-exactly at 0 dB and to float noise elsewhere.
fn am_gaussian_smooth(magnitude_db: &[f64], sigma_bins: f64) -> Vec<f64> {
    const K: usize = 4;
    let mut y = magnitude_db.to_vec();
    // Below ~1e-3 bins the kernel is a delta well past f64 resolution AND the
    // ν formula hits catastrophic cancellation (ν rounds to 0, which would
    // zero the scale and the data with it). Identity is the exact limit.
    if sigma_bins < 1e-3 || y.is_empty() {
        return y;
    }
    let k = K as f64;
    let kp = k + 0.7818;
    let q = sigma_bins * (1.0 + (0.3165 * k + 0.5695) / (kp * kp));
    let lambda = q * q / (2.0 * k);
    let nu = (1.0 + 2.0 * lambda - (1.0 + 4.0 * lambda).sqrt()) / (2.0 * lambda);
    let boundary = 1.0 / (1.0 - nu);
    let scale = (nu / lambda).powi(K as i32);
    let last = y.len() - 1;
    for _ in 0..K {
        y[0] *= boundary;
        for i in 1..=last {
            y[i] += nu * y[i - 1];
        }
        y[last] *= boundary;
        for i in (0..last).rev() {
            y[i] += nu * y[i + 1];
        }
    }
    for v in &mut y {
        *v *= scale;
    }
    y
}

/// REW's variable-smoothing fraction as a function of frequency: 1/48 octave
/// below 100 Hz, 1/6 at 1 kHz, 1/3 above 10 kHz, linear in log f between the
/// anchors (each span is exactly one decade), clamped outside.
fn variable_fraction(f: f64) -> f64 {
    const FRAC_BASS: f64 = 1.0 / 48.0;
    const FRAC_MID: f64 = 1.0 / 6.0;
    const FRAC_TREBLE: f64 = 1.0 / 3.0;
    if f <= 100.0 {
        FRAC_BASS
    } else if f <= 1000.0 {
        FRAC_BASS + (FRAC_MID - FRAC_BASS) * (f / 100.0).log10()
    } else if f <= 10_000.0 {
        FRAC_MID + (FRAC_TREBLE - FRAC_MID) * (f / 1000.0).log10()
    } else {
        FRAC_TREBLE
    }
}

/// The Variable profile: per-bin σ from [`variable_fraction`], applied as a
/// direct truncated sampled-Gaussian convolution (scipy's truncate=4.0
/// convention, normalized weights, clamp-to-edge extension).
///
/// Direct rather than Alvarez–Mazorra because AM's recursion coefficients
/// assume ONE σ for the whole pass; a per-bin ν would no longer have a
/// Gaussian limit. The cost is O(N·W) with W = 8σ_max+1 ≈ 110 bins at
/// 96 ppo — and N here is the log grid (~10³ bins, ≤10⁶ by construction),
/// not the 65536-bin linear FFT axis that made the boxcar hang, so the
/// spec's performance complaint cannot re-arise through this path.
fn variable_gaussian_smooth(magnitude_db: &[f64], grid: &LogGrid) -> Vec<f64> {
    let ppo = f64::from(grid.points_per_octave());
    let n = magnitude_db.len() as isize;
    grid.freqs()
        .iter()
        .enumerate()
        .map(|(i, &f)| {
            let sigma = variable_fraction(f) / GAUSSIAN_FWHM_PER_SIGMA * ppo;
            let radius = (4.0 * sigma).ceil() as isize;
            let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);
            let mut num = 0.0;
            let mut den = 0.0;
            for m in -radius..=radius {
                let w = (-(m * m) as f64 * inv_two_sigma_sq).exp();
                let j = (i as isize + m).clamp(0, n - 1) as usize;
                num += w * magnitude_db[j];
                den += w;
            }
            num / den
        })
        .collect()
}
