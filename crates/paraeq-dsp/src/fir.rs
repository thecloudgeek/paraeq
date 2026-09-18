//! FIR correction design: frequency sampling (linear phase) + homomorphic
//! minimum phase. Oracles: prototype/paraeq/correction/fir_filter.py and
//! scipy.signal.minimum_phase (homomorphic, half=True), pinned to scipy
//! 1.18.0 by prototype/pyproject.toml's `fixtures` extra and enforced by
//! generate_fixtures.py at startup (fixtures/manifest.json only records
//! what actually ran — it is provenance, not the pin).
//!
//! Test tier: 1 + 2 + 3.
//!
//! - **Tier 1, FROZEN**: `design_fir_correction` and everything private it
//!   reaches, against `fixtures/fir/{linear,minimum}_{512,4096}`. Not
//!   regenerated, not refactored.
//! - **Tier 2**: [`minimum_phase_spectrum`], against
//!   `scipy.signal.minimum_phase(h, method="homomorphic", n_fft, half=False)`
//!   (`fixtures/fir/min_phase_spectrum`). **half=False, NOT the half=True call
//!   `minimum_phase_homomorphic` ports** — that is the same cepstrum with a ½
//!   on the log magnitude, so it returns a filter of half the order whose
//!   magnitude is `√|H|` and whose phase is therefore HALF the minimum phase of
//!   `|H|`. It is correct where it lives, because `design_fir_correction`
//!   squares its target magnitude first and the two halvings cancel; reached
//!   for directly by `fr::excess_group_delay_s` it would halve every EGD
//!   number, silently. `half` requires scipy ≥ 1.14 and the pin is 1.18.0.
//! - **Tier 3**: the two invariants in `tests/test_fir.rs` that state the trap
//!   rather than assume it — half=False preserves the input magnitude (half=True
//!   would return its square root), and the shipped half=True path carries
//!   exactly half the phase [`minimum_phase_spectrum`] does on the same
//!   prototype.

use realfft::RealFftPlanner;
use rustfft::{num_complex::Complex, FftPlanner};

/// Phase behavior for [`design_fir_correction`].
pub enum FirPhase {
    Linear,
    Minimum,
}

/// Design a FIR correction filter from a correction curve, via frequency
/// sampling. Port of `prototype/paraeq/correction/fir_filter.py::design_fir_correction`.
///
/// Divergence from the Python: the Python signature also takes a `freqs`
/// parameter that is documented but never used by the implementation
/// (resampling onto the target bin count is purely positional); the Rust
/// port drops it.
pub fn design_fir_correction(correction_db: &[f64], n_taps: usize, phase: FirPhase) -> Vec<f64> {
    let magnitude: Vec<f64> = correction_db
        .iter()
        .map(|db| 10f64.powf(db / 20.0))
        .collect();
    match phase {
        FirPhase::Linear => {
            let bins = n_taps / 2 + 1;
            let resampled = resample(&magnitude, bins);
            windowed_ir(&resampled, n_taps, true)
        }
        FirPhase::Minimum => {
            let n_proto = 2 * n_taps - 1;
            let bins = n_proto / 2 + 1;
            let resampled = resample(&magnitude, bins);
            // Homomorphic half=True takes the square root of the prototype's
            // magnitude spectrum (via cepstrum halving); square the spectrum
            // so sqrt(mag^2) == mag after conversion. Order matters:
            // `fir_filter.py::_design_minimum_phase` resamples the ORIGINAL
            // magnitude first (`target_mag = np.interp(...)`) and only then
            // squares it (`target_mag_sq = target_mag ** 2`). Squaring before
            // resampling would be wrong — squaring doesn't commute with
            // (linear) interpolation — so the square happens on `resampled`
            // here, not on `magnitude`.
            let squared: Vec<f64> = resampled.iter().map(|m| m * m).collect();
            let proto = windowed_ir(&squared, n_proto, false);
            let n_fft = 1usize << ((4 * n_proto) as f64).log2().ceil() as u32;
            let min_ph = minimum_phase_homomorphic(&proto, n_fft);
            if min_ph.len() >= n_taps {
                min_ph.into_iter().take(n_taps).collect()
            } else {
                let mut out = vec![0.0; n_taps];
                out[..min_ph.len()].copy_from_slice(&min_ph);
                out
            }
        }
    }
}

