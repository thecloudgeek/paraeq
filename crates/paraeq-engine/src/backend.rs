//! The engine <-> platform seam (spec's "acquisition behind a trait").
//!
//! The engine defines [`AudioBackend`]; platform crates implement it
//! (`paraeq-coreaudio`'s `TapBackend` on macOS, `MockBackend` in tests).
//! The dependency points *into* the engine, so the engine stays free of
//! CoreAudio/Tauri and daemon extraction remains "move the crate behind a
//! socket". All lifecycle logic (engage gating, watchdog, rebuild-on-change)
//! lives in the engine controller, unit-testable against a mock.

use crate::shared::RtProcessor;
use crate::EngineError;

/// The effective stream geometry a backend is running with, as reported by
/// a successful [`AudioBackend::start`]. The controller compares it against
/// its provisional chain parameters and renegotiates (one stop+start cycle)
/// when `buffer_frames` or `channels` differ.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct StreamInfo {
    pub buffer_frames: usize,
    pub channels: usize,
    pub device_uid: String,
    pub sample_rate: f64,
}

/// Out-of-band device change reported by [`AudioBackend::poll_event`].
/// Every variant triggers the same controller reaction: a full stop +
/// rebuild + start cycle from controller-retained config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendEvent {
    DefaultOutputChanged,
    DeviceDied,
    FormatChanged,
}

/// A platform audio backend driven by the engine controller.
///
/// Contract:
/// - `start` moves the [`RtProcessor`] into the backend's realtime context
///   (the IOProc closure on macOS) and begins streaming.
///   `requested_buffer_frames` is a hint; the returned [`StreamInfo`]
///   reports the *effective* geometry, which may differ. A failed `start`
///   must leave the backend fully stopped.
/// - `stop` runs the backend's full teardown (for the tap backend: stop ->
///   destroy IOProc -> destroy aggregate -> destroy tap, every OSStatus
///   surfaced) and MUST be idempotent -- the controller calls it on every
///   exit path, including panic unwinding, and a dead ParaEQ must never
///   leave the system muted.
/// - `poll_event` is non-blocking; the controller drains it once per tick.
///   Backends queue events from listener callbacks (control plane), never
///   from the realtime path.
/// - `self_excluded` reports the LIVE capture's self-exclusion and MUST be
///   `false` whenever nothing is running -- see the method's own contract.
pub trait AudioBackend: Send {
    fn start(
        &mut self,
        processor: RtProcessor,
        requested_buffer_frames: Option<usize>,
    ) -> Result<StreamInfo, EngineError>;

    fn stop(&mut self) -> Result<(), EngineError>;

    fn poll_event(&mut self) -> Option<BackendEvent>;

    /// Whether the backend's LIVE capture excludes this process's own audio
    /// right now -- the MS-6 witness (measurement-safety `MS-6`). The
    /// controller publishes it as
    /// [`EngineState::self_excluded`](crate::controller::EngineState), and the
    /// measurement wizard refuses to begin a `Direct` capture when it is
    /// `false` (wizard `§ self_excluded Requirement`): an "uncorrected" baseline that was silently
    /// corrected is worse than no baseline, and feedback is live.
    ///
    /// Contract: `false` whenever nothing is running -- before the first
    /// `start`, after `stop`, after a failed `start`, and between the two
    /// halves of a rebuild. The invariant is not being *witnessed* then, and a
    /// capture can come up on the very next controller tick, so reporting
    /// `true` would be a claim about a topology that is not there.
    ///
    /// No default implementation, deliberately. A `true` default would be a
    /// silent lie about exactly the safety fact this method carries, and a
    /// `false` default would make every measurement refuse; either way the
    /// compiler would stop telling a new backend that it owes an answer.
    fn self_excluded(&self) -> bool;
}
