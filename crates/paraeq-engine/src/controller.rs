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
use paraeq_dsp::peq::EQBand;

use crate::backend::{AudioBackend, StreamInfo};
use crate::chain::{build_fir, build_iir, Correction, RealtimeChain};
use crate::shared::{links, ControlLink, RtMsg, RtProcessor, RtShared};
use crate::status::{EngineStatus, Watchdog, WatchdogConfig};
use crate::EngineError;

/// Maximum absolute per-band gain, in dB. `|gain_db|` above this is rejected.
pub const GAIN_LIMIT_DB: f64 = 30.0;
/// Maximum band Q. Q above this (or `< Q_MIN`) is rejected.
pub const Q_MAX: f64 = 100.0;
/// Minimum band Q. Q below this is rejected. Set far above the subnormal
/// overflow floor (~1e-309, where `alpha = sin(w0)/(2*q)` overflows to +inf
/// and yields NaN SOS coefficients) while still admitting the full realistic
/// audio Q range (~0.1-100).
pub const Q_MIN: f64 = 0.1;

/// Serializable correction description -- the controller-retained source of
/// truth. Rebuilt into a live [`Correction`] (with warm-up) for the current
/// stream geometry, AT THE LIVE STREAM RATE, on every apply.
///
/// ## `design_rate` means two different things (R1-6 / Open Q1)
///
/// [`CorrectionConfig::Peq`] carries design *intent* (bands), so
/// [`build_correction`] re-derives coefficients at whatever rate the stream
/// reports and a correction SURVIVES a rate switch -- a 47 Hz mode filter
/// stays at 47 Hz across an AirPods 44.1<->48 kHz handoff. There its
/// `design_rate` is **provenance only** and is never compared.
///
/// [`CorrectionConfig::Fir`] and [`CorrectionConfig::Iir`] carry baked
/// coefficients, which cannot be re-derived (`design_fir_correction` takes a
/// positional magnitude vector with no Hz grid; an SOS row has forgotten the
/// band it came from). There `design_rate` is a **compare key**: a mismatch
/// against the live rate is refused and the chain fails open to flat
/// pass-through, because audibly un-EQ'd beats audibly wrong-EQ'd.
///
/// See `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q1 and
/// `docs/specs/2026-07-15-engine-hardening-design.md:403`.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum CorrectionConfig {
    Fir {
        design_rate: f64,
        firs: Vec<Vec<f64>>,
    },
    Iir {
        design_rate: f64,
        sos_per_channel: Vec<Vec<[f64; 6]>>,
    },
    Peq {
        /// Per channel; the last set is broadcast to any remaining channels.
        bands: Vec<Vec<EQBand>>,
        design_rate: f64,
    },
}

impl CorrectionConfig {
    /// Short human-readable descriptor for state snapshots.
    fn descriptor(&self) -> String {
        match self {
            CorrectionConfig::Fir { firs, .. } => {
                format!("fir:{}-tap", firs.iter().map(Vec::len).max().unwrap_or(0))
            }
            CorrectionConfig::Iir {
                sos_per_channel, ..
            } => format!(
                "iir:{}-band",
                sos_per_channel.iter().map(Vec::len).max().unwrap_or(0)
            ),
            CorrectionConfig::Peq { bands, .. } => {
                format!("peq:{}-band", bands.iter().map(Vec::len).max().unwrap_or(0))
            }
        }
    }
}

/// What [`build_correction`] had to do to make a config installable.
/// Control-plane telemetry: nothing here is on the realtime path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BuildReport {
    /// Bands that are not designable at the live stream rate (at or above
    /// the new Nyquist, or otherwise out of range) and were dropped. The
    /// rest of the set still designs -- R1-3's rule, "the auto front-end
    /// must never be bricked by one bad band".
    pub bands_dropped: usize,
    /// SOS rows the R1-3 Jury funnel replaced with the identity section.
    pub sections_substituted: usize,
}

