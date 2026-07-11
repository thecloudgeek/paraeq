//! Engine controller: a control-plane thread that owns an [`AudioBackend`],
//! drives the silence watchdog, applies commands, and publishes serialized
//! [`EngineState`] snapshots on every change.
//!
//! ## Source of truth across rebuilds
//!
//! `Watchdog::started()` re-baselines against ZEROED counters, so **every**
//! backend start (initial, renegotiation, rebuild-on-change, re-enable)
//! constructs a fresh `RtShared` + ring pair + chain/`RtProcessor`, and the
//! controller re-applies its retained config (bypass, gain, correction,
//! requested buffer frames) to the fresh state. Nothing on the realtime
//! side survives a restart.
//!
//! ## Provisional start + renegotiation
//!
//! The backend needs the `RtProcessor` up front, but the effective
//! [`StreamInfo`] only comes back from `start`. The controller therefore
//! builds the chain provisionally for 2 channels (the parity tap is stereo)
//! and `block_size = requested_buffer_frames.unwrap_or(512)`; if the
//! reported `buffer_frames` OR `channels` differ, it renegotiates with a
//! stop+start cycle that rebuilds the chain at the last-REPORTED geometry
//! and requests exactly the last-reported buffer frames -- looping until
//! the report matches the chain, capped at 3 total starts. If the report
//! is *still* moving after the cap, the mismatch is accepted with a
//! warning: the chain's uniform frame guard degrades gracefully
//! (pass-through + `frame_mismatch` telemetry) until a `FormatChanged`
//! event triggers the next rebuild.
//!
//! ## Realtime discipline
//!
//! This module is control-plane only: allocation is fine here, but nothing
//! in it may block the realtime side. All cross-thread communication is the
//! `Arc<RtShared>` atomics and the SPSC rings -- no locks are shared with
//! `process_block`. Correction processors are built AND warmed up here,
//! then moved through the ring; retired processors come back through the
//! retire ring and are dropped here.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::backend::{AudioBackend, StreamInfo};
use crate::chain::{build_fir, build_iir, Correction, RealtimeChain};
use crate::shared::{links, ControlLink, RtMsg, RtProcessor, RtShared};
use crate::status::{EngineStatus, Watchdog, WatchdogConfig};
use crate::EngineError;

/// Serializable correction description -- the controller-retained source of
/// truth. Rebuilt into a live [`Correction`] (with warm-up) for the current
/// stream geometry on every apply.
#[derive(Clone, Debug, serde::Serialize)]
pub enum CorrectionConfig {
    Fir { firs: Vec<Vec<f64>> },
    Iir { sos_per_channel: Vec<Vec<[f64; 6]>> },
}

impl CorrectionConfig {
    /// Short human-readable descriptor for state snapshots.
    fn descriptor(&self) -> String {
        match self {
            CorrectionConfig::Fir { firs } => {
                format!("fir:{}-tap", firs.iter().map(Vec::len).max().unwrap_or(0))
            }
            CorrectionConfig::Iir { sos_per_channel } => format!(
                "iir:{}-band",
                sos_per_channel.iter().map(Vec::len).max().unwrap_or(0)
            ),
        }
    }
}

/// `None` when `config` is buildable; `Some(reason)` for degenerate configs
/// that would panic the underlying builders (empty FIR list, an empty FIR,
/// or an empty SOS-set list). The controller checks this before every
/// `SetCorrection` so a malformed command can never panic its thread.
fn validate_correction(config: &CorrectionConfig) -> Option<String> {
    match config {
        CorrectionConfig::Fir { firs } => {
            if firs.is_empty() {
                Some("Fir config has no FIRs".into())
            } else if firs.iter().any(Vec::is_empty) {
                Some("Fir config contains an empty (0-tap) FIR".into())
            } else {
                None
            }
        }
        CorrectionConfig::Iir { sos_per_channel } => {
            if sos_per_channel.is_empty() {
                Some("Iir config has no SOS sets".into())
            } else {
                None
            }
        }
    }
}

/// Build + warm up a correction for the given stream geometry. Control
/// plane only (allocates). Panics on degenerate configs (empty FIR list) --
/// a programmer-error contract inherited from the underlying builders; the
/// controller pre-screens commands with [`validate_correction`].
pub fn build_correction(
    config: &CorrectionConfig,
    channels: usize,
    block_size: usize,
) -> Correction {
    match config {
        CorrectionConfig::Fir { firs } => build_fir(firs.clone(), channels, block_size),
        CorrectionConfig::Iir { sos_per_channel } => {
            build_iir(sos_per_channel.clone(), channels, block_size)
        }
    }
}

