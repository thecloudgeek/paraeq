//! The decoder and its two refusals.
//!
//! There is ONE WAV convention in this product — mono 32-bit float, the same
//! shape the per-position IR store writes — and these tests are the reason it
//! stays one. The 16-bit refusal is the one that matters: quantizing silently
//! moves the file's RMS, the parent's analysis differences that level against a
//! gate of a couple of dB, and a wrong level there is a plausible wrong answer
//! rather than an obvious failure.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use paraeq_stimulus::protocol::ExitCode;
use paraeq_stimulus::wav;

/// A scratch directory under the crate's own `target/`, unique to one test in
/// one process. The same integration binary is built and run twice — once per
/// feature set, locally and in CI — so a fixed name would let two processes
/// race on the same file.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("{test}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_wav(path: &Path, spec: hound::WavSpec, samples: &[f32]) {
    let mut writer = hound::WavWriter::create(path, spec).expect("a writable WAV");
    for &v in samples {
        match spec.sample_format {
            hound::SampleFormat::Float => writer.write_sample(v).expect("write"),
            hound::SampleFormat::Int => writer.write_sample((v * 32_767.0) as i16).expect("write"),
        }
    }
    writer.finalize().expect("finalize");
}

const FLOAT_MONO: hound::WavSpec = hound::WavSpec {
    bits_per_sample: 32,
    channels: 1,
    sample_format: hound::SampleFormat::Float,
    sample_rate: 48_000,
};

#[test]
fn a_mono_float32_wav_round_trips_sample_for_sample() {
    // The child half of the parent's bit-for-bit claim: the f64 -> f32 cast is
    // the parent's and happens once, in its writer, so what this side reads
    // back must be exactly what was written.
    let scratch = Scratch::new("round-trip");
    let path = scratch.file("stimulus.wav");
    let samples: Vec<f32> = (0..1024)
        .map(|i| ((i as f32) / 1024.0 - 0.5) * 1.6)
        .collect();
    write_wav(&path, FLOAT_MONO, &samples);

    let read = wav::read(&path).expect("a mono 32-bit-float file");
    assert_eq!(read.sample_rate_hz, 48_000);
    assert_eq!(read.samples, samples, "bit-for-bit, not approximately");
}

#[test]
fn a_stereo_wav_is_refused_with_exit_3() {
    // The stimulus is always mono. A stereo file is a file assembled by
    // something that thought otherwise, and reinterpreting it would play at
    // half rate on one channel rather than failing.
    let scratch = Scratch::new("stereo");
    let path = scratch.file("stereo.wav");
    write_wav(
        &path,
        hound::WavSpec {
            channels: 2,
            ..FLOAT_MONO
        },
        &[0.0; 64],
    );

    let refusal = wav::read(&path).expect_err("stereo is refused");
    assert_eq!(refusal.code, ExitCode::Wav);
    assert_eq!(refusal.code.code(), 3);
    assert!(refusal.message.contains("mono"), "{}", refusal.message);
}

#[test]
fn a_sixteen_bit_wav_is_refused_with_exit_3() {
    let scratch = Scratch::new("sixteen-bit");
    let path = scratch.file("quantized.wav");
    write_wav(
        &path,
        hound::WavSpec {
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
            ..FLOAT_MONO
        },
        &[0.25; 64],
    );

    let refusal = wav::read(&path).expect_err("16-bit is refused");
    assert_eq!(refusal.code.code(), 3);
    assert!(
        refusal.message.contains("32-bit float"),
        "{}",
        refusal.message
    );
}

#[test]
fn a_missing_file_is_exit_3_rather_than_a_panic() {
    let refusal = wav::read(Path::new("/nonexistent/paraeq/stimulus.wav"))
        .expect_err("a missing file is refused");
    assert_eq!(refusal.code.code(), 3);
}

#[test]
fn a_rate_mismatch_is_refused_and_names_which_side_disagreed() {
    // Never resampled: the parent generated the file at the device's own rate,
    // and a child that resampled would be a second, unreviewed resampler on
    // the one signal path where timing is the answer.
    wav::check_rate(48_000, 48_000.0, 48_000.0).expect("matching rates play");

    // The wrapper and its sub-device disagree — which is a real possibility,
    // because nothing establishes that a private aggregate's nominal rate
    // equals its single sub-device's. Without naming which side, this failure
    // reads as a WAV bug.
    let refusal = wav::check_rate(44_100, 48_000.0, 44_100.0).expect_err("a mismatch is refused");
    assert_eq!(refusal.code.code(), 3);
    assert!(
        refusal.message.contains("sub-device"),
        "{}",
        refusal.message
    );

    let refusal = wav::check_rate(96_000, 48_000.0, 48_000.0).expect_err("a mismatch is refused");
    assert!(refusal.message.contains("both"), "{}", refusal.message);
}
