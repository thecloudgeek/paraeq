//! Pure-math DSP core. Zero platform dependencies (spec constraint:
//! this crate must never import CoreAudio, Tauri, or any OS API).

/// Crate version, used by the desktop app's about dialog later.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
