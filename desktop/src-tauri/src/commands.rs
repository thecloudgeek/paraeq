//! The `#[tauri::command]` surface, grouped by domain (`engine_*`, `eq_*`, and
//! the `get_app_state` initial-fetch). Every mutating command ends by calling
//! [`engine_bridge::publish_current`] so the UI reflects the change immediately
//! -- the `data`-side change (bands, preamp, devices) is visible at once, and
//! the engine-side change follows on the next forwarder snapshot.
//!
//! Validation is the enforcement wall: no band or preamp reaches the engine
//! without passing [`eq::validate_bands`] / [`eq::validate_preamp`] at the live
//! stream rate first.

use crate::engine_bridge;
use crate::eq::{self, ResponseData};
use crate::profiles;
use crate::state::{AppShared, AppState, OutputDeviceInfo};
use paraeq_dsp::peq::{EQBand, FilterType};
use paraeq_engine::controller::EngineCommand;
use tauri::Manager;

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
