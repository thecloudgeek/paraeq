//! `decide()`'s input. One serializable value: the fixture, the bug-report
//! attachment, and the profile's analysis record are the same thing.

use crate::decisions::Overrides;
use crate::outcome::CorrectionPlan;
use paraeq_dsp::targets::{TargetCurve, TransducerClass};
use serde::{Deserialize, Serialize};

/// The complete, serializable input to [`crate::decide`].
///
/// Everything `decide()` needs arrives already parsed: `targets::list_targets`
/// reads a directory, and a `decide()` that could do that would not be pure
/// and could not be fixture-tested.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MeasurementBundle {
    /// Verbatim cal file + parsed curve + parsed metadata. NEVER normalized.
    pub cal: Option<CalFile>,
    /// What was actually captured (echoed as Recapture-tier decisions).
    pub capture: CapturePlan,
    /// The wizard's one unavoidable question.
    pub class: TransducerClass,
    /// Silence capture taken before the first sweep, per channel.
    pub noise_floor: NoiseFloor,
    /// User overrides. In the bundle, not a second argument, so the whole
    /// input stays one value.
    pub overrides: Overrides,
    /// Accepted positions only. A cancelled or rejected capture is absent —
    /// the type makes it unrepresentable rather than the analysis defending
    /// against it.
    pub positions: Vec<Position>,
    /// Caller-loaded (`targets::list_targets` does I/O; `decide()` does not).
    pub targets: Vec<TargetCurve>,
    /// Present only after a closed-loop verification pass.
    pub verification: Option<Verification>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Position {
    /// Provenance only. NEVER a decision input — `decide()` is deterministic.
    pub captured_at_ms: u64,
    pub index: usize,
    pub ir: ImpulseResponse,
    /// "Reseat 3", "Position 4 (left of centre)".
    pub label: String,
}

/// A deconvolved impulse response with its time axis intact. Every gating
/// operation needs t=0, which is why this carries `peak` and `sample_rate`
/// rather than being a bare `Vec<f64>`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ImpulseResponse {
    /// Direct-arrival index (argmax + parabolic refine). ~46–64 ms into the
    /// capture on this architecture: tap latency + propagation.
    pub peak: usize,
    pub sample_rate: u32,
    /// Per channel.
    pub samples: Vec<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CalFile {
    /// Raw bytes as shipped. The parser's input and the provenance record.
    pub content: String,
    /// Interpolatable curve. NEVER normalized to 0 dB at any frequency: the
    /// EARS jig encodes a real 2.1 dB L/R capsule offset IN the curve, and
    /// normalizing would bake a channel imbalance into every measurement.
    pub curve: (Vec<f64>, Vec<f64>),
    pub gain_db: Option<f64>,
    pub sensitivity_db: Option<f64>,
    pub serial: Option<String>,
    /// EARS HEQ/HPN/IDF variants already bake a target into the cal.
    pub variant: CalVariant,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CalVariant {
    /// A target is already baked into the curve: the target decision is forced
    /// to `flat` and a `CalHasTargetBakedIn` warning is raised, because
    /// applying another would apply it twice.
    EarsHeq,
    EarsHpn,
    EarsIdf,
    /// A plain transfer-function cal (UMIK-1, 711-class coupler).
    Plain,
}

/// What was actually captured. Echoed as Recapture-tier decisions, and the
/// source of every device-identity and clock refusal.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapturePlan {
    pub input_rate: u32,
    /// A UID that no longer resolves is `MicNotConnected`.
    pub input_uid: String,
    pub output_rate: u32,
    pub output_uid: String,
    /// `tap.rs`'s fail-open fallback leaves the exclusion list empty and taps
    /// ParaEQ's own audio. `decide()` cannot observe that; the capture layer
    /// records it here, and `false` on a measurement (not verification)
    /// capture is a Refuse.
    pub self_excluded: bool,
    pub sweep: SweepPlan,
    /// The rate the sweep was generated at. `!= ir.sample_rate` is an internal
    /// error, not a user condition.
    pub sweep_rate: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SweepPlan {
    pub duration_s: f64,
    pub f_end_hz: f64,
    /// Set from the class table and never extended downward: excursion rises
    /// as 1/f² below box tuning and ported boxes unload entirely.
    pub f_start_hz: f64,
    /// Playback level. Lives here, not in `paraeq-dsp::sweep` — the level is
    /// platform/product policy, and `generate_sweep` is peak-1.0 by oracle.
    pub level_dbfs: f64,
}

/// Silence capture taken before the first sweep. Broadband RMS gates the room;
/// the spectrum is what SNR is evaluated against, per band, over the decided
/// `correction_range`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NoiseFloor {
    /// The grid `spectrum_db` is sampled on.
    pub freqs_hz: Vec<f64>,
    /// Per channel, dBFS.
    pub rms_dbfs: Vec<f64>,
    /// Per channel, dB, on `freqs_hz`.
    pub spectrum_db: Vec<Vec<f64>>,
}

/// A closed-loop verification pass: the stimulus played from a helper child
/// process (which the tap can see) and re-measured through the real chain.
/// Never a sweep pre-convolved with the correction — that measures the
/// filter's math and defeats self-exclusion's documented purpose.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Verification {
    /// The re-measured response.
    pub ir: ImpulseResponse,
    /// The plan the engine was running when `ir` was captured. `decide()`
    /// re-derives the prediction from it — the gate is residual vs
    /// prediction, which is rig-independent.
    pub installed: CorrectionPlan,
    /// Which position this pass re-measured (auto mode verifies one).
    pub position_index: usize,
}
