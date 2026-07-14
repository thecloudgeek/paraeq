//! The `#[tauri::command]` surface, grouped by domain (`engine_*`, `eq_*`, and
//! the `get_app_state` initial-fetch). Every mutating command ends by calling
//! [`engine_bridge::publish_current`] so the UI reflects the change immediately
//! -- the `data`-side change (bands, preamp, devices) is visible at once, and
//! the engine-side change follows on the next forwarder snapshot.
//!
//! Validation is the enforcement wall: no band or preamp reaches the engine
//! without passing [`eq::validate_bands`] / [`eq::validate_preamp`] at the live
//! stream rate first.

use crate::autoeq::{self, AutoEqClient, IndexEntry, ParsedPresetDto, ReqwestFetch};
use crate::engine_bridge;
use crate::eq::{self, ImportResult, ResponseData};
use crate::profiles;
use crate::setup::{self, ProbeVerdict};
use crate::state::{AppShared, AppState, OutputDeviceInfo};
use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};
use paraeq_engine::controller::EngineCommand;
use tauri::Manager;

/// Max search results returned to the browse dialog (the same model recurs
/// under many sources/rigs; the dialog disambiguates but stays bounded).
const AUTOEQ_SEARCH_CAP: usize = 200;

/// Build a Tauri-free [`AutoEqClient`] pointed at `app_data_dir()/autoeq/`.
/// This is the ONLY place that couples the client to Tauri (path resolution);
/// the client itself holds no Tauri types.
fn autoeq_client(app: &tauri::AppHandle) -> Result<AutoEqClient<ReqwestFetch>, String> {
    let cache_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("autoeq");
    Ok(AutoEqClient::new(
        ReqwestFetch::new()?,
        cache_dir,
        None,
        None,
    ))
}

/// The live stream sample rate, or the 48 kHz fallback when no stream is up.
/// Bands are validated and designed at this rate.
fn live_rate(shared: &AppShared) -> f64 {
    engine_bridge::current_engine_state(shared)
        .stream
        .map(|s| s.sample_rate)
        .unwrap_or(48_000.0)
}

/// The shared apply path for every band edit (set/add/remove): validate at the
/// live rate, send the designed correction (or `ClearCorrection` when empty),
/// store the bands, then publish. `bands` is the FULL new band set.
fn apply_bands(app: &tauri::AppHandle, bands: Vec<EQBand>) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    let rate = live_rate(&shared);
    eq::validate_bands(&bands, rate)?;
    match eq::design_correction(&bands, rate) {
        Some(cfg) => engine_bridge::send_cmd(&shared, EngineCommand::SetCorrection(cfg)),
        None => engine_bridge::send_cmd(&shared, EngineCommand::ClearCorrection),
    }
    {
        let mut data = shared.data.lock().unwrap();
        data.bands = bands;
    }
    engine_bridge::publish_current(app);
    Ok(())
}

/// Serve the initial UI state (events emitted before `listen` are lost, so the
/// UI fetches this once on mount). Composes `data` + the current engine
/// snapshot.
#[tauri::command]
pub fn get_app_state(app: tauri::AppHandle) -> Result<AppState, String> {
    let shared = app.state::<AppShared>();
    let engine = engine_bridge::current_engine_state(&shared);
    let data = shared.data.lock().unwrap();
    Ok(data.app_state(&engine))
}

/// Enable the EQ (records persisted intent -- survives fail-open across
/// launches).
#[tauri::command]
pub fn engine_enable(app: tauri::AppHandle) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    engine_bridge::send_cmd(&shared, EngineCommand::Enable);
    shared.data.lock().unwrap().engine_enabled = true;
    engine_bridge::publish_current(&app);
    Ok(())
}

/// Disable the EQ (full teardown: session stopped, tap destroyed, device
/// unmuted). Records persisted intent.
#[tauri::command]
pub fn engine_disable(app: tauri::AppHandle) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    engine_bridge::send_cmd(&shared, EngineCommand::Disable);
    shared.data.lock().unwrap().engine_enabled = false;
    engine_bridge::publish_current(&app);
    Ok(())
}

/// Toggle A/B bypass (correction retained; chain passes through when bypassed).
#[tauri::command]
pub fn engine_set_bypass(app: tauri::AppHandle, bypass: bool) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    engine_bridge::send_cmd(&shared, EngineCommand::SetBypass(bypass));
    engine_bridge::publish_current(&app);
    Ok(())
}

