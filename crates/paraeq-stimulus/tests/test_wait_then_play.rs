//! R-B5: the second play path plays the VETTED bytes.
//!
//! `play()` checks the file, then the windowed level backstop, then opens the
//! device, then the channel bound, then the rate fence — in that order, and the
//! order is the point: the interlocks are in front of the thing that makes
//! sound. On an unrecognized stdin line it hands off to `wait_then_play`, which
//! used to call `wav::read` a second time and play whatever came back, with no
//! backstop check and no rate check and the device already open.
//!
//! No in-tree parent can drive that branch today, so this is defence in depth
//! rather than a live hazard — but the branch is one `Command::Unknown` away
//! from the only code in this product that plays at measurement level, and the
//! fix costs a parameter.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use paraeq_coreaudio::{MeasureError, RenderSink, StreamFormat};
use paraeq_stimulus::protocol::{Args, Command};
use paraeq_stimulus::{backstop, player, wait_then_play, wav};

const BLOCK: usize = 64;
const RATE: u32 = 48_000;

/// A scratch directory under the crate's own `target/`, unique to one test in
/// one process (the `test_wav.rs` helper): the same integration binary is built
/// and run twice — once per feature set — so a fixed name would let two
/// processes race on the same file.
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

fn write_mono_float_wav(path: &Path, samples: &[f32]) {
    let spec = hound::WavSpec {
        bits_per_sample: 32,
        channels: 1,
        sample_format: hound::SampleFormat::Float,
        sample_rate: RATE,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("a writable WAV");
    for &v in samples {
        writer.write_sample(v).expect("write");
    }
    writer.finalize().expect("finalize");
}

/// Records every sample that reached the device. No HAL, no process.
#[derive(Clone, Default)]
struct MockSink {
    written: Arc<Mutex<Vec<f32>>>,
}

impl RenderSink for MockSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: BLOCK,
            sample_rate_hz: f64::from(RATE),
        }
    }

    fn ramp_out(&mut self) {}

    fn stop(&mut self) {}

    fn write(&mut self, block: &[f32]) -> Result<(), MeasureError> {
        self.written
            .lock()
            .expect("uncontended")
            .extend_from_slice(block);
        Ok(())
    }
}

#[test]
fn the_second_play_path_plays_the_checked_wav_not_a_fresh_read_of_the_file() {
    let scratch = Scratch::new("wait-then-play");
    let path = scratch.file("verify.wav");

    // The file as the parent wrote it, and as `play()` vets it.
    let vetted_samples: Vec<f32> = (0..BLOCK * 3)
        .map(|i| 0.1 * ((i % 11) as f32 / 11.0 - 0.5))
        .collect();
    write_mono_float_wav(&path, &vetted_samples);
    let vetted = wav::read(&path).expect("the file decodes");
    backstop::check(&vetted.samples, vetted.sample_rate_hz).expect("and clears the backstop");

    // The file, swapped hot AFTER the check and BEFORE the play — the shape of
    // any second read. Full scale, so it is not a subtle difference: it is the
    // level the backstop exists to refuse.
    let hot: Vec<f32> = (0..BLOCK * 3)
        .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
        .collect();
    write_mono_float_wav(&path, &hot);
    let reread = wav::read(&path).expect("the swapped file decodes too");
    backstop::check(&reread.samples, reread.sample_rate_hz)
        .expect_err("which is exactly what the interlock would have refused");

    let sink = MockSink::default();
    let written = Arc::clone(&sink.written);
    let player = player::Player::new(Box::new(sink));

    let (tx, rx) = std::sync::mpsc::channel();
    // The line that reaches this path at all, then the decision.
    tx.send(Command::Unknown).expect("queued");
    tx.send(Command::Play).expect("queued");
    drop(tx);

    let args = Args {
        device_uid: "com.paraeq.test.output".to_owned(),
        json: false,
        routing: paraeq_coreaudio::measure_aggregate::StimulusRouting::Both,
        wav: path.clone(),
    };
    let code = wait_then_play(&args, &vetted, player, rx).expect("a clean play");
    assert_eq!(code, 0);

    let written = written.lock().expect("uncontended");
    assert_eq!(
        *written, vetted_samples,
        "the device got the bytes that were checked, not the bytes on disk"
    );
    assert!(
        written.iter().all(|v| v.abs() <= 0.1),
        "and nothing at full scale reached it"
    );
}
