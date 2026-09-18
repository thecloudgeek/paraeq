//! Tier 3 (analytic/policy) for the per-position IR store: the derived
//! storage window at every clamp, the edge taper's invariants, a real
//! round-trip through a temporary directory, and the format refusals.

use paraeq_dsp::gating::ImpulseResponse;
use paraeq_measure::store::{
    IrStore, StoreError, StoredWindow, STORE_POST_MS, STORE_PRE_MS, STORE_TAPER_MS,
};
use std::sync::atomic::{AtomicU32, Ordering};

const RATE: u32 = 48_000;

fn samples_at(ms: f64) -> usize {
    (ms / 1000.0 * f64::from(RATE)).round() as usize
}

/// A scratch directory under the crate's own `target/`, unique to one test in
/// one process, removed when that test ends.
///
/// The directory needs no `tempfile` dependency and leaves nothing in the
/// user's tmp. Uniqueness is the load-bearing part: `CARGO_TARGET_TMPDIR` is a
/// single directory shared by every integration-test binary of this crate, and
/// the same binary is built and run twice — once per feature set (G4 and G5
/// locally, the two `cargo test` steps in CI). Under a fixed directory name
/// two concurrent runs of the same test raced: one process's
/// `remove_dir_all` deleted the WAV the other had just written, and the
/// refusal tests went red claiming the store had accepted a stereo or 16-bit
/// file. Reproduced before this guard existed — two copies of the test binary
/// in parallel, 5 failures in 24 runs across
/// `reading_a_multichannel_wav_refuses`,
/// `reading_a_non_float32_wav_refuses_rather_than_reinterpreting_it` and
/// `a_rewritten_position_replaces_rather_than_appends`.
///
/// The name carries the test's own name so a leftover directory is legible,
/// the process id so two processes cannot collide, and a counter so two calls
/// inside one process cannot either.
struct Scratch {
    path: std::path::PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("{test}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for Scratch {
    /// Best effort: a test that has already failed should not fail a second
    /// time over a directory, and the name is unique, so a leftover cannot
    /// affect any later run.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// An IR whose peak sits `peak_ms` in, with a decaying tail, so the taper has
/// something non-trivial to act on.
fn impulse(peak_ms: f64, total_ms: f64) -> ImpulseResponse {
    let peak = samples_at(peak_ms);
    let n = samples_at(total_ms);
    let samples: Vec<f64> = (0..n)
        .map(|i| {
            if i < peak {
                0.01
            } else {
                (-((i - peak) as f64) / 4800.0).exp()
            }
        })
        .collect();
    ImpulseResponse {
        peak: peak as f64,
        sample_rate: RATE,
        samples,
    }
}

// ─────────────────────────── the derived window ──────────────────────────────

#[test]
fn a_long_ir_gets_the_full_derived_window() {
    let peak = samples_at(200.0);
    let w = StoredWindow::plan(samples_at(3000.0), peak, RATE);
    assert_eq!(w.peak_in_store, samples_at(STORE_PRE_MS));
    assert_eq!(w.start, peak - samples_at(STORE_PRE_MS));
    assert_eq!(
        w.len,
        samples_at(STORE_PRE_MS) + 1 + samples_at(STORE_POST_MS)
    );
}

#[test]
fn the_pre_window_clamps_to_the_peak_index() {
    // This is the NORMAL case, not a fallback: `deconvolve` puts the IR peak
    // at only ~46-64 ms here (tap latency plus propagation), which is why
    // REW's 125 ms left window is physically impossible on this architecture.
    let peak = samples_at(50.0);
    let w = StoredWindow::plan(samples_at(3000.0), peak, RATE);
    assert_eq!(w.start, 0, "there is nothing before sample 0 to store");
    assert_eq!(w.peak_in_store, peak);
    assert!(w.peak_in_store < samples_at(STORE_PRE_MS));
}

#[test]
fn the_post_window_clamps_to_what_the_buffer_holds() {
    let peak = samples_at(200.0);
    let total = samples_at(500.0);
    let w = StoredWindow::plan(total, peak, RATE);
    assert_eq!(w.start + w.len, total, "stores to the end, invents nothing");
}

#[test]
fn a_peak_at_the_very_last_sample_still_produces_a_valid_window() {
    let total = samples_at(300.0);
    let w = StoredWindow::plan(total, total - 1, RATE);
    assert_eq!(w.len, samples_at(STORE_PRE_MS) + 1);
    assert!(w.start + w.len <= total);
}

#[test]
fn a_single_sample_ir_degenerates_without_panicking() {
    let w = StoredWindow::plan(1, 0, RATE);
    assert_eq!(w.start, 0);
    assert_eq!(w.len, 1);
    assert_eq!(w.peak_in_store, 0);
    assert_eq!(w.taper, 0);
}

#[test]
fn the_taper_can_never_reach_the_peak() {
    // The reconciliation this constant exists for: a Tukey alpha of 0.25 over
    // a 1.6 s store would be a 200 ms ramp at each end, consuming the entire
    // pre-peak region AND 100 ms past the peak. A duration-based taper,
    // clamped to half the realized pre-roll, cannot.
    for peak_ms in [1.0, 5.0, 15.0, 50.0, 100.0, 400.0] {
        let peak = samples_at(peak_ms);
        let w = StoredWindow::plan(samples_at(3000.0), peak, RATE);
        assert!(
            w.taper * 2 <= w.peak_in_store || w.peak_in_store == 0,
            "peak at {peak_ms} ms: taper {} reaches peak at {}",
            w.taper,
            w.peak_in_store
        );
        assert!(w.taper <= samples_at(STORE_TAPER_MS));
    }
}

// ───────────────────────────── round trip ────────────────────────────────────

#[test]
fn an_ir_round_trips_through_the_store() {
    let dir = Scratch::new("an_ir_round_trips_through_the_store");
    let store = IrStore::create(dir.path()).expect("create");
    let ir = impulse(200.0, 3000.0);
    let written = store.write(&ir, 3, 1).expect("write");
    let read = store.read(3, 1, written.window).expect("read");

    assert_eq!(read.sample_rate, RATE);
    assert_eq!(read.samples.len(), written.samples.len());
    assert_eq!(read.window, written.window);
    // f32 storage is the intended lossy step, and its relative error is
    // ~1e-7 — orders below the capture converter's own resolution.
    for (a, b) in written.samples.iter().zip(&read.samples) {
        assert!((a - b).abs() <= 1e-6 * a.abs().max(1e-6), "{a} vs {b}");
    }
}

#[test]
fn the_stored_peak_is_where_the_window_says_it_is() {
    let dir = Scratch::new("the_stored_peak_is_where_the_window_says_it_is");
    let store = IrStore::create(dir.path()).expect("create");
    let ir = impulse(200.0, 3000.0);
    let written = store.write(&ir, 0, 0).expect("write");
    let argmax = written
        .samples
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .map(|(i, _)| i)
        .expect("non-empty");
    assert_eq!(
        argmax, written.window.peak_in_store,
        "t=0 of the stored file must be recoverable from the window alone"
    );
}

#[test]
fn the_stored_edges_are_tapered_to_zero() {
    let dir = Scratch::new("the_stored_edges_are_tapered_to_zero");
    let store = IrStore::create(dir.path()).expect("create");
    let ir = impulse(200.0, 3000.0);
    let written = store.write(&ir, 0, 0).expect("write");
    let s = &written.samples;
    assert_eq!(s[0], 0.0, "leading edge");
    assert_eq!(s[s.len() - 1], 0.0, "trailing edge");
    // ...and the peak is untouched by it.
    let peak_value = s[written.window.peak_in_store];
    assert!(
        (peak_value - 1.0).abs() < 1e-9,
        "the taper must not touch the direct arrival, got {peak_value}"
    );
}

#[test]
fn every_position_and_channel_gets_its_own_file() {
    let dir = Scratch::new("every_position_and_channel_gets_its_own_file");
    let store = IrStore::create(dir.path()).expect("create");
    let ir = impulse(100.0, 2000.0);
    for position in 0..3 {
        for channel in 0..2 {
            store.write(&ir, position, channel).expect("write");
        }
    }
    let mut names: Vec<String> = std::fs::read_dir(store.root())
        .expect("readable")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "pos00_ch0.wav",
            "pos00_ch1.wav",
            "pos01_ch0.wav",
            "pos01_ch1.wav",
            "pos02_ch0.wav",
            "pos02_ch1.wav",
        ],
        "zero-padded so a directory listing sorts in capture order"
    );
}

