//! The one production implementation of the measurement <-> engine seam:
//! `paraeq_measure::{EngineFacts, EngineControl}` over the live
//! `paraeq_engine::EngineHandle`.
//!
//! # Why the seam exists at all
//!
//! The verification pass reads and writes `paraeq-engine` facts from inside
//! `paraeq-measure`, and measurement-safety `MS-1` forbids the link verbatim:
//! "`crates/paraeq-measure` exists: depends on `paraeq-dsp`, no Tauri dep, no
//! CoreAudio dep, no `unsafe`. Sinks/sources are traits". So `paraeq-measure`
//! declares the traits and mirror types (`EngineStatusKind`, `TapActivity`)
//! and this crate implements them. No `paraeq-engine` type crosses the
//! boundary; the mapping from one vocabulary to the other happens here, once.
//!
//! # Why the desktop and not `paraeq-coreaudio`
//!
//! Precedent puts HAL seams in `paraeq-coreaudio` (`volume.rs`,
//! `measure_aggregate.rs`), and that crate already depends on both
//! `paraeq-engine` and `paraeq-measure`, so it *could* host these. But these
//! are CONTROLLER facts, not HAL facts: they live on `EngineState` behind an
//! `EngineHandle`, and the desktop is the only thing that holds one. Putting
//! the impl in `paraeq-coreaudio` would mean passing a handle down into the
//! crate that is supposed to sit *below* the controller.
//!
//! # What this module does NOT implement
//!
//! `paraeq_measure::TapStatus`. The MS-6 self-exclusion witness already has
//! exactly one production implementor -- `ExclusionWitness` in
//! `paraeq-coreaudio` -- and the desktop already parks a clone of it in
//! `AppShared`. [`tap_status`] passes that one through. A second implementor
//! with different capabilities is two answers to one safety question, which is
//! the defect this arrangement exists to prevent, and
//! `there_is_exactly_one_production_tap_status_impl` below is what stops it
//! coming back.

use crate::state::AppShared;
use paraeq_engine::controller::{
    EngineCommand, EngineHandle, EngineState, MeasurementLease, TapActivity,
};
use paraeq_engine::preamp;
use paraeq_engine::status::EngineStatus;
use paraeq_measure::seam::{DeviceFacts, HelperLine};
use paraeq_measure::{
    EngineControl, EngineFacts, EngineStatusKind, GainPin, HelperExit, HelperProcess,
    HelperRouting, MeasureError, MeasurementLeaseToken, StimulusHelper, TapStatus,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long [`set_gain_db`] waits for the engine to echo a trim back.
///
/// `EngineCommand` is fire-and-forget by design, so a write is only confirmed
/// by reading the next published snapshot. The controller publishes
/// immediately after applying a command, so this normally resolves in one tick
/// (10s of ms); the budget is generous because the alternative to waiting is
/// pinning a trim we never confirmed and then measuring through it.
const GAIN_READBACK_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll period for the same read-back. Control plane only.
const GAIN_READBACK_POLL: Duration = Duration::from_millis(2);

/// The `TapStatus` half of the verification `SessionSeam`, passed through from
/// the witness the desktop already holds.
///
/// `AppShared::exclusion_witness` is cloned off the `TapBackend` in
/// `engine_bridge::spawn_engine` BEFORE the backend is moved into the
/// controller thread -- the only moment it can be taken -- and it stays live
/// and truthful across every start and stop. That is the fact `MS-6` wants
/// polled at every gate that precedes emission, which is why the session reads
/// it rather than `EngineState::self_excluded` (the same fact, up to one
/// controller tick stale).
pub fn tap_status(shared: &AppShared) -> Box<dyn TapStatus> {
    Box::new(shared.exclusion_witness.clone())
}

/// How a [`EngineSeam`] reaches the live [`EngineHandle`].
///
/// Two ways in, because the handle lives in Tauri-managed state in production
/// and nowhere near a Tauri app in a unit test. Both are cheap to clone, which
/// matters: the gain pin's RAII restore closure has to outlive any borrow of
/// this seam, so it captures a clone.
#[derive(Clone)]
enum EngineSource {
    /// The handle in [`AppShared::engine`], reached through the app.
    Managed(tauri::AppHandle),
    /// The handle directly -- the tests below, and any future headless caller
    /// that owns one without a Tauri app around it.
    Direct(Arc<Mutex<Option<EngineHandle>>>),
}

impl EngineSource {
    /// Run `f` against the live handle; `None` when there is none (only after
    /// exit teardown has taken it).
    fn with<T>(&self, f: impl FnOnce(&EngineHandle) -> T) -> Option<T> {
        match self {
            EngineSource::Managed(app) => {
                let shared = tauri::Manager::state::<AppShared>(app);
                let guard = shared.engine.lock().expect("engine handle lock");
                guard.as_ref().map(f)
            }
            EngineSource::Direct(engine) => {
                let guard = engine.lock().expect("engine handle lock");
                guard.as_ref().map(f)
            }
        }
    }
}

/// Every [`EngineFacts`] answer, mapped from ONE `EngineState` snapshot.
///
/// A record rather than eleven separate accessors so the whole mapping is one
/// readable place and one PURE function -- `EngineState` in, seam vocabulary
/// out -- which is what makes
/// `engine_facts_maps_every_field_of_the_shipped_engine_state` writable
/// without an engine, a device or a Tauri app.
#[derive(Clone, Debug, PartialEq)]
struct Facts {
    bands_dropped: usize,
    bypass: bool,
    clipped_samples: u64,
    correction_installed: bool,
    correction_rate_mismatch_hz: Option<f64>,
    engine_engaged: bool,
    gain_db: f32,
    installed_preamp_lin: Option<f32>,
    latency_ms: Option<f64>,
    sections_substituted: usize,
    status_kind: EngineStatusKind,
    stream_rate_hz: Option<f64>,
}

impl Facts {
    /// The answer when there is no `EngineHandle` at all.
    ///
    /// Every field is the value that CANNOT be mistaken for a healthy engine:
    /// `engine_engaged: false` is what the first gate reads, and it refuses
    /// there, so nothing downstream ever sees the rest. Notably
    /// `installed_preamp_lin` is `None`, never `1.0` -- a silently-unity
    /// preamp is precisely the failure verification exists to catch.
    fn disengaged() -> Facts {
        Facts {
            bands_dropped: 0,
            bypass: false,
            clipped_samples: 0,
            correction_installed: false,
            correction_rate_mismatch_hz: None,
            engine_engaged: false,
            gain_db: 0.0,
            installed_preamp_lin: None,
            latency_ms: None,
            sections_substituted: 0,
            status_kind: EngineStatusKind::Stopped,
            stream_rate_hz: None,
        }
    }

    fn from_state(state: &EngineState) -> Facts {
        Facts {
            bands_dropped: state.bands_dropped,
            bypass: state.bypass,
            clipped_samples: state.clipped_samples,
            correction_installed: state.correction.is_some(),
            correction_rate_mismatch_hz: state.correction_rate_mismatch,
            engine_engaged: engine_engaged(state),
            gain_db: state.gain_db,
            installed_preamp_lin: installed_preamp_lin(state),
            latency_ms: state.latency_ms,
            sections_substituted: state.sections_substituted,
            status_kind: status_kind(&state.status),
            stream_rate_hz: state.stream.as_ref().map(|s| s.sample_rate),
        }
    }
}

/// Is the engine ENGAGED -- a live stream with the chain up -- irrespective of
/// whether audio happens to be flowing right now?
///
/// **NOT `status == Running`**, and that distinction is the whole reason this
/// function exists. The shipped watchdog sets `Running` only when
/// `nonzero_blocks` advances ("Nonzero input is the ground truth for 'audio is
/// flowing': gate `Running` on it, never on `start()` having returned"), and
/// it decays to `InputSilent` after `silence_window_ms` and then to `Idle`
/// after `idle_window_ms`. Verification REQUIRES a quiet machine -- its
/// pre-roll asserts that `nonzero_blocks` is stationary, because the tap is
/// global and excludes only ParaEQ, so any other app's audio would land in the
/// corrected capture. So a gate written against the literal `Running` would
/// refuse a perfectly healthy engine on every single run.
///
/// `Starting`, `NoInputDetected`, `Running`, `InputSilent` and `Idle` are all
/// ENGAGED: each means the backend is up and the chain is installed. The three
/// excluded states are the three in which no audio can be processed at all.
fn engine_engaged(state: &EngineState) -> bool {
    state.enabled
        && state.stream.is_some()
        && !matches!(
            state.status,
            EngineStatus::AutoDisabledNoInput { .. }
                | EngineStatus::Failed { .. }
                | EngineStatus::Stopped
        )
}

/// R1-1's auto-preamp AS ARMED, linear.
///
/// The shipped field is `auto_preamp_db: Option<f32>` -- dB, and `Option` --
/// so the derivation is stated once, here, and the `None` case is propagated
/// rather than defaulted: `None` means NO CORRECTION IS INSTALLED, which is a
/// different prediction from a unity preamp, and collapsing the two makes the
/// residual blame the chain for our own default.
fn installed_preamp_lin(state: &EngineState) -> Option<f32> {
    state
        .auto_preamp_db
        .map(|db| preamp::preamp_lin(f64::from(db)))
}

/// The ONE place the engine's lifecycle vocabulary meets the measurement
/// crate's mirror of it. Exhaustive on purpose: a ninth `EngineStatus` variant
/// cannot be added without a decision here.
///
/// Payloads (`since_ms`, `after_ms`, `reason`) are dropped, because no gate
/// reads them and carrying them would invite one to.
fn status_kind(status: &EngineStatus) -> EngineStatusKind {
    match status {
        EngineStatus::AutoDisabledNoInput { .. } => EngineStatusKind::AutoDisabledNoInput,
        EngineStatus::Failed { .. } => EngineStatusKind::Failed,
        EngineStatus::Idle { .. } => EngineStatusKind::Idle,
        EngineStatus::InputSilent { .. } => EngineStatusKind::InputSilent,
        EngineStatus::NoInputDetected { .. } => EngineStatusKind::NoInputDetected,
        EngineStatus::Running => EngineStatusKind::Running,
        EngineStatus::Starting { .. } => EngineStatusKind::Starting,
        EngineStatus::Stopped => EngineStatusKind::Stopped,
    }
}

/// Set the user's trim and wait for the engine to echo it back.
///
/// Commands are fire-and-forget, so "it was set" is only knowable from a
/// published snapshot. The compare is exact because nothing computes on the
/// way: the controller stores the `f32` verbatim and `effectively_equal`
/// compares `gain_db` exactly, so any change publishes and the value that
/// comes back is bit-for-bit the one that went out.
fn set_gain_db(source: &EngineSource, db: f32) -> Result<(), MeasureError> {
    source
        .with(|handle| handle.send(EngineCommand::SetGainDb(db)))
        .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_string()))?;
    let deadline = Instant::now() + GAIN_READBACK_TIMEOUT;
    loop {
        let current = source
            .with(|handle| handle.state().gain_db)
            .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_string()))?;
        if current == db {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(MeasureError::Sink(format!(
                "the engine did not apply a {db} dB trim within \
                 {} ms (it still reports {current} dB)",
                GAIN_READBACK_TIMEOUT.as_millis()
            )));
        }
        std::thread::sleep(GAIN_READBACK_POLL);
    }
}

