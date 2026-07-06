//! Pure-math DSP core. Zero platform dependencies (spec constraint:
//! this crate must never import CoreAudio, Tauri, or any OS API).

/// Crate version, used by the desktop app's about dialog later.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod biquad;
pub mod fr;
pub mod sweep;

/// Error type shared by parsing/validation entry points across modules.
#[derive(Debug, thiserror::Error)]
pub enum DspError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
}