#[test]
fn a_rewritten_position_replaces_rather_than_appends() {
    let dir = Scratch::new("a_rewritten_position_replaces_rather_than_appends");
    let store = IrStore::create(dir.path()).expect("create");
    let first = store.write(&impulse(100.0, 2000.0), 0, 0).expect("write");
    let second = store.write(&impulse(100.0, 1000.0), 0, 0).expect("rewrite");
    let read = store.read(0, 0, second.window).expect("read");
    assert_eq!(read.samples.len(), second.samples.len());
    assert!(second.samples.len() < first.samples.len());
}

// ───────────────────────────── refusals ──────────────────────────────────────

#[test]
fn an_empty_or_out_of_range_ir_refuses() {
    let dir = Scratch::new("an_empty_or_out_of_range_ir_refuses");
    let store = IrStore::create(dir.path()).expect("create");
    let empty = ImpulseResponse {
        peak: 0.0,
        sample_rate: RATE,
        samples: Vec::new(),
    };
    assert!(matches!(
        store.write(&empty, 0, 0),
        Err(StoreError::PeakOutOfRange { .. })
    ));
    let past_end = ImpulseResponse {
        peak: 500.0,
        sample_rate: RATE,
        samples: vec![0.0; 100],
    };
    assert!(matches!(
        store.write(&past_end, 0, 0),
        Err(StoreError::PeakOutOfRange { .. })
    ));
}

