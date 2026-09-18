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
//!
//! **[`RenderSink`] is the documented exception to the paragraph above, and it
//! is an exception rather than a violation.** measurement-safety § The
//! Load-Bearing Invariant, consequence 4: "Closed-loop verification is the one
//! exception, and it inverts the reasoning. Measuring the *corrected* output
//! requires the stimulus to go *through* the engine, which self-exclusion
//! prevents. The resolution is … **play the verification stimulus from a helper
//! child process**". That stimulus is rendered by a different process on the
//! physical output device, is therefore tapped, and is therefore clamped at
//! both `RealtimeChain` sites — so it is the one stimulus in the product the
//! engine's safety clamps DO see. Nothing here is pre-convolved: the child
//! renders the same bytes the baseline played, and the correction arrives from
//! the engine, live.

use crate::level::SweepLevel;
use crate::MeasureError;
use std::path::Path;
use std::time::Duration;

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
///   not an error; the session decides what an underrun means. **Zero frames
///   means the stream ENDED**, which is how [`record`](crate::capture::record)
///   reads it — so a non-blocking implementation whose zero means "nothing
///   queued yet" must be wrapped in an adapter that waits, with its own
///   deadline, before passing a zero through. See `record`'s own doc for why
///   the waiting may not live in the capture loop.
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

/// Where PRE-LEVELLED raw blocks are rendered, when the MS-2 type interlock
/// has already been discharged upstream and BYTES, not types, are the contract.
///
/// # Why this is not `StimulusSink`, and why that is not a loophole
///
/// [`StimulusSink::emit`] takes a [`SweepLevel`] and no other level type, and
/// `tests/ui/` proves a bare `f64` cannot reach it. That interlock **cannot
/// cross a process boundary**: a child receives a file, not a type. Rather than
/// weaken `SweepLevel` with a second constructor — which
/// `tests/ui/sweep_level_has_no_second_constructor.rs` exists to forbid — this
/// trait states the honest thing: it has NO level parameter at all, because on
/// this side of the boundary there is nothing that could disagree with the WAV.
///
/// The replacement interlock, all four parts required:
///
/// 1. **The WAV *is* the level.** The helper binary has no `--level` flag and
///    never will; there is nothing to pass and nothing to mis-pass.
/// 2. **The child re-checks before opening a device**, against
///    [`ABSOLUTE_MAX_DBFS_RMS`](crate::level::ABSOLUTE_MAX_DBFS_RMS), as a
///    max-over-windows bound rather than a whole-file RMS (which padding
///    defeats), plus `peak > 1.0`, plus its own MS-4 guard.
/// 3. **The parent proves the bytes** the child played are the bytes it
///    assembled.
/// 4. **A `RenderSink` can never stand in for a `StimulusSink`.**
///    [`SessionSeam`](crate::session::SessionSeam) has no `RenderSink` slot,
///    and `tests/ui/render_sink_cannot_be_passed_where_a_stimulus_sink_is_required.rs`
///    pins it.
///
/// Contract:
///
/// - `write` is PACED by the hardware, exactly as [`StimulusSink::emit`] is and
///   for exactly the same MS-14 reason: it must not return until the device is
///   within about one block of caught up, so the caller's poll-per-block abort
///   cadence is real rather than fictional.
/// - `write` applies **NO gain of its own**. The block arrives already at
///   level; multiplying here would double-apply it.
/// - `ramp_out` ARMS the abort: the NEXT block written is the 5 ms raised
///   cosine, it is the last audio the sink accepts, and everything after it is
///   silence. It is never a hard stop — "A hard stop is itself a full-scale
///   click" — and never a re-render of the stimulus. The envelope itself is
///   [`abort_envelope`](crate::ramp::abort_envelope), so the in-process abort
///   and the cross-process abort are one shape with one test.
/// - `stop` is idempotent and must not click: it drops still-queued audio
///   rather than flushing it at level. By the time a caller reaches it the
///   ramp has already played, so there is nothing left at level to drop.
/// - Neither `ramp_out` nor `stop` returns a `Result`. Both run on the teardown
///   ladder, on every exit path including panic, and a rung that could fail
///   into a `?` is a rung that can abort the ladder before the render device is
///   destroyed. Failures are the implementation's to log and collect.
pub trait RenderSink: Send {
    fn format(&self) -> StreamFormat;

    fn ramp_out(&mut self);

    fn stop(&mut self);

    fn write(&mut self, block: &[f32]) -> Result<(), MeasureError>;
}

