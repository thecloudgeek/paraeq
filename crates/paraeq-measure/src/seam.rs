//! The measurement <-> platform seam, mirroring the engine's `AudioBackend`.
//!
//! This crate defines the traits; `paraeq-coreaudio` implements them (Stage 4)
//! and mocks implement them in tests. The dependency points *into* this crate,
//! which is what keeps the level policy free of CoreAudio and Tauri, reachable
//! from a future `paraeqd`, and exercisable with no hardware attached.
//!
//! Measurement plays on a *separate, untapped* stream — that is the whole point
//! of the tap's self-exclusion (design spec line 147: the sweep plays cleanly
//! "so correction state cannot contaminate the measurement"). A sink is
//! therefore not the engine's output path and must never be wired to it: do not
//! pre-convolve the stimulus with the active correction, which would defeat the
//! exclusion and corrupt the measurement it protects.

use crate::level::SweepLevel;
use crate::MeasureError;

/// Effective geometry of one stream. Reported per-stream rather than assumed
/// shared: a UMIK-1 runs at its own fixed rate on its own crystal against an
/// output device on another, and that mismatch is the session's to reconcile
/// (MS-22's both-devices aggregate with drift compensation), not something to
/// discover inside a deconvolution.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamFormat {
    pub channels: usize,
    pub frames_per_block: usize,
    pub sample_rate_hz: f64,
}

/// Where a stimulus goes.
///
/// Contract:
/// - `emit` takes a [`SweepLevel`] and no other level type. This is the MS-2
///   interlock: an implementation may not be handed a bare `f64`, so a level
///   that skipped the caps table cannot be emitted. The `.stderr` files under
///   `tests/ui/` are the proof.
/// - The block arrives ALREADY scaled to `level` by the assembly pipeline
///   (`AssembledStimulus::emit_to` in `stimulus.rs`); implementations apply
///   NO gain of their own — scaling here would double-apply the level. The
///   `level` parameter is provenance: log it and verify against it, never
///   multiply by it. The stimulus path does not traverse `RealtimeChain`, so
///   it inherits neither the trim gain nor either ±1.0 clamp; the emitter
///   carries its own clamp and non-finite guard (MS-4) and cannot borrow the
///   engine's.
/// - `emit` is PACED by the hardware: it must not return until the device has
///   consumed the block (or is within ~one block of consuming it), so the sink
///   buffers at most about one block ahead. The session's MS-14 abort model
///   depends on this — it polls the abort handle once per block and expects the
///   ramp to reach the device within roughly one block of the trigger; a sink
///   that accepted the whole sweep into a deep queue and returned immediately
///   would make the poll-per-block cadence fictional and the ramp arrive after
///   seconds of already-queued full-level audio.
/// - `stop` runs the sink's full teardown and MUST be idempotent — every exit
///   path, including panic unwinding, runs it, and it precedes the volume
///   restore in the abort sequence. `stop` must not click: it drops any
///   still-queued audio rather than flushing it at level. The system must never
///   be left at measurement volume.
pub trait StimulusSink: Send {
    fn format(&self) -> StreamFormat;

    fn emit(&mut self, block: &[f64], level: SweepLevel) -> Result<(), MeasureError>;

    fn stop(&mut self) -> Result<(), MeasureError>;
}

/// Where a measurement comes from.
///
/// Contract:
/// - `capture` fills `block` and returns the frames written. A short read is
///   not an error; the session decides what an underrun means.
/// - Non-finite samples are sanitized before they reach a caller (MS-4's
///   boundary 2): a NaN in a recorded IR propagates through `deconvolve` into
///   NaN correction coefficients, and one NaN poisons the DF2T feedback state
///   permanently.
/// - `stop` is idempotent, as for [`StimulusSink`].
pub trait CaptureSource: Send {
    fn format(&self) -> StreamFormat;

    fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError>;

    fn stop(&mut self) -> Result<(), MeasureError>;
}

/// Live read-back of the engine's tap self-exclusion — the MS-6 witness.
///
/// The engine side (Stage 4, over the live `TapSystem`) implements this; mocks
/// implement it in tests. What it witnesses is the invariant the crate header
/// states: the stimulus path is validated **only** when the tap excludes
/// ParaEQ's own process. When `translate_pid` fell back to an empty exclusion
/// list, a sweep would be muted at the device and routed through the
/// correction chain — an unvalidated topology. The session refuses
/// (`SelfExclusionUnavailable`) **before a single sample is emitted**, and
/// re-checks at the sweep gate because exclusion can vanish mid-session (a
/// device change forces a tap rebuild).
///
/// Contract:
/// - `self_excluded` reports the tap's *current* state, not the state at
///   construction; the session polls it at every gate that precedes emission.
pub trait TapStatus: Send {
    fn self_excluded(&self) -> bool;
}

/// Output-device volume — the software half of MS-5.
///
/// `paraeq-coreaudio` implements this on the default output device (a later
/// stage; the spec records that no volume plumbing exists in the tree today);
/// mocks implement it in tests. `scalar` follows the CoreAudio volume-scalar
/// convention: 0.0 silent, 1.0 full scale.
///
/// Contract:
/// - `volume` reads without side effects; the session pins the pre-measurement
///   value from it exactly once, at `begin`.
/// - `set_volume` is called on the RAII restore path on **every** exit —
///   command, drop, panic — in the same teardown position tap destruction
///   occupies in the engine. The system must never be left at measurement
///   volume. Implementations must therefore be safe to call during unwinding:
///   no panics of their own on the restore path, failures reported as `Err`
///   (the session records them; it never masks the remaining teardown steps).
pub trait VolumeControl: Send {
    fn volume(&self) -> Result<f64, MeasureError>;

    fn set_volume(&mut self, scalar: f64) -> Result<(), MeasureError>;
}