/// Set the preamp trim (dB). Validated into the accepted range before it
/// reaches the (unclamped) engine gain command.
#[tauri::command]
pub fn engine_set_preamp_db(app: tauri::AppHandle, db: f64) -> Result<(), String> {
    eq::validate_preamp(db)?;
    let shared = app.state::<AppShared>();
    engine_bridge::send_cmd(&shared, EngineCommand::SetGainDb(db as f32));
    shared.data.lock().unwrap().preamp_db = db;
    engine_bridge::publish_current(&app);
    Ok(())
}

/// Set the system default output device. The engine's own
/// `DefaultOutputChanged` listener drives the rebuild -- no engine command is
/// sent from here.
#[tauri::command]
pub fn engine_set_default_output(app: tauri::AppHandle, uid: String) -> Result<(), String> {
    paraeq_coreaudio::devices::set_default_output_device(&uid).map_err(|e| e.to_string())?;
    let shared = app.state::<AppShared>();
    shared.data.lock().unwrap().default_output_uid = Some(uid);
    engine_bridge::publish_current(&app);
    Ok(())
}

/// Re-enumerate output devices, refresh the cached list, publish, and return
/// it.
#[tauri::command]
pub fn engine_list_outputs(app: tauri::AppHandle) -> Result<Vec<OutputDeviceInfo>, String> {
    let list: Vec<OutputDeviceInfo> = paraeq_coreaudio::devices::list_output_devices()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|d| OutputDeviceInfo {
            name: d.name,
            uid: d.uid,
        })
        .collect();
    let shared = app.state::<AppShared>();
    shared.data.lock().unwrap().devices = list.clone();
    engine_bridge::publish_current(&app);
    Ok(list)
}

/// THE apply path (prototype contract: every edit applies immediately, no Apply
/// button). Replaces the entire band set.
#[tauri::command]
pub fn eq_set_bands(app: tauri::AppHandle, bands: Vec<EQBand>) -> Result<(), String> {
    apply_bands(&app, bands)
}

/// Append a default peaking band, then apply.
#[tauri::command]
pub fn eq_add_band(app: tauri::AppHandle) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    let mut bands = shared.data.lock().unwrap().bands.clone();
    bands.push(EQBand {
        filter_type: FilterType::Peaking,
        fc: 1000.0,
        gain_db: 0.0,
        q: 1.41,
    });
    apply_bands(&app, bands)
}

/// Remove the band at `index`, or the LAST band when `None` (prototype parity:
/// remove-with-no-selection deletes the last row). A no-op on an empty set.
#[tauri::command]
pub fn eq_remove_band(app: tauri::AppHandle, index: Option<usize>) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    let mut bands = shared.data.lock().unwrap().bands.clone();
    if bands.is_empty() {
        return Ok(());
    }
    let idx = match index {
        Some(i) => i,
        None => bands.len() - 1,
    };
    if idx >= bands.len() {
        return Err(format!(
            "band index {idx} out of range (have {} bands)",
            bands.len()
        ));
    }
    bands.remove(idx);
    apply_bands(&app, bands)
}

/// The magnitude response the plot draws, over the caller's frequency grid, at
/// the live stream rate (48 kHz fallback).
#[tauri::command]
pub fn eq_response(app: tauri::AppHandle, freqs: Vec<f64>) -> Result<ResponseData, String> {
    let shared = app.state::<AppShared>();
    let rate = live_rate(&shared);
    let bands = shared.data.lock().unwrap().bands.clone();
    Ok(eq::response(&bands, &freqs, rate))
}

/// Export the current bands + preamp to an AutoEQ ParametricEq text file. The
/// path comes from the native save dialog (capability-safe). Format is the
/// DSP-owned `export_autoeq_format`; the sample rate is irrelevant to export.
#[tauri::command]
pub fn eq_export_autoeq(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    let (bands, preamp_db) = {
        let data = shared.data.lock().unwrap();
        (data.bands.clone(), data.preamp_db)
    };
    let rate = live_rate(&shared);
    let text = ParametricEQ {
        bands,
        sample_rate: rate,
    }
    .export_autoeq_format(preamp_db);
    std::fs::write(&path, text).map_err(|e| format!("failed to write {path}: {e}"))
}

/// Import an AutoEQ ParametricEq text file (path from the native open dialog).
/// Zero filter lines is an error carrying the prototype's exact message. Else
/// the bands REPLACE the current set through the validating apply path, and the
/// file's preamp -- clamped into the accepted range -- is applied through the
/// preamp path. Bands are applied first, so an invalid band set errors before
/// the preamp is touched (no partial apply). Returns the [`ImportResult`] so the
/// UI can report the band count and whether the preamp was clamped.
#[tauri::command]
pub fn eq_import_autoeq(app: tauri::AppHandle, path: String) -> Result<ImportResult, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| format!("failed to read {path}: {e}"))?;
    let (bands, result) = eq::prepare_import(&text)?;
    // Bands first: apply_bands validates at the live rate and returns Err
    // WITHOUT mutating anything, so an invalid file never applies a partial
    // (preamp-only) change.
    apply_bands(&app, bands)?;
    let shared = app.state::<AppShared>();
    engine_bridge::send_cmd(&shared, EngineCommand::SetGainDb(result.preamp_db as f32));
    shared.data.lock().unwrap().preamp_db = result.preamp_db;
    engine_bridge::publish_current(&app);
    Ok(result)
}