/// The verification pass's view of the live engine.
///
/// Reads one `EngineState` snapshot per call (a lock-free `ArcSwap` load), so
/// two facts read back to back can in principle come from two snapshots. That
/// is deliberate and harmless here: every gate reads a fact, decides, and
/// refuses on the spot, and a fact that changed between two reads describes an
/// engine that changed under the measurement -- which is itself a refusal.
pub struct EngineSeam {
    engine: EngineSource,
}

impl EngineSeam {
    /// Production: reach the handle through the app's managed [`AppShared`].
    pub fn managed(app: tauri::AppHandle) -> EngineSeam {
        EngineSeam {
            engine: EngineSource::Managed(app),
        }
    }

    /// For a caller that owns the handle slot directly (no Tauri app).
    pub fn direct(engine: Arc<Mutex<Option<EngineHandle>>>) -> EngineSeam {
        EngineSeam {
            engine: EngineSource::Direct(engine),
        }
    }

    fn facts(&self) -> Facts {
        self.engine
            .with(|handle| Facts::from_state(&handle.state()))
            .unwrap_or_else(Facts::disengaged)
    }
}

impl EngineFacts for EngineSeam {
    fn bands_dropped(&self) -> usize {
        self.facts().bands_dropped
    }

    fn bypass(&self) -> bool {
        self.facts().bypass
    }

    fn clipped_samples(&self) -> u64 {
        self.facts().clipped_samples
    }

    fn correction_installed(&self) -> bool {
        self.facts().correction_installed
    }

    fn correction_rate_mismatch_hz(&self) -> Option<f64> {
        self.facts().correction_rate_mismatch_hz
    }

    fn engine_engaged(&self) -> bool {
        self.facts().engine_engaged
    }

    fn gain_db(&self) -> f32 {
        self.facts().gain_db
    }

    fn installed_preamp_lin(&self) -> Option<f32> {
        self.facts().installed_preamp_lin
    }

    fn latency_ms(&self) -> Option<f64> {
        self.facts().latency_ms
    }

    fn sections_substituted(&self) -> usize {
        self.facts().sections_substituted
    }

    fn status_kind(&self) -> EngineStatusKind {
        self.facts().status_kind
    }

    fn stream_rate_hz(&self) -> Option<f64> {
        self.facts().stream_rate_hz
    }

    /// The realtime activity witness. Reads `RtShared` directly through the
    /// handle's non-publishing accessor, so polling it through a witness
    /// window emits no snapshot and no `app-state` event.
    ///
    /// `None` here means the same thing it means there: no session, so nothing
    /// can be witnessed. It is NOT "nothing is flowing".
    fn tap_activity(&self) -> Option<paraeq_measure::TapActivity> {
        self.engine.with(EngineHandle::tap_activity).flatten().map(
            |TapActivity {
                 callbacks,
                 nonzero_blocks,
             }| paraeq_measure::TapActivity {
                callbacks,
                nonzero_blocks,
            },
        )
    }
}

impl EngineControl for EngineSeam {
    /// Suspend the fail-open auto-disable for the run.
    ///
    /// Two distinct failures, with two distinct messages, because they have
    /// two distinct remedies: no handle at all ("the engine is not running" --
    /// turn ParaEQ on) versus a lease already outstanding ("one measurement at
    /// a time" -- wait for the other one). The shipped lease deliberately
    /// knows nothing about engine status, so acquiring against a stopped
    /// engine succeeds; the status gate is a separate one, reading
    /// [`EngineFacts::engine_engaged`].
    fn acquire_measurement_lease(
        &mut self,
    ) -> Result<Box<dyn MeasurementLeaseToken>, MeasureError> {
        let lease = self
            .engine
            .with(EngineHandle::acquire_measurement_lease)
            .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_string()))?
            .ok_or_else(|| {
                MeasureError::Sink(
                    "a measurement lease is already outstanding -- one measurement at a time"
                        .to_string(),
                )
            })?;
        Ok(Box::new(LeaseToken { lease }))
    }

    /// Pin the USER's trim and hand back an RAII restore.
    ///
    /// This is the user's trim (`gain_db`), not the correction's preamp.
    /// Zeroing it is safe precisely because R1-1 puts the computed preamp
    /// INSIDE the installed `Correction` (applied on the corrected path only)
    /// rather than on the gain stage -- under the rejected `SetGainDb` carrier
    /// this pin would have erased the very thing under test.
    ///
    /// The restore closure captures a clone of the engine source, not a borrow
    /// of `self`, so it can run from `Drop` during unwinding long after this
    /// call returned.
    fn pin_gain_db(&mut self, db: f32) -> Result<GainPin, MeasureError> {
        let previous_db = self
            .engine
            .with(|handle| handle.state().gain_db)
            .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_string()))?;
        set_gain_db(&self.engine, db)?;
        let engine = self.engine.clone();
        Ok(GainPin::new(
            previous_db,
            db,
            Box::new(move |restore_to| set_gain_db(&engine, restore_to)),
        ))
    }
}

/// The opaque lease token the measurement crate holds.
///
/// It owns the engine's RAII `MeasurementLease`, so dropping this drops that,
/// which re-arms the fail-open watchdog and re-baselines its window -- on
/// every exit path, including unwinding.
struct LeaseToken {
    lease: MeasurementLease,
}

