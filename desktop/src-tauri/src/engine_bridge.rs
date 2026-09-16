//! The bridge between the Tauri app and the `paraeq-engine` controller:
//! spawning the engine honoring persisted state, the single snapshot
//! forwarder thread, and the `publish` choke point that emits `app-state` and
//! persists the durable settings subset.
//!
//! Safety-critical wiring lives here (never-leave-muted + no-NaN + lock
//! discipline). Two rules the whole module upholds:
//!   1. NEVER hold the [`AppShared::data`] lock across an `EngineHandle::send`
//!      or a Tauri `emit` -- lock, copy what is needed, drop the guard, THEN
//!      send/emit.
//!   2. NEVER send bands that were not [`eq::validate_bands`]-validated, so the
//!      user gets a named reason instead of a silently dropped band. Since
//!      R1-6 this is feedback, not the safety wall: `paraeq-engine` re-derives
//!      every band at the LIVE stream rate at install time and drops (or, if
//!      nothing survives, refuses) anything at or above the new Nyquist, so no
//!      NaN/Inf coefficient can reach the realtime chain by any route.

use crate::eq;
use crate::settings::{self, Settings};
use crate::state::{AppShared, OutputDeviceInfo};
use paraeq_coreaudio::backend::TapBackend;
use paraeq_coreaudio::devices;
use paraeq_engine::controller::{EngineCommand, EngineConfig, EngineHandle, EngineState};
use paraeq_engine::status::EngineStatus;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, Manager};

/// How long the forwarder blocks on the snapshot channel before looping to
/// re-check for disconnection.
const RECV_TIMEOUT: Duration = Duration::from_millis(500);

/// Spawn the engine controller with a real Core Audio [`TapBackend`], honoring
/// persisted state: the tap engages on spawn ONLY when the user has both
/// completed setup and left the EQ enabled. Called ONCE from `.setup`, AFTER
/// [`settings::load`] -- never spawn before reading persisted state, or the tap
/// mutes system audio pre-wizard until fail-open.
pub fn spawn_engine(settings: &Settings) -> EngineHandle {
    EngineHandle::spawn(
        TapBackend::new(),
        EngineConfig {
            enabled: settings.engine_enabled && settings.setup_complete,
            ..EngineConfig::default()
        },
    )
}

/// THE choke point every command and the forwarder call after a mutation:
/// compose [`AppState`](crate::state::AppState) from `AppShared::data` plus the
/// given engine snapshot, persist the durable `Settings` subset if it changed
/// since the last write, and emit the `app-state` event. The data guard is
/// dropped before any disk I/O or emit; no lock is held across the emit.
pub fn publish(app: &tauri::AppHandle, engine: &EngineState) {
    let shared = app.state::<AppShared>();
    let (app_state, durable) = {
        let data = shared.data.lock().unwrap();
        (data.app_state(engine), data.to_settings())
    };
    // Persist only when the durable subset actually changed. Compare against the
    // in-memory mirror of what is on disk (`AppShared::persisted`, seeded from
    // the file at setup) instead of re-reading settings.json -- the forwarder
    // publishes on every input_peak change during playback (tens/sec), and none
    // of those engine-only deltas (status, peak, latency) touch the durable
    // subset, so this keeps engine ticks off the disk entirely, reads included.
    //
    // The `persisted` mutex is held across the compare AND the save so the
    // check-and-write is atomic and the mirror only advances on a *successful*
    // write (a failed save leaves it stale so the next publish retries, matching
    // the old re-read-from-disk semantics). It is acquired after the `data`
    // guard is dropped and released before the emit below -- the data/engine
    // lock is never held here, and no lock spans the emit.
    {
        let mut persisted = shared.persisted.lock().unwrap();
        if *persisted != durable {
            match settings::save(&shared.settings_path, &durable) {
                Ok(()) => *persisted = durable,
                Err(e) => log::warn!("failed to persist settings: {e}"),
            }
        }
    }
    // Sync the tray (toggle text, bypass checkmark, tooltip, profile submenu)
    // BEFORE the emit consumes `app_state`. sync_tray only schedules the
    // main-thread mutation, so it never blocks the emit.
    crate::tray::sync_tray(app, &app_state);
    if let Err(e) = app.emit("app-state", app_state) {
        log::warn!("failed to emit app-state: {e}");
    }
}

/// Compose the engine half of the snapshot from the live handle, or a disabled
/// `Stopped` default if the handle is somehow absent (e.g. after teardown).
pub fn current_engine_state(shared: &AppShared) -> EngineState {
    let guard = shared.engine.lock().unwrap();
    match guard.as_ref() {
        Some(handle) => (*handle.state()).clone(),
        None => stopped_state(),
    }
}

