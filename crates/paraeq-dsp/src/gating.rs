//! Establish `t = 0` on a measured IR and apply a fixed time gate around it,
//! reporting honestly what the gate can and cannot resolve.
//! Test tier: 3 — analytic physics. The two-path identity
//! `h = δ(t₀) + g·δ(t₀+τ)` has ungated `|H| = |1 + g·e^{−jωτ}|`; a right window
//! shorter than τ must return it exactly flat. No oracle can be wrong here.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`gating.rs` — new".
//!
//! STUB — room-dsp/4, lands in Stage 3.

use crate::{window::WindowSpec, DspError};

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
pub fn detect_peak(samples: &[f64]) -> Result<f64, DspError> {
    let _ = samples;
    unimplemented!("room-dsp/4 — lands in Stage 3")
}

/// Apply a fixed gate around `ir.peak`, returning the gated SLICE (not a
/// zero-padded full-length IR) and the report.
///
/// Contract, fixed here because the tests depend on it:
/// - `left_samples = round(applied_left_ms · sr / 1000)`, likewise right.
/// - Output length is `left_samples + right_samples + 1`; output index 0 is
///   input index `ir.peak_index() − left_samples`, so the peak sits at output
///   index `left_samples`.
/// - The left clamp is MANDATORY:
///   `applied_left_ms = min(requested_left_ms, peak_time_ms, farina_h2_bound_ms)`.
///   `deconvolve()` puts the peak at only ~46–64 ms, so REW's 125 ms default left
///   window is physically impossible here — unclamped, the start index goes
///   negative and the slice is empty. The binding constraint is reported in
///   `clamped_left`.
pub fn apply_gate(
    ir: &ImpulseResponse,
    spec: &GateSpec,
) -> Result<(Vec<f64>, GateReport), DspError> {
    let _ = (ir, spec);
    unimplemented!("room-dsp/4 — lands in Stage 3")
}

/// `1000.0 / right_ms`. A window of length `T` resolves nothing below ~`1/T`.
pub fn min_valid_freq(right_ms: f64) -> f64 {
    let _ = right_ms;
    unimplemented!("room-dsp/4 — lands in Stage 3")
}

/// The stricter, honest 1/N-octave validity criterion:
/// `f = (1/T) / (2^(1/2N) − 2^(−1/2N))`. A 10 ms gate is only 1/6-octave-valid
/// above ~865 Hz. Both numbers are reported because `1/T` is the absolute floor
/// and this is the one a user should believe.
pub fn resolution_limit_hz(t_s: f64, fraction: u32) -> f64 {
    let _ = (t_s, fraction);
    unimplemented!("room-dsp/4 — lands in Stage 3")
}

/// Farina second-harmonic arrival, ahead of the linear IR:
/// `dt₂ = T · ln2 / ln(f₂/f₁)`. A long left window folds harmonic distortion
/// into the "linear" response.
pub fn farina_h2_bound_s(sweep: &SweepParams) -> f64 {
    let _ = sweep;
    unimplemented!("room-dsp/4 — lands in Stage 3")
}
