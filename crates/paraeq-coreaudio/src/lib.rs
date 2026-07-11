//! All unsafe CoreAudio FFI lives here — the ONLY macOS-specific crate
//! (spec constraint). Tap lifecycle, devices, listeners land in stage 3.

#![warn(clippy::undocumented_unsafe_blocks)]

pub mod error;
pub mod properties;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