/// Positional resample onto `bins` points: `np.interp(linspace(0, 1, bins),
/// linspace(0, 1, len(magnitude)), magnitude)`.
fn resample(magnitude: &[f64], bins: usize) -> Vec<f64> {
    (0..bins)
        .map(|i| {
            let pos = i as f64 / (bins - 1) as f64 * (magnitude.len() - 1) as f64;
            let j = pos.floor() as usize;
            if j + 1 >= magnitude.len() {
                magnitude[magnitude.len() - 1]
            } else {
                let t = pos - j as f64;
                magnitude[j] * (1.0 - t) + magnitude[j + 1] * t
            }
        })
        .collect()
}

/// Frequency-sampling impulse response from an already-resampled half-spectrum:
/// irfft, roll, Hann window, and (optionally) peak-normalize + symmetrize.
/// Verified against `fir_filter.py`: `_design_linear_phase` (the public linear
/// path) does peak-normalize and symmetrize; `_design_minimum_phase` builds
/// its prototype inline via the same irfft/roll/hanning steps but WITHOUT
/// normalizing or symmetrizing, feeding the raw windowed prototype straight
/// into `minimum_phase(...)`.
fn windowed_ir(resampled_spectrum: &[f64], n: usize, normalize: bool) -> Vec<f64> {
    // irfft(real spectrum): np.fft.irfft(spectrum, n)
    let mut ir = irfft(resampled_spectrum, n);
    // roll by n/2
    ir.rotate_right(n / 2);
    // symmetric Hann (np.hanning): 0.5 - 0.5*cos(2*pi*i/(n-1))
    for (i, v) in ir.iter_mut().enumerate() {
        *v *= 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
    }
    if normalize {
        let peak = ir.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        if peak > 0.0 {
            for v in &mut ir {
                *v /= peak;
            }
        }
        let rev: Vec<f64> = ir.iter().rev().copied().collect();
        for (v, r) in ir.iter_mut().zip(&rev) {
            *v = (*v + r) / 2.0;
        }
    }
    ir
}

/// Inverse real FFT of a purely-real, non-negative half-spectrum of `bins =
/// n/2 + 1` values (`np.fft.irfft(spectrum, n)` with `spectrum.imag == 0`
/// everywhere, which is all this module ever feeds it). `n` may be even
/// (the linear-phase path) or odd (`n_proto = 2*n_taps - 1`, always odd, for
/// the minimum-phase prototype) — `realfft` handles both and matches numpy's
/// irfft to machine epsilon for both parities (verified against the golden
/// fixtures at every tap, not just within the 1e-6 spec tolerance).
fn irfft(spectrum: &[f64], n: usize) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(n);
    let mut spec: Vec<Complex<f64>> = spectrum.iter().map(|m| Complex::new(*m, 0.0)).collect();
    let mut out = ifft.make_output_vec();
    ifft.process(&mut spec, &mut out).unwrap();
    for v in &mut out {
        *v /= n as f64;
    }
    out
}