impl MeasurementLeaseToken for LeaseToken {
    /// `true` for as long as this token exists, and that is the honest answer
    /// rather than a placeholder: the shipped engine has NO path that breaks a
    /// lease out from under its holder -- there is deliberately no
    /// `MEASUREMENT_LEASE_MAX_MS` in v1, because re-arming a safety net under
    /// a running measurement is an owner call. The cell is set by
    /// `acquire_measurement_lease` and cleared only by
    /// `MeasurementLease::drop`, which cannot run while this token is alive.
    ///
    /// If a break-the-lease timeout ever lands, THIS is the method that has to
    /// start reading the engine's cell instead of reasoning about ownership.
    fn is_held(&self) -> bool {
        // Borrow the lease so the field is unambiguously load-bearing: this
        // token's answer is exactly "I still own it".
        let _ = &self.lease;
        true
    }
}

// ─────────────────────────── the helper child process ──────────────────────

/// The helper executable's file name, in the bundle and in the target dir
/// alike. ONE spelling, because the bundler copies it under this name and the
/// resolver looks for it under this name.
const HELPER_BIN: &str = "paraeq-stimulus";

/// The directory an app bundle puts its executables in, relative to the
/// bundle root. The resolver only uses it to LABEL which branch it took --
/// both branches look beside the running executable -- but the label is what
/// makes a packaging failure legible instead of "file not found".
const BUNDLE_EXEC_DIR: &str = "Contents/MacOS";

/// Which layout the helper was found in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperSource {
    /// Beside the app executable inside `ParaEQ.app/Contents/MacOS/`.
    Bundle,
    /// Beside the dev binary in cargo's target directory.
    TargetDir,
}

/// Where the helper is, and which layout it was found in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HelperLocation {
    pub path: std::path::PathBuf,
    pub source: HelperSource,
}

/// Resolve the helper's path from the directory holding the running executable.
///
/// **Both branches answer "the sibling", and that is not a coincidence to be
/// tidied away.** In a release bundle the bundler copies the helper next to the
/// app binary in `Contents/MacOS/`; in dev, cargo builds both workspace
/// binaries into the same target directory. The two layouts agree on the
/// answer and disagree on the reason, so the reason is carried in
/// [`HelperSource`] and a "helper missing" report can say which one it expected.
///
/// Pure: it touches no filesystem, which is what lets
/// `the_helper_path_resolves_in_dev_and_in_a_bundle` pin both branches against
/// fixture directory layouts.
pub fn resolve_helper(exe_dir: &std::path::Path) -> HelperLocation {
    let bundled = exe_dir.ends_with(BUNDLE_EXEC_DIR);
    HelperLocation {
        path: exe_dir.join(HELPER_BIN),
        source: if bundled {
            HelperSource::Bundle
        } else {
            HelperSource::TargetDir
        },
    }
}

/// Is this a file we could actually exec?
///
/// Checked before spawning so a packaging mistake reports itself as "the helper
/// is not where it should be" rather than as a generic spawn failure at the one
/// moment the user is waiting to hear a sweep.
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The production [`StimulusHelper`]: resolve, spawn, wire the pipes.
///
/// Plain [`std::process::Command`], exactly as `setup.rs` already spawns
/// `afplay`, and deliberately NOT `tauri-plugin-shell`'s sidecar API: the
/// plugin exists to let the FRONTEND launch processes, which is a permission
/// surface this app has no reason to open. The bundler's only job is to copy
/// the binary into `Contents/MacOS/`.
pub struct HelperSpawner {
    /// Overridable so a test can point at a fixture without a bundle. `None`
    /// resolves from the running executable.
    exe_dir: Option<std::path::PathBuf>,
}

impl Default for HelperSpawner {
    fn default() -> Self {
        HelperSpawner::new()
    }
}

impl HelperSpawner {
    pub fn new() -> HelperSpawner {
        HelperSpawner { exe_dir: None }
    }

    /// Point the resolver at `dir` instead of the running executable's own
    /// directory. For tests and for a future headless driver that is not
    /// installed beside the helper.
    pub fn with_exe_dir(dir: impl Into<std::path::PathBuf>) -> HelperSpawner {
        HelperSpawner {
            exe_dir: Some(dir.into()),
        }
    }

    fn locate(&self) -> Result<HelperLocation, MeasureError> {
        let exe_dir = match &self.exe_dir {
            Some(dir) => dir.clone(),
            None => std::env::current_exe()
                .map_err(|e| MeasureError::Sink(format!("cannot locate this executable: {e}")))?
                .parent()
                .ok_or_else(|| MeasureError::Sink("this executable has no directory".to_owned()))?
                .to_path_buf(),
        };
        let found = resolve_helper(&exe_dir);
        if !is_executable(&found.path) {
            return Err(MeasureError::Sink(format!(
                "the verification helper is missing or not executable at {} (expected the {:?} \
                 layout)",
                found.path.display(),
                found.source
            )));
        }
        Ok(found)
    }
}

impl StimulusHelper for HelperSpawner {
    fn spawn(
        &mut self,
        wav_path: &std::path::Path,
        device_uid: &str,
        routing: HelperRouting,
    ) -> Result<Box<dyn HelperProcess>, MeasureError> {
        let found = self.locate()?;
        log::info!(
            "spawning the verification helper from {} ({:?} layout)",
            found.path.display(),
            found.source
        );
        let child = std::process::Command::new(&found.path)
            .arg("--wav")
            .arg(wav_path)
            .arg("--device-uid")
            .arg(device_uid)
            .arg("--channel")
            // `as_channel_arg`, never a local `format!`: the parent's enum and
            // the child's CLI must not be able to disagree about what `Only(n)`
            // means, and one mapping in the seam is how that is guaranteed.
            .arg(routing.as_channel_arg())
            // One JSON object per line on stdout. The parent reads one stream.
            .arg("--json")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr is human-only and is inherited, so a refusal lands in the
            // app's own log rather than in a pipe nothing drains.
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| {
                MeasureError::Sink(format!("cannot spawn {}: {e}", found.path.display()))
            })?;

        Ok(Box::new(Helper::adopt(child)?))
    }
}

/// One live helper child.
///
/// **Every method is safe to call on an already-dead child**, which the seam
/// requires and the teardown ladder depends on: a rung that failed loudly on a
/// corpse would abort the ladder before the device-gone check runs.
struct Helper {
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
    /// Cached exit status. Its presence is ALSO the interlock that stops any
    /// signal reaching a reaped pid -- see [`Helper::signal`].
    reaped: Option<HelperExit>,
    /// `None` once the pipe has been closed or lost.
    stdin: Option<std::process::ChildStdin>,
}

