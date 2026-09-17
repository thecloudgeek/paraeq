//! The child's decoder, and the only place this process reads a file.
//!
//! **Mono 32-bit float, and nothing else is accepted.** The refusals mirror the
//! per-position IR store's (`crates/paraeq-measure/src/store.rs`) in shape,
//! because there is one WAV convention in this product and not two. The 16-bit
//! refusal is the one that matters: quantizing silently moves the file's RMS,
//! and the parent's analysis differences that level against a gate of a couple
//! of dB. A wrong level there produces a plausible wrong answer rather than an
//! obvious failure.
//!
//! **The child never casts and never scales.** The single f64 -> f32 cast on
//! this path is the parent's, in its writer; this side reads f32 and hands f32
//! blocks onward.

use std::path::Path;

use crate::protocol::{ExitCode, Refusal};

/// A decoded stimulus file.
///
/// `Debug` prints the sample COUNT, not the samples: a stimulus is a quarter
/// of a million floats and a test assertion that dumped them would be unusable.
pub struct Wav {
    pub sample_rate_hz: u32,
    pub samples: Vec<f32>,
}

impl std::fmt::Debug for Wav {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wav")
            .field("sample_rate_hz", &self.sample_rate_hz)
            .field("samples", &self.samples.len())
            .finish()
    }
}

/// Read `path`, refusing anything that is not mono 32-bit float.
pub fn read(path: &Path) -> Result<Wav, Refusal> {
    let wav = |message: String| Refusal::new(ExitCode::Wav, message);
    let display = path.display();

    let mut reader = hound::WavReader::open(path)
        .map_err(|source| wav(format!("cannot open '{display}': {source}")))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(wav(format!(
            "'{display}': expected mono, got {} channels",
            spec.channels
        )));
    }
    if spec.bits_per_sample != 32 || spec.sample_format != hound::SampleFormat::Float {
        return Err(wav(format!(
            "'{display}': expected 32-bit float, got {} bit {:?}",
            spec.bits_per_sample, spec.sample_format
        )));
    }
    let samples: Result<Vec<f32>, hound::Error> = reader.samples::<f32>().collect();
    let samples = samples.map_err(|source| wav(format!("'{display}': {source}")))?;

    Ok(Wav {
        sample_rate_hz: spec.sample_rate,
        samples,
    })
}

/// Refuse a file whose rate disagrees with the device's.
///
/// NEVER resampled. The parent generated this file at the device's own rate; a
/// child that resampled would be a second, unreviewed resampler on the one
/// signal path where timing is the answer.
///
/// The message names WHICH rate disagreed, because there are two on the device
/// side — the render wrapper's and its sole sub-device's — and nothing
/// establishes that a private aggregate's nominal rate equals its sub-device's.
/// Without that, a mismatch reads as a WAV bug.
pub fn check_rate(
    wav_rate_hz: u32,
    render_rate_hz: f64,
    sub_device_rate_hz: f64,
) -> Result<(), Refusal> {
    let wav_rate = f64::from(wav_rate_hz);
    if (wav_rate - render_rate_hz).abs() <= 0.5 {
        return Ok(());
    }
    let which = if (wav_rate - sub_device_rate_hz).abs() <= 0.5 {
        "the render device disagrees with its own sub-device"
    } else {
        "the WAV disagrees with both"
    };
    Err(Refusal::new(
        ExitCode::Wav,
        format!(
            "rate mismatch: WAV {wav_rate} Hz, render device {render_rate_hz} Hz, \
             sub-device {sub_device_rate_hz} Hz — {which}"
        ),
    ))
}
