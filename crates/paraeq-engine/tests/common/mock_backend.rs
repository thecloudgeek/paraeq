//! Shared `MockBackend` for controller tests: a scriptable, synchronous
//! [`AudioBackend`] with all state behind an `Arc` so tests keep a clone
//! (assertion side) and hand the other to `EngineHandle::spawn`.
//!
//! `pump` drives the engine's realtime entry point (`process_block`)
//! synchronously on the test thread; the `Mutex` around the processor is a
//! test-only lock (the production backend never locks on the IOProc path).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use paraeq_engine::backend::{AudioBackend, BackendEvent, StreamInfo};
use paraeq_engine::shared::RtProcessor;
use paraeq_engine::EngineError;

/// One recorded backend call, in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Call {
    Start {
        requested_buffer_frames: Option<usize>,
    },
    Stop,
}

struct Inner {
    calls: Vec<Call>,
    events: VecDeque<BackendEvent>,
    /// Injected failures: while > 0, `start` records the call, decrements,
    /// and returns an error (leaving the backend stopped).
    fail_next_starts: usize,
    /// The realtime processor moved in by the last successful `start`.
    processor: Option<RtProcessor>,
    /// `Some(n)`: report `buffer_frames = n` regardless of the request.
    /// `None`: honor the request (default 512).
    reported_buffer_frames: Option<usize>,
    reported_channels: usize,
    /// Per-start `(channels, buffer_frames)` overrides: each successful
    /// `start` pops the front one; when empty, the sticky
    /// `reported_channels` / `reported_buffer_frames` apply.
    reports: VecDeque<(usize, usize)>,
    /// out-vs-in sample-time delta fed to `process_block` by `pump`.
    sample_time_delta: f64,
    /// Channel count of the currently running stream (set by `start`).
    stream_channels: usize,
}

/// Scriptable in-process audio backend. `Clone` shares all state.
#[derive(Clone)]
pub struct MockBackend {
    inner: Arc<Mutex<Inner>>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub fn new() -> MockBackend {
        MockBackend {
            inner: Arc::new(Mutex::new(Inner {
                calls: Vec::new(),
                events: VecDeque::new(),
                fail_next_starts: 0,
                processor: None,
                reported_buffer_frames: None,
                reported_channels: 2,
                reports: VecDeque::new(),
                sample_time_delta: 512.0,
                stream_channels: 2,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("mock backend lock")
    }

    /// Report this effective buffer size from every `start`, regardless of
    /// what was requested (renegotiation trigger).
    pub fn set_reported_buffer_frames(&self, frames: usize) {
        self.lock().reported_buffer_frames = Some(frames);
    }

    /// Report this channel count from every `start` (renegotiation trigger).
    pub fn set_reported_channels(&self, channels: usize) {
        self.lock().reported_channels = channels;
    }

    /// Queue a per-start `(channels, buffer_frames)` report: the NEXT
    /// successful `start` reports exactly this geometry (then the queue
    /// advances; when empty, the sticky settings apply). Lets tests script
    /// a backend whose reported geometry shifts between starts.
    pub fn queue_report(&self, channels: usize, buffer_frames: usize) {
        self.lock().reports.push_back((channels, buffer_frames));
    }

    /// Make the next `n` calls to `start` fail.
    pub fn fail_next_starts(&self, n: usize) {
        self.lock().fail_next_starts = n;
    }

    /// Queue an event for the controller's next `poll_event`.
    pub fn queue_event(&self, event: BackendEvent) {
        self.lock().events.push_back(event);
    }

    /// The recorded call sequence so far.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    pub fn start_count(&self) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Start { .. }))
            .count()
    }

    pub fn stop_count(&self) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Stop))
            .count()
    }

    /// A processor is installed (last `start` succeeded, no `stop` since).
    pub fn is_running(&self) -> bool {
        self.lock().processor.is_some()
    }

    /// Drive one synchronous realtime callback: `frames` samples of constant
    /// `amplitude` on every channel of the current stream. Returns the
    /// per-channel output, or `None` when the backend is not running.
    pub fn pump(&self, frames: usize, amplitude: f32) -> Option<Vec<Vec<f32>>> {
        let mut inner = self.lock();
        let channels = inner.stream_channels;
        let delta = inner.sample_time_delta;
        let processor = inner.processor.as_mut()?;

        let input: Vec<Vec<f32>> = vec![vec![amplitude; frames]; channels];
        let input_views: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
        let mut output: Vec<Vec<f32>> = vec![vec![0.0; frames]; channels];
        {
            let mut output_views: Vec<&mut [f32]> =
                output.iter_mut().map(Vec::as_mut_slice).collect();
            processor.process_block(&input_views, &mut output_views, delta);
        }
        Some(output)
    }
}

impl AudioBackend for MockBackend {
    fn start(
        &mut self,
        processor: RtProcessor,
        requested_buffer_frames: Option<usize>,
    ) -> Result<StreamInfo, EngineError> {
        let mut inner = self.lock();
        inner.calls.push(Call::Start {
            requested_buffer_frames,
        });
        if inner.fail_next_starts > 0 {
            inner.fail_next_starts -= 1;
            return Err(EngineError::Backend("injected start failure".into()));
        }
        let (channels, buffer_frames) = match inner.reports.pop_front() {
            Some(report) => report,
            None => (
                inner.reported_channels,
                inner
                    .reported_buffer_frames
                    .or(requested_buffer_frames)
                    .unwrap_or(512),
            ),
        };
        inner.stream_channels = channels;
        inner.processor = Some(processor);
        Ok(StreamInfo {
            buffer_frames,
            channels,
            device_uid: "mock-device".into(),
            sample_rate: 48_000.0,
        })
    }

    fn stop(&mut self) -> Result<(), EngineError> {
        let mut inner = self.lock();
        inner.calls.push(Call::Stop);
        // Dropping the processor here lands on the controller thread --
        // exactly where the engine wants deallocation to happen.
        inner.processor = None;
        Ok(())
    }

    fn poll_event(&mut self) -> Option<BackendEvent> {
        self.lock().events.pop_front()
    }
}
