mod autoeq;
mod commands;
mod engine_bridge;
mod eq;
mod profiles;
mod settings;
mod state;
mod tray;

use state::{AppData, AppShared};
use std::sync::Mutex;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance MUST be the FIRST plugin: two ParaEQ instances would
        // mean two taps fighting over the same output device. A second launch
        // re-focuses the existing window instead of starting a rival process.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // Dialog plugin: registration only here (JS usage -- AutoEq import/export
        // pickers -- lands in a later UI batch; the `dialog:default` capability
        // is granted in capabilities/default.json).
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Resolve persistence paths, then load persisted state BEFORE
            // spawning the engine -- spawning enabled pre-wizard would mute
            // system audio until fail-open.
            let app_data_dir = app.path().app_data_dir()?;
            let settings_path = app_data_dir.join("settings.json");
            let profiles_dir = app_data_dir.join("profiles");
            let settings = settings::load(&settings_path);

            // Spawn honoring persisted enabled/setup state; subscribe BEFORE
            // the handle is stored so the forwarder misses no early snapshot.
            let handle = engine_bridge::spawn_engine(&settings);
            let rx = handle.subscribe();

            // Restore the persisted preamp: clamp the (hand-editable) value
            // into range and push it to the engine. Without this the engine
            // runs at 0 dB while the UI shows the persisted value -- nothing
            // else ever sends SetGainDb at startup.
            let clamped = settings
                .preamp_db
                .clamp(eq::PREAMP_MIN_DB, eq::PREAMP_MAX_DB);
            if clamped != settings.preamp_db {
                log::warn!(
                    "persisted preamp {} dB out of range; clamped to {clamped} dB",
                    settings.preamp_db
                );
            }
            handle.send(paraeq_engine::controller::EngineCommand::SetGainDb(
                clamped as f32,
            ));

            let mut data = AppData::from_settings(&settings);
            data.preamp_db = clamped;
            // Populate the profile list from disk BEFORE the first publish --
            // Settings carries no profile list (only active_profile), so
            // without this the tray/UI would show no profiles at relaunch
            // even though `active_profile` was restored.
            data.profiles = profiles::list_profiles(&profiles_dir);
            app.manage(AppShared {
                data: Mutex::new(data),
                engine: Mutex::new(Some(handle)),
                profiles_dir,
                settings_path,
            });

            // Build the tray from the initial snapshot and manage it BEFORE the
            // forwarder starts -- the forwarder's `sync_tray` (via `publish`)
            // needs `TrayHandles` present to have anything to update.
            let initial_state = {
                let shared = app.state::<AppShared>();
                let engine = engine_bridge::current_engine_state(&shared);
                let data = shared.data.lock().unwrap();
                data.app_state(&engine)
            };
            let handles = tray::build_tray(app.handle(), &initial_state)?;
            app.manage(handles);

            // Start the forwarder AFTER the state is managed (it reads
            // AppShared through the AppHandle).
            engine_bridge::start_forwarder(app.handle().clone(), rx);
            Ok(())
        })
        // Closing the window HIDES it and keeps the app (and tap) alive -- the
        // tray is the persistent surface. Only genuine quit paths tear down.
        // `WindowEvent` is #[non_exhaustive]; the if-let handles just the one
        // variant we care about.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::autoeq_fetch_preset,
            commands::autoeq_search,
            commands::autoeq_sync_index,
            commands::eq_add_band,
            commands::eq_remove_band,
            commands::eq_response,
            commands::eq_set_bands,
            commands::engine_disable,
            commands::engine_enable,
            commands::engine_list_outputs,
            commands::engine_set_bypass,
            commands::engine_set_default_output,
            commands::engine_set_preamp_db,
            commands::get_app_state,
            commands::profiles_activate,
            commands::profiles_list,
            commands::profiles_save,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| match event {
            // A window close never reaches here (it is intercepted and turned
            // into a hide). Only genuine quit paths -- tray Quit (`app.exit(0)`)
            // and Cmd-Q -- raise ExitRequested; we do NOT prevent it, so the
            // app proceeds to RunEvent::Exit and the teardown below.
            tauri::RunEvent::ExitRequested { .. } => {
                log::debug!("exit requested; proceeding to engine teardown");
            }
            // Never leave the system muted: on exit, take the handle out of
            // managed state, send Disable (full teardown), and drop it (joins
            // the controller thread) BEFORE the process exits.
            tauri::RunEvent::Exit => {
                let shared = app_handle.state::<AppShared>();
                let handle = shared.engine.lock().unwrap().take();
                if let Some(h) = handle {
                    h.send(paraeq_engine::controller::EngineCommand::Disable);
                    drop(h);
                }
            }
            _ => {}
        });
}
