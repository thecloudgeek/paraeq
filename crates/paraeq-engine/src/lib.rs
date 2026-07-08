//! Realtime engine graph (source -> correction -> gain -> sink).
//! Zero Tauri dependencies (spec constraint: daemon-ready seams).

pub mod chain;
pub mod convolver;
pub mod iir;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Engine-level errors (control plane only; the realtime path never errors,
/// it degrades -- see [`chain::RealtimeChain::process`]).
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("backend: {0}")]
    Backend(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
}

#[cfg(test)]
mod tests {
    #[test]
    fn depends_on_dsp_crate() {
        assert_eq!(paraeq_dsp::VERSION, super::VERSION);
    }
}