#[test]
fn reading_a_non_float32_wav_refuses_rather_than_reinterpreting_it() {
    // Misreading an IR's sample format produces a *plausible* wrong
    // correction, not an obvious failure — so the store refuses.
    let dir = Scratch::new("reading_a_non_float32_wav_refuses_rather_than_reinterpreting_it");
    let store = IrStore::create(dir.path()).expect("create");
    let path = store.path_for(0, 0);
    let spec = hound::WavSpec {
        bits_per_sample: 16,
        channels: 1,
        sample_format: hound::SampleFormat::Int,
        sample_rate: RATE,
    };
    let mut writer = hound::WavWriter::create(&path, spec).expect("create wav");
    for i in 0..100 {
        writer.write_sample(i as i16).expect("write");
    }
    writer.finalize().expect("finalize");

    let window = StoredWindow::plan(100, 10, RATE);
    assert!(matches!(
        store.read(0, 0, window),
        Err(StoreError::NotFloat32 { .. })
    ));
}

#[test]
fn reading_a_multichannel_wav_refuses() {
    let dir = Scratch::new("reading_a_multichannel_wav_refuses");
    let store = IrStore::create(dir.path()).expect("create");
    let path = store.path_for(0, 0);
    let spec = hound::WavSpec {
        bits_per_sample: 32,
        channels: 2,
        sample_format: hound::SampleFormat::Float,
        sample_rate: RATE,
    };
    let mut writer = hound::WavWriter::create(&path, spec).expect("create wav");
    for _ in 0..100 {
        writer.write_sample(0.5f32).expect("write");
    }
    writer.finalize().expect("finalize");

    let window = StoredWindow::plan(100, 10, RATE);
    assert!(matches!(
        store.read(0, 0, window),
        Err(StoreError::NotMono { .. })
    ));
}

#[test]
fn reading_a_missing_position_is_an_error() {
    let dir = Scratch::new("reading_a_missing_position_is_an_error");
    let store = IrStore::create(dir.path()).expect("create");
    let window = StoredWindow::plan(100, 10, RATE);
    assert!(matches!(
        store.read(7, 0, window),
        Err(StoreError::Wav { .. })
    ));
}

// ─────────────────────────── the size claim ──────────────────────────────────

#[test]
fn a_stored_position_costs_what_the_spec_says_it_does() {
    // The spec's storage budget is load-bearing for the reconciliation of the
    // two window figures — assert it rather than trusting arithmetic in a
    // comment. 1.6 s x 48 kHz x 4 B is ~307 KB per position per channel, so a
    // 9-position stereo room profile is ~5.5 MB.
    let dir = Scratch::new("a_stored_position_costs_what_the_spec_says_it_does");
    let store = IrStore::create(dir.path()).expect("create");
    let written = store.write(&impulse(200.0, 3000.0), 0, 0).expect("write");
    let bytes = std::fs::metadata(store.path_for(0, 0)).expect("stat").len();
    let expected = written.samples.len() as u64 * 4;
    assert!(
        bytes >= expected && bytes < expected + 200,
        "{bytes} bytes for {} samples (header aside)",
        written.samples.len()
    );
    assert!(
        (300_000..320_000).contains(&bytes),
        "a stored position should be ~307 KB, got {bytes}"
    );
}

#[test]
fn a_manifest_window_that_disagrees_with_the_file_refuses() {
    // The window comes from the manifest and the samples from the file. If they
    // disagree, `peak_in_store` points somewhere that is not the peak, and
    // every gate downstream is applied to the wrong time origin — which
    // produces a plausible correction, not a visible failure.
    let dir = Scratch::new("a_manifest_window_that_disagrees_with_the_file_refuses");
    let store = IrStore::create(dir.path()).expect("create");
    let written = store.write(&impulse(200.0, 3000.0), 0, 0).expect("write");
    let wrong = StoredWindow {
        len: written.window.len - 1,
        ..written.window
    };
    assert!(matches!(
        store.read(0, 0, wrong),
        Err(StoreError::WindowMismatch { .. })
    ));
    assert!(
        store.read(0, 0, written.window).is_ok(),
        "the true one still reads"
    );
}
