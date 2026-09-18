//! Establish `t = 0` on a measured IR and apply a fixed time gate around it,
//! reporting honestly what the gate can and cannot resolve.
//! Test tier: 3 — analytic physics. The two-path identity
//! `h = δ(t₀) + g·δ(t₀+τ)` has ungated `|H| = |1 + g·e^{−jωτ}|`; a right window
//! shorter than τ must return it exactly flat. No oracle can be wrong here.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`gating.rs` — new".

use crate::{
    window::{WindowKind, WindowSpec},
    DspError,
};

/// The 1/N-octave fraction `apply_gate` reports `resolution_limit_hz` at:
/// 1/6 octave, the spec's running example and the standard room-correction
/// smoothing fraction.
pub const RESOLUTION_FRACTION: u32 = 6;

/// A measured impulse response with a known time origin.
#[derive(Clone, Debug)]
pub struct ImpulseResponse {
    pub samples: Vec<f64>,
    /// Fractional sample index of the direct-sound peak (parabolic-refined).
    pub peak: f64,
    pub sample_rate: u32,
}

impl ImpulseResponse {
    pub fn peak_index(&self) -> usize {
        self.peak.round() as usize
    }

    pub fn peak_time_s(&self) -> f64 {
        self.peak / self.sample_rate as f64
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SweepParams {
    pub duration_s: f64,
    pub f1: f64,
    pub f2: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct GateSpec {
    pub left_ms: f64,
    pub right_ms: f64,
    /// Sweep parameters when known, so the left window can be bounded by the
    /// Farina H2 arrival. `None` skips that check (synthetic IRs, MLS).
    pub sweep: Option<SweepParams>,
    pub window: WindowSpec,
}

#[derive(Clone, Debug)]
pub struct GateReport {
    pub applied_left_ms: f64,
    pub applied_right_ms: f64,
    pub clamped_left: Option<LeftClamp>,
    /// `1 / T_right`. The ABSOLUTE floor.
    pub min_valid_freq_hz: f64,
    /// The honest, 1/N-octave-aware limit. Always >= `min_valid_freq_hz`.
    pub resolution_limit_hz: f64,
    pub harmonic_bound_ms: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LeftClamp {
    PeakTooEarly { peak_ms: f64 },
    HarmonicBound { dt2_ms: f64 },
}

/// `argmax|h|` + parabolic vertex refinement on `(i−1, i, i+1)`:
/// `δ = 0.5·(y[i−1] − y[i+1]) / (y[i−1] − 2y[i] + y[i+1])`, `peak = i + δ`,
/// with `δ` clamped to `[−0.5, 0.5]` and `δ = 0` when the denominator is within
/// 1e-30 of zero.
///
/// Fallback: if the first `0.5·max|h|` crossing precedes the argmax by more than
/// 1 ms, take that crossing (parabolic-refined) instead — more than 1 ms of
/// half-amplitude energy before the argmax means the argmax is a reflection, not
/// the direct sound.
///
/// This signature (spec-fixed) carries no sample rate, so the 1 ms threshold
/// is expressed as [`PEAK_FALLBACK_LEAD_SAMPLES`] — 1 ms at the pipeline's
/// 48 kHz. **Pipeline code MUST use [`detect_peak_with_lead`]** with
/// `sample_rate / 1000`: this wrapper assumes 48 kHz and DISCARDS the
/// structured `gate.peak_fallback` warning, which the measurement session is
/// required to surface. It exists to satisfy the spec's signature and for
/// rate-agnostic tests, not for the capture path.
pub fn detect_peak(samples: &[f64]) -> Result<f64, DspError> {
    Ok(detect_peak_with_lead(samples, PEAK_FALLBACK_LEAD_SAMPLES)?.peak)
}

/// Result of [`detect_peak_with_lead`]: the refined peak plus the structured
/// `gate.peak_fallback` warning when the 0.5·max fallback fired — returned,
/// not raised, per this crate's warning convention (`compensation.rs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakDetection {
    pub peak: f64,
    pub fallback: Option<PeakFallback>,
}

/// The structured `gate.peak_fallback` warning: the argmax was a reflection,
/// the reported peak is the (parabolic-refined) first 0.5·max crossing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakFallback {
    pub argmax_index: usize,
    pub crossing_index: usize,
}

/// The spec's 1 ms fallback threshold expressed in samples at the pipeline's
/// 48 kHz — the default [`detect_peak`] uses, its signature having no rate.
pub const PEAK_FALLBACK_LEAD_SAMPLES: f64 = 48.0;

/// [`detect_peak`] with the fallback rule's lead threshold in samples: the
/// spec states it as "more than 1 ms", so rate-aware callers pass
/// `sample_rate as f64 / 1000.0`.
pub fn detect_peak_with_lead(
    samples: &[f64],
    fallback_lead_samples: f64,
) -> Result<PeakDetection, DspError> {
    if samples.is_empty() {
        return Err(DspError::InvalidInput(
            "detect_peak: empty impulse response".into(),
        ));
    }
    if samples.iter().any(|s| !s.is_finite()) {
        return Err(DspError::InvalidInput(
            "detect_peak: non-finite sample in impulse response".into(),
        ));
    }
    let abs: Vec<f64> = samples.iter().map(|s| s.abs()).collect();
    let mut argmax = 0usize;
    let mut max_abs = 0.0f64;
    for (i, &a) in abs.iter().enumerate() {
        if a > max_abs {
            max_abs = a;
            argmax = i;
        }
    }
    if max_abs == 0.0 {
        return Err(DspError::InvalidInput(
            "detect_peak: all-zero impulse response has no peak".into(),
        ));
    }
    // First 0.5·max crossing; exists and is <= argmax by construction.
    let crossing = abs
        .iter()
        .position(|&a| a >= 0.5 * max_abs)
        .expect("argmax itself crosses 0.5*max");
    let (index, fallback) = if (argmax - crossing) as f64 > fallback_lead_samples {
        (
            crossing,
            Some(PeakFallback {
                argmax_index: argmax,
                crossing_index: crossing,
            }),
        )
    } else {
        (argmax, None)
    };
    Ok(PeakDetection {
        peak: index as f64 + parabolic_delta(&abs, index),
        fallback,
    })
}

/// Parabolic vertex offset at `i` on `|h|`, clamped to `[−0.5, 0.5]`; 0 at the
/// edges and when the denominator is within 1e-30 of zero.
fn parabolic_delta(y: &[f64], i: usize) -> f64 {
    if i == 0 || i + 1 >= y.len() {
        return 0.0;
    }
    let den = y[i - 1] - 2.0 * y[i] + y[i + 1];
    if den.abs() < 1e-30 {
        return 0.0;
    }
    (0.5 * (y[i - 1] - y[i + 1]) / den).clamp(-0.5, 0.5)
}

/// Apply a fixed gate around `ir.peak`, returning the gated SLICE (not a
/// zero-padded full-length IR) and the report.
///
/// Contract, fixed here because the tests depend on it:
/// - `left_samples = round(applied_left_ms · sr / 1000)`, likewise right.
/// - Output length is `left_samples + right_samples + 1`; output index 0 is
///   input index `ir.peak_index() − left_samples`, so the peak sits at output
///   index `left_samples`. A right gate running past the recording's end
///   zero-fills (the FFT consumer zero-pads anyway; the length contract holds).
/// - The left clamp is MANDATORY:
///   `applied_left_ms = min(requested_left_ms, peak_time_ms, farina_h2_bound_ms)`.
///   `deconvolve()` puts the peak at only ~46–64 ms, so REW's 125 ms default left
///   window is physically impossible here — unclamped, the start index goes
///   negative and the slice is empty. The binding constraint is reported in
///   `clamped_left`.
/// - The half-tapers are applied with their inner edge (exactly 1.0) at the
///   peak — `window.left` pre-peak, `window.right` post-peak — so the peak
///   sample carries weight 1.0.
/// - `resolution_limit_hz` is reported at [`RESOLUTION_FRACTION`] (1/6 octave).
///   Both report figures use the EFFECTIVE right duration — the shorter of the
///   applied right gate and the post-peak data the recording actually holds —
///   because a zero-filled tail adds no resolution.
pub fn apply_gate(
    ir: &ImpulseResponse,
    spec: &GateSpec,
) -> Result<(Vec<f64>, GateReport), DspError> {
    if ir.samples.is_empty() {
        return Err(DspError::InvalidInput(
            "apply_gate: empty impulse response".into(),
        ));
    }
    if ir.sample_rate == 0 {
        return Err(DspError::InvalidInput(
            "apply_gate: sample_rate is 0".into(),
        ));
    }
    if !ir.peak.is_finite() || ir.peak < 0.0 {
        return Err(DspError::InvalidInput(format!(
            "apply_gate: invalid peak index {}",
            ir.peak
        )));
    }
    let peak_index = ir.peak_index();
    if peak_index >= ir.samples.len() {
        return Err(DspError::InvalidInput(format!(
            "apply_gate: peak index {peak_index} outside the IR (len {})",
            ir.samples.len()
        )));
    }
    // Comparisons are written so a NaN fails them: NaN compares false.
    let right_ok = spec.right_ms.is_finite() && spec.right_ms > 0.0;
    if !right_ok {
        return Err(DspError::InvalidInput(format!(
            "apply_gate: right_ms must be finite and positive, got {}",
            spec.right_ms
        )));
    }
    let left_ok = spec.left_ms.is_finite() && spec.left_ms >= 0.0;
    if !left_ok {
        return Err(DspError::InvalidInput(format!(
            "apply_gate: left_ms must be finite and non-negative, got {}",
            spec.left_ms
        )));
    }
    if let Some(s) = &spec.sweep {
        let sweep_ok = s.duration_s.is_finite()
            && s.duration_s > 0.0
            && s.f1 > 0.0
            && s.f2.is_finite()
            && s.f2 > s.f1;
        if !sweep_ok {
            return Err(DspError::InvalidInput(format!(
                "apply_gate: invalid sweep params (duration {} s, {} → {} Hz)",
                s.duration_s, s.f1, s.f2
            )));
        }
    }
    for kind in [spec.window.left, spec.window.right] {
        if let WindowKind::Tukey { alpha } = kind {
            if !alpha.is_finite() {
                return Err(DspError::InvalidInput(format!(
                    "apply_gate: Tukey alpha must be finite, got {alpha}"
                )));
            }
        }
    }

    let sr = ir.sample_rate as f64;
    // `peak · 1000 / sr` (not `peak / sr · 1000`) keeps integer-millisecond
    // peaks exact: 2208 samples at 48 kHz is exactly 46.0 ms.
    let peak_time_ms = ir.peak * 1000.0 / sr;
    let harmonic_bound_ms = spec.sweep.as_ref().map(|s| farina_h2_bound_s(s) * 1000.0);

    // applied_left = min(requested, farina H2 bound, peak time), reporting the
    // binding constraint. Checked in that order so the physically harder limit
    // (the peak) wins a tie.
    let mut applied_left_ms = spec.left_ms;
    let mut clamped_left = None;
    if let Some(dt2_ms) = harmonic_bound_ms {
        if dt2_ms < applied_left_ms {
            applied_left_ms = dt2_ms;
            clamped_left = Some(LeftClamp::HarmonicBound { dt2_ms });
        }
    }
    if peak_time_ms < applied_left_ms {
        applied_left_ms = peak_time_ms;
        clamped_left = Some(LeftClamp::PeakTooEarly {
            peak_ms: peak_time_ms,
        });
    }
    let applied_right_ms = spec.right_ms;

    // `applied_left_ms <= peak_time_ms` and round() is monotone, so
    // `left_samples <= peak_index` up to float dust; the min() guards the dust.
    let left_samples = ((applied_left_ms * sr / 1000.0).round() as usize).min(peak_index);
    let right_samples = (applied_right_ms * sr / 1000.0).round() as usize;
    // A right gate somewhat past the recording's end is legal (zero-fill,
    // below); one longer than the ENTIRE recording is a programmer error and,
    // unchecked, an unbounded allocation.
    if right_samples > ir.samples.len() {
        return Err(DspError::InvalidInput(format!(
            "apply_gate: right_ms {} ({} samples) exceeds the whole recording ({} samples)",
            applied_right_ms,
            right_samples,
            ir.samples.len()
        )));
    }
    let start = peak_index - left_samples;
    let len = left_samples + right_samples + 1;

    let mut gated = vec![0.0; len];
    let available = (ir.samples.len() - start).min(len);
    gated[..available].copy_from_slice(&ir.samples[start..start + available]);

    // Inner edge (taper index 0, exactly 1.0) at the peak; skip(1) leaves the
    // peak sample itself untouched.
    let left_taper = spec.window.left.half_taper(left_samples + 1);
    for (g, w) in gated[..=left_samples]
        .iter_mut()
        .rev()
        .zip(&left_taper)
        .skip(1)
    {
        *g *= w;
    }
    let right_taper = spec.window.right.half_taper(right_samples + 1);
    for (g, w) in gated[left_samples..].iter_mut().zip(&right_taper).skip(1) {
        *g *= w;
    }

    // The report's resolution figures come from the data that EXISTS: when the
    // recording ends before the right gate does, the zero-filled tail adds no
    // information, and reporting 1/T of the requested window would overstate
    // low-frequency validity to exactly the consumers (`authority.rs`) that
    // must respect it.
    let available_right_ms = (ir.samples.len() - 1 - peak_index) as f64 * 1000.0 / sr;
    let effective_right_ms = applied_right_ms.min(available_right_ms).max(1000.0 / sr);
    let report = GateReport {
        applied_left_ms,
        applied_right_ms,
        clamped_left,
        min_valid_freq_hz: min_valid_freq(effective_right_ms),
        resolution_limit_hz: resolution_limit_hz(effective_right_ms / 1000.0, RESOLUTION_FRACTION),
        harmonic_bound_ms,
    };
    Ok((gated, report))
}

/// `1000.0 / right_ms`. A window of length `T` resolves nothing below ~`1/T`.
pub fn min_valid_freq(right_ms: f64) -> f64 {
    1000.0 / right_ms
}

/// The stricter, honest 1/N-octave validity criterion:
/// `f = (1/T) / (2^(1/2N) − 2^(−1/2N))`. A 10 ms gate is only 1/6-octave-valid
/// above ~865 Hz. Both numbers are reported because `1/T` is the absolute floor
/// and this is the one a user should believe. `fraction` must be >= 1.
pub fn resolution_limit_hz(t_s: f64, fraction: u32) -> f64 {
    let half_bw = 1.0 / (2.0 * fraction as f64);
    (1.0 / t_s) / (2f64.powf(half_bw) - 2f64.powf(-half_bw))
}

/// Farina second-harmonic arrival, ahead of the linear IR:
/// `dt₂ = T · ln2 / ln(f₂/f₁)`. A long left window folds harmonic distortion
/// into the "linear" response.
pub fn farina_h2_bound_s(sweep: &SweepParams) -> f64 {
    sweep.duration_s * std::f64::consts::LN_2 / (sweep.f2 / sweep.f1).ln()
}