/// What one [`HelperProcess::read_line`] call found.
///
/// THREE states, not two, and the third is the whole reason this type exists.
/// A `Result<Option<String>, _>` cannot tell "the deadline expired and the
/// child is still alive" apart from "stdout closed, so the child is gone", and
/// the teardown ladder turns on exactly that distinction: it escalates from the
/// polite `abort` rung to SIGTERM only when the child did NOT exit inside the
/// ramp deadline. Conflating the two makes a wedged child read as a dead one,
/// the ladder stops after rung 1, and a helper is left playing into the user's
/// output device — which is the failure the ladder exists to prevent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HelperLine {
    /// The deadline passed with nothing to read. The child may still be alive;
    /// ask [`HelperProcess::try_reap`] if you need to know.
    DeadlineExpired,
    /// Stdout closed. The child has exited (or closed the pipe on its way out).
    Eof,
    /// One line, with its trailing newline already stripped.
    Line(String),
}

/// Spawns and owns the verification helper child process.
///
/// Declared here and implemented by the shell for the same reason
/// [`StimulusSink`] is: this crate owns the POLICY (which rungs, in what order,
/// with what deadlines) and must stay testable with no process at all. The
/// implementation owns the mechanism (path resolution, spawn, signals, reap).
///
/// **The trait cannot be handed a level or a stimulus.** The WAV is the level,
/// so `spawn` takes a path and nothing that could disagree with it — the same
/// structural interlock [`RenderSink`] carries, on the parent's side of the
/// boundary.
pub trait StimulusHelper: Send {
    /// Spawn the child on `wav_path` with `device_uid` and `routing`, and
    /// return it with stdin/stdout already wired.
    ///
    /// The child opens its device and emits `ready` BEFORE any audio, so a
    /// spawn that returns `Ok` has not yet played a sample.
    fn spawn(
        &mut self,
        wav_path: &Path,
        device_uid: &str,
        routing: HelperRouting,
    ) -> Result<Box<dyn HelperProcess>, MeasureError>;
}

/// One live helper.
///
/// **EVERY method is safe to call on an already-dead child.** The teardown
/// ladder runs all five rungs on every exit path including panic, and a rung
/// that panicked on a dead child would abort the ladder before the render
/// device is destroyed — which is the exact failure the ladder exists to
/// prevent.
pub trait HelperProcess: Send {
    /// The child's OS process id.
    ///
    /// Used ONLY to build the render aggregate's expected UID for the
    /// device-gone assertion at the end of teardown. Never used to signal —
    /// that is [`request_abort`](Self::request_abort),
    /// [`request_terminate`](Self::request_terminate) and [`kill`](Self::kill),
    /// so the signalling mechanism has one home.
    fn pid(&self) -> u32;

    /// Teardown rung 1: write `abort\n` to stdin.
    ///
    /// The POLITE rung — the child runs the shared 5 ms raised cosine rather
    /// than stopping. "A hard stop is itself a full-scale click."
    fn request_abort(&mut self) -> Result<(), MeasureError>;

    /// Teardown rung 1b: SIGTERM.
    ///
    /// A SECOND ramp request, not a stop — the child's signal handler arms the
    /// SAME abort flag `abort\n` does, so a child that missed the stdin write
    /// still fades. MUST NOT be SIGKILL.
    fn request_terminate(&mut self) -> Result<(), MeasureError>;

    /// Teardown rung 1c: SIGKILL.
    ///
    /// LAST, and only after both deadlines. It bypasses `Drop`, so the child's
    /// render-aggregate RAII does not run and the render device survives —
    /// which is why the next rung asserts the device is gone and this one is
    /// reported rather than being silent.
    fn kill(&mut self) -> Result<(), MeasureError>;

    /// Teardown rung 1d: wait for the child to exit and reap it.
    ///
    /// Idempotent: a second reap returns the cached status rather than blocking
    /// forever on an already-reaped pid.
    fn reap(&mut self) -> Result<HelperExit, MeasureError>;

    /// The NON-BLOCKING twin of [`reap`](Self::reap): `None` means "still
    /// running", `Some` means "exited, and here is the status".
    ///
    /// The ladder needs a liveness probe it can poll against a deadline, and
    /// [`reap`](Self::reap) blocks on a live child so it cannot be one.
    /// Inferring liveness from stdout instead — treating EOF as "gone" — is
    /// wrong in both directions: a child that closed stdout but is still
    /// rendering reads as dead, and a child that keeps emitting progress lines
    /// past its deadline reads as alive forever.
    ///
    /// Idempotent for the same reason `reap` is: once a status has been
    /// observed it is cached and returned again rather than re-waited.
    fn try_reap(&mut self) -> Result<Option<HelperExit>, MeasureError>;