/// Commands accepted by [`EngineHandle::send`].
pub enum EngineCommand {
    ClearCorrection,
    Disable,
    Enable,
    SetBufferFrames(usize),
    SetBypass(bool),
    SetCorrection(CorrectionConfig),
    SetGainDb(f32),
    Shutdown,
}

/// One serialized engine snapshot, published on every change (spec line
/// 135); stage 4 pushes it over Tauri as-is.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct EngineState {
    pub bypass: bool,
    /// Short descriptor of the retained correction, e.g. `"iir:5-band"`.
    pub correction: Option<String>,
    pub gain_db: f32,
    pub input_peak: f32,
    /// `sample_time_delta / sample_rate * 1000` for the live stream.
    pub latency_ms: Option<f64>,
    pub status: EngineStatus,
    pub stream: Option<StreamInfo>,
}

/// Controller tuning. Tests shorten the watchdog windows and tick so the
/// suite runs in real time without real waits.
#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    /// Buffer-frame-size hint passed to the backend (user-settable via
    /// [`EngineCommand::SetBufferFrames`]).
    pub requested_buffer_frames: Option<usize>,
    /// Slots per SPSC ring (swap + retire). Must be >= 1.
    pub ring_capacity: usize,
    /// Controller tick period (watchdog observe + snapshot publish +
    /// retired drain + event poll).
    pub tick_ms: u64,
    pub watchdog: WatchdogConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            requested_buffer_frames: None,
            ring_capacity: 4,
            tick_ms: 250,
            watchdog: WatchdogConfig::default(),
        }
    }
}

/// Handle to a spawned engine controller thread.
///
/// Dropping the handle sends [`EngineCommand::Shutdown`] and joins the
/// controller thread (which stops the backend on every exit path).
pub struct EngineHandle {
    cmd_tx: Sender<EngineCommand>,
    join: Option<JoinHandle<()>>,
    state: Arc<ArcSwap<EngineState>>,
    subscribers: Arc<Mutex<Vec<SyncSender<Arc<EngineState>>>>>,
}

impl EngineHandle {
    /// Spawn the controller thread and immediately start the backend.
    pub fn spawn<B: AudioBackend + 'static>(backend: B, config: EngineConfig) -> EngineHandle {
        assert!(config.ring_capacity >= 1, "ring_capacity must be >= 1");
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let initial = Arc::new(EngineState {
            bypass: false,
            correction: None,
            gain_db: 0.0,
            input_peak: 0.0,
            latency_ms: None,
            status: EngineStatus::Stopped,
            stream: None,
        });
        let state = Arc::new(ArcSwap::new(Arc::clone(&initial)));
        let subscribers: Arc<Mutex<Vec<SyncSender<Arc<EngineState>>>>> =
            Arc::new(Mutex::new(Vec::new()));

        let thread_state = Arc::clone(&state);
        let thread_subscribers = Arc::clone(&subscribers);
        let join = std::thread::Builder::new()
            .name("paraeq-engine-controller".into())
            .spawn(move || {
                Controller {
                    backend: StopGuard(backend),
                    bypass: false,
                    cmd_rx,
                    consecutive_start_failures: 0,
                    correction: None,
                    enabled: true,
                    epoch: Instant::now(),
                    failed: None,
                    gain_db: 0.0,
                    last_tick: Instant::now(),
                    published: initial,
                    requested_buffer_frames: config.requested_buffer_frames,
                    ring_capacity: config.ring_capacity,
                    session: None,
                    state: thread_state,
                    subscribers: thread_subscribers,
                    swap_pending: false,
                    tick: Duration::from_millis(config.tick_ms),
                    watchdog: Watchdog::new(config.watchdog),
                }
                .run();
            })
            .expect("spawn paraeq-engine-controller thread");

