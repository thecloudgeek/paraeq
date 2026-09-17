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
    /// Capture-side peak + clip metering for this position (MS-21).
    pub capture: CaptureStats,
    /// Provenance only. NEVER a decision input — `decide()` is deterministic.
    pub captured_at_ms: u64,
    pub index: usize,
    pub ir: ImpulseResponse,
    /// "Reseat 3", "Position 4 (left of centre)".
    pub label: String,
    /// Which output channel(s) carried the stimulus for this capture.
    ///
    /// Deliberately NOT `Option` and deliberately NOT `#[serde(default)]`:
    /// [`MeasurementBundle`] derives plain `Deserialize` with no field
    /// defaults, so a bundle without it fails to load, which is the wanted
    /// behaviour. A default of `Both` on a coupler run means differencing a
    /// per-ear baseline against an L+R sum — the exact failure the
    /// verification routing fence exists to refuse.
    pub routing: CaptureRouting,
}

/// Capture-side peak + clip metering for one pass (MS-21,
/// `docs/specs/2026-07-15-measurement-safety-design.md`: "Measurement capture
/// has its own peak + clip metering (the engine's counters watch the *tap*
/// path, which never sees the stimulus …)").
///
/// Serde twin of the three numbers `paraeq_measure::CaptureMeter` produces —
/// `clipped_samples()`, `peak_dbfs()` and the run's RMS
/// (`crates/paraeq-measure/src/capture.rs`). It is a twin rather than a
/// re-export because `paraeq-decide` may not depend on `paraeq-measure`; the
/// precedent is `AuthorityCurve`'s move, not a code-level mapping.
///
/// **Wire hazard, recorded so it is not discovered as a broken fixture:**
/// `CaptureMeter::peak_dbfs()` answers `f64::NEG_INFINITY` on digital silence
/// rather than flooring, and `serde_json` writes a non-finite `f64` as `null`
/// and then refuses to read it back into an `f64`. The capture layer must
/// therefore floor or refuse before a silent pass reaches a bundle. Pinned by
/// `a_non_finite_peak_dbfs_does_not_round_trip`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct CaptureStats {
    /// Cumulative over the pass, including non-finite samples (which
    /// `CaptureMeter` counts as clipped so a NaN storm cannot read as silence).
    pub clipped_samples: u64,
    /// The decayed peak, dBFS, full scale = 1.0.
    pub peak_dbfs: f64,
    /// Broadband RMS of the captured pass, dBFS, full scale = 1.0.
    pub rms_dbfs: f64,
}

/// Where a mono stimulus went on the output side of one capture.
///
/// Serde twin of `paraeq_coreaudio::StimulusRouting`
/// (`crates/paraeq-coreaudio/src/measure_aggregate.rs`), which is
/// `Both | Only(usize)`. Width is **`u32`, not `usize`**: this type is a frozen
/// wire shape in `fixtures/decide/`, and a platform-width integer does not
/// belong in one. The map is TOTAL in the direction the product uses
/// (`CaptureRouting` → `StimulusRouting`, `u32` → `usize` is infallible on
/// every supported target) and FALLIBLE the other way — a `TryFrom` refuses
/// `Only(n)` for `n > u32::MAX` rather than truncating a channel index, because
/// truncation would silently re-route audio. Both mappings and their two named
/// tests live in `desktop/src-tauri`, the one place that can see both types.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CaptureRouting {
    /// Every output channel. Correct for a single-driver path, and for a room
    /// measurement of a system being corrected as one.
    Both,
    /// One channel index only; every other channel stayed silent.
    Only(u32),
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
    /// The solved chain sensitivity, dB SPL per dBFS RMS, when the level solve
    /// produced one. `None` when it did not — MS-17's envelope check has
    /// nothing to compare and the class cross-check cannot fire.
    pub chain_sensitivity_spl_per_dbfs: Option<f64>,
    /// Whether the two-clock resample was applied to this capture.
    ///
    /// The drawer half of the 2026-07-21 record's §Q6: "**Drawer override:**
    /// clock-adjust toggle with the estimated ppm shown, **defaulted on**."
    /// The decision that drives it is `Decisions::clock_adjust`; this is the
    /// witness that it actually ran, because `decide()` cannot observe the
    /// capture layer.
    pub clock_adjusted: bool,
    /// The estimated mic-vs-output clock skew, parts per million, when a
    /// bracketed-marker fit could be formed.
    ///
    /// D-Q, verbatim: "Add `CapturePlan.clock_skew_ppm: Option<f64>`; fire
    /// `TwoClock`/Warn only when it is `None`, carrying the ppm as
    /// `Diagnostic.value` when it is `Some`." `Some(ppm)` means the estimate
    /// was formed and applied, so the unconditional `TwoClock` warning does
    /// **not** fire; `None` is the spec's fallback case and does.
    pub clock_skew_ppm: Option<f64>,
    /// Whether the input device resolved at capture time.
    ///
    /// Resolving a UID is I/O, which `decide()` forbids itself, so the capture
    /// layer records a boolean witness — the same shape `self_excluded` takes
    /// and for the same reason. `false` is `MicNotConnected`.
    pub input_present: bool,
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
    ///
    /// Sweep-span RMS, the same convention [`Verification::level_dbfs`] uses,
    /// so `K` is a difference of like quantities. It is also the **post-margin
    /// emitted** level (`crates/paraeq-measure/src/session.rs`'s
    /// `install_solve` runs `margined_emit_dbfs` before `SweepLevel::new`),
    /// which is why `K` is a difference of like quantities in both senses.
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

