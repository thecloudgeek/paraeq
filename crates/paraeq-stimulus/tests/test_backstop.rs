//! The child's independent level interlock, its MS-4 guard, and the two
//! properties that keep both honest.
//!
//! Test tier: 3 (analytic/policy). There is no oracle for an output-level
//! interlock — it is product policy, not DSP — so what is pinned is the
//! behaviour at the ceiling, the shape of the window, and parity with the
//! parent's guard contract.

use paraeq_coreaudio::ABSOLUTE_MAX_DBFS_RMS;
use paraeq_stimulus::backstop::{check, guard_block, max_window_rms_dbfs, BACKSTOP_WINDOW_MS};
use paraeq_stimulus::protocol::ExitCode;

const RATE: u32 = 48_000;

/// A square wave at a known RMS.
///
/// A SQUARE, not a sine, and that choice is the whole reason these tests test
/// what they say they do. A square's crest factor is 1, so its peak IS its RMS
/// and a buffer at -2.9 dBFS RMS peaks at 0.715 — comfortably inside full
/// scale. A sine at the same RMS peaks at 1.012, which trips the backstop's
/// PEAK bound, so a "refused at the ceiling" test built on sines would pass
/// without ever exercising the windowed RMS it claims to pin. The two bounds
/// are independent and are tested independently.
fn square_at_dbfs_rms(dbfs: f64, seconds: f64) -> Vec<f32> {
    let amplitude = 10f64.powf(dbfs / 20.0);
    let n = (seconds * f64::from(RATE)) as usize;
    let half_period = 24; // ~1 kHz at 48 kHz
    (0..n)
        .map(|i| {
            let sign = if (i / half_period) % 2 == 0 {
                1.0
            } else {
                -1.0
            };
            (amplitude * sign) as f32
        })
        .collect()
}

#[test]
fn a_hot_wav_is_refused_before_the_device_opens() {
    // 0.1 dB over the absolute maximum. The refusal has to come from a process
    // that has opened nothing: an interlock that fires after the device is up
    // has already let the hazard exist.
    let hot = square_at_dbfs_rms(ABSOLUTE_MAX_DBFS_RMS + 0.1, 2.0);
    assert!(
        hot.iter().all(|&v| v.abs() <= 1.0),
        "this file is hot by RMS and INSIDE full scale, so only the windowed \
         bound can refuse it"
    );
    let refusal = check(&hot, RATE).expect_err("a hot file is refused");
    assert_eq!(refusal.code, ExitCode::BackstopRefused);
    assert_eq!(refusal.code.code(), 6);
    assert!(
        refusal.message.contains("window"),
        "the WINDOWED bound is what fired, not the peak: {}",
        refusal.message
    );

    // And the ORDER, through the whole program: give it a hot file AND a
    // device UID that cannot resolve. If the device were opened first the exit
    // would be 4. It is 6, which means no device was touched — the assertion
    // "before the device opens" is not a comment, it is the exit code.
    let scratch = Scratch::new("hot-before-device");
    let path = scratch.file("hot.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            bits_per_sample: 32,
            channels: 1,
            sample_format: hound::SampleFormat::Float,
            sample_rate: RATE,
        },
    )
    .expect("a writable WAV");
    for &v in &hot {
        writer.write_sample(v).expect("write");
    }
    writer.finalize().expect("finalize");

    let code = paraeq_stimulus::run(
        [
            "paraeq-stimulus",
            "--wav",
            path.to_str().expect("a utf-8 scratch path"),
            "--device-uid",
            "com.paraeq.no-such-device",
            "--channel",
            "both",
        ]
        .into_iter()
        .map(std::ffi::OsString::from),
    );
    assert_eq!(
        code, 6,
        "a hot file must be refused by a process that has opened nothing"
    );
}

/// A scratch directory under the crate's own `target/`, unique to one test in
/// one process.
///
/// Duplicated from `test_wav.rs` on purpose: each integration test is its own
/// crate, and sharing fifteen lines would cost a `tests/common/` module that
/// every file then has to `mod` in.
struct Scratch {
    path: std::path::PathBuf,
}