impl Helper {
    /// Take ownership of a spawned child: claim its pipes and start the reader.
    ///
    /// Factored out of `spawn` so the tests below can drive the whole
    /// `HelperProcess` contract -- the signal ladder, the idempotent reap, the
    /// deadline on a read -- against a REAL process without going anywhere near
    /// the real helper, a device, or a sample of audio.
    fn adopt(mut child: std::process::Child) -> Result<Helper, MeasureError> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| MeasureError::Sink("the helper's stdin was not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MeasureError::Sink("the helper's stdout was not piped".to_owned()))?;

        // A dedicated blocking reader thread, mirroring the child's own stdin
        // reader. `read_line` then becomes `recv_timeout`, which is the only
        // way to put a DEADLINE on a pipe read in std: `Read` has no timeout,
        // and a wedged child must not be able to block the teardown ladder.
        // It also keeps the pipe drained -- a child blocked writing into a full
        // stdout buffer can never reach its own teardown.
        let (lines_tx, lines_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("paraeq-helper-stdout".into())
            .spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if lines_tx.send(line).is_err() {
                        return;
                    }
                }
                // Dropping `lines_tx` here is the EOF signal.
            })
            .map_err(|e| MeasureError::Sink(format!("cannot read the helper's stdout: {e}")))?;

        Ok(Helper {
            child,
            lines: lines_rx,
            reaped: None,
            stdin: Some(stdin),
        })
    }

    /// Write one protocol line, tolerating a child that has already gone.
    ///
    /// A closed pipe is not a fault to report: the child this line was for is
    /// no longer listening, which is the outcome the line was asking for. Any
    /// OTHER write failure is reported, because it means the pipe is in a state
    /// this process does not understand.
    fn write_line(&mut self, line: &str) -> Result<(), MeasureError> {
        use std::io::Write;
        let Some(stdin) = self.stdin.as_mut() else {
            return Ok(());
        };
        match stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
        {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                self.stdin = None;
                Ok(())
            }
            Err(e) => Err(MeasureError::Sink(format!(
                "cannot write '{}' to the helper: {e}",
                line.trim_end()
            ))),
        }
    }

    /// Send `signal` to the child, or do nothing if it has been reaped.
    ///
    /// **The reaped check is a safety interlock, not an optimization.** Once a
    /// pid has been waited on the kernel may reuse it, and a SIGTERM aimed at a
    /// recycled pid is a signal delivered to an unrelated process on the user's
    /// machine. `self.reaped` is set only by `reap`/`try_reap`, i.e. only after
    /// a successful `wait`, so it is exactly the condition under which the pid
    /// stops being ours.
    fn signal(&mut self, signal: i32) -> Result<(), MeasureError> {
        if self.reaped.is_some() {
            return Ok(());
        }
        let pid = self.child.id() as i32;
        // SAFETY: two preconditions, both established above. (1) `pid` names a
        // child this process spawned and has NOT yet reaped -- `self.child`
        // owns it and `self.reaped` is None, and only a successful `wait` sets
        // that field. (2) `signal` is a valid signal constant: every caller
        // passes a `libc::SIG*`. `kill` has no other requirements and touches
        // no memory.
        let status = unsafe { libc::kill(pid, signal) };
        if status == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // ESRCH: no such process. The child exited between the check and the
        // call, which is the normal race on this path and not a failure --
        // the signal was asking it to stop, and it has.
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(MeasureError::Sink(format!(
            "cannot signal the helper ({signal}): {error}"
        )))
    }
}

/// `std`'s exit status in the seam's vocabulary.
///
/// `signalled` is read off the signal number rather than inferred from a
/// missing code, so "killed by SIGKILL" and "exited without a code we can read"
/// cannot be confused.
fn helper_exit(status: std::process::ExitStatus) -> HelperExit {
    use std::os::unix::process::ExitStatusExt;
    HelperExit {
        code: status.code(),
        signalled: status.signal().is_some(),
    }
}

impl HelperProcess for Helper {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn request_abort(&mut self) -> Result<(), MeasureError> {
        self.write_line("abort\n")
    }

    fn request_terminate(&mut self) -> Result<(), MeasureError> {
        // SIGTERM, never SIGKILL. The child installs a handler that arms the
        // same abort flag `abort\n` does, so this is a SECOND ramp request: a
        // child that missed the stdin write still fades rather than stopping
        // dead, and "a hard stop is itself a full-scale click".
        self.signal(libc::SIGTERM)
    }

    fn kill(&mut self) -> Result<(), MeasureError> {
        // SIGKILL, and only after both deadlines. It bypasses `Drop`, so the
        // child's render aggregate can outlive it -- which is why the ladder
        // asks the HAL whether the device is gone immediately after this.
        self.signal(libc::SIGKILL)
    }

    fn reap(&mut self) -> Result<HelperExit, MeasureError> {
        if let Some(cached) = self.reaped {
            return Ok(cached);
        }
        // Drop stdin before blocking: the child treats EOF as an abort, so a
        // still-open pipe is a reason for it not to exit and this a reason to
        // block forever.
        self.stdin = None;
        let status = self
            .child
            .wait()
            .map_err(|e| MeasureError::Sink(format!("cannot reap the helper: {e}")))?;
        let exit = helper_exit(status);
        self.reaped = Some(exit);
        Ok(exit)
    }

    fn try_reap(&mut self) -> Result<Option<HelperExit>, MeasureError> {
        if let Some(cached) = self.reaped {
            return Ok(Some(cached));
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let exit = helper_exit(status);
                self.reaped = Some(exit);
                Ok(Some(exit))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(MeasureError::Sink(format!(
                "cannot read the helper's status: {e}"
            ))),
        }
    }

    fn send_play(&mut self) -> Result<(), MeasureError> {
        self.write_line("play\n")
    }

    fn read_line(&mut self, deadline: Duration) -> Result<HelperLine, MeasureError> {
        match self.lines.recv_timeout(deadline) {
            Ok(line) => Ok(HelperLine::Line(line)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(HelperLine::DeadlineExpired),
            // The reader thread ended, which it only does on EOF or a read
            // error on the pipe. Either way nothing more will arrive.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(HelperLine::Eof),
        }
    }
}

// ───────────────────────────── HAL device facts ────────────────────────────

/// The production [`DeviceFacts`]: two by-UID HAL reads.
///
/// Stateless, because both questions are asked of the HAL at the moment they
/// are asked. Caching either one would be a way to answer "is the leaked device
/// still there?" with a value read before it leaked.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeviceSeam;

impl DeviceFacts for DeviceSeam {
    /// **An unreadable answer is `true`.** The one caller is the verification
    /// teardown asking whether the helper's private render aggregate is gone,
    /// and the two ways to be wrong are not symmetric: a false report costs a
    /// log line, a false clean leaves a private device wrapping the user's
    /// output with nothing watching it.
    fn device_exists(&self, uid: &str) -> bool {
        match paraeq_coreaudio::properties::device_exists(uid) {
            Ok(present) => present,
            Err(e) => {
                log::warn!("cannot tell whether '{uid}' still exists ({e}); reporting it present");
                true
            }
        }
    }

