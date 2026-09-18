//! Regularized (Wiener) spectral-division deconvolution.
//! Oracle: prototype/paraeq/measurement/deconvolution.py
//!
//! Test tiers: Tier 1 (frozen prototype fixture
//! `fixtures/deconvolution/delta_plus_tail`, 1e-9) for the Wiener core, which
//! [`wiener`] carries VERBATIM; Tier 3 (analytic: `sweep ⊛ δ(t₀)` deconvolves
//! to a peak at exactly t₀) for the time axis `deconvolve_ir` adds. Spec:
//! docs/specs/2026-07-15-room-dsp-design.md, "`deconvolution.rs` — reshape".

use crate::{
    gating::{detect_peak_with_lead, ImpulseResponse, PeakFallback},
    DspError,
};
use realfft::RealFftPlanner;

/// The pre-time-axis API, kept so the Tier-1 fixture test is untouched.
/// Delegates to [`deconvolve_ir`]; the samples are bit-for-bit identical.
///
/// Panics where `deconvolve_ir` errors (degenerate input: empty buffers, a
/// zero sample rate, or an IR whose peak is undetectable because it is
/// all-zero or non-finite). The original silently returned garbage on those
/// inputs; no caller feeds them.
#[deprecated(note = "use deconvolve_ir; this loses the time axis")]
pub fn deconvolve(recorded: &[f64], sweep: &[f64], sample_rate: u32) -> Vec<f64> {
    deconvolve_ir(recorded, sweep, sample_rate)
        .expect("deconvolve: degenerate input")
        .samples
}

/// Wiener deconvolution with a time axis: the frozen Tier-1 numerics of
/// [`wiener`], then rate-aware peak detection
/// ([`detect_peak_with_lead`] at the spec's 1 ms = `sample_rate / 1000`
/// fallback lead), packaged as an [`ImpulseResponse`] carrying `sample_rate`.
///
/// This spec-fixed signature returns [`ImpulseResponse`], which has no warning
/// slot, so the structured `gate.peak_fallback` warning is DISCARDED here.
/// The measurement session is required to surface that warning (see
/// `gating.rs`) — pipeline code calls [`deconvolve_ir_with_fallback`] instead.
pub fn deconvolve_ir(
    recorded: &[f64],
    sweep: &[f64],
    sample_rate: u32,
) -> Result<ImpulseResponse, DspError> {
    Ok(deconvolve_ir_with_fallback(recorded, sweep, sample_rate)?.0)
}

/// [`deconvolve_ir`] plus the structured `gate.peak_fallback` warning, which
/// the measurement session must surface to the user (returned, not raised,
/// per this crate's warning convention).
pub fn deconvolve_ir_with_fallback(
    recorded: &[f64],
    sweep: &[f64],
    sample_rate: u32,
) -> Result<(ImpulseResponse, Option<PeakFallback>), DspError> {
    if sample_rate == 0 {
        return Err(DspError::InvalidInput(
            "deconvolve_ir: sample_rate is 0".into(),
        ));
    }
    if recorded.is_empty() || sweep.is_empty() {
        return Err(DspError::InvalidInput(
            "deconvolve_ir: recorded and sweep must be non-empty".into(),
        ));
    }
    let samples = wiener(recorded, sweep);
    // Non-finite or all-zero deconvolution output errors here: no peak, no
    // time axis, nothing downstream could gate.
    let detection = detect_peak_with_lead(&samples, sample_rate as f64 / 1000.0)?;
    Ok((
        ImpulseResponse {
            samples,
            peak: detection.peak,
            sample_rate,
        },
        detection.fallback,
    ))
}

/// The Tier-1 Wiener core, VERBATIM from the original `deconvolve` (its
/// fixture parity is frozen): regularized spectral division with
/// `eps = 1e-10 · max power`.
fn wiener(recorded: &[f64], sweep: &[f64]) -> Vec<f64> {
    let n = recorded.len();
    let n_fft = (n + sweep.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let inv = planner.plan_fft_inverse(n_fft);

    let mut sweep_padded = vec![0.0; n_fft];
    sweep_padded[..sweep.len()].copy_from_slice(sweep);
    let mut s = fwd.make_output_vec();
    fwd.process(&mut sweep_padded, &mut s).unwrap();

    let power: Vec<f64> = s.iter().map(|c| c.norm_sqr()).collect();
    let eps = 1e-10 * power.iter().fold(0.0f64, |m, v| m.max(*v));

    let mut rec_padded = vec![0.0; n_fft];
    rec_padded[..n].copy_from_slice(recorded);
    let mut r = fwd.make_output_vec();
    fwd.process(&mut rec_padded, &mut r).unwrap();

    for ((ri, si), p) in r.iter_mut().zip(&s).zip(&power) {
        *ri *= si.conj() / (p + eps);
    }
    let mut ir = inv.make_output_vec();
    inv.process(&mut r, &mut ir).unwrap();
    ir.truncate(n);
    for v in &mut ir {
        *v /= n_fft as f64;
    }
    ir
}
