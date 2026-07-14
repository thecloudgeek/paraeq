// Consumed by the command layer and app bootstrap in later tasks (6-10, 17);
// the pub items are exercised by their own unit tests until then.
#[allow(dead_code)]
mod eq;
#[allow(dead_code)]
mod settings;
#[allow(dead_code)]
mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