    /// `None` covers both "no device carries this UID" and "the HAL refused",
    /// which is what the seam asks for: the caller treats an unanswerable read
    /// as an expected race, not as a rate failure.
    fn nominal_sample_rate(&self, device_uid: &str) -> Option<f64> {
        match paraeq_coreaudio::properties::nominal_sample_rate_for_uid(device_uid) {
            Ok(rate) => rate,
            Err(e) => {
                log::warn!("cannot read '{device_uid}''s nominal rate: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paraeq_dsp::peq::{EQBand, FilterType};
    use paraeq_engine::backend::{AudioBackend, BackendEvent, StreamInfo};
    use paraeq_engine::controller::{CorrectionConfig, EngineConfig};
    use paraeq_engine::shared::RtProcessor;
    use paraeq_engine::status::WatchdogConfig;
    use paraeq_engine::EngineError;
    use std::path::Path;

    const FAIL_OPEN_MS: u64 = 300;
    const RATE: f64 = 48_000.0;
    const TICK_MS: u64 = 10;
    const WAIT: Duration = Duration::from_secs(5);

    /// A stopped-clock `AudioBackend`: it accepts the `RtProcessor`, reports a
    /// fixed 48 kHz stereo stream, and never drives a realtime callback.
    ///
    /// Enough to bring the SHIPPED controller up in-process, so the impls
    /// below are exercised against the real `EngineHandle` rather than a mock
    /// of it. Realtime behaviour is `paraeq-engine`'s own suite to cover; what
    /// is under test here is the mapping and the RAII.
    struct NullBackend {
        processor: Option<RtProcessor>,
    }

    impl AudioBackend for NullBackend {
        fn start(
            &mut self,
            processor: RtProcessor,
            _requested_buffer_frames: Option<usize>,
        ) -> Result<StreamInfo, EngineError> {
            self.processor = Some(processor);
            Ok(StreamInfo {
                buffer_frames: 512,
                channels: 2,
                device_uid: "null-device".to_string(),
                sample_rate: RATE,
            })
        }

        fn stop(&mut self) -> Result<(), EngineError> {
            self.processor = None;
            Ok(())
        }

        fn poll_event(&mut self) -> Option<BackendEvent> {
            None
        }

        fn self_excluded(&self) -> bool {
            self.processor.is_some()
        }
    }

    fn test_config(fail_open_after_ms: Option<u64>) -> EngineConfig {
        EngineConfig {
            enabled: true,
            fail_open_after_ms,
            requested_buffer_frames: None,
            ring_capacity: 4,
            tick_ms: TICK_MS,
            watchdog: WatchdogConfig {
                engage_tolerance_ms: 100,
                idle_window_ms: 150,
                silence_window_ms: 150,
            },
        }
    }

    /// A seam over a real, running controller. The handle slot is returned so
    /// the test can assert against the engine directly as well as through the
    /// seam.
    fn live_seam(
        fail_open_after_ms: Option<u64>,
    ) -> (Arc<Mutex<Option<EngineHandle>>>, EngineSeam) {
        let handle = EngineHandle::spawn(
            NullBackend { processor: None },
            test_config(fail_open_after_ms),
        );
        let slot = Arc::new(Mutex::new(Some(handle)));
        let seam = EngineSeam::direct(Arc::clone(&slot));
        (slot, seam)
    }

    fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if pred() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn peaking(fc: f64, gain_db: f64) -> EQBand {
        EQBand {
            filter_type: FilterType::Peaking,
            fc,
            gain_db,
            q: 1.0,
        }
    }

    /// Every field distinct and non-default, so a mapping that reads the wrong
    /// one cannot accidentally agree.
    fn sample_state() -> EngineState {
        EngineState {
            auto_preamp_db: Some(-9.5),
            bands_dropped: 3,
            bypass: true,
            clipped_samples: 7,
            correction: Some("peq:5-band".to_string()),
            correction_rate_mismatch: Some(44_100.0),
            enabled: true,
            frame_mismatch_blocks: 11,
            gain_db: -3.5,
            input_peak: 0.25,
            input_peak_session: 0.75,
            invalid_samples: 13,
            latency_ms: Some(62.3),
            output_peak: 1.5,
            sections_substituted: 5,
            self_excluded: true,
            status: EngineStatus::Idle { since_ms: 900 },
            stream: Some(StreamInfo {
                buffer_frames: 512,
                channels: 2,
                device_uid: "uid-1".to_string(),
                sample_rate: RATE,
            }),
        }
    }

    /// Every shipped `EngineStatus`, one of each, for the exhaustive walks.
    fn all_statuses() -> [EngineStatus; 8] {
        [
            EngineStatus::AutoDisabledNoInput { after_ms: 15_000 },
            EngineStatus::Failed {
                reason: "boom".to_string(),
            },
            EngineStatus::Idle { since_ms: 1 },
            EngineStatus::InputSilent { since_ms: 2 },
            EngineStatus::NoInputDetected { since_ms: 3 },
            EngineStatus::Running,
            EngineStatus::Starting { since_ms: 4 },
            EngineStatus::Stopped,
        ]
    }

    /// EXHAUSTIVENESS. The destructuring below has no `..`, so adding a field
    /// to the shipped `EngineState` fails THIS test to compile until somebody
    /// decides whether verification reads it -- which is the decision that was
    /// being skipped when facts were pulled out of the snapshot ad hoc.
    ///
    /// Each consumed field is asserted through the mapping. Each UNCONSUMED
    /// field is named with the reason it is not read, below.
    #[test]
    fn engine_facts_maps_every_field_of_the_shipped_engine_state() {
        let state = sample_state();
        let facts = Facts::from_state(&state);
        let EngineState {
            auto_preamp_db,
            bands_dropped,
            bypass,
            clipped_samples,
            correction,
            correction_rate_mismatch,
            enabled,
            frame_mismatch_blocks,
            gain_db,
            input_peak,
            input_peak_session,
            invalid_samples,
            latency_ms,
            output_peak,
            sections_substituted,
            self_excluded,
            status,
            stream,
        } = state.clone();

        // ---- CONSUMED ----
        assert_eq!(
            facts.installed_preamp_lin,
            auto_preamp_db.map(|db| preamp::preamp_lin(f64::from(db))),
            "the armed preamp is DERIVED from auto_preamp_db, dB -> linear"
        );
        assert_eq!(facts.bands_dropped, bands_dropped);
        assert_eq!(facts.bypass, bypass);
        assert_eq!(facts.clipped_samples, clipped_samples);
        assert_eq!(facts.correction_installed, correction.is_some());
        assert_eq!(facts.correction_rate_mismatch_hz, correction_rate_mismatch);
        assert_eq!(facts.gain_db, gain_db);
        assert_eq!(facts.latency_ms, latency_ms);
        assert_eq!(facts.sections_substituted, sections_substituted);
        assert_eq!(facts.status_kind, status_kind(&status));
        assert_eq!(
            facts.stream_rate_hz,
            stream.as_ref().map(|s| s.sample_rate),
            "the LIVE rate -- never the plan's design rate"
        );
        // `enabled`, `status` and `stream` are consumed a second time, jointly,
        // as the engagement predicate.
        assert_eq!(facts.engine_engaged, enabled && stream.is_some());

        // ---- NOT CONSUMED, with the reason for each ----
        //
        // `frame_mismatch_blocks`: a realtime block counter, published as a
        // `> 0` boolean rather than exactly (a degraded stretch increments it
        // every block), so it carries no value a gate could act on. The
        // verification analogue is `clipped_samples`, which IS read, as a
        // delta across the window.
        let _ = frame_mismatch_blocks;
        // `input_peak`: a DECAYING meter (20 dB / 1.7 s release), so two reads
        // a tick apart differ for reasons that have nothing to do with the
        // chain. The activity witness uses `nonzero_blocks` instead, which is
        // monotonic.
        let _ = input_peak;
        // `input_peak_session`: session-scoped and never reset inside a run,
        // so it says nothing about the verification window specifically.
        let _ = input_peak_session;
        // `invalid_samples`: counts non-finite INPUT samples the chain
        // sanitized. A non-finite reaching a sink is already refused upstream
        // by the stimulus guards, and a nonzero value here is a bug report
        // about the tap, not about the correction.
        let _ = invalid_samples;
        // `output_peak`: the other decaying meter, and the acoustic quantity
        // verification cares about is measured at the MIC, not at the chain's
        // output; reading this would be a weaker proxy that can disagree with
        // the real check.
        let _ = output_peak;
        // `self_excluded`: telemetry, not the gate. Its own doc says so. The
        // MS-6 gate polls the backend's live witness through `TapStatus`, so a
        // safety check is never up to a tick stale, and a second copy of the
        // fact on this trait could disagree with it.
        let _ = self_excluded;
    }

    /// The one place the two vocabularies meet, walked exhaustively: a ninth
    /// shipped variant cannot appear without a decision here.
    #[test]
    fn status_kind_maps_every_engine_status_variant() {
        let mapped: Vec<EngineStatusKind> = all_statuses().iter().map(status_kind).collect();
        assert_eq!(
            mapped,
            vec![
                EngineStatusKind::AutoDisabledNoInput,
                EngineStatusKind::Failed,
                EngineStatusKind::Idle,
                EngineStatusKind::InputSilent,
                EngineStatusKind::NoInputDetected,
                EngineStatusKind::Running,
                EngineStatusKind::Starting,
                EngineStatusKind::Stopped,
            ]
        );
        for (i, kind) in mapped.iter().enumerate() {
            assert!(
                !mapped[..i].contains(kind),
                "two statuses collapsed onto {kind:?}; a refusal could not name what it saw"
            );
        }
    }

    /// The falsifier for the engagement definition. Walked over all eight
    /// statuses times `enabled` times `stream`, which is stricter than driving
    /// a live engine through the states it happens to be able to reach -- and
    /// deterministic.
    #[test]
    fn engine_engaged_is_false_exactly_for_stopped_failed_and_auto_disabled() {
        for status in all_statuses() {
            let disengaging = matches!(
                status,
                EngineStatus::AutoDisabledNoInput { .. }
                    | EngineStatus::Failed { .. }
                    | EngineStatus::Stopped
            );
            for enabled in [false, true] {
                for has_stream in [false, true] {
                    let state = EngineState {
                        enabled,
                        status: status.clone(),
                        stream: has_stream.then(|| StreamInfo {
                            buffer_frames: 512,
                            channels: 2,
                            device_uid: "uid-1".to_string(),
                            sample_rate: RATE,
                        }),
                        ..sample_state()
                    };
                    let expected = enabled && has_stream && !disengaging;
                    assert_eq!(
                        engine_engaged(&state),
                        expected,
                        "enabled={enabled} stream={has_stream} status={status:?}"
                    );
                }
            }
        }

        // And the sentence the definition exists to refute: `Running` is not
        // required, because a verification pre-roll demands the very silence
        // that decays it away.
        for status in [
            EngineStatus::Idle { since_ms: 1 },
            EngineStatus::InputSilent { since_ms: 2 },
            EngineStatus::NoInputDetected { since_ms: 3 },
            EngineStatus::Starting { since_ms: 4 },
        ] {
            assert!(engine_engaged(&EngineState {
                status,
                ..sample_state()
            }));
        }
    }

    /// `None` means "no correction is installed", and it must stay `None` all
    /// the way to the caller. Defaulting it to 1.0 would predict a response
    /// the chain is not producing and blame the chain for the difference.
    #[test]
    fn installed_preamp_lin_is_none_when_no_correction_is_installed() {
        let state = EngineState {
            auto_preamp_db: None,
            correction: None,
            ..sample_state()
        };
        assert_eq!(installed_preamp_lin(&state), None);
        assert_eq!(Facts::from_state(&state).installed_preamp_lin, None);
        assert_eq!(Facts::disengaged().installed_preamp_lin, None);
    }

    /// The derivation, pinned, so nobody re-derives the number from the plan:
    /// it is `preamp_lin(auto_preamp_db)`, the engine's own function over the
    /// engine's own field.
    #[test]
    fn installed_preamp_lin_is_preamp_lin_of_auto_preamp_db() {
        for db in [-24.0f32, -9.5, -0.1, 0.0] {
            let state = EngineState {
                auto_preamp_db: Some(db),
                ..sample_state()
            };
            assert_eq!(
                installed_preamp_lin(&state),
                Some(preamp::preamp_lin(f64::from(db)))
            );
        }
        // Worked example, so a wrong sign or a /10 is visible rather than
        // merely self-consistent: -20 dB is exactly 0.1 linear.
        let state = EngineState {
            auto_preamp_db: Some(-20.0),
            ..sample_state()
        };
        let lin = installed_preamp_lin(&state).expect("a correction is installed");
        assert!(
            (lin - 0.1).abs() < 1e-6,
            "-20 dB must be 0.1 linear, got {lin}"
        );
    }

    /// The seam reads the two `BuildReport` counts off the live engine, which
    /// is the whole reason they were put on the wire in Stage 6.
    #[test]
    fn engine_facts_maps_bands_dropped_and_sections_substituted_from_the_build_report() {
        let (slot, seam) = live_seam(None);
        assert!(wait_until(WAIT, || seam.engine_engaged()));
        assert_eq!(seam.bands_dropped(), 0);
        assert_eq!(seam.sections_substituted(), 0);

        // 30 kHz is above the null backend's 24 kHz Nyquist; 1 kHz survives,
        // so the correction still installs and exactly one band is dropped.
        slot.lock()
            .unwrap()
            .as_ref()
            .expect("a live handle")
            .send(EngineCommand::SetCorrection(CorrectionConfig::Peq {
                bands: vec![vec![peaking(1_000.0, -3.0), peaking(30_000.0, -3.0)]],
                design_rate: RATE,
            }));

        assert!(wait_until(WAIT, || seam.bands_dropped() == 1));
        assert!(seam.correction_installed());
        assert!(
            seam.installed_preamp_lin().is_some(),
            "an installed correction always carries an armed preamp"
        );
        assert_eq!(
            seam.stream_rate_hz(),
            Some(RATE),
            "the gate recomputes at the LIVE rate"
        );
    }

    /// A5's "Export/engine agreement" row, DESKTOP HALF: the preamp in the
    /// exported AutoEQ text equals `EngineState.auto_preamp_db` -- the number
    /// the live engine is actually applying, read off a running controller
    /// rather than off `build_correction`'s return value.
    ///
    /// The crate-side half of this test (`eq.rs`'s
    /// `exported_preamp_equals_engine_state_auto_preamp_db`) compares the
    /// export against `BuildReport::preamp_db`. That is one hop short of the
    /// claim: it proves the BUILDER agrees, not that the field the UI reads and
    /// the verification gate compares against carries the same number. This
    /// closes the hop.
    ///
    /// Compared through the exporter's own formatter rather than as raw
    /// numbers, for the reason that test gives: the engine publishes an f32 and
    /// the exporter rewrites any preamp in `(-0.05, 0]` as `0.0`, so the
    /// agreement that matters is the agreement of the RENDERED line.
    #[test]
    fn exported_preamp_equals_engine_state_auto_preamp_db() {
        let (slot, seam) = live_seam(None);
        assert!(wait_until(WAIT, || seam.engine_engaged()));

        // A boosting set, so the preamp is a real number and the assertion is
        // not vacuous.
        let bands = vec![peaking(45.0, 9.0), peaking(1_000.0, 12.0)];
        slot.lock()
            .unwrap()
            .as_ref()
            .expect("a live handle")
            .send(EngineCommand::SetCorrection(CorrectionConfig::Peq {
                bands: vec![bands.clone()],
                design_rate: RATE,
            }));
        assert!(wait_until(WAIT, || seam.installed_preamp_lin().is_some()));

        let armed_db = slot
            .lock()
            .unwrap()
            .as_ref()
            .expect("a live handle")
            .state()
            .auto_preamp_db
            .expect("an installed correction publishes its armed preamp");
        assert!(armed_db < 0.0, "boosts must pull the output down");

        let exported = paraeq_dsp::peq::ParametricEQ {
            bands,
            sample_rate: RATE,
        }
        .export_autoeq_format_with_preamp();
        let exported_preamp = exported.lines().next().expect("a Preamp line");
        assert_ne!(
            exported_preamp, "Preamp: 0.0 dB",
            "a boosting band set must export a real preamp, or this test is vacuous"
        );

        let engine_preamp = paraeq_dsp::peq::ParametricEQ {
            bands: Vec::new(),
            sample_rate: RATE,
        }
        .export_autoeq_format_with_preamp_db(f64::from(armed_db));
        assert_eq!(
            engine_preamp, exported_preamp,
            "the engine is applying a preamp the export does not name"
        );
    }

    /// The lease is acquired through the seam, suspends the fail-open
    /// auto-disable, and suspends NOTHING ELSE -- `NoInputDetected` keeps being
    /// produced and published while it is held, because a genuine silent
    /// failure during a measurement is still something the wizard must be able
    /// to see. Releasing the token re-arms the watchdog.
    #[test]
    fn the_lease_suspends_only_the_fail_open_watchdog() {
        let (slot, mut seam) = live_seam(Some(FAIL_OPEN_MS));
        let token = seam
            .acquire_measurement_lease()
            .expect("a fresh engine hands out the first lease");
        assert!(token.is_held());

        // The null backend never drives a callback, so the watchdog parks in
        // `NoInputDetected` -- the state fail-open fires from.
        assert!(wait_until(WAIT, || seam.status_kind()
            == EngineStatusKind::NoInputDetected));

        // Sit far past the window. Without the lease this auto-disables.
        let deadline = Instant::now() + Duration::from_millis(FAIL_OPEN_MS * 3);
        while Instant::now() < deadline {
            assert_ne!(
                seam.status_kind(),
                EngineStatusKind::AutoDisabledNoInput,
                "fail-open fired while a measurement lease was held"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        // Reporting is NOT suspended, and the engine is still engaged, so the
        // helper would have had a chain to play through.
        assert_eq!(seam.status_kind(), EngineStatusKind::NoInputDetected);
        assert!(seam.engine_engaged());

        drop(token);
        assert!(
            wait_until(WAIT, || seam.status_kind()
                == EngineStatusKind::AutoDisabledNoInput),
            "releasing the lease must re-arm fail-open"
        );
        assert!(!seam.engine_engaged());
        drop(slot);
    }

    /// One measurement at a time, with a refusal that names WHICH failure it
    /// is -- "already outstanding" has a different remedy from "the engine is
    /// not running".
    #[test]
    fn a_second_lease_is_refused_while_one_is_outstanding() {
        let (_slot, mut seam) = live_seam(None);
        let first = seam.acquire_measurement_lease().expect("the first lease");
        // `Box<dyn MeasurementLeaseToken>` is not `Debug`, so unwrap the
        // refusal by hand rather than through `expect_err`.
        let Err(refusal) = seam.acquire_measurement_lease() else {
            panic!("a second lease must be refused, not queued");
        };
        assert!(
            refusal.to_string().contains("already outstanding"),
            "the refusal must say which failure it is: {refusal}"
        );
        drop(first);
        drop(
            seam.acquire_measurement_lease()
                .expect("the cell is free again"),
        );
    }

    /// MS-5's discipline, applied to the trim: the pin restores on `Drop`, and
    /// `Drop` runs during unwinding, so a panicking verification run does not
    /// leave the user's EQ silently re-trimmed.
    #[test]
    fn a_gain_pin_restores_on_drop_and_on_unwind() {
        let (_slot, mut seam) = live_seam(None);
        assert!(wait_until(WAIT, || seam.engine_engaged()));

        // A non-zero starting trim, so "restored" cannot be confused with
        // "never changed".
        let pin = seam.pin_gain_db(-6.0).expect("the engine accepts a trim");
        assert!((pin.pinned_db() - -6.0).abs() < f32::EPSILON);
        assert!(wait_until(WAIT, || (seam.gain_db() - -6.0).abs() < f32::EPSILON));
        drop(pin);
        assert!(
            wait_until(WAIT, || (seam.gain_db() - 0.0).abs() < f32::EPSILON),
            "dropping the pin must restore the previous trim"
        );

        // Now the panic path. The pin is taken inside the unwinding scope, so
        // only `Drop` can put the trim back.
        let seam = Arc::new(Mutex::new(seam));
        let inner = Arc::clone(&seam);
        let unwind = std::panic::catch_unwind(move || {
            let mut guard = inner.lock().expect("seam lock");
            guard.pin_gain_db(-12.0).expect("the engine accepts a trim");
            panic!("verification blew up with the trim pinned");
        });
        assert!(unwind.is_err(), "the injected panic must propagate");

        let seam = Arc::try_unwrap(seam)
            .unwrap_or_else(|_| unreachable!("the panicking closure dropped its clone"));
        let seam = seam
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            wait_until(WAIT, || (seam.gain_db() - 0.0).abs() < f32::EPSILON),
            "the trim was left pinned after an unwind"
        );
    }

    /// One production implementor of `TapStatus`, and this module is not a
    /// second one.
    ///
    /// The shipped implementor is `ExclusionWitness` -- one shared bool cell,
    /// which is all the MS-6 fact needs and strictly less than the realtime
    /// activity counters need. Splitting one safety fact across two seams that
    /// can disagree is the defect that put `tap_activity` on `EngineFacts`
    /// instead; this grep is what stops it coming back.
    ///
    /// The list below carries the ONE production implementor plus every MOCK.
    /// `tests/test_verify.rs` is the verification pass's, added with that pass:
    /// its gate 3 reads `TapStatus::self_excluded()` and the whole pass is
    /// mock-driven, so it needs one, and an integration test is its own crate
    /// and cannot borrow `test_session.rs`'s. A row under `tests/` is a mock,
    /// which is what this assertion permits; a row under any `src/` is a second
    /// production implementor, which is what it forbids.
    #[test]
    fn there_is_exactly_one_production_tap_status_impl() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("desktop/src-tauri sits two levels below the repo root")
            .to_path_buf();
        let mut hits: Vec<String> = Vec::new();
        for dir in ["crates", "desktop"] {
            collect_tap_status_impls(&root.join(dir), &root, &mut hits);
        }
        hits.sort();
        assert_eq!(
            hits,
            vec![
                "crates/paraeq-coreaudio/src/backend.rs".to_string(),
                "crates/paraeq-measure/tests/test_session.rs".to_string(),
                "crates/paraeq-measure/tests/test_verify.rs".to_string(),
            ],
            "exactly one production `TapStatus` impl (paraeq-coreaudio) plus the \
             mocks under tests/; anything else splits one safety fact across two \
             seams"
        );
    }

    /// Recursive `.rs` walk for the grep test above. Skips `target/` and
    /// `node_modules/`, which are build output rather than source.
    fn collect_tap_status_impls(dir: &Path, root: &Path, hits: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if name != "target" && name != "node_modules" {
                    collect_tap_status_impls(&path, root, hits);
                }
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text
                .lines()
                .any(|line| line.trim_start().starts_with("impl") && line.contains("TapStatus for"))
            {
                hits.push(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use std::path::PathBuf;

    /// Spawn a short-lived shell as a stand-in child.
    ///
    /// **Never the real helper.** Every property under test here — where the
    /// binary is looked for, that SIGTERM precedes SIGKILL, that a reap is
    /// idempotent, that a read carries a deadline — is about process
    /// lifecycle and nothing about audio. Spawning `paraeq-stimulus` would
    /// open a device and play a sweep to prove things a shell proves for free.
    fn shell_child(script: &str) -> Helper {
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("/bin/sh spawns");
        Helper::adopt(child).expect("the pipes are wired")
    }

    fn touch_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// Both layouts, against fixture directory trees.
    ///
    /// The two branches compute the same path — the helper is always a SIBLING
    /// of the running executable — and they are still two branches, because the
    /// REASON differs and a packaging failure has to be able to say which
    /// layout it expected. Pinning both is what stops someone "simplifying" the
    /// bundle branch away and then reading `Contents/MacOS` in a bug report
    /// with nothing in the code that ever mentions it.
    #[test]
    fn the_helper_path_resolves_in_dev_and_in_a_bundle() {
        let root = tempfile::tempdir().expect("a scratch dir");

        // Release: ParaEQ.app/Contents/MacOS/{ParaEQ, paraeq-stimulus}
        let bundle_exec = root.path().join("ParaEQ.app/Contents/MacOS");
        let bundled_helper = bundle_exec.join(HELPER_BIN);
        touch_executable(&bundled_helper);
        let found = resolve_helper(&bundle_exec);
        assert_eq!(found.source, HelperSource::Bundle);
        assert_eq!(found.path, bundled_helper);
        assert_eq!(
            HelperSpawner::with_exe_dir(&bundle_exec)
                .locate()
                .expect("the bundled helper is found")
                .source,
            HelperSource::Bundle
        );

        // Dev: target/debug/{paraeq-desktop, paraeq-stimulus}
        let target_dir = root.path().join("target/debug");
        let dev_helper = target_dir.join(HELPER_BIN);
        touch_executable(&dev_helper);
        let found = resolve_helper(&target_dir);
        assert_eq!(found.source, HelperSource::TargetDir);
        assert_eq!(found.path, dev_helper);
        assert_eq!(
            HelperSpawner::with_exe_dir(&target_dir)
                .locate()
                .expect("the dev helper is found")
                .source,
            HelperSource::TargetDir
        );
    }

    /// A packaging mistake must report itself as one. Without the check, the
    /// user meets a generic spawn failure at the one moment they are waiting to
    /// hear a sweep, and the PR that forgot the bundler entry looks green.
    #[test]
    fn a_missing_or_unexecutable_helper_refuses_by_path_before_spawning() {
        let root = tempfile::tempdir().expect("a scratch dir");
        let dir: PathBuf = root.path().join("Contents/MacOS");
        std::fs::create_dir_all(&dir).expect("mkdir");

        let error = HelperSpawner::with_exe_dir(&dir)
            .locate()
            .expect_err("nothing is there");
        let message = error.to_string();
        assert!(message.contains(HELPER_BIN), "names the file: {message}");
        assert!(message.contains("Bundle"), "names the layout: {message}");

        // Present but not executable is the same failure with a different
        // cause, and it must not read as "found".
        std::fs::write(dir.join(HELPER_BIN), b"not a program").expect("write");
        HelperSpawner::with_exe_dir(&dir)
            .locate()
            .expect_err("a non-executable file is not a helper");
    }

    /// The packaging instructions and the runtime resolver must name the SAME
    /// binary.
    ///
    /// They are two files that cannot see each other: `binaries/README.md`
    /// tells the owner what the bundler will copy, and `HELPER_BIN` tells the
    /// app what to look for. Rename one and the app ships a helper it cannot
    /// find -- a failure that appears only in a real bundle, only at the moment
    /// a user asks for a verification, and never in any test that does not
    /// compare these two strings.
    ///
    /// It reads the README rather than `tauri.conf.json` because the
    /// `externalBin` entry is deliberately NOT in the config yet: `tauri-build`
    /// resolves that key in the BUILD SCRIPT, on every `cargo build`, so an
    /// entry with nothing staged turns `cargo test --workspace` red for
    /// everyone. The README carries the entry verbatim and the staging command
    /// beside it; this test is what keeps that copy honest until it lands.
    ///
    /// The `-<target-triple>` suffix is the BUNDLER's, not the runtime's -- it
    /// exists on disk before packaging so the bundler can pick the right
    /// architecture, and the copy inside the bundle carries the plain name.
    #[test]
    fn the_packaged_helper_and_the_resolver_name_the_same_binary() {
        let readme = include_str!("../binaries/README.md");
        assert!(
            readme.contains(&format!("\"externalBin\": [\"binaries/{HELPER_BIN}\"]")),
            "the staging instructions must name '{HELPER_BIN}' in the bundler entry"
        );
        assert!(
            readme.contains(&format!("{HELPER_BIN}-$(rustc --print host-tuple)")),
            "and the staged file must carry the target-triple suffix"
        );
        // The config does not carry the entry yet, and the reason is in the
        // README. When it lands, THIS assertion is the one to invert.
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid JSON");
        assert!(
            config["bundle"]["externalBin"].is_null(),
            "externalBin landed in the config: stage the binary in the same \
             commit and invert this assertion, or every cargo build fails"
        );
    }

    /// Rung 1b is SIGTERM and rung 1c is SIGKILL, and the order is the whole
    /// safety property: SIGTERM is a SECOND ramp request that the child handles
    /// and fades on, while SIGKILL bypasses its `Drop` and leaves a private
    /// render aggregate on the user's output device.
    ///
    /// The stand-in traps TERM and exits 42 — the shell's stand-in for "I
    /// handled it and shut down on my own terms". A code of 42 with
    /// `signalled == false` is only reachable if SIGTERM arrived and SIGKILL
    /// did not.
    #[test]
    fn sigterm_is_sent_before_sigkill_and_the_child_exits_on_its_own_terms() {
        // `armed` is not decoration: without it the SIGTERM can arrive before
        // the shell has run `trap`, and the child then dies OF the signal --
        // which is the very outcome this test exists to distinguish from
        // handling it. Real helpers have the same window, which is why the
        // ladder's SIGTERM rung is not the FIRST rung.
        let mut child =
            shell_child("trap 'exit 42' TERM; echo armed; while :; do sleep 0.02; done");
        assert_eq!(
            child.read_line(Duration::from_secs(5)).expect("a read"),
            HelperLine::Line("armed".to_owned())
        );
        // It is alive: the non-blocking probe says so, which is also the probe
        // the teardown ladder uses to decide whether to escalate.
        assert_eq!(child.try_reap().expect("a status read"), None);

        child.request_terminate().expect("SIGTERM");
        let exit = child.reap().expect("it exits");
        assert_eq!(
            exit,
            HelperExit {
                code: Some(42),
                signalled: false
            },
            "the child handled SIGTERM; a SIGKILL would have left code None and signalled true"
        );
    }

    /// A child that ignores SIGTERM is why rung 1c exists at all.
    #[test]
    fn a_child_that_ignores_sigterm_is_still_killed() {
        let mut child = shell_child("trap '' TERM; echo armed; while :; do sleep 0.02; done");
        assert_eq!(
            child.read_line(Duration::from_secs(5)).expect("a read"),
            HelperLine::Line("armed".to_owned())
        );
        child.request_terminate().expect("SIGTERM is ignored");
        assert_eq!(
            child.try_reap().expect("a status read"),
            None,
            "it ignored the polite rung"
        );
        child.kill().expect("SIGKILL");
        let exit = child.reap().expect("it dies");
        assert!(exit.signalled, "SIGKILL shows up as signalled: {exit:?}");
    }

    /// `reap` is called from the teardown ladder, which runs on every exit path
    /// including a panic — so it can run twice. A second `wait` on a reaped pid
    /// blocks forever on some platforms and returns nonsense on others; the
    /// cached status is the only safe answer.
    #[test]
    fn reap_is_idempotent_and_does_not_block_on_an_already_reaped_child() {
        let mut child = shell_child("exit 3");
        let first = child.reap().expect("the first reap");
        assert_eq!(first.code, Some(3));

        let started = Instant::now();
        assert_eq!(child.reap().expect("the second reap"), first);
        assert_eq!(child.try_reap().expect("and the probe"), Some(first));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a second reap must be answered from the cache, not by waiting again"
        );
    }

    /// The seam's own contract, on a real corpse. A rung that failed loudly
    /// here would abort the teardown ladder BEFORE the device-gone check — the
    /// exact failure the ladder exists to prevent.
    ///
    /// The signal rungs are the sharp ones: after a reap the kernel may reuse
    /// the pid, so `signal` must refuse to fire at all rather than deliver
    /// SIGKILL to whatever process inherited the number.
    #[test]
    fn every_helper_process_method_is_safe_to_call_after_the_child_has_exited() {
        let mut child = shell_child("exit 0");
        let first = child.reap().expect("the first reap");

        for _ in 0..2 {
            child.request_abort().expect("abort on a dead child");
            child
                .request_terminate()
                .expect("terminate on a dead child");
            child.kill().expect("kill on a dead child");
            child.send_play().expect("play on a dead child");
            assert_eq!(child.reap().expect("reap"), first);
            assert_eq!(child.try_reap().expect("try_reap"), Some(first));
            let _ = child.read_line(Duration::from_millis(1));
            let _ = child.pid();
        }
    }

    /// A read carries a deadline, and an expired one is its OWN answer. Under
    /// a two-state read this was spelled the same way as EOF, and the teardown
    /// ladder then believed a wedged child had exited cleanly and stopped
    /// escalating — leaving it playing.
    #[test]
    fn a_read_that_times_out_is_not_an_eof() {
        let mut child = shell_child("while :; do sleep 0.02; done");
        let started = Instant::now();
        assert_eq!(
            child.read_line(Duration::from_millis(50)).expect("a read"),
            HelperLine::DeadlineExpired
        );
        assert!(started.elapsed() >= Duration::from_millis(50));
        child.kill().expect("SIGKILL");
        child.reap().expect("reaped");

        // ... and once the pipe really is closed, EOF is reported as EOF.
        let mut gone = shell_child("exit 0");
        gone.reap().expect("reaped");
        assert_eq!(
            gone.read_line(Duration::from_millis(250)).expect("a read"),
            HelperLine::Eof
        );
    }

    /// The protocol lines go over stdin verbatim, newline included, and the
    /// child reads them one per line.
    #[test]
    fn play_and_abort_are_written_as_whole_protocol_lines() {
        let mut child = shell_child("while read line; do echo \"got:$line\"; done");
        child.send_play().expect("play");
        assert_eq!(
            child.read_line(Duration::from_secs(5)).expect("a read"),
            HelperLine::Line("got:play".to_owned())
        );
        child.request_abort().expect("abort");
        assert_eq!(
            child.read_line(Duration::from_secs(5)).expect("a read"),
            HelperLine::Line("got:abort".to_owned())
        );
        child.kill().expect("SIGKILL");
        child.reap().expect("reaped");
    }

    /// The routing spelling is the SEAM's, never a local `format!`. A second
    /// spelling is how a per-ear coupler verification silently differences a
    /// per-ear baseline against an L+R sum.
    #[test]
    fn the_channel_argument_is_the_seams_own_spelling() {
        assert_eq!(HelperRouting::Both.as_channel_arg(), "both");
        assert_eq!(HelperRouting::Only(1).as_channel_arg(), "1");
        let spawn_site = include_str!("verify_seam.rs")
            .split_once("impl StimulusHelper for HelperSpawner")
            .expect("the spawn site")
            .1;
        let spawn_site = spawn_site
            .split_once("impl Helper {")
            .expect("the impl that follows")
            .0;
        assert!(
            spawn_site.contains("routing.as_channel_arg()"),
            "the spawn site must call the seam's mapping"
        );
    }

    /// The HAL can refuse a read. When it does, the teardown must report a
    /// POSSIBLE leak rather than a clean one: the two ways to be wrong are not
    /// symmetric. A UID nothing can carry exercises the same code path without
    /// hardware.
    #[test]
    fn an_unreadable_device_is_never_reported_as_absent() {
        let seam = DeviceSeam;
        // Either the HAL answers "nobody has it" (false) or it refuses, in
        // which case the seam answers `true`. What it may NEVER do is answer
        // `false` because the read failed.
        let _ = seam.device_exists("com.paraeq.no-such-device.deadbeef");
        // The rate side has the opposite default: unanswerable is `None`, which
        // the rate fence treats as permissive.
        assert_eq!(
            seam.nominal_sample_rate("com.paraeq.no-such-device.deadbeef"),
            None
        );
    }
}