/// Send a command to the engine, no-op if the handle has been torn down. Holds
/// only the engine-handle lock (never the data lock) across the fire-and-forget
/// send.
pub fn send_cmd(shared: &AppShared, cmd: EngineCommand) {
    let guard = shared.engine.lock().unwrap();
    if let Some(handle) = guard.as_ref() {
        handle.send(cmd);
    }
}

/// Convenience: compose the current engine snapshot and [`publish`].
pub fn publish_current(app: &tauri::AppHandle) {
    let shared = app.state::<AppShared>();
    let engine = current_engine_state(&shared);
    publish(app, &engine);
}

/// The dedicated forwarder thread. Owns `rx` (the subscriber receiver, created
/// BEFORE the handle was stored so no early snapshot is missed) and reconciles
/// each published snapshot:
///
/// 1. On an engine-published redesign refusal (or the first stream),
///    re-validate the bands at the live rate and re-send the correction (or
///    `ClearCorrection` on failure) -- the R1-6 fallback path. A bare rate
///    change does NOT come through here any more: the engine re-derives a
///    `Peq` correction at the new rate itself.
/// 2. On a device change, refresh the selectable device list.
/// 3. Publish the snapshot (emit + persist).
///
/// `handle.state()` is always the reconciliation source; channel pushes are
/// lossy change-notifications.
pub fn start_forwarder(app: tauri::AppHandle, rx: Receiver<Arc<EngineState>>) {
    std::thread::Builder::new()
        .name("paraeq-forwarder".into())
        .spawn(move || {
            // "Have we seen a stream yet", since R1-6: the rate it carries is
            // bookkeeping, not the redesign trigger.
            let mut last_rate: Option<f64> = None;
            let mut last_device_uid: Option<String> = None;
            loop {
                match rx.recv_timeout(RECV_TIMEOUT) {
                    Ok(snapshot) => {
                        let shared = app.state::<AppShared>();

                        // 1. Redesign re-send: the engine refused the config
                        //    it holds, or this is the first stream we have
                        //    seen. Same shape as before, re-pointed at the
                        //    engine's own verdict.
                        let bands = { shared.data.lock().unwrap().bands.clone() };
                        let have_bands = !bands.is_empty();
                        if let Some(rate) = eq::resend_decision(last_rate, &snapshot, have_bands) {
                            match eq::validate_bands(&bands, rate) {
                                Ok(()) => {
                                    if let Some(cfg) = eq::design_correction(&bands, rate) {
                                        send_cmd(&shared, EngineCommand::SetCorrection(cfg));
                                    }
                                }
                                Err(e) => {
                                    log::warn!(
                                        "bands invalid at {rate} Hz: {e}; clearing correction \
                                         (flat passthrough)"
                                    );
                                    send_cmd(&shared, EngineCommand::ClearCorrection);
                                }
                            }
                            last_rate = Some(rate);
                        }

                        // 2. Device change -> refresh the selectable list.
                        let device_uid = snapshot.stream.as_ref().map(|s| s.device_uid.clone());
                        if device_uid != last_device_uid {
                            if device_uid.is_some() {
                                match devices::list_output_devices() {
                                    Ok(list) => {
                                        let infos = to_device_info(list);
                                        shared.data.lock().unwrap().devices = infos;
                                    }
                                    Err(e) => {
                                        log::warn!("output device enumeration failed: {e}")
                                    }
                                }
                            }
                            last_device_uid = device_uid;
                        }

                        // 3. Publish (emits app-state, persists the durable
                        //    subset, and syncs the tray inside publish()).
                        publish(&app, &snapshot);
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("spawn paraeq-forwarder thread");
}

/// Project the coreaudio (serde-free) device list into the UI wire struct.
fn to_device_info(list: Vec<devices::OutputDevice>) -> Vec<OutputDeviceInfo> {
    list.into_iter()
        .map(|d| OutputDeviceInfo {
            name: d.name,
            uid: d.uid,
        })
        .collect()
}

/// A disabled, stopped snapshot -- the fallback when the engine handle is
/// absent (only after Exit teardown has taken it).
fn stopped_state() -> EngineState {
    EngineState {
        bypass: false,
        correction: None,
        correction_rate_mismatch: None,
        enabled: false,
        frame_mismatch_blocks: 0,
        gain_db: 0.0,
        input_peak: 0.0,
        latency_ms: None,
        status: EngineStatus::Stopped,
        stream: None,
    }
}