    /// Write one protocol line to stdin (`play\n`).
    ///
    /// Separate from [`request_abort`](Self::request_abort) so the abort path
    /// cannot be reached by a typo in a string.
    fn send_play(&mut self) -> Result<(), MeasureError>;

    /// Read one line from stdout, with a deadline.
    ///
    /// The parent never blocks forever on a wedged child: every read that can
    /// hang carries the deadline of the gate that made it, and an expired
    /// deadline is its own answer ([`HelperLine::DeadlineExpired`]) rather than
    /// being spelled the same way as EOF.
    fn read_line(&mut self, deadline: Duration) -> Result<HelperLine, MeasureError>;
}

/// The child's routing argument.
///
/// A THIRD spelling of the same idea would be wrong, so this is a plain mirror
/// of the capture-side routing and of `paraeq_coreaudio::StimulusRouting`,
/// declared here because this crate may name neither crate's type.
///
/// Routing is **provenance, not level**: which channel carries the mono
/// stimulus, never how loud it is, so the MS-2 interlock is untouched. It
/// matters because "left and right are separate measurements, so playing to
/// both at once measures their sum and nothing useful".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperRouting {
    /// Every output channel.
    Both,
    /// One channel index only; every other channel stays silent.
    Only(u32),
}

impl HelperRouting {
    /// The `--channel` argument value this routing spells.
    ///
    /// ONE mapping, here, so the parent's enum and the child's CLI cannot
    /// disagree about what `Only(n)` means. The implementation that spawns the
    /// child calls this instead of formatting its own string.
    pub fn as_channel_arg(self) -> String {
        match self {
            HelperRouting::Both => "both".to_owned(),
            HelperRouting::Only(ch) => ch.to_string(),
        }
    }
}

/// How the child ended. `signalled` distinguishes "we had to SIGKILL it" from
/// "it exited on its own", which is the difference between a clean abort and a
/// possibly-leaked render device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelperExit {
    pub code: Option<i32>,
    pub signalled: bool,
}

/// Read-only facts about audio devices, by UID.
///
/// # Why a seam and not a call
///
/// Both answers are HAL property reads, and MS-1 forbids this crate from
/// naming CoreAudio at all ("depends on `paraeq-dsp`, no Tauri dep, no
/// CoreAudio dep, no `unsafe`"). So the questions are declared here and
/// answered by the platform side, exactly as [`VolumeControl`] is.
///
/// # Why this is not [`EngineFacts`](crate::engine_seam::EngineFacts)
///
/// The engine's facts describe the CHAIN — what the controller has installed
/// and what its stream is doing. These describe the HARDWARE, under UIDs the
/// engine may never have heard of: the helper's private render aggregate is
/// created and destroyed by a different process entirely, and the engine has
/// no field that could report it.
pub trait DeviceFacts: Send {
    /// Is a device with this UID present in the HAL right now?
    ///
    /// The verification teardown asks it about `com.paraeq.render.<pid>` — the
    /// private aggregate the helper wraps the output device in. A SIGKILLed
    /// child bypasses `Drop`, so its aggregate can survive its process and be
    /// left wrapping the user's output; a child that exited cleanly can still
    /// have failed to destroy it. Neither case is inferable from an exit
    /// status, which is why this is asked rather than deduced.
    ///
    /// Contract: **when the answer cannot be established, say `true`.** A
    /// teardown that could not verify the device is gone must report a possible
    /// leak, not assume a clean one — the cost of a false report is a log line,
    /// and the cost of a false clean is a private device left on the user's
    /// output with nothing watching it.
    fn device_exists(&self, uid: &str) -> bool;

    /// The device's nominal sample rate, or `None` when it cannot be read —
    /// including when no device carries this UID.
    ///
    /// `None` is "cannot answer", never "0 Hz", and callers decide what that
    /// means for them. The verification rate fence treats it as permissive on
    /// purpose: it asks about the helper's render aggregate at a moment the
    /// helper may already have destroyed it, so an unanswerable read there is
    /// an expected benign race rather than a missing witness, and the same
    /// fence has already refused on the engine's own stream rate.
    fn nominal_sample_rate(&self, device_uid: &str) -> Option<f64>;
}
