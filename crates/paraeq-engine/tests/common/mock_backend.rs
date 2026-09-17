//! Shared `MockBackend` for controller tests: a scriptable, synchronous
//! [`AudioBackend`] with all state behind an `Arc` so tests keep a clone
//! (assertion side) and hand the other to `EngineHandle::spawn`.
//!
//! `pump` drives the engine's realtime entry point (`process_block`)
//! synchronously on the test thread; the `Mutex` around the processor is a
//! test-only lock (the production backend never locks on the IOProc path).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// `Some(n)`: the n-th `start` of the whole run (1-based) fails,
    /// whichever attempt it is. `fail_next_starts` cannot express this --
    /// it always fails from the NEXT start onwards, while the renegotiation
    /// loop makes several starts inside ONE `start_once` with no chance for
    /// the test thread to intervene between them.
    fail_start_at: Option<usize>,
    /// The realtime processor moved in by the last successful `start`.
    processor: Option<RtProcessor>,
    /// Per-start sample-rate overrides: each successful `start` pops the
    /// front one; when empty, the sticky `reported_sample_rate` applies.
    /// Lets ONE session be scripted 44.1 -> 48 -> 96 kHz across three
    /// `FormatChanged` rebuilds (R1-6).
    rates: VecDeque<f64>,
    /// `Some(n)`: report `buffer_frames = n` regardless of the request.
    /// `None`: honor the request (default 512).
    reported_buffer_frames: Option<usize>,
    reported_channels: usize,
    /// Sample rate reported by every `start` once `rates` is exhausted.
    reported_sample_rate: f64,
    /// Per-start `(channels, buffer_frames)` overrides: each successful
    /// `start` pops the front one; when empty, the sticky
    /// `reported_channels` / `reported_buffer_frames` apply.
    reports: VecDeque<(usize, usize)>,
    /// out-vs-in sample-time delta fed to `process_block` by `pump`.
    sample_time_delta: f64,
    /// Scripted MS-6 answer for a RUNNING backend: `true` (a healthy tap
    /// that excluded our process) unless a test says otherwise. `start`
    /// publishes it into `exclusion`; `stop` clears that cell.
    self_excluded: bool,
    /// Channel count of the currently running stream (set by `start`).
    stream_channels: usize,
}

