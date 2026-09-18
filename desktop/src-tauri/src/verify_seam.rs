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
use paraeq_measure::{
    EngineControl, EngineFacts, EngineStatusKind, GainPin, MeasureError, MeasurementLeaseToken,
    TapStatus,
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