/// Rate-INDEPENDENT band sanity, the guard's owner-decided home
/// (`docs/decisions/2026-07-22-owner-value-calls.md`: "`validate_bands` guard
/// location | `paraeq-engine` (next to `validate_correction`) | Keeps
/// `paraeq-dsp` free of policy").
///
/// Checks everything that does not depend on the sample rate: `fc` finite and
/// `> 0`, `q` finite and in `[Q_MIN, Q_MAX]`, `gain_db` finite and within
/// `GAIN_LIMIT_DB`. The Nyquist bound is rate-DEPENDENT and lives in
/// [`validate_band_at`], because a band legal at 96 kHz can be above Nyquist
/// at 44.1 kHz and that verdict can only be reached once the stream rate is
/// known.
///
/// The message names the offending field but NOT the band index: callers hold
/// the index and prefix it (`desktop/src-tauri/src/eq.rs` surfaces these
/// strings verbatim in the UI).
pub fn validate_band(band: &EQBand) -> Result<(), String> {
    if !band.fc.is_finite() {
        return Err("fc must be a finite number".to_string());
    }
    if band.fc <= 0.0 {
        return Err(format!("fc {} Hz must be greater than 0", band.fc));
    }
    if !band.q.is_finite() {
        return Err("q must be a finite number".to_string());
    }
    if band.q < Q_MIN || band.q > Q_MAX {
        return Err(format!("q {} must be between {Q_MIN} and {Q_MAX}", band.q));
    }
    if !band.gain_db.is_finite() {
        return Err("gain_db must be a finite number".to_string());
    }
    if band.gain_db.abs() > GAIN_LIMIT_DB {
        return Err(format!(
            "gain_db {} must be within +/-{GAIN_LIMIT_DB} dB",
            band.gain_db
        ));
    }
    Ok(())
}

/// [`validate_band`] plus the rate-dependent Nyquist bound: `fc` must lie in
/// the OPEN interval `(0, sample_rate / 2)`. At `fc >= sample_rate / 2` the
/// biquad math is finite but meaningless (aliased around Nyquist -- spec
/// `:188`), which `paraeq_dsp::biquad::is_stable` does not catch, so it is
/// checked explicitly here.
pub fn validate_band_at(band: &EQBand, sample_rate: f64) -> Result<(), String> {
    let nyquist = sample_rate / 2.0;
    if band.fc.is_finite() && (band.fc <= 0.0 || band.fc >= nyquist) {
        return Err(format!(
            "fc {} Hz must be between 0 and Nyquist ({nyquist} Hz), exclusive",
            band.fc
        ));
    }
    validate_band(band)
}

/// `Some(reason)` for the shapes that would PANIC the underlying builders
/// (an empty FIR list, an empty FIR, an empty SOS-set list, an empty
/// band-set list). Separated from [`validate_correction`] because
/// [`build_correction`] must be panic-free for any caller, while bad band
/// *values* it drops one by one rather than refusing wholesale.
fn structural_defect(config: &CorrectionConfig) -> Option<String> {
    match config {
        CorrectionConfig::Fir { firs, .. } => {
            if firs.is_empty() {
                Some("Fir config has no FIRs".into())
            } else if firs.iter().any(Vec::is_empty) {
                Some("Fir config contains an empty (0-tap) FIR".into())
            } else {
                None
            }
        }
        CorrectionConfig::Iir {
            sos_per_channel, ..
        } => {
            if sos_per_channel.is_empty() {
                Some("Iir config has no SOS sets".into())
            } else {
                None
            }
        }
        CorrectionConfig::Peq { bands, .. } => {
            if bands.is_empty() {
                Some("Peq config has no band sets".into())
            } else {
                None
            }
        }
    }
}