/// scipy.signal.minimum_phase(h, method="homomorphic", n_fft, half=True), exact port.
fn minimum_phase_homomorphic(h: &[f64], n_fft: usize) -> Vec<f64> {
    let mut planner = FftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let inv = planner.plan_fft_inverse(n_fft);

    let mut buf: Vec<Complex<f64>> = h
        .iter()
        .map(|v| Complex::new(*v, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
        .take(n_fft)
        .collect();
    fwd.process(&mut buf);
    let mut mag: Vec<f64> = buf.iter().map(|c| c.norm()).collect();
    let min_pos = mag
        .iter()
        .copied()
        .filter(|v| *v > 0.0)
        .fold(f64::INFINITY, f64::min);
    for v in &mut mag {
        *v = (*v + 1e-7 * min_pos).ln() * 0.5; // += eps, log, half=True
    }
    // real cepstrum: Re(IFFT(log-mag))
    let mut cep: Vec<Complex<f64>> = mag.iter().map(|v| Complex::new(*v, 0.0)).collect();
    inv.process(&mut cep);
    for c in &mut cep {
        *c /= n_fft as f64;
    }
    // Homomorphic window (scipy `_fir_filter_design.py`, `minimum_phase`):
    //   win[0] = 1
    //   win[1..stop] = 2                       (stop = n_fft // 2)
    //   win[stop] = 1 + (n_fft % 2)             (1 when n_fft even, 2 when odd)
    //   win[stop+1..] = 0
    // n_fft is always a power of two here (even), so win[stop] == 1.
    let stop = n_fft / 2;
    let mut windowed: Vec<Complex<f64>> = vec![Complex::new(0.0, 0.0); n_fft];
    windowed[0] = Complex::new(cep[0].re, 0.0);
    for i in 1..stop {
        windowed[i] = Complex::new(2.0 * cep[i].re, 0.0);
    }
    let nyquist_weight = 1.0 + (n_fft % 2) as f64;
    windowed[stop] = Complex::new(nyquist_weight * cep[stop].re, 0.0);
    // exp(FFT(windowed)), then IFFT, take real
    fwd.process(&mut windowed);
    for c in &mut windowed {
        *c = c.exp();
    }
    inv.process(&mut windowed);
    let scale = 1.0 / n_fft as f64;
    let n_out = h.len().div_ceil(2);
    windowed.iter().take(n_out).map(|c| c.re * scale).collect()
}

/// The minimum-phase spectrum of `h` on the rfft axis of `n_fft` — Tier 2
/// against `scipy.signal.minimum_phase(h, method="homomorphic", n_fft=n_fft,
/// half=False)` followed by `np.fft.rfft(..., n_fft)`.
///
/// Same arguments as the delegate, so the fixture grades the whole call.
/// `fr::excess_group_delay_s` is the consumer: the phase of this spectrum is
/// the `φ_min` an excess-group-delay trace subtracts from the measured phase.
///
/// # half=False, and why the private port below is not simply made `pub`
///
/// [`minimum_phase_homomorphic`] is the same cepstral chain with `half=True` —
/// the `* 0.5` on the log magnitude. That halving makes its output's magnitude
/// `√|H|` and therefore its phase exactly half the minimum phase of `|H|`. It
/// is right where it is used, because `design_fir_correction` hands it a
/// prototype built from the target magnitude SQUARED and the two halvings
/// cancel. Exposed and called directly, it would produce `φ_meas − ½·φ_min`:
/// wrong at every frequency, plausible on a plot, and invisible unless a test
/// states the factor of two. `tests/test_fir.rs` states it.
///
/// The Tier-1 frozen function is left byte-identical; this is a new function
/// beside it, per the measurement-suite spec's Tier-1 rule ("Room behaviour
/// arrives as **new functions, never modified ones**"). The duplicated cepstrum
/// below is that rule's cost, paid deliberately.
///
/// # The truncation is scipy's, and it is kept
///
/// `minimum_phase(..., half=False)` returns `h_minimum[:len(h)]` — the
/// reconstruction cut back to the input's order, because the minimum-phase
/// counterpart of an order-N FIR is itself an order-N FIR. The untruncated
/// `exp(FFT(windowed cepstrum))` differs from the rfft of those taps by up to
/// 1.3e-3 relative at n_fft = 4096 (measured), so a variant that skipped the
/// truncation could not be graded against this delegate at all. The round trip
/// is the price of a real oracle.
pub fn minimum_phase_spectrum(
    h: &[f64],
    n_fft: usize,
) -> Result<Vec<Complex<f64>>, crate::DspError> {
    // scipy's own two preconditions, mirrored: "h must be 1-D and at least 2
    // samples long" (it checks `shape[0] <= 2`) and "n_fft must be at least
    // len(h)".
    if h.len() <= 2 {
        return Err(crate::DspError::InvalidInput(format!(
            "minimum_phase_spectrum: h has {} taps; at least 3 are required",
            h.len()
        )));
    }
    if n_fft < h.len() {
        return Err(crate::DspError::InvalidInput(format!(
            "minimum_phase_spectrum: n_fft {n_fft} is below len(h) {}",
            h.len()
        )));
    }
    if h.iter().any(|v| !v.is_finite()) {
        return Err(crate::DspError::InvalidInput(
            "minimum_phase_spectrum: h contains non-finite values".into(),
        ));
    }
    if h.iter().all(|v| *v == 0.0) {
        // The log guard is `+ 1e-7 * min(|H| over the strictly positive bins)`.
        // With an all-zero filter there are no positive bins, so the guard is
        // undefined and the cepstrum would be log(0) throughout. scipy walks
        // into a nan here; refusing is the honest answer.
        return Err(crate::DspError::InvalidInput(
            "minimum_phase_spectrum: h is all zeros; it has no minimum-phase counterpart".into(),
        ));
    }
    let taps = minimum_phase_half_false(h, n_fft);
    Ok(rfft(&taps, n_fft))
}

/// `scipy.signal.minimum_phase(h, method="homomorphic", n_fft, half=False)`,
/// exact port. Differs from [`minimum_phase_homomorphic`] in exactly two
/// places, both of which `half` controls in scipy: the log magnitude is NOT
/// halved, and `n_out` is `len(h)` rather than `ceil(len(h)/2)`.
///
/// Written out rather than sharing a body with the half=True port: that port is
/// Tier-1 fixture-pinned through `design_fir_correction`, and refactoring a
/// frozen function to host a flag it never sets would edit pinned code for no
/// behavioural gain. `minimum_phase_spectrum_is_twice_the_phase_of_the_half_true_port`
/// is the test that keeps the two from drifting apart instead.
fn minimum_phase_half_false(h: &[f64], n_fft: usize) -> Vec<f64> {
    let mut planner = FftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let inv = planner.plan_fft_inverse(n_fft);

    let mut buf: Vec<Complex<f64>> = h
        .iter()
        .map(|v| Complex::new(*v, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
        .take(n_fft)
        .collect();
    fwd.process(&mut buf);
    let mut mag: Vec<f64> = buf.iter().map(|c| c.norm()).collect();
    let min_pos = mag
        .iter()
        .copied()
        .filter(|v| *v > 0.0)
        .fold(f64::INFINITY, f64::min);
    for v in &mut mag {
        *v = (*v + 1e-7 * min_pos).ln(); // += eps, log; NO *= 0.5 (half=False)
    }
    // real cepstrum: Re(IFFT(log-mag))
    let mut cep: Vec<Complex<f64>> = mag.iter().map(|v| Complex::new(*v, 0.0)).collect();
    inv.process(&mut cep);
    for c in &mut cep {
        *c /= n_fft as f64;
    }
    // Homomorphic window — identical to the half=True port's; `half` does not
    // touch it. win[0] = 1, win[1..stop] = 2, win[stop] = 1 + (n_fft % 2),
    // win[stop+1..] = 0.
    let stop = n_fft / 2;
    let mut windowed: Vec<Complex<f64>> = vec![Complex::new(0.0, 0.0); n_fft];
    windowed[0] = Complex::new(cep[0].re, 0.0);
    for i in 1..stop {
        windowed[i] = Complex::new(2.0 * cep[i].re, 0.0);
    }
    let nyquist_weight = 1.0 + (n_fft % 2) as f64;
    windowed[stop] = Complex::new(nyquist_weight * cep[stop].re, 0.0);
    fwd.process(&mut windowed);
    for c in &mut windowed {
        *c = c.exp();
    }
    inv.process(&mut windowed);
    let scale = 1.0 / n_fft as f64;
    // half=False: n_out = len(h), the full input order, not half of it.
    windowed
        .iter()
        .take(h.len())
        .map(|c| c.re * scale)
        .collect()
}

/// `np.fft.rfft(x, n)`: zero-pad (never truncate — `x` is `n` or shorter at
/// every call site here) and take the non-negative-frequency half.
fn rfft(x: &[f64], n: usize) -> Vec<Complex<f64>> {
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = vec![0.0; n];
    let m = x.len().min(n);
    input[..m].copy_from_slice(&x[..m]);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    spectrum
}
