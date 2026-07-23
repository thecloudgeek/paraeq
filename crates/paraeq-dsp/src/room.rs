//! Room decay metrics: Schroeder backward integration, T60 via the ISO 3382
//! T20 fit, the Schroeder frequency, and the modal→statistical transition
//! range.
//!
//! # DISPLAY ONLY — this module does NOT gate authority
//!
//! Everything here is a display value and a sanity check: the UI may say
//! "your room's transition is around 180 Hz", and an implausible σ(f) profile
//! may be flagged against [`TransitionRange`]. **Authority comes from σ(f)
//! and nothing else.** Do not wire any output of this module into
//! `authority.rs` or any correction decision: the transition frequency marks
//! modal vs. statistical dominance — NOT minimum-phase vs. non-minimum-phase
//! behaviour. That refuted reasoning creeps back in precisely through this
//! seam (spec: docs/specs/2026-07-15-room-dsp-design.md, "`room.rs` — new").
//!
//! Test tiers: Tier 2 (`fixtures/room/schroeder_decay`, a numpy reverse-cumsum
//! reference, 1e-12) for [`schroeder_decay_db`]; Tier 3 (analytic — a
//! synthetic `noise · exp(−t·ln(1000)/T60)` IR has a KNOWN T60, and
//! `f_s = 2000·√(T60/V)` is closed-form) for the rest.

use crate::{gating::ImpulseResponse, DspError};

/// The fit's upper edge: −5 dB skips the direct sound and earliest
/// reflections, per ISO 3382 practice.
const FIT_TOP_DB: f64 = -5.0;
/// The fit's lower edge: −25 dB. Top-to-bottom is the 20 dB T20 range.
const FIT_BOTTOM_DB: f64 = -25.0;
/// `fit_r2` below this ⇒ the decay is not exponential ⇒ `usable = false`.
const MIN_FIT_R2: f64 = 0.95;
/// The decay must have fallen at least this far below [`FIT_BOTTOM_DB`] at
/// the floor probe (ISO 3382-2 wants ≥ 10 dB between the evaluation range
/// and the noise floor).
const FLOOR_HEADROOM_DB: f64 = 10.0;
/// Fewer fit points than this and a 20 dB decay is spanning < 0.2 ms at
/// 48 kHz — not a room, and not a fittable line.
const MIN_FIT_POINTS: usize = 10;

/// A T60 estimate from the Schroeder decay curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecayEstimate {
    /// `3 × t20_s` — the ISO 3382 T20 estimator's ×3 extrapolation to 60 dB.
    pub t60_s: f64,
    /// The range actually fitted (−5..−25 dB), extrapolated ×3 to 60 dB:
    /// this is the raw time to fall 20 dB along the fitted line
    /// (`−20 / slope`); `t60_s` is its ×3 extrapolation.
    pub t20_s: f64,
    pub fit_r2: f64,
    /// `false` when SNR or curvature make the fit untrustworthy
    /// (`fit_r2 < 0.95`, or the noise floor arrives before −25 dB).
    pub usable: bool,
}

/// Where a [`TransitionRange`] came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransitionSource {
    Measured { t60_s: f64, volume_m3: f64 },
    Fallback,
}

/// The modal→statistical transition, as a RANGE, not a number: the Schroeder
/// frequency is a statistical boundary whose leading constant is a convention
/// (the 2000 coefficient varies across the literature), so a single number
/// would imply precision the physics does not have. Display only — see the
/// module header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionRange {
    /// `0.5 · f_s`.
    pub low_hz: f64,
    /// `f_s`.
    pub center_hz: f64,
    /// `2.0 · f_s`.
    pub high_hz: f64,
    pub source: TransitionSource,
}