/// `None` when `config` is acceptable to retain; `Some(reason)` for a
/// degenerate shape ([`structural_defect`]) or a band that is nonsense at
/// ANY rate ([`validate_band`]). The controller checks this before every
/// `SetCorrection` so a malformed command can never panic its thread and a
/// junk band is rejected at the seam where a user-facing error still exists.
///
/// Deliberately rate-INDEPENDENT: a `SetCorrection` may arrive before any
/// stream exists, and under R1-6 the rate-dependent verdicts (Nyquist, the
/// baked variants' `design_rate` compare) belong at build time, where the
/// live rate is in hand.
fn validate_correction(config: &CorrectionConfig) -> Option<String> {
    if let Some(reason) = structural_defect(config) {
        return Some(reason);
    }
    if let CorrectionConfig::Peq { bands, .. } = config {
        for (set, set_bands) in bands.iter().enumerate() {
            for (index, band) in set_bands.iter().enumerate() {
                if let Err(reason) = validate_band(band) {
                    return Some(format!("Peq set {set} band {index}: {reason}"));
                }
            }
        }
    }
    None
}

/// Build + warm up a correction for the given stream geometry AT
/// `stream_rate`. Control plane only (allocates); never panics.
///
/// `Peq` re-derives every band's SOS at `stream_rate` (its `design_rate` is
/// ignored). A band that is not designable there -- at or above the new
/// Nyquist -- is DROPPED and counted in the [`BuildReport`] rather than
/// failing the whole set, mirroring R1-3's "never bricked by one bad band"
/// (spec `:216`); the config is refused only when nothing survives.
///
/// `Fir` / `Iir` carry baked coefficients that cannot be re-derived, so they
/// are refused unless `design_rate == stream_rate` -- an EXACT comparison,
/// not a tolerance (spec `:420`: device-reported rates round-trip exactly in
/// f64 and a fuzzy compare would silently accept a genuinely different rate).
///
/// On `Err` the caller sends no correction at all: flat pass-through, the
/// same fail-open philosophy as the `AutoDisabledNoInput` path.
pub fn build_correction(
    config: &CorrectionConfig,
    channels: usize,
    block_size: usize,
    stream_rate: f64,
) -> Result<(Correction, BuildReport), EngineError> {
    if !stream_rate.is_finite() || stream_rate <= 0.0 {
        return Err(EngineError::InvalidConfig(format!(
            "stream sample rate {stream_rate} is not a usable rate"
        )));
    }
    // STRUCTURAL guard only -- the builders assert on these, and this
    // function must never panic even when called outside the controller's
    // `SetCorrection` pre-screen. Bad *values* are dropped band by band
    // below, not refused wholesale (R1-3, spec `:216`).
    if let Some(reason) = structural_defect(config) {
        return Err(EngineError::InvalidConfig(reason));
    }
    match config {
        CorrectionConfig::Fir { design_rate, firs } => {
            refuse_on_rate_mismatch(*design_rate, stream_rate, "Fir")?;
            Ok((
                build_fir(firs.clone(), channels, block_size),
                BuildReport::default(),
            ))
        }
        CorrectionConfig::Iir {
            design_rate,
            sos_per_channel,
        } => {
            refuse_on_rate_mismatch(*design_rate, stream_rate, "Iir")?;
            let (correction, substituted) =
                build_iir(sos_per_channel.clone(), channels, block_size);
            warn_on_substitutions(substituted);
            Ok((
                correction,
                BuildReport {
                    bands_dropped: 0,
                    sections_substituted: substituted,
                },
            ))
        }
        CorrectionConfig::Peq { bands, .. } => {
            let mut bands_dropped = 0;
            let mut kept = 0;
            let sos_per_channel: Vec<Vec<[f64; 6]>> = bands
                .iter()
                .map(|set| {
                    set.iter()
                        .filter_map(|band| match validate_band_at(band, stream_rate) {
                            Ok(()) => {
                                kept += 1;
                                Some(band.to_sos(stream_rate))
                            }
                            Err(_) => {
                                bands_dropped += 1;
                                None
                            }
                        })
                        .collect()
                })
                .collect();
            if kept == 0 && bands_dropped > 0 {
                return Err(EngineError::InvalidConfig(format!(
                    "no band of {bands_dropped} is designable at {stream_rate} Hz"
                )));
            }
            if bands_dropped > 0 {
                log::warn!(
                    "{bands_dropped} band(s) are at or above Nyquist at {stream_rate} Hz \
                     and were dropped; the rest of the correction still applies"
                );
            }
            let (correction, substituted) = build_iir(sos_per_channel, channels, block_size);
            warn_on_substitutions(substituted);
            Ok((
                correction,
                BuildReport {
                    bands_dropped,
                    sections_substituted: substituted,
                },
            ))
        }
    }
}

