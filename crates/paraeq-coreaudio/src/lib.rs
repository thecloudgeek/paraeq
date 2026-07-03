//! All unsafe CoreAudio FFI lives here — the ONLY macOS-specific crate
//! (spec constraint). Tap lifecycle, devices, listeners land in stage 3.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