/// Schroeder backward integration: `E(t) = ∫_t^∞ h²(τ) dτ`, reported as
/// `10·log10(E(t)/E(0))` — a reverse cumulative sum of `h²` normalized to its
/// own maximum, which is `E(0)` (the total energy; `h²` is non-negative).
/// The summation order matches numpy's sequential `cumsum`, so the Tier-2
/// fixture pins this to 1e-12.
///
/// Degenerate inputs are the caller's lookout (display-only module): an empty
/// slice returns an empty vec, and an all-zero IR returns all-NaN (`0/0`);
/// [`estimate_t60`] rejects both before calling here.
pub fn schroeder_decay_db(ir: &[f64]) -> Vec<f64> {
    let mut energy = vec![0.0; ir.len()];
    let mut acc = 0.0;
    for (e, h) in energy.iter_mut().rev().zip(ir.iter().rev()) {
        acc += h * h;
        *e = acc;
    }
    let e0 = energy.first().copied().unwrap_or(0.0);
    for e in &mut energy {
        *e = 10.0 * (*e / e0).log10();
    }
    energy
}

/// T60 from a least-squares line over the Schroeder decay curve's
/// [−5, −25] dB range (the ISO 3382 T20 estimator), extrapolated ×3.
///
/// `usable` is `false` — the value is still returned, for display — when
/// `fit_r2 < 0.95` or the noise floor arrives before −25 dB. The floor test
/// is a deliberately simple probe, not Lundeby's iteration: the −25 dB
/// crossing must land in the first 90% of the record (later means it is the
/// end-of-integration plunge, not decay), and the curve at the 90% point must
/// sit ≥ 10 dB below −25 dB.
///
/// `band` is DEFERRED: octave-band filtering (ISO 3382 band-wise RT) is not
/// implemented, and silently returning a broadband number under a band label
/// would be a lie — so `Some(_)` is an explicit `Err`. Pass `None` for the
/// broadband estimate.
///
/// The whole sample buffer is integrated (matching the Tier-2 fixture); the
/// time axis enters only through the fitted slope, so leading delay before
/// the direct sound does not bias the estimate, though its noise energy
/// enters the integral like any other noise.
pub fn estimate_t60(
    ir: &ImpulseResponse,
    band: Option<(f64, f64)>,
) -> Result<DecayEstimate, DspError> {
    if band.is_some() {
        return Err(DspError::InvalidInput(
            "estimate_t60: band-limited decay (octave-band filtering per ISO 3382) \
             is not implemented; pass None for the broadband estimate"
                .into(),
        ));
    }
    if ir.sample_rate == 0 {
        return Err(DspError::InvalidInput(
            "estimate_t60: sample_rate is 0".into(),
        ));
    }
    if ir.samples.len() < 2 {
        return Err(DspError::InvalidInput(format!(
            "estimate_t60: IR of {} sample(s) has no decay",
            ir.samples.len()
        )));
    }
    if ir.samples.iter().any(|s| !s.is_finite()) {
        return Err(DspError::InvalidInput(
            "estimate_t60: non-finite sample in impulse response".into(),
        ));
    }
    if ir.samples.iter().all(|&s| s == 0.0) {
        return Err(DspError::InvalidInput(
            "estimate_t60: all-zero impulse response has no decay".into(),
        ));
    }

    let decay = schroeder_decay_db(&ir.samples);
    // First crossings; the curve is monotone non-increasing (reverse cumsum of
    // h² ≥ 0), so these delimit the [−5, −25] range.
    let top = decay
        .iter()
        .position(|&d| d <= FIT_TOP_DB)
        .ok_or_else(|| DspError::InvalidInput("estimate_t60: decay never reaches −5 dB".into()))?;
    let bottom = decay
        .iter()
        .position(|&d| d <= FIT_BOTTOM_DB)
        .ok_or_else(|| DspError::InvalidInput("estimate_t60: decay never reaches −25 dB".into()))?;
    let fit = &decay[top..=bottom];
    if fit.len() < MIN_FIT_POINTS {
        return Err(DspError::InvalidInput(format!(
            "estimate_t60: only {} samples between −5 and −25 dB; too few to fit",
            fit.len()
        )));
    }
    // A digital-silence tail makes the Schroeder curve jump to −∞ (log of zero
    // energy); if the first ≤ −25 dB crossing lands ON that −∞ sample, the fit
    // window carries it, `mean_y`/`slope` go non-finite, and the `slope < 0`
    // guard below (NaN fails every comparison) would let a NaN `DecayEstimate`
    // reach the display. Refuse here, matching the degenerate-input guards
    // above — the record decays into silence before −25 dB, so it cannot be fit.
    if fit.iter().any(|d| !d.is_finite()) {
        return Err(DspError::InvalidInput(
            "estimate_t60: decay reaches −∞ (digital-silence tail) before −25 dB; cannot fit"
                .into(),
        ));
    }

    // Least squares of decay_db against time. x is relative to `top` (a shift
    // does not move the slope) in seconds, so the slope is in dB/s.
    let sr = ir.sample_rate as f64;
    let n = fit.len() as f64;
    let mean_x = 0.5 * (fit.len() - 1) as f64 / sr;
    let mean_y = fit.iter().sum::<f64>() / n;
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for (k, &d) in fit.iter().enumerate() {
        let dx = k as f64 / sr - mean_x;
        let dy = d - mean_y;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    // fit[0] ≤ −5 while the pre-`bottom` part sits above −25 and fit's last
    // point at/below it, so y varies and x has ≥ MIN_FIT_POINTS distinct
    // values: sxx > 0 and syy > 0. Monotone decrease also forces slope < 0;
    // guard anyway rather than divide a display value by a surprise.
    let slope = sxy / sxx;
    // Explicit non-finite check so a NaN/∞ slope takes the error path rather
    // than dividing a display value by a surprise (the fit-window guard above
    // already rejects the −∞ source, but sxx == 0 could still produce one).
    if !slope.is_finite() || slope >= 0.0 {
        return Err(DspError::InvalidInput(format!(
            "estimate_t60: non-decaying fit (slope {slope} dB/s)"
        )));
    }
    let fit_r2 = (sxy * sxy) / (sxx * syy);
    let t20_s = -20.0 / slope;
    let t60_s = 3.0 * t20_s;

    // Floor probe at 90% of the record: see the doc comment.
    let probe = (decay.len() - 1) * 9 / 10;
    let floor_ok = bottom < probe && decay[probe] <= FIT_BOTTOM_DB - FLOOR_HEADROOM_DB;

    Ok(DecayEstimate {
        t60_s,
        t20_s,
        fit_r2,
        usable: fit_r2 >= MIN_FIT_R2 && floor_ok,
    })
}

/// `f_s = 2000·√(T60/V)` (SI: T60 in seconds, V in m³). Example:
/// `T60 = 0.4 s, V = 50 m³` → `2000·√0.008 ≈ 178.9 Hz`.
pub fn schroeder_frequency(t60_s: f64, volume_m3: f64) -> f64 {
    2000.0 * (t60_s / volume_m3).sqrt()
}

/// `0.5·f_s … 2·f_s` around the Schroeder frequency, or the fallback
/// (100–400 Hz around 200 Hz) when T60 or volume is unknown. Non-finite or
/// non-positive inputs count as unknown: a NaN range would be strictly worse
/// than the honest fallback.
pub fn transition_range(t60_s: Option<f64>, volume_m3: Option<f64>) -> TransitionRange {
    match (t60_s, volume_m3) {
        (Some(t60), Some(vol)) if t60.is_finite() && t60 > 0.0 && vol.is_finite() && vol > 0.0 => {
            let f_s = schroeder_frequency(t60, vol);
            TransitionRange {
                low_hz: 0.5 * f_s,
                center_hz: f_s,
                high_hz: 2.0 * f_s,
                source: TransitionSource::Measured {
                    t60_s: t60,
                    volume_m3: vol,
                },
            }
        }
        _ => TransitionRange {
            low_hz: 100.0,
            center_hz: 200.0,
            high_hz: 400.0,
            source: TransitionSource::Fallback,
        },
    }
}
