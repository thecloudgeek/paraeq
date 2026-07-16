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
/// - Implementations scale by `level` and by nothing else. The stimulus path
///   does not traverse `RealtimeChain`, so it inherits neither the trim gain
///   nor either ±1.0 clamp; the emitter carries its own clamp and non-finite
///   guard (MS-4) and cannot borrow the engine's.
/// - `stop` runs the sink's full teardown and MUST be idempotent — every exit
///   path, including panic unwinding, runs it, and it precedes the volume
///   restore in the abort sequence. The system must never be left at
///   measurement volume.
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
