//! Stimulus level policy and measurement orchestration.
//!
//! This crate exists because the policy had no home in the tree and the place
//! it obviously belonged was the one place it must not go. `paraeq-dsp`'s
//! `generate_sweep` returns an unscaled sine (peak 1.0, **-3.01 dBFS RMS** —
//! REW's absolute maximum) and is correct to: it is bit-exact against an
//! oracle that is also unscaled, and an output-level hearing-safety constant is
//! product policy, not math. The prototype's `SWEEP_AMPLITUDE = 0.5` lives one
//! layer up in a PyQt playback worker that was never ported. Nothing was
//! dropped and nothing ships hot — `generate_sweep` has zero non-test callers.
//! The exposure is the *first* playback wiring, which is why MS-1/MS-2 land
//! before any playback path exists.
//!
//! The load-bearing invariant, stated here so it is never re-litigated:
//! **whenever tap self-exclusion succeeds, the sweep ParaEQ plays from its own
//! process never passes through `RealtimeChain` and is therefore subject to
//! neither the trim gain nor either of its two ±1.0 clamp sites.** The one
//! signal in the product that can injure a person is the one signal the
//! engine's safety clamps never see. That exclusion is the design goal, not an
//! obstacle — so this crate's level control is the sole protection on the
//! stimulus path, and gets the scrutiny a safety interlock gets.
//!
//! Test tier: **Tier 3 (analytic)** — output level is policy, not DSP, and it
//! has no oracle. The prototype's one playback constant is a guess about an
//! unknown chain, and porting it would launder that guess into a fixture. What
//! is pinned instead: the caps table field-by-field, every refusal edge of
//! [`SweepLevel::new`], and a `trybuild` compile-fail proof that no other type
//! reaches a [`StimulusSink`].
//!
//! Scope so far: MS-1/MS-2 (the seam and the level interlock), MS-3/MS-4
//! (stimulus assembly, the post-fade assertion set, and the emit guard —
//! [`stimulus`]), MS-9/MS-20 (the refusal framework and the numbered
//! diagnostic contract — [`diagnostic`]), MS-10/MS-11 (the cal-load
//! full-scale witness rule and the gain-referenced margin — [`cal`]), and
//! MS-6/MS-14/MS-18/MS-23 plus the RAII half of MS-5 (the sequencing state
//! machine — [`session`]).
//!
//! Stage 5 added the rest of the runtime: the closed-loop level ladder
//! (MS-7/MS-8/MS-13/MS-17 — [`ladder`]), which enters the session through
//! [`session::SolveOutcome`] and computes without ever emitting; the capture
//! runtime and its own metering (MS-21, MS-4's boundary 2 — [`capture`]),
//! which the engine's meters cannot substitute for even now that R1-8 has
//! made them decay (different signal path, different release law, and no
//! per-attempt reset -- see [`capture`]'s header); and the
//! per-position IR store ([`store`]), without which every row of the
//! decision engine's `Reanalyze` tier collapses into `Recapture`.
//!
//! The live [`seam::TapStatus`] witness now exists: `TapSystem` records its
//! self-exclusion at create time, `TapBackend` mirrors it into a cloneable
//! `ExclusionWitness` across every start and stop, and the desktop takes that
//! clone before the backend is moved into the controller thread. A real
//! session therefore no longer needs a mock — pass the witness. It is still a
//! trait and not a `TapSystem` in [`session::MeasurementSession::begin`],
//! deliberately: MS-1 keeps this crate free of CoreAudio, and the witness must
//! be readable from outside the thread that owns the tap.

#![forbid(unsafe_code)]

pub mod align;
pub mod cal;
pub mod capture;
pub mod diagnostic;
pub mod engine_seam;
pub mod ladder;
pub mod level;
pub mod ramp;
pub mod seam;
pub mod session;
pub mod stimulus;
pub mod store;
pub mod verify;

pub use align::{
    align, AlignRequest, Alignment, MARKER_SNR_WARN_DB, TWO_CLOCK_RESIDUAL_REFUSE_SAMPLES,
    TWO_CLOCK_RESIDUAL_WARN_SAMPLES,
};
pub use cal::{
    margined_emit_dbfs, CalSummary, PinnedGain, CAL_ERROR_MARGIN_DB, GAIN_MATCH_TOLERANCE,
};
pub use capture::{record, CaptureEnd, CaptureMeter, CaptureRun, CLIP_BLOCK_FRACTION};
pub use diagnostic::{CalSensitivity, MeasurementDiagnostic, MicSensitivity, Refusal, Severity};
pub use engine_seam::{
    EngineControl, EngineFacts, EngineStatusKind, GainPin, GainRestore, MeasurementLeaseToken,
    TapActivity,
};
pub use ladder::{
    analyze_magnitude_db, snr_band_hz, LadderError, LevelLadder, NoiseFloor, Remedy, Rung,
    SnrOutcome, MAX_REMEDIES, MAX_RUNG_STEP_DB, NOISE_FLOOR_MAX_DBFS,
};
pub use level::{caps_for, LevelError, SweepLevel, TransducerCaps, ABSOLUTE_MAX_DBFS_RMS};
/// Re-exported: the enum lives in `paraeq-dsp` because that is the only crate
/// every consumer may depend on. Its semantics are `paraeq-decide`'s; this
/// crate uses it as the key into the caps table and as `SweepLevel::new`'s
/// second argument.
pub use paraeq_dsp::targets::TransducerClass;
pub use ramp::{abort_envelope, abort_ramp_len};
pub use seam::{
    CaptureSource, HelperExit, HelperProcess, HelperRouting, RenderSink, StimulusHelper,
    StimulusSink, StreamFormat, TapStatus, VolumeControl,
};
pub use session::{
    AbortHandle, AbortReason, MeasurementSession, SessionError, SessionEvent, SessionLog,
    SessionPhase, SessionSeam, SolveOutcome, SweepOutcome, ABORT_RAMP_MS, ACK_SPL_TOLERANCE_DB,
};
pub use stimulus::{
    assemble_bracketed, assemble_pilot, assemble_sweep, emit_guard, verify_stimulus,
    AssembledStimulus, GuardCounts, StimulusError, StimulusKind,
};
pub use store::{IrStore, StoreError, StoredIr, StoredWindow};
pub use verify::{
    classify_exit, exit_diagnostic, expected_render_device_uid, ArmingFailure, HelperExitKind,
    HelperReady, SweepShape, VerificationPass, VerifyCaptureStats, VerifyError, VerifyOutcome,
    VerifyPhase, VerifyPlan, VerifyRequest, VerifySeam, VerifyTiming, VerifyTwoClockFit,
    CAPTURE_FLOOR_DBFS, PREAMP_MATCH_TOLERANCE, VERIFY_MIN_SNR_DB,
};

/// Error type shared by this crate's entry points.
///
/// Distinct from [`level::LevelError`], which is the sole constructor's
/// refusal set and is deliberately `Copy + PartialEq` so a refusal can be
/// asserted on and recorded in the session log verbatim.
#[derive(Debug, thiserror::Error)]
pub enum MeasureError {
    #[error("capture source failed: {0}")]
    Capture(String),
    #[error("invalid level: {0}")]
    Level(#[from] LevelError),
    #[error("stimulus sink failed: {0}")]
    Sink(String),
}