/// Scriptable in-process audio backend. `Clone` shares all state.
#[derive(Clone)]
pub struct MockBackend {
    /// The MS-6 witness cell, OUTSIDE the mutex on purpose -- `TapBackend`
    /// keeps its `ExclusionWitness` in a lock-free `Arc<AtomicBool>` for the
    /// same reason, and a mock that answered from behind `inner` would be
    /// lying about the cost as well as the value. The controller reads this on
    /// every `publish`, i.e. on every tick and after every command, while the
    /// test thread is inside `pump` holding `inner`: routing it through the
    /// mutex made `publish` queue behind a 512-frame block and turned an
    /// existing microsecond-wide race in `set_correction_swaps_and_retires_off_thread`
    /// (observe the realtime effect, then read the snapshot) into a ~60%
    /// failure. Measured: 2/5 runs green through the mutex, 8/8 without it.
    exclusion: Arc<AtomicBool>,
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
            // Nothing started yet, so nothing is witnessed (trait contract).
            exclusion: Arc::new(AtomicBool::new(false)),
            inner: Arc::new(Mutex::new(Inner {
                calls: Vec::new(),
                events: VecDeque::new(),
                fail_next_starts: 0,
                fail_start_at: None,
                processor: None,
                rates: VecDeque::new(),
                reported_buffer_frames: None,
                reported_channels: 2,
                reported_sample_rate: 48_000.0,
                reports: VecDeque::new(),
                sample_time_delta: 512.0,
                self_excluded: true,
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

    /// Report this sample rate from every `start` (sticky), once any queued
    /// per-start rates are exhausted. The real backend gets this from the
    /// device; R1-6 turns it into the rate corrections are DESIGNED at.
    pub fn set_reported_sample_rate(&self, sample_rate: f64) {
        self.lock().reported_sample_rate = sample_rate;
    }

    /// Queue a per-start sample rate: the NEXT successful `start` reports
    /// exactly this rate (then the queue advances; when empty the sticky
    /// `set_reported_sample_rate` value applies). Scripts an AirPods-style
    /// 44.1 <-> 48 kHz handoff across rebuilds.
    pub fn queue_sample_rate(&self, sample_rate: f64) {
        self.lock().rates.push_back(sample_rate);
    }

    /// Queue a per-start `(channels, buffer_frames)` report: the NEXT
    /// successful `start` reports exactly this geometry (then the queue
    /// advances; when empty, the sticky settings apply). Lets tests script
    /// a backend whose reported geometry shifts between starts.
    pub fn queue_report(&self, channels: usize, buffer_frames: usize) {
        self.lock().reports.push_back((channels, buffer_frames));
    }

    /// Script the MS-6 self-exclusion answer for while the backend is
    /// RUNNING, and apply it immediately if it already is. `false` reproduces
    /// `tap.rs`'s fail-open path (`translate_pid` returned 0 twice, so the
    /// tap's exclusion list went out empty and ParaEQ's own audio IS captured)
    /// -- the one state the measurement wizard refuses on.
    ///
    /// It cannot make a STOPPED backend report `true`: the witness only ever
    /// goes true in `start`, exactly as `TapBackend`'s does.
    pub fn set_self_excluded(&self, self_excluded: bool) {
        let mut inner = self.lock();
        inner.self_excluded = self_excluded;
        if inner.processor.is_some() {
            self.exclusion.store(self_excluded, Ordering::Release);
        }
    }

    /// Make the next `n` calls to `start` fail.
    pub fn fail_next_starts(&self, n: usize) {
        self.lock().fail_next_starts = n;
    }

    /// Fail the `n`-th `start` of the whole run (1-based), leaving every
    /// other start alone. Scripts the one window `fail_next_starts` cannot
    /// reach: a geometry renegotiation whose RETRY start fails, all inside a
    /// single `start_once`.
    pub fn fail_start_number(&self, n: usize) {
        self.lock().fail_start_at = Some(n);
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

    /// Drive one synchronous realtime callback with EXPLICIT per-channel
    /// input samples. `pump`'s constant block cannot discriminate a filter
    /// (a peaking band's DC response is 0 dB at every rate), so rate-
    /// independence tests feed an impulse through here instead. `input`
    /// must carry one buffer per stream channel, all the same length.
    /// Returns the per-channel output, or `None` when the backend is not
    /// running.
    pub fn pump_samples(&self, input: &[Vec<f32>]) -> Option<Vec<Vec<f32>>> {
        let mut inner = self.lock();
        let channels = inner.stream_channels;
        let delta = inner.sample_time_delta;
        assert_eq!(
            input.len(),
            channels,
            "pump_samples needs one buffer per stream channel"
        );
        let frames = input[0].len();
        assert!(
            input.iter().all(|c| c.len() == frames),
            "every channel must supply the same frame count"
        );
        let processor = inner.processor.as_mut()?;

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
        let start_number = inner
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Start { .. }))
            .count();
        if inner.fail_start_at == Some(start_number) {
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
        let sample_rate = inner
            .rates
            .pop_front()
            .unwrap_or(inner.reported_sample_rate);
        inner.stream_channels = channels;
        inner.processor = Some(processor);
        // MS-6: there is a capture now, so publish what it excludes. A FAILED
        // start returns above without touching the cell, matching the trait's
        // "false when nothing is running".
        self.exclusion.store(inner.self_excluded, Ordering::Release);
        Ok(StreamInfo {
            buffer_frames,
            channels,
            device_uid: "mock-device".into(),
            sample_rate,
        })
    }

    fn stop(&mut self) -> Result<(), EngineError> {
        let mut inner = self.lock();
        inner.calls.push(Call::Stop);
        // Dropping the processor here lands on the controller thread --
        // exactly where the engine wants deallocation to happen.
        inner.processor = None;
        // MS-6: no capture, nothing witnessed. `TapBackend::stop` does this
        // after its teardown, and it is also the first half of a rebuild.
        self.exclusion.store(false, Ordering::Release);
        Ok(())
    }

    fn poll_event(&mut self) -> Option<BackendEvent> {
        self.lock().events.pop_front()
    }

    /// Read the witness cell, never the script: `start` puts the scripted
    /// answer in, `stop` clears it. A mock that kept answering `true` through a
    /// teardown would be a dishonest backend, and every controller test written
    /// against it would be pinning a topology that cannot exist.
    fn self_excluded(&self) -> bool {
        self.exclusion.load(Ordering::Acquire)
    }
}