/// R1-6's last-resort net for the baked (un-re-derivable) variants.
fn refuse_on_rate_mismatch(
    design_rate: f64,
    stream_rate: f64,
    kind: &str,
) -> Result<(), EngineError> {
    if design_rate == stream_rate {
        return Ok(());
    }
    Err(EngineError::InvalidConfig(format!(
        "{kind} config carries baked coefficients designed at {design_rate} Hz, \
         which cannot be re-derived for the live {stream_rate} Hz stream"
    )))
}

fn warn_on_substitutions(substituted: usize) {
    if substituted > 0 {
        // Stability backstop (spec R1-3): the dropped bands do nothing
        // rather than destabilize the chain; surface the count. Control
        // plane only -- never the realtime path.
        log::warn!(
            "correction contained {substituted} unstable SOS section(s); \
             each was replaced with the identity section (band dropped)"
        );
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
    /// `Some(stream_rate)` when the retained correction could NOT be built
    /// for the live stream and the chain is therefore running flat (R1-6):
    /// the rate it must be redesigned for. `None` whenever a correction is
    /// installed, cleared, or there is no session.
    ///
    /// Session-scoped by construction -- it describes the LIVE stream's rate,
    /// so a teardown clears it. The desktop keys its redesign off this field
    /// rather than diffing rates itself, so the daemon seam inherits the
    /// behavior and the refusal lands in the same call that used to install
    /// stale coefficients (spec `:424`, gap 3 at `:407`).
    pub correction_rate_mismatch: Option<f64>,
    /// The controller's current enabled flag: `true` between an
    /// [`EngineCommand::Enable`] (or an enabled spawn) and the next
    /// [`EngineCommand::Disable`]/fail-open auto-disable. Lets the UI tell a
    /// user-disabled `Stopped` apart from a pre-start or failed one.
    pub enabled: bool,
    /// Cumulative count of realtime blocks the chain passed through instead
    /// of correcting because their frame count did not match the built
    /// geometry (degraded pass-through telemetry). Read from `RtShared` at
    /// snapshot time; retains the last observed count after a `Disable`
    /// tears the session down, and is re-zeroed by the fresh `RtShared` on
    /// every start.
    pub frame_mismatch_blocks: u64,
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
    /// Whether the controller starts the backend on spawn. `true` (the
    /// [`Default`]) preserves the historical spawn-and-start behavior; the
    /// desktop passes `false` pre-wizard so the tap is not engaged (and the
    /// system not muted) until the user explicitly enables the EQ. Either
    /// way, [`EngineCommand::Enable`]/[`EngineCommand::Disable`] flip it at
    /// runtime.
    pub enabled: bool,
    /// Fail-open window: how long [`EngineStatus::NoInputDetected`] may
    /// persist before the controller auto-disables (same path as
    /// [`EngineCommand::Disable`]: backend stopped, tap destroyed, device
    /// unmuted) and publishes [`EngineStatus::AutoDisabledNoInput`].
    /// `NoInputDetected` only occurs before the FIRST nonzero input since
    /// start -- the TCC silent-failure signature (the tap mutes the device
    /// but delivers zeros) -- so failing open restores the user's un-EQ'd
    /// audio instead of holding the system muted with silence indefinitely.
    /// Post-`Running` states (`Idle`, `InputSilent`) never fail open:
    /// pausing music must never disable the EQ. Measured engage worst case
    /// is ~4-5 s, so the 15 s default is comfortably conservative. `None`
    /// disables the behavior.
    pub fail_open_after_ms: Option<u64>,
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
            enabled: true,
            fail_open_after_ms: Some(15_000),
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
    /// Spawn the controller thread; it starts the backend immediately when
    /// `config.enabled` (the default), or waits for [`EngineCommand::Enable`]
    /// when spawned disabled.
    pub fn spawn<B: AudioBackend + 'static>(backend: B, config: EngineConfig) -> EngineHandle {
        assert!(config.ring_capacity >= 1, "ring_capacity must be >= 1");
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let initial = Arc::new(EngineState {
            bypass: false,
            correction: None,
            correction_rate_mismatch: None,
            enabled: config.enabled,
            frame_mismatch_blocks: 0,
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
                    auto_disabled: None,
                    backend: StopGuard(backend),
                    bypass: false,
                    cmd_rx,
                    consecutive_start_failures: 0,
                    correction: None,
                    correction_rate_mismatch: None,
                    enabled: config.enabled,
                    epoch: Instant::now(),
                    fail_open_after_ms: config.fail_open_after_ms,
                    failed: None,
                    frame_mismatch_blocks: 0,
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
    /// `invalid_samples` value already reported via log: the tick warns on
    /// the delta only (the rt side never logs; the tick period is the rate
    /// limit).
    invalid_seen: u64,
    shared: Arc<RtShared>,
    stream: StreamInfo,
}

struct Controller<B: AudioBackend> {
    /// `Some(after_ms)` after a fail-open auto-disable. Like `failed`, a
    /// controller-owned status override: `effective_status` keeps
    /// publishing `AutoDisabledNoInput` (not the watchdog's `Stopped`)
    /// until an explicit Enable/Disable clears it.
    auto_disabled: Option<u64>,
    backend: StopGuard<B>,
    bypass: bool,
    cmd_rx: Receiver<EngineCommand>,
    /// Consecutive `start` failures; at 2 the controller publishes `Failed`
    /// and stops retrying (no rebuild loop on a dead device).
    consecutive_start_failures: u32,
    correction: Option<CorrectionConfig>,
    /// Mirror of [`EngineState::correction_rate_mismatch`]: set when
    /// `build_correction` refuses the retained config at the live stream
    /// rate, cleared on every successful install, on `ClearCorrection`, and
    /// on teardown.
    correction_rate_mismatch: Option<f64>,
    enabled: bool,
    epoch: Instant,
    /// Fail-open window from [`EngineConfig::fail_open_after_ms`]
    /// (`None` = behavior disabled).
    fail_open_after_ms: Option<u64>,
    failed: Option<String>,
    /// Last `RtShared::frame_mismatch_blocks` observed at snapshot time.
    /// Retained across a torn-down session (so a `Disable` does not reset
    /// the surfaced count to 0); re-zeroed when the fresh `RtShared` of the
    /// next start reports 0.
    frame_mismatch_blocks: u64,
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
                // exactly as if ParaEQ never ran". An explicit Disable also
                // supersedes a fail-open latch: the honest status is now
                // plain Stopped.
                self.auto_disabled = None;
                self.enabled = false;
                self.stop_session();
            }
            EngineCommand::Enable => {
                self.auto_disabled = None;
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
            // Non-finite sanitization report, moved OFF the realtime thread
            // (the rt side only bumps the atomic): warn on the delta, at
            // most once per tick.
            let invalid = s.shared.invalid_samples.load(Ordering::Relaxed);
            if invalid > s.invalid_seen {
                log::warn!(
                    "ignored {} non-finite samples (zeroed at the capture/output guards)",
                    invalid - s.invalid_seen
                );
                s.invalid_seen = invalid;
            }
            // Free retired corrections on the control thread.
            s.control.drain_retired();
        }

        // Fail-open (spec: fail-safe ordering / TCC silent failure).
        // `NoInputDetected` means the tap has NEVER captured a nonzero
        // sample since start: either nothing is playing, or the TCC grant
        // is missing and the tap mutes the device while delivering zeros.
        // Holding that state forever risks a silent system, so past the
        // window the controller takes the same path as Disable -- backend
        // stopped, tap destroyed, device unmuted -- and latches
        // `AutoDisabledNoInput`. This can only fire from `NoInputDetected`
        // (pre-first-input by construction); post-`Running` states (`Idle`,
        // `InputSilent`) never reach here -- pausing music must never
        // disable the EQ. It does not touch the start-failure counter, and
        // there is no auto-retry (re-engaging would re-mute the system);
        // an explicit Enable starts again.
        if let (Some(window), &EngineStatus::NoInputDetected { since_ms }) =
            (self.fail_open_after_ms, self.watchdog.status())
        {
            let waited = self.now_ms().saturating_sub(since_ms);
            if waited >= window {
                log::warn!(
                    "fail-open: no input captured for {waited} ms -- auto-disabling \
                     (tap destroyed, un-EQ'd audio restored); send Enable to retry"
                );
                self.enabled = false;
                self.stop_session();
                self.auto_disabled = Some(waited);
            }
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

    /// Build a FRESH `RtShared` + links + chain, hand the `RtProcessor` to
    /// the backend, and re-apply the retained bypass/gain/correction.
    /// `request` is the buffer-frame hint passed to the backend: the user's
    /// stored request on the first start, the last-reported size on
    /// renegotiation retries.
    ///
    /// Ordering is load-bearing (R1-6): bypass and gain go on the shared
    /// state BEFORE `start`, but the correction is built AFTER it returns,
    /// because only then is the stream's actual sample rate known.
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

        let (control, rt) = links(self.ring_capacity);
        let chain = RealtimeChain::new(channels, block_size);

        let processor = RtProcessor::new(Arc::clone(&shared), rt, chain);
        let stream = self.backend.0.start(processor, request)?;
        self.session = Some(Session {
            block_size,
            channels,
            control,
            invalid_seen: 0,
            shared,
            stream: stream.clone(),
        });

        // R1-6 gap 3 (spec `:407`). The stored correction is re-applied
        // AFTER `start` returns, so it is built at the rate the backend just
        // REPORTED. Building it earlier is what made a rate switch re-send
        // coefficients designed for the old rate -- live and audible for a
        // whole snapshot round-trip (up to `tick_ms`, 250 ms by default)
        // before the desktop could even notice. Now the refusal happens in
        // the same call that used to install them.
        //
        // The cost is the few microseconds between `start` returning and the
        // ring send, during which the chain has no correction: flat
        // pass-through, the fail-open direction, and the same class of
        // transient the renegotiation loop above already produces. A fresh
        // ring (capacity >= 1) cannot be full, so this send never defers.
        self.send_correction();
        Ok(stream)
    }

    /// Stop the running session (if any): drain retired corrections, tear
    /// the backend down, mark the watchdog stopped.
    fn stop_session(&mut self) {
        self.swap_pending = false;
        // Session-scoped: the flag names the LIVE stream's rate, and there
        // is no longer a live stream.
        self.correction_rate_mismatch = None;
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
    /// geometry AND ITS REPORTED RATE, then send it through the ring.
    /// Ring-full keeps it pending; the next tick rebuilds from the stored
    /// config and retries.
    ///
    /// R1-6: when `build_correction` refuses the config at the live rate
    /// (baked coefficients from another rate, or a band set with nothing
    /// designable below the new Nyquist), the controller sends an explicit
    /// `Correction(None)` -- flat pass-through, not "leave the old one
    /// running" -- and publishes the rate it must be redesigned for.
    fn send_correction(&mut self) {
        let Some(s) = &mut self.session else {
            // Not running: nothing has been attempted at any rate, so there
            // is no refusal to report. The stored config is applied (and
            // judged) at the next start.
            self.swap_pending = false;
            self.correction_rate_mismatch = None;
            return;
        };
        let stream_rate = s.stream.sample_rate;
        let (msg, mismatch) = match self.correction.as_ref() {
            None => (RtMsg::Correction(None), None),
            Some(config) => match build_correction(config, s.channels, s.block_size, stream_rate) {
                // `_report`'s drop/substitution counts are already logged by
                // `build_correction`; publishing them in the Advanced drawer
                // is queued behind R1-8's meter fields.
                Ok((correction, _report)) => (RtMsg::Correction(Some(correction)), None),
                Err(e) => {
                    log::warn!(
                        "correction refused at {stream_rate} Hz ({e}); \
                         running flat pass-through until it is redesigned"
                    );
                    (RtMsg::Correction(None), Some(stream_rate))
                }
            },
        };
        match s.control.send(msg) {
            Ok(()) => {
                self.swap_pending = false;
                self.correction_rate_mismatch = mismatch;
            }
            // Ring-full: the built correction was dropped (control plane --
            // safe); nothing reached the chain, so the published verdict is
            // left alone. Retry from the stored config next tick.
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

    /// Controller-owned overrides first (`Failed`, then the fail-open
    /// `AutoDisabledNoInput` latch -- which must NOT be overwritten by the
    /// watchdog's `Stopped` after the auto-disable), else the watchdog's
    /// status.
    fn effective_status(&self) -> EngineStatus {
        if let Some(reason) = &self.failed {
            return EngineStatus::Failed {
                reason: reason.clone(),
            };
        }
        if let Some(after_ms) = self.auto_disabled {
            return EngineStatus::AutoDisabledNoInput { after_ms };
        }
        self.watchdog.status().clone()
    }

    /// Publish a snapshot iff it differs from the last published one.
    fn publish(&mut self) {
        let (stream, latency_ms, input_peak, mismatch) = match &self.session {
            Some(s) => (
                Some(s.stream.clone()),
                (s.stream.sample_rate > 0.0)
                    .then(|| s.shared.sample_time_delta() / s.stream.sample_rate * 1000.0),
                s.shared.peak_in(),
                Some(s.shared.frame_mismatch_blocks.load(Ordering::Relaxed)),
            ),
            None => (None, None, 0.0, None),
        };
        // A live session owns the current count (fresh RtShared -> 0 on each
        // start); a torn-down session retains the last observed count.
        if let Some(count) = mismatch {
            self.frame_mismatch_blocks = count;
        }
        let next = EngineState {
            bypass: self.bypass,
            correction: self.correction.as_ref().map(CorrectionConfig::descriptor),
            correction_rate_mismatch: self.correction_rate_mismatch,
            enabled: self.enabled,
            frame_mismatch_blocks: self.frame_mismatch_blocks,
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
/// `frame_mismatch_blocks` is compared only as `> 0` (the boolean the UI
/// needs, Task 14): a degraded stretch increments it once per block, and
/// comparing it raw would emit a fresh snapshot + Tauri event every tick for
/// the whole stretch. `enabled` compares exactly so a flip alone publishes.
/// `correction_rate_mismatch` compares EXACTLY (R1-6): it is the desktop's
/// redesign trigger and the user's only disclosure that the EQ is paused, so
/// a change in the rate it names must publish. It has no chatter to prevent
/// -- in a healthy session it is pinned at `None`.
fn effectively_equal(a: &EngineState, b: &EngineState) -> bool {
    let q_latency = |l: Option<f64>| l.map(|v| (v * 10.0).round() as i64);
    let q_peak = |p: f32| (f64::from(p) * 1000.0).round() as i64;
    a.bypass == b.bypass
        && a.correction == b.correction
        && a.correction_rate_mismatch == b.correction_rate_mismatch
        && a.enabled == b.enabled
        && (a.frame_mismatch_blocks > 0) == (b.frame_mismatch_blocks > 0)
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
