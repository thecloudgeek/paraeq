//! Realtime engine graph (source -> correction -> gain -> sink).
//! Zero Tauri dependencies (spec constraint: daemon-ready seams).

pub mod convolver;
pub mod iir;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn depends_on_dsp_crate() {
        assert_eq!(paraeq_dsp::VERSION, super::VERSION);
    }
}
