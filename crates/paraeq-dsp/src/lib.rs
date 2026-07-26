//! Pure-math DSP core. Zero platform dependencies (spec constraint:
//! this crate must never import CoreAudio, Tauri, or any OS API).

/// Crate version, used by the desktop app's about dialog later.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod authority;
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
pub mod room;
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

/// Ordered per-channel payload. Index is the ENGINE's channel index
/// (0 = left, 1 = right for stereo). Non-empty by construction.
///
/// The mono/multi seam sits at the DSP/engine boundary — the engine is
/// already per-channel capable (`CorrectionConfig::Iir { sos_per_channel }`),
/// so this container is the DSP side of that seam. Spec:
/// docs/specs/2026-07-15-room-dsp-design.md, "`PerChannel<T>` — new, in
/// `lib.rs`".
#[derive(Clone, Debug)]
pub struct PerChannel<T>(Vec<T>);

impl<T> PerChannel<T> {
    /// Wrap an ordered channel payload. `Err` on empty: a zero-channel value
    /// has no meaning anywhere downstream, so emptiness is unrepresentable.
    pub fn new(values: Vec<T>) -> Result<Self, DspError> {
        if values.is_empty() {
            return Err(DspError::InvalidInput(
                "PerChannel needs at least 1 channel".into(),
            ));
        }
        Ok(Self(values))
    }

    /// One shared value cloned into every channel. `Err` on `channels == 0`.
    pub fn splat(value: T, channels: usize) -> Result<Self, DspError>
    where
        T: Clone,
    {
        Self::new(vec![value; channels])
    }

    pub fn channels(&self) -> usize {
        self.0.len()
    }

    pub fn get(&self, ch: usize) -> Option<&T> {
        self.0.get(ch)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }

    /// Apply `f` to every channel, preserving order and channel count.
    pub fn map<U>(&self, f: impl FnMut(&T) -> U) -> PerChannel<U> {
        PerChannel(self.0.iter().map(f).collect())
    }

    /// Fallible [`PerChannel::map`]: the first `Err` aborts and propagates.
    pub fn try_map<U, E>(&self, f: impl FnMut(&T) -> Result<U, E>) -> Result<PerChannel<U>, E> {
        Ok(PerChannel(self.0.iter().map(f).collect::<Result<_, E>>()?))
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<'a, T> IntoIterator for &'a PerChannel<T> {
    type IntoIter = std::slice::Iter<'a, T>;
    type Item = &'a T;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
