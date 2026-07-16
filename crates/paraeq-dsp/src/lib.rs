//! Pure-math DSP core. Zero platform dependencies (spec constraint:
//! this crate must never import CoreAudio, Tauri, or any OS API).

/// Crate version, used by the desktop app's about dialog later.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod autofit;
pub mod biquad;
pub mod compensation;
pub mod deconvolution;
pub mod fdw;
pub mod fir;
pub mod fr;
pub mod gating;
pub mod logf;
pub mod peq;
pub mod splice;
pub mod spline;
pub mod sweep;
pub mod targets;
pub mod window;

/// Re-exported so consumers (and this crate's integration tests) can name the
/// complex type on `logf`/`fdw`/`fr` signatures without depending on rustfft.
/// Already in the tree via realfft; `fir.rs` uses the same path.
pub use rustfft::num_complex::Complex;

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
