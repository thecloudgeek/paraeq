mod commands;
mod engine_bridge;
mod eq;
mod settings;
mod state;

use state::{AppData, AppShared};
use std::sync::Mutex;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
            app.manage(AppShared {
                data: Mutex::new(data),
                engine: Mutex::new(Some(handle)),
                profiles_dir,
                settings_path,
            });

            // Start the forwarder AFTER the state is managed (it reads
            // AppShared through the AppHandle).
            engine_bridge::start_forwarder(app.handle().clone(), rx);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Never leave the system muted: on exit, take the handle out of
            // managed state, send Disable (full teardown), and drop it (joins
            // the controller thread) BEFORE the process exits.
            if let tauri::RunEvent::Exit = event {
                let shared = app_handle.state::<AppShared>();
                let handle = shared.engine.lock().unwrap().take();
                if let Some(h) = handle {
                    h.send(paraeq_engine::controller::EngineCommand::Disable);
                    drop(h);
                }
            }
        });
}