/// Save the current bands + preamp as a named profile, refresh the cached
/// profile list, mark it active, then publish. Rejects an empty/whitespace-only
/// name (would slugify to a useless filename).
#[tauri::command]
pub fn profiles_save(app: tauri::AppHandle, name: String) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("profile name must not be empty".to_string());
    }
    let shared = app.state::<AppShared>();
    let (bands, preamp_db) = {
        let data = shared.data.lock().unwrap();
        (data.bands.clone(), data.preamp_db)
    };
    let profile = profiles::Profile {
        bands,
        name: name.clone(),
        preamp_db,
    };
    profiles::save_profile(&shared.profiles_dir, &profile).map_err(|e| e.to_string())?;
    {
        let mut data = shared.data.lock().unwrap();
        data.profiles = profiles::list_profiles(&shared.profiles_dir);
        data.active_profile = Some(name);
    }
    engine_bridge::publish_current(&app);
    Ok(())
}

/// Activate a saved profile: load it, apply its bands through EXACTLY the
/// `eq_set_bands` path (validate at the live rate, then `SetCorrection`/
/// `ClearCorrection`) and its preamp through the `engine_set_preamp_db` path,
/// mark it active, then publish once. Errors if no profile matches `name`.
#[tauri::command]
pub fn profiles_activate(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    let profile = profiles::load_profile(&shared.profiles_dir, &name)
        .ok_or_else(|| format!("no profile named {name:?}"))?;
    let rate = live_rate(&shared);
    eq::validate_bands(&profile.bands, rate)?;
    eq::validate_preamp(profile.preamp_db)?;
    match eq::design_correction(&profile.bands, rate) {
        Some(cfg) => engine_bridge::send_cmd(&shared, EngineCommand::SetCorrection(cfg)),
        None => engine_bridge::send_cmd(&shared, EngineCommand::ClearCorrection),
    }
    engine_bridge::send_cmd(&shared, EngineCommand::SetGainDb(profile.preamp_db as f32));
    {
        let mut data = shared.data.lock().unwrap();
        data.bands = profile.bands;
        data.preamp_db = profile.preamp_db;
        data.active_profile = Some(name);
    }
    engine_bridge::publish_current(&app);
    Ok(())
}

/// The cached list of saved profile display names (populated at startup from
/// disk and refreshed on every save).
#[tauri::command]
pub fn profiles_list(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let shared = app.state::<AppShared>();
    let profiles = shared.data.lock().unwrap().profiles.clone();
    Ok(profiles)
}

// --- AutoEq DB (async: network must not block a sync command) --------------
//
// The AutoEq index is NOT kept in `AppShared`; each command re-reads it from
// the on-disk cache via `sync_index(false)` (a zero-fetch cache hit once
// synced). This keeps `AppShared` free of AutoEq state and the client
// stateless per call, at the cost of one small cache read per browse action --
// negligible for a user-driven dialog.

/// Sync the model index (cache-first; `force` refetches). Returns the entry
/// count for the dialog's "N models" affordance.
#[tauri::command]
pub async fn autoeq_sync_index(app: tauri::AppHandle, force: bool) -> Result<usize, String> {
    let client = autoeq_client(&app)?;
    Ok(client.sync_index(force).await?.len())
}

/// Case-insensitive substring search on model name over the synced index
/// (prototype parity), returning full entries (so the dialog can disambiguate
/// duplicates) capped at [`AUTOEQ_SEARCH_CAP`].
#[tauri::command]
pub async fn autoeq_search(
    app: tauri::AppHandle,
    query: String,
) -> Result<Vec<IndexEntry>, String> {
    let client = autoeq_client(&app)?;
    let entries = client.sync_index(false).await?;
    Ok(AutoEqClient::<ReqwestFetch>::search(&entries, &query)
        .into_iter()
        .take(AUTOEQ_SEARCH_CAP)
        .cloned()
        .collect())
}