        EngineHandle {
            cmd_tx,
            join: Some(join),
            state,
            subscribers,
        }
    }

    /// Queue a command for the controller (fire-and-forget; ignored after
    /// shutdown).
    pub fn send(&self, cmd: EngineCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// The latest published snapshot (lock-free read).
    pub fn state(&self) -> Arc<EngineState> {
        self.state.load_full()
    }

    /// Subscribe to snapshots published from now on.
    ///
    /// The channel is BOUNDED (64 snapshots): a subscriber that stops
    /// reading gets updates dropped, not queued without limit -- the latest
    /// state is always available via [`state`](Self::state). A dropped
    /// receiver prunes the subscription on the next publish.
    pub fn subscribe(&self) -> Receiver<Arc<EngineState>> {
        let (tx, rx) = mpsc::sync_channel(64);
        self.subscribers
            .lock()
            .expect("subscriber list lock")
            .push(tx);
        rx
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(EngineCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Stops the backend when dropped, so ANY controller-thread exit path --
/// Shutdown, channel disconnect, panic unwinding -- runs the backend's
/// (idempotent) full teardown. A dead ParaEQ must never leave the system
/// muted.
struct StopGuard<B: AudioBackend>(B);

impl<B: AudioBackend> Drop for StopGuard<B> {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

/// Per-start realtime session: the control halves of the fresh state built
/// for one backend start. Dropped (with any queued/retired corrections) on
/// the control thread at stop.
struct Session {
    /// Chain build geometry -- corrections must be built to match.
    block_size: usize,
    channels: usize,
    control: ControlLink,
    shared: Arc<RtShared>,
    stream: StreamInfo,
}

struct Controller<B: AudioBackend> {
    backend: StopGuard<B>,
    bypass: bool,
    cmd_rx: Receiver<EngineCommand>,
    /// Consecutive `start` failures; at 2 the controller publishes `Failed`
    /// and stops retrying (no rebuild loop on a dead device).
    consecutive_start_failures: u32,
    correction: Option<CorrectionConfig>,
    enabled: bool,
    epoch: Instant,
    failed: Option<String>,
    gain_db: f32,
    /// When `on_tick` last ran. A command flood keeps `recv_timeout`
    /// returning `Ok` and would otherwise starve the tick work entirely
    /// (watchdog, event poll, retired drain, swap retry); `run` checks this
    /// after every command and runs the tick when it is due.
    last_tick: Instant,
    published: Arc<EngineState>,
    requested_buffer_frames: Option<usize>,
    ring_capacity: usize,
    session: Option<Session>,
    state: Arc<ArcSwap<EngineState>>,
    subscribers: Arc<Mutex<Vec<SyncSender<Arc<EngineState>>>>>,
    /// A correction send hit a full ring; rebuild from the stored config
    /// and retry next tick.
    swap_pending: bool,
    tick: Duration,
    watchdog: Watchdog,
}

impl<B: AudioBackend> Controller<B> {
    fn run(mut self) {
        self.try_start();
        self.publish();
        loop {
            match self.cmd_rx.recv_timeout(self.tick) {
                Ok(EngineCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => {
                    self.handle(cmd);
                    // Tick starvation guard: commands arriving faster than
                    // the tick keep recv_timeout from ever timing out, so
                    // run the due tick work here too.
                    if self.last_tick.elapsed() >= self.tick {
                        self.on_tick();
                    }
                    self.publish();
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.on_tick();
                    self.publish();
                }
            }
        }
        // Orderly teardown; the StopGuard additionally covers panic paths.
        self.stop_session();
        self.publish();
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn handle(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::ClearCorrection => {
                self.correction = None;
                self.send_correction();
            }
            EngineCommand::Disable => {
                // Full disable: tap destroyed, device unmuted -- "system
                // exactly as if ParaEQ never ran".
                self.enabled = false;
                self.stop_session();
            }
            EngineCommand::Enable => {
                self.enabled = true;
                self.failed = None;
                self.consecutive_start_failures = 0;
                self.try_start(); // no-op when already running
            }
            EngineCommand::SetBufferFrames(frames) => {
                self.requested_buffer_frames = Some(frames);
                if self.session.is_some() {
                    self.rebuild();
                }
            }
            EngineCommand::SetBypass(bypass) => {
                self.bypass = bypass;
                if let Some(s) = &self.session {
                    s.shared.bypass.store(bypass, Ordering::Relaxed);
                }
            }
            EngineCommand::SetCorrection(config) => {
                // Malformed configs would panic the builders (and take the
                // controller thread with them): warn + ignore, no state
                // change.
                if let Some(reason) = validate_correction(&config) {
                    log::warn!("SetCorrection ignored: {reason}");
                    return;
                }
                self.correction = Some(config);
                self.send_correction();
            }
            EngineCommand::SetGainDb(db) => {
                self.gain_db = db;
                if let Some(s) = &self.session {
                    s.shared.set_gain(db_to_linear(db));
                }
            }
            EngineCommand::Shutdown => unreachable!("Shutdown breaks the loop in run()"),
        }
    }

    fn on_tick(&mut self) {
        self.last_tick = Instant::now();
        // Retry a start deferred by a single earlier failure.
        if self.session.is_none() {
            self.try_start();
        }

        if let Some(s) = &mut self.session {
            let callbacks = s.shared.callbacks.load(Ordering::Relaxed);
            let nonzero = s.shared.nonzero_blocks.load(Ordering::Relaxed);
            let now = self.epoch.elapsed().as_millis() as u64;
            // Drives the informational status machine (Idle, InputSilent,
            // ...). No watchdog state is ever a rebuild trigger: the tap
            // aggregate's IOProc only cycles while the system renders audio
            // (hardware finding 2026-07-11), so a frozen callback counter
            // is normal idling -- rebuilding on it would loop forever while
            // the system is idle. Device death arrives as backend events.
            self.watchdog.observe(now, callbacks, nonzero);
            // Free retired corrections on the control thread.
            s.control.drain_retired();
        }

        // Rebuilds trigger ONLY on backend events (drain them all -> one
        // rebuild).
        let mut rebuild = false;
        while self.backend.0.poll_event().is_some() {
            rebuild = true;
        }

        if rebuild {
            self.rebuild();
        } else if self.swap_pending {
            self.send_correction();
        }
    }

    /// Start the backend if enabled, not running, and not failed. Two
    /// consecutive start failures latch `Failed`.
    fn try_start(&mut self) {
        if !self.enabled || self.failed.is_some() || self.session.is_some() {
            return;
        }
        match self.start_once() {
            Ok(()) => self.consecutive_start_failures = 0,
            Err(e) => {
                self.consecutive_start_failures += 1;
                if self.consecutive_start_failures >= 2 {
                    self.fail(format!("backend start failed twice: {e}"));
                }
                // else: retry on the next tick.
            }
        }
    }

    /// One start attempt: provisional geometry, then a renegotiation loop
    /// (capped at 3 total starts). Each retry rebuilds the chain at the
    /// last-REPORTED geometry and requests exactly the last-reported buffer
    /// frames; the loop breaks when the report matches the chain. If the
    /// report is still moving after the cap, the mismatch is accepted with
    /// a warning (the chain's uniform frame guard degrades gracefully).
    fn start_once(&mut self) -> Result<(), EngineError> {
        // Provisional: the parity tap is stereo; block from the stored
        // request. See the module docs ("Provisional start + renegotiation").
        const MAX_STARTS: u32 = 3;
        let mut channels = 2;
        let mut block_size = self.requested_buffer_frames.unwrap_or(512);
        let mut request = self.requested_buffer_frames;
        for attempt in 1..=MAX_STARTS {
            let info = self.start_with(channels, block_size, request)?;
            if info.buffer_frames == block_size && info.channels == channels {
                break;
            }
            if attempt == MAX_STARTS {
                log::warn!(
                    "renegotiation did not converge after {MAX_STARTS} starts \
                     (chain {channels} ch x {block_size} frames, reported {info:?}); \
                     accepting degraded pass-through until the next format event"
                );
                break;
            }
            // Renegotiate at the reported geometry. Stop errors are
            // non-fatal here (the backend contract keeps stop idempotent).
            let _ = self.backend.0.stop();
            self.session = None;
            channels = info.channels;
            block_size = info.buffer_frames;
            request = Some(info.buffer_frames);
        }

        self.watchdog.started(self.now_ms());
        Ok(())
    }

    /// Build a FRESH `RtShared` + links + chain, re-apply the retained
    /// bypass/gain/correction, and hand the `RtProcessor` to the backend.
    /// `request` is the buffer-frame hint passed to the backend: the user's
    /// stored request on the first start, the last-reported size on
    /// renegotiation retries.
    fn start_with(
        &mut self,
        channels: usize,
        block_size: usize,
        request: Option<usize>,
    ) -> Result<StreamInfo, EngineError> {
        let shared = Arc::new(RtShared::default());
        // Retained params re-applied to the fresh shared state BEFORE start.
        shared.bypass.store(self.bypass, Ordering::Relaxed);
        shared.set_gain(db_to_linear(self.gain_db));

        let (mut control, rt) = links(self.ring_capacity);
        let chain = RealtimeChain::new(channels, block_size);

        // Re-apply the stored correction through the fresh ring; it is
        // polled on the first callback. A fresh ring (capacity >= 1) cannot
        // be full, so this send never defers.
        self.swap_pending = false;
        if let Some(config) = &self.correction {
            let correction = build_correction(config, channels, block_size);
            control.send(RtMsg::Correction(Some(correction)))?;
        }

        let processor = RtProcessor::new(Arc::clone(&shared), rt, chain);
        let stream = self.backend.0.start(processor, request)?;
        self.session = Some(Session {
            block_size,
            channels,
            control,
            shared,
            stream: stream.clone(),
        });
        Ok(stream)
    }

    /// Stop the running session (if any): drain retired corrections, tear
    /// the backend down, mark the watchdog stopped.
    fn stop_session(&mut self) {
        self.swap_pending = false;
        if let Some(mut s) = self.session.take() {
            s.control.drain_retired();
            let _ = self.backend.0.stop();
        }
        self.watchdog.stopped();
    }

    /// Rebuild-on-change: full stop, then a fresh start from retained
    /// config (spec line 93).
    fn rebuild(&mut self) {
        self.stop_session();
        self.try_start();
    }

    /// Build the retained correction (or a clear) for the live session's
    /// geometry and send it through the ring. Ring-full keeps it pending;
    /// the next tick rebuilds from the stored config and retries.
    fn send_correction(&mut self) {
        let Some(s) = &mut self.session else {
            // Not running: the stored config is applied at the next start.
            self.swap_pending = false;
            return;
        };
        let msg = RtMsg::Correction(
            self.correction
                .as_ref()
                .map(|config| build_correction(config, s.channels, s.block_size)),
        );
        match s.control.send(msg) {
            Ok(()) => self.swap_pending = false,
            // Ring-full: the built correction was dropped (control plane --
            // safe); retry from the stored config next tick.
            Err(_) => self.swap_pending = true,
        }
    }

    /// Latch `Failed` (controller-owned status) and make sure the backend
    /// is fully stopped.
    fn fail(&mut self, reason: String) {
        self.stop_session();
        let _ = self.backend.0.stop();
        self.failed = Some(reason);
    }

    fn effective_status(&self) -> EngineStatus {
        match &self.failed {
            Some(reason) => EngineStatus::Failed {
                reason: reason.clone(),
            },
            None => self.watchdog.status().clone(),
        }
    }

    /// Publish a snapshot iff it differs from the last published one.
    fn publish(&mut self) {
        let (stream, latency_ms, input_peak) = match &self.session {
            Some(s) => (
                Some(s.stream.clone()),
                (s.stream.sample_rate > 0.0)
                    .then(|| s.shared.sample_time_delta() / s.stream.sample_rate * 1000.0),
                s.shared.peak_in(),
            ),
            None => (None, None, 0.0),
        };
        let next = EngineState {
            bypass: self.bypass,
            correction: self.correction.as_ref().map(CorrectionConfig::descriptor),
            gain_db: self.gain_db,
            input_peak,
            latency_ms,
            status: self.effective_status(),
            stream,
        };
        if effectively_equal(&next, &self.published) {
            return;
        }
        let next = Arc::new(next);
        self.state.store(Arc::clone(&next));
        self.published = Arc::clone(&next);
        let mut subscribers = self.subscribers.lock().expect("subscriber list lock");
        subscribers.retain(|tx| match tx.try_send(Arc::clone(&next)) {
            // Full: the subscriber stopped reading -- drop THIS update (it
            // can always read the latest via `state()`), keep the channel.
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

/// Change detection for `publish`, with the jittery telemetry quantized:
/// `latency_ms` compares at 0.1 ms and `input_peak` at 1e-3 resolution, so
/// HAL sample-time jitter does not publish a fresh snapshot every tick.
fn effectively_equal(a: &EngineState, b: &EngineState) -> bool {
    let q_latency = |l: Option<f64>| l.map(|v| (v * 10.0).round() as i64);
    let q_peak = |p: f32| (f64::from(p) * 1000.0).round() as i64;
    a.bypass == b.bypass
        && a.correction == b.correction
        && a.gain_db == b.gain_db
        && q_peak(a.input_peak) == q_peak(b.input_peak)
        && q_latency(a.latency_ms) == q_latency(b.latency_ms)
        && a.status == b.status
        && a.stream == b.stream
}

/// dB -> linear amplitude (f64 math, f32 at the atomics boundary).
fn db_to_linear(db: f32) -> f32 {
    10f64.powf(f64::from(db) / 20.0) as f32
}