/// Least-squares marker fit of measured timing markers against expected ones,
/// `measured ≈ intercept + (1 + skew_ppm·1e-6) · expected`.
///
/// Serde twin of `paraeq_coreaudio::two_clock::SkewEstimate` minus its
/// per-marker `residuals_samples` vector, which is fit-internal detail a frozen
/// bundle does not need. Twin rather than re-export for the crate-DAG reason
/// [`CaptureStats`] gives.
///
/// **Evidence only, never gated** (`docs/specs/2026-07-15-wizard-design.md`:
/// "Report it, plot it, **never gate on it**").
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct TwoClockFit {
    /// Fit intercept in capture samples — the constant transport offset
    /// (playback start latency + acoustic propagation).
    pub intercept_samples: f64,
    /// Worst per-marker residual, capture samples. Fractional: a matched-filter
    /// residual is not an integer.
    pub residual_peak_samples: f64,
    /// RMS of the per-marker residuals, capture samples. Fractional, same
    /// reason.
    pub residual_rms_samples: f64,
    /// Clock-rate difference in parts per million.
    pub skew_ppm: f64,
}

/// A closed-loop verification pass: the stimulus played from a helper child
/// process (which the tap can see) and re-measured through the real chain.
/// Never a sweep pre-convolved with the correction — that measures the
/// filter's math and defeats self-exclusion's documented purpose.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Verification {
    /// Capture-side peak + clip metering for THIS pass (MS-21), from the same
    /// `CaptureMeter` the Direct path uses
    /// (`crates/paraeq-measure/src/capture.rs`). This is the MIC's ADC, on the
    /// far side of the transducer.
    ///
    /// NOT the same fact as `MeasurementDiagnostic::VerificationChainClipped`,
    /// which reads `EngineState.clipped_samples` — the engine's own ±1.0 output
    /// clamp, on the near side. Both can fire, neither implies the other, and a
    /// residual computed over a railed capture is meaningless whichever one is
    /// silent.
    ///
    /// Non-`Option` for the reason [`Position::routing`] is: a
    /// `#[serde(default)]` would silently report "nothing clipped".
    pub capture: CaptureStats,
    /// The engine gain stage at capture, dB. MUST be 0.0 — pinned by the verify
    /// gate, which reads it back and RAII-restores it; carried so `decide()`
    /// can refuse rather than trust.
    pub gain_db: f64,
    /// The plan the engine was RUNNING when `ir` was captured. `decide()`
    /// re-derives the prediction from it — the gate is residual vs prediction,
    /// which is rig-independent. `installed.preamp_db` is `decide()`'s number
    /// at `installed.design_rate`; the engine's own is
    /// [`Verification::installed_preamp_db`].
    pub installed: CorrectionPlan,
    /// The engine's OWN armed preamp, dB, read back through the measurement
    /// seam's `installed_preamp_lin()` and converted.
    ///
    /// NOT `installed.preamp_db`: the engine computes at the LIVE rate, over
    /// the SURVIVING bands after Nyquist drops, folded `min` across channels
    /// ("**The worst channel wins**", `crates/paraeq-engine/src/controller.rs`),
    /// while the plan's number is `decide()`'s at `design_rate` over all bands.
    /// With no dropped bands and `running_rate_hz == design_rate` the two
    /// agree; `decide()` recomputes and refuses with
    /// [`crate::DiagnosticCode::VerificationPreampMismatch`] when they do not,
    /// rather than assuming.
    pub installed_preamp_db: f64,
    /// The re-measured response.
    pub ir: ImpulseResponse,
    /// `L_verify`: the RMS of the **sweep span only**, dBFS, full scale = 1.0 —
    /// NOT of the assembled file. The marker bracket adds ~2.20 s of
    /// near-silence, which would pull a whole-file RMS down by
    /// `10·log10(5.5/7.7) = 1.46 dB` against a `2·flatness_target_db` = 2.0 dB
    /// gate.
    ///
    /// `K = bundle.capture.sweep.level_dbfs − this`, EXACT, and that term is
    /// span RMS too. Never re-derive it from `preamp_db` — that assumes the
    /// level solver ran.
    ///
    /// The MS-11 cal-error margin is **already in it**: it is inherited from
    /// `L_measure`, which `install_solve` produced post-margin. Re-applying
    /// `CAL_ERROR_MARGIN_DB` here subtracts 6 dB twice.
    pub level_dbfs: f64,
    /// Which position this pass re-measured (auto mode verifies one).
    pub position_index: usize,
    /// Which output channel(s) the verification sweep used. MUST equal
    /// `positions[position_index].routing`, or the residual differences a
    /// per-ear baseline against an L+R sum.
    pub routing: CaptureRouting,
    /// The LIVE stream rate during this capture. `H(f)` is evaluated here,
    /// never at `installed.design_rate`.
    pub running_rate_hz: f64,
    /// Marker-fit provenance. EVIDENCE ONLY, never gated.
    pub two_clock: Option<TwoClockFit>,
}