/// Fetch + parse a preset, keyed by the entry `path` (resolved against the
/// synced index; unknown paths error). Fetch/parse ONLY -- applying the bands
/// and preamp is the UI's explicit second step (Task 16). A preset with no
/// parametric filters is an error.
#[tauri::command]
pub async fn autoeq_fetch_preset(
    app: tauri::AppHandle,
    path: String,
) -> Result<ParsedPresetDto, String> {
    let client = autoeq_client(&app)?;
    let entry = client
        .sync_index(false)
        .await?
        .into_iter()
        .find(|e| e.path == path)
        .ok_or_else(|| format!("no AutoEq entry for path {path:?}"))?;
    let text = client.fetch_preset(&entry).await?;
    let parsed = autoeq::parse_preset(&text);
    if parsed.bands.is_empty() {
        return Err("Preset contains no parametric EQ filters".to_string());
    }
    Ok(parsed)
}

// --- Setup wizard + chime probe -------------------------------------------
//
// The probe verifies audio CAPTURE (tap + TCC grant), which is otherwise
// unobservable: a missing grant is indistinguishable from silence. See
// `setup.rs` for the full rationale and the never-orphan-a-child safety story.

/// Start the deterministic chime probe: ensure the engine is enabled (so a tap
/// engages) and spawn the looping `afplay` helper. Enabling here is idempotent
/// and also clears any fail-open latch, so a fresh "Enable EQ" click and a
/// "Re-test" both funnel through the same path. `ProbeState::start` is itself
/// idempotent (it stops any prior probe first), so repeated calls never stack
/// helper loops.
#[tauri::command]
pub fn setup_probe_start(app: tauri::AppHandle) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    engine_bridge::send_cmd(&shared, EngineCommand::Enable);
    shared.data.lock().unwrap().engine_enabled = true;
    shared.probe.start();
    engine_bridge::publish_current(&app);
    Ok(())
}

/// Stop the chime probe: kill any live `afplay` child and join the loop thread.
/// Safe to call when no probe is running.
#[tauri::command]
pub fn setup_probe_stop(app: tauri::AppHandle) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    shared.probe.stop();
    Ok(())
}

/// Compute the probe verdict from the LIVE engine status plus the client's
/// `elapsed_ms` since the probe started. The timing logic lives in Rust
/// ([`setup::probe_verdict`], unit-tested) so the wizard renders from a single
/// source of truth rather than re-implementing the 10 s window in TS.
#[tauri::command]
pub fn setup_probe_verdict(app: tauri::AppHandle, elapsed_ms: u64) -> ProbeVerdict {
    let shared = app.state::<AppShared>();
    let status = engine_bridge::current_engine_state(&shared).status;
    setup::probe_verdict(
        &status,
        std::time::Duration::from_millis(elapsed_ms),
        setup::PROBE_TIMEOUT,
    )
}

/// Open System Settings at the Screen & System Audio Recording pane so the user
/// can grant the permission a process tap needs.
#[tauri::command]
pub fn setup_open_privacy_settings() -> Result<(), String> {
    std::process::Command::new("open")
        .arg(setup::PRIVACY_URL)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to open privacy settings: {e}"))
}

/// Mark setup complete (persisted) and stop the probe, recording the user's
/// ACTUAL terminal choice via `enable` -- decoupled from the probe's *temporary*
/// enable ([`setup_probe_start`] flips `engine_enabled` on the instant the
/// diagnostic runs, before any grant is verified):
///
/// - `enable == true` -- the user opted in (a verified-`Running` probe, then
///   Finish). `engine_enabled` persists `true`, so the next launch auto-enables;
///   this holds even if the engine later fails open, because the persisted
///   intent is the user's, not the tap's (decision 1). No engine command is
///   sent -- the probe already engaged it.
/// - `enable == false` -- the user skipped / declined without a successful
///   enable. Any temporary enable the probe left on is reverted with `Disable`
///   (full teardown: session stopped, tap destroyed, device unmuted) and
///   `engine_enabled` persists `false`, so future launches do NOT re-engage the
///   tap and mute audio for ~15 s while the grant is still missing.
///
/// Stopping the probe here guarantees the wizard never leaves an `afplay` helper
/// running. Lock discipline: the `data` guard is dropped before the `Disable`
/// send and before `publish_current` -- no lock spans a send or emit.
#[tauri::command]
pub fn setup_complete(app: tauri::AppHandle, enable: bool) -> Result<(), String> {
    let shared = app.state::<AppShared>();
    shared.probe.stop();
    let must_disable = { shared.data.lock().unwrap().complete_setup(enable) };
    if must_disable {
        engine_bridge::send_cmd(&shared, EngineCommand::Disable);
    }
    engine_bridge::publish_current(&app);
    Ok(())
}