impl Scratch {
    fn new(test: &str) -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("{test}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch { path }
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        self.path.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn a_hot_sweep_padded_with_silence_is_still_refused() {
    // The falsifier for the window. A 5.5 s hot sweep inside a 7.7 s bracketed
    // file reads 10*log10(5.5/7.7) = -1.46 dB low on a WHOLE-FILE RMS, and by
    // arbitrarily more as the lead-in grows — so a whole-file check is an
    // interlock a caller defeats by adding silence.
    let mut padded = vec![0.0f32; (0.5 * f64::from(RATE)) as usize];
    padded.extend(square_at_dbfs_rms(ABSOLUTE_MAX_DBFS_RMS + 0.1, 5.5));
    padded.extend(std::iter::repeat_n(
        0.0f32,
        (1.7 * f64::from(RATE)) as usize,
    ));

    // The whole-file number a naive check would have used.
    let mean_square = padded
        .iter()
        .map(|&v| f64::from(v) * f64::from(v))
        .sum::<f64>()
        / padded.len() as f64;
    let whole_file_dbfs = 10.0 * mean_square.log10();
    assert!(
        whole_file_dbfs < ABSOLUTE_MAX_DBFS_RMS,
        "the padding really does hide the sweep from a whole-file RMS: {whole_file_dbfs:.3} dBFS"
    );

    let refusal = check(&padded, RATE).expect_err("the window sees the sweep the padding hides");
    assert_eq!(refusal.code, ExitCode::BackstopRefused);
    assert!(
        refusal.message.contains("window"),
        "the WINDOWED bound is what fired: {}",
        refusal.message
    );
}

#[test]
fn the_backstop_refuses_a_tenth_of_a_db_over_the_parents_ceiling_and_plays_a_tenth_under() {
    // Behavioural, and computed FROM the re-exported constant rather than from
    // a copy. Two locally-declared constants kept in step by hand would pass an
    // equality assertion right up to the moment someone forgot; a bound derived
    // from the parent's number moves with it by construction.
    let hot = square_at_dbfs_rms(ABSOLUTE_MAX_DBFS_RMS + 0.1, 2.0);
    let cool = square_at_dbfs_rms(ABSOLUTE_MAX_DBFS_RMS - 0.1, 2.0);
    // Both are inside full scale, so the peak bound cannot decide either one.
    assert!(hot.iter().chain(&cool).all(|&v| v.abs() <= 1.0));

    let refusal = check(&hot, RATE).expect_err("above the ceiling");
    assert_eq!(refusal.code.code(), 6);
    assert!(refusal.message.contains("window"), "{}", refusal.message);
    check(&cool, RATE).expect("below the ceiling, so it plays");
}

/// The sum `max_window_rms_dbfs` folds, in the same order it folds it — so a
/// buffer can be built to land on a chosen value of it. Written as a fold over
/// the samples rather than `count × square` because f64 addition is not
/// associative and the two answers differ in the last bits, which is the only
/// part of the answer this file's boundary test is about.
fn sum_of_squares(buf: &[f32]) -> f64 {
    buf.iter().map(|&v| f64::from(v) * f64::from(v)).sum()
}

/// A buffer whose max-window mean square is EXACTLY `mean_square`, bit for bit
/// — and so whose measured dBFS RMS is exactly what that mean square reads as.
///
/// [`square_at_dbfs_rms`] cannot do this and no amplitude can: one f32
/// amplitude quantises the mean square to ~1.2e-7 relative, i.e. ±2.5e-7 dB,
/// which is five orders of magnitude too coarse to sit ON a ceiling instead of
/// near it. So all but the last few samples are the square wave, and those few
/// are TRIMMED to carry the shortfall — each one is the square root of what is
/// still missing, so the error falls by about 1e-7 per trimmed sample and two
/// of them reach the f64 floor of the sum itself. They are at the end, where
/// they cannot disturb the partial sums before them.
///
/// The length is a power of two so the `sum / len` inside the measurement is
/// exact, which makes "the sum hit its target" and "the mean hit its target"
/// the same statement; and it is under one window, so the file is measured
/// whole and there is exactly one window to land.
fn square_with_exact_mean_square(mean_square: f64) -> Vec<f32> {
    const LEN: usize = 4096;
    const TRIMMED: usize = 6;
    let target_sum = mean_square * LEN as f64; // exact: LEN is a power of two
    let base_len = LEN - TRIMMED;
    // Rounded DOWN until the untrimmed part is under the target: a trimmed
    // sample can only ADD, so an overshooting base could never be walked back.
    let mut amplitude = (target_sum / base_len as f64).sqrt() as f32;
    while sum_of_squares(&vec![amplitude; base_len]) > target_sum {
        amplitude = f32::from_bits(amplitude.to_bits() - 1);
    }
    let mut buf: Vec<f32> = (0..LEN)
        .map(|i| {
            if i >= base_len {
                0.0
            } else if (i / 24) % 2 == 0 {
                amplitude
            } else {
                -amplitude
            }
        })
        .collect();
    for slot in base_len..LEN {
        let sum = sum_of_squares(&buf);
        if sum == target_sum {
            break;
        }
        let mut sample = (target_sum - sum).sqrt() as f32;
        buf[slot] = sample;
        // `as f32` rounds to nearest and can overshoot; step down until this
        // slot is under the target again and let the next one make up the rest.
        while sum_of_squares(&buf) > target_sum {
            sample = f32::from_bits(sample.to_bits() - 1);
            buf[slot] = sample;
        }
    }
    buf
}

#[test]
fn the_hottest_level_that_is_not_above_the_ceiling_still_plays() {
    // The boundary itself, at the finest resolution the number has. The test
    // above straddles the ceiling by a tenth of a dB, which pins that the
    // comparison is in the right place but says nothing about which side of it
    // a file sitting ON the ceiling falls.
    //
    // **The ceiling cannot be hit exactly, and that is arithmetic, not a gap
    // in this test.** `max_window_rms_dbfs` answers `10 * mean_square.log10()`,
    // and no f64 mean square makes that expression return exactly -3.0: the
    // reachable values either side are -3.0000000000000004 and
    // -2.9999999999999991, because one f64 step in the mean square moves the
    // answer by about two f64 steps (checked by walking 4096 steps either way).
    // So `loudest > ceiling` and `loudest >= ceiling` accept and refuse exactly
    // the same files. What is pinned here is therefore the tightest statement
    // that HAS a witness — the last level below the ceiling plays and the first
    // level above it is refused — and a mutation run that flips that comparison
    // is looking at an equivalent mutant, not at a missing test.
    let target = 10f64.powf(ABSOLUTE_MAX_DBFS_RMS / 10.0);
    let at_ceiling = square_with_exact_mean_square(target);
    // Neither of the other two bounds can decide this file: every sample is
    // finite, and a square's crest factor of 1 keeps the peak at its RMS.
    assert!(at_ceiling.iter().all(|v| v.is_finite() && v.abs() <= 1.0));

    let loudest = max_window_rms_dbfs(&at_ceiling, RATE);
    // One step UP in the bit pattern of a negative f64 is one step DOWN in
    // value, so this is the largest level that is still not above the ceiling.
    assert_eq!(
        loudest.to_bits(),
        ABSOLUTE_MAX_DBFS_RMS.to_bits() + 1,
        "the file must sit on the last f64 step below the ceiling rather than \
         near it, got {loudest:.17} dBFS RMS"
    );
    assert!(
        ABSOLUTE_MAX_DBFS_RMS - loudest < 1e-12,
        "which is {} dB under the ceiling",
        ABSOLUTE_MAX_DBFS_RMS - loudest
    );
    check(&at_ceiling, RATE).expect("at the ceiling is not ABOVE it, so it plays");

    // And the very next level the type can express IS above the ceiling — by
    // under 1e-12 dB — and is refused, by the WINDOWED bound rather than by
    // the peak or the non-finite one.
    let above = square_with_exact_mean_square(f64::from_bits(target.to_bits() + 1));
    assert!(above.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
    let hotter = max_window_rms_dbfs(&above, RATE);
    assert!(
        hotter > ABSOLUTE_MAX_DBFS_RMS && hotter - ABSOLUTE_MAX_DBFS_RMS < 1e-12,
        "one f64 step over the ceiling, got {hotter:.17} dBFS RMS"
    );
    let refusal = check(&above, RATE).expect_err("above the ceiling");
    assert_eq!(refusal.code.code(), 6);
    assert!(
        refusal.message.contains("window"),
        "the WINDOWED bound is what fired: {}",
        refusal.message
    );
}

#[test]
fn a_file_shorter_than_one_window_is_measured_whole_rather_than_skipped() {
    // A short file is still a file that can be hot, and "no complete window"
    // must not read as "nothing to check".
    let short_seconds = BACKSTOP_WINDOW_MS / 1000.0 / 4.0;
    let hot = square_at_dbfs_rms(ABSOLUTE_MAX_DBFS_RMS + 3.0, short_seconds);
    assert!(hot.iter().all(|&v| v.abs() <= 1.0));
    assert!(check(&hot, RATE).is_err());
}

#[test]
fn a_full_scale_transient_is_refused_even_at_a_quiet_average() {
    // The two bounds fail differently: the windowed RMS catches a file levelled
    // too hot, and the peak catches one whose average is fine but which
    // contains a full-scale sample.
    let mut quiet = square_at_dbfs_rms(-40.0, 2.0);
    quiet[1000] = 1.5;
    let refusal = check(&quiet, RATE).expect_err("a transient over full scale is refused");
    assert!(refusal.message.contains("peak"), "{}", refusal.message);
}

/// Both level bounds are NaN-BLIND, which is what makes this a refusal rather
/// than a nit.
///
/// A window holding one NaN has a NaN `mean_square`, and `NaN > 0.0` is false,
/// so the window is SKIPPED rather than failing. The peak fold drops non-finite
/// samples the same way. Salt a hot file with one NaN per 400 ms hop and it
/// clears both bounds while carrying full-scale content — the exact file this
/// interlock exists to stop, waved through by the interlock itself.
#[test]
fn a_file_with_a_nan_is_refused_rather_than_skipped() {
    // Full scale throughout: this file is as hot as a file gets.
    let mut hot = vec![1.0f32; RATE as usize];
    hot[7] = f32::NAN;
    let refusal = check(&hot, RATE).expect_err("a non-finite sample is refused");
    assert_eq!(refusal.code, ExitCode::BackstopRefused);
    assert!(
        refusal.message.contains("not finite"),
        "the refusal must name the reason: {}",
        refusal.message
    );

    // The falsifier for the old behaviour: hide a NaN in EVERY window and the
    // windowed RMS sees nothing at all, while the peak fold skips it too.
    let mut salted = vec![1.0f32; RATE as usize];
    let hop = (BACKSTOP_WINDOW_MS / 1000.0 * f64::from(RATE)) as usize;
    for i in (0..salted.len()).step_by(hop.max(1)) {
        salted[i] = f32::INFINITY;
    }
    check(&salted, RATE).expect_err("and an infinity is refused on the same rung");
}

#[test]
fn silence_is_accepted_and_reads_as_negative_infinity() {
    let silence = vec![0.0f32; RATE as usize];
    assert_eq!(max_window_rms_dbfs(&silence, RATE), f64::NEG_INFINITY);
    check(&silence, RATE).expect("silence is not hot");
    assert_eq!(max_window_rms_dbfs(&[], RATE), f64::NEG_INFINITY);
}

#[test]
fn the_childs_f32_guard_matches_the_parents_f64_guard_case_for_case() {
    // The anti-drift device for the guard is this test, not an import: the
    // parent's `emit_guard` takes `&mut [f64]` and cannot take this crate's
    // blocks, so it is the wrong type at every possible dependency
    // arrangement.
    //
    // The NaN case is FIRST because the ordering is the constraint:
    // `NaN.clamp(-1.0, 1.0)` is NaN by documented Rust behaviour, so
    // finiteness must be tested before the clamp. A guard that clamped first
    // would leave a NaN in the buffer and count it as clamped.
    let mut block = [f32::NAN, f32::INFINITY, 1.5, -1.5, 0.25];
    let (clamped, sanitized) = guard_block(&mut block);

    assert_eq!(block[0], 0.0, "NaN is ZEROED, not clamped");
    assert_eq!(
        block[1], 0.0,
        "+inf is zeroed too: it is a defect, not a level"
    );
    assert_eq!(
        block[2], 1.0,
        "a finite sample over full scale is clamped in"
    );
    assert_eq!(block[3], -1.0);
    assert_eq!(block[4], 0.25, "a clean sample is untouched");
    assert_eq!(sanitized, 2, "NaN and +inf");
    assert_eq!(clamped, 2, "+1.5 and -1.5");
}

#[test]
fn a_clean_block_reports_no_counts() {
    // The guard is belt-and-braces, not a processing stage: a verified stimulus
    // produces zeros, and a nonzero count is what the parent turns into a
    // warning.
    let mut block = [0.0f32, 0.5, -0.5, 1.0, -1.0];
    assert_eq!(guard_block(&mut block), (0, 0));
    assert_eq!(block, [0.0, 0.5, -0.5, 1.0, -1.0]);
}

#[test]
fn the_backstop_header_names_no_forbidden_identifier() {
    // The child's docs point at the parent's FILES, never at its TYPES. Naming
    // a file is the anti-drift pointer — it says where the single definition
    // lives — while naming a type is the beginning of a second copy of the
    // level book in a process that must not have one.
    //
    // This runs the same identifier list CI greps for, under `cargo test`, so
    // the rule fails locally rather than at the end of a CI queue. It does NOT
    // check for parent file paths: those are expected and deliberate.
    const FORBIDDEN: [&str; 6] = [
        "SweepLevel",
        "TransducerClass",
        "caps_for",
        "LevelLadder",
        "MeasurementSession",
        "SPL",
    ];
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(src_dir).expect("src/ is readable") {
        let path = entry.expect("a readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("a readable source file");
        for (i, line) in src.lines().enumerate() {
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    hits.push(format!("{}:{} {needle}", path.display(), i + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the helper names the parent's level policy: {hits:?}"
    );
}

#[test]
fn the_helper_exposes_no_buffer_size_or_rate_setter() {
    // `set_buffer_frame_size` is device-GLOBAL. Calling it would renegotiate
    // the engine's own geometry mid-verification — a fault injection, not a
    // configuration — and the engine under test is on the same device.
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    for entry in std::fs::read_dir(src_dir).expect("src/ is readable") {
        let path = entry.expect("a readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("a readable source file");
        let code: String = src
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in ["set_buffer_frame_size", "set_nominal_sample_rate"] {
            assert!(
                !code.contains(forbidden),
                "{} calls {forbidden}",
                path.display()
            );
        }
    }
}
