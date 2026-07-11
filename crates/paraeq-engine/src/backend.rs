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
    pub sample_rate: f64,
    pub channels: usize,
    pub buffer_frames: usize,
    pub device_uid: String,
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
pub trait AudioBackend: Send {
    fn start(
        &mut self,
        processor: RtProcessor,
        requested_buffer_frames: Option<usize>,
    ) -> Result<StreamInfo, EngineError>;

    fn stop(&mut self) -> Result<(), EngineError>;

    fn poll_event(&mut self) -> Option<BackendEvent>;
}
