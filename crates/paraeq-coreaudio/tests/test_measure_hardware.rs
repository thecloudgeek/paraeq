//! MS-22 hardware suite: the measurement aggregate (default output + named
//! mic, drift-compensated) and the two-clock experiment harness
//! (measurement-suite/9). Every test is `#[ignore]` because CI has no audio
//! devices; the two-clock tests are the experiment every spec calls the
//! biggest unpriced risk, and they print structured results for the owner to
//! read rather than asserting on unknown physics.
//!
//! Run on the rig (TCC-granted terminal — MICROPHONE grant required, unlike
//! the tap suite) with `--release` (the matched filter is O(N·M) and slow in
//! debug):
//!
//! ```sh
//! cargo test -p paraeq-coreaudio --release --test test_measure_hardware -- --ignored --nocapture
//! ```
//!
//! Mic selection: set `PARAEQ_MIC_UID=<device UID>` to name the UMIK-1 (find
//! UIDs via `system_profiler SPAudioDataType` or the lifecycle test's
//! printout); unset, the system default input is used.
//!
//! Two-clock stimulus design (documented per the task): a timing marker is a
//! 50 ms Hann-windowed 2→8 kHz linear chirp (`two_clock::default_marker`)
//! whose matched filter compresses to a sub-millisecond pulse.
//!
//! - **Marker train**: 0.5 s lead-in, then 6 markers at exactly 1.0 s
//!   spacing (a 5.0 s span — the length the skew evidence stops at), 0.5 s
//!   tail. The 6 arrival times vs the 6 known playback positions give a
//!   least-squares clock ratio (skew, ppm) AND per-marker residuals (the
//!   t=0 jitter figure).
//! - **Bracketed sweep** (the REW-shaped configuration, decision doc §Q6):
//!   0.5 s lead-in, marker, 0.25 s gap, 5 s 20 Hz–20 kHz log sweep at 0.25
//!   peak (faded 10 ms in/out), 0.25 s gap, closing marker, 0.5 s tail.
//!   Skew from the two-point bracket, marker SNR measured against sweep
//!   spill and room noise.
//!
//! Playback is `afplay` of a generated 16-bit WAV at the aggregate's rate
//! (no resampling in the player), per the existing hardware-test shape.
//! Raw-skew runs create the aggregate with drift compensation OFF so the
//! capture stays on the mic's own clock; the compensated run repeats the
//! train with drift ON, where the printed ppm is the HAL's RESIDUAL — the
//! escalation path's option (3) measured against option (2)'s input data.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use paraeq_coreaudio::measure_aggregate::{
    MeasureAggregateConfig, MeasureAggregateError, MicCapture, MicSelector,
};
use paraeq_coreaudio::two_clock::{self, SkewEstimate};
use paraeq_measure::CaptureSource;

/// The marker-train span (5 s — exactly the length the existing skew evidence
/// covers, per the safety spec).
const TRAIN_MARKERS: usize = 6;
const TRAIN_INTERVAL_S: f64 = 1.0;

fn mic_selector_from_env() -> MicSelector {
    match std::env::var("PARAEQ_MIC_UID") {
        Ok(uid) if !uid.is_empty() => MicSelector::Uid(uid),
        _ => MicSelector::DefaultInput,
    }
}

/// A generated stimulus plus where its markers sit (playback-clock samples).
struct Stimulus {
    marker: Vec<f64>,
    marker_starts: Vec<usize>,
    samples: Vec<f64>,
}

impl Stimulus {
    fn duration_s(&self, rate: f64) -> f64 {
        self.samples.len() as f64 / rate
    }

    /// Half the smallest marker spacing — the matched-filter exclusion zone.
    fn min_separation(&self) -> usize {
        self.marker_starts
            .windows(2)
            .map(|w| w[1] - w[0])
            .min()
            .expect("at least two markers")
            / 2
    }
}

/// Lead-in, then `TRAIN_MARKERS` markers at `TRAIN_INTERVAL_S`, then tail.
fn marker_train_stimulus(rate: f64) -> Stimulus {
    let marker = two_clock::default_marker(rate);
    let lead = (0.5 * rate) as usize;
    let interval = (TRAIN_INTERVAL_S * rate) as usize;
    let tail = (0.5 * rate) as usize;
    let mut samples = vec![0.0; lead + (TRAIN_MARKERS - 1) * interval + marker.len() + tail];
    let mut marker_starts = Vec::with_capacity(TRAIN_MARKERS);
    for k in 0..TRAIN_MARKERS {
        let start = lead + k * interval;
        embed(&mut samples, &marker, start, 0.5);
        marker_starts.push(start);
    }
    Stimulus {
        marker,
        marker_starts,
        samples,
    }
}

/// Lead-in, marker, gap, 5 s log sweep, gap, closing marker, tail.
fn bracketed_sweep_stimulus(rate: f64) -> Stimulus {
    let marker = two_clock::default_marker(rate);
    let lead = (0.5 * rate) as usize;
    let gap = (0.25 * rate) as usize;
    let tail = (0.5 * rate) as usize;
    let fade = (0.01 * rate) as usize;

    let mut sweep = paraeq_dsp::sweep::generate_sweep(5.0, rate as u32, 20.0, 20_000.0);
    paraeq_dsp::sweep::apply_fade(&mut sweep, fade, fade);
    for s in &mut sweep {
        *s *= 0.25;
    }

    let open = lead;
    let close = lead + marker.len() + gap + sweep.len() + gap;
    let mut samples = vec![0.0; close + marker.len() + tail];
    embed(&mut samples, &marker, open, 0.5);
    embed(&mut samples, &marker, close, 0.5);
    let sweep_at = lead + marker.len() + gap;
    for (i, &s) in sweep.iter().enumerate() {
        samples[sweep_at + i] = s;
    }
    Stimulus {
        marker,
        marker_starts: vec![open, close],
        samples,
    }
}

fn embed(signal: &mut [f64], marker: &[f64], at: usize, gain: f64) {
    for (i, &m) in marker.iter().enumerate() {
        signal[at + i] += gain * m;
    }
}

/// Minimal 16-bit PCM mono WAV writer (kept dependency-free on purpose:
/// `hound` is not in the tree and a 44-byte canonical header needs no crate).
fn write_wav_mono16(path: &std::path::Path, sample_rate: u32, samples: &[f64]) {
    let data_bytes = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write stimulus wav");
}

fn scratch_wav_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "paraeq-two-clock-{label}-{}.wav",
        std::process::id()
    ))
}

/// Drain `cap` into a growing buffer for `seconds` of wall time.
fn capture_for(cap: &mut MicCapture, seconds: f64) -> Vec<f64> {
    let mut collected = Vec::new();
    let mut block = vec![0.0f64; 4800];
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        let n = cap.capture(&mut block).expect("capture");
        collected.extend_from_slice(&block[..n]);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    collected
}

/// One full two-clock run: create the aggregate (drift per `drift_comp`),
/// play `stimulus` through afplay on the default output, capture on the mic,
/// locate the markers, fit the skew, and PRINT structured results. Panics
/// only on harness failures (no markers, no callbacks), never on the
/// physics — the numbers are the experiment's output, not its precondition.
fn run_two_clock(label: &str, drift_comp: bool, build: fn(f64) -> Stimulus) {
    let config = MeasureAggregateConfig {
        drift_compensation: drift_comp,
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    };
    let mut cap = MicCapture::create(config).expect("MicCapture::create");
    let rate = cap.sample_rate_hz();
    eprintln!(
        "TWO-CLOCK[{label}]: aggregate rate {rate} Hz, mic '{}' nominal {} Hz, drift_comp={}",
        cap.mic_uid(),
        cap.mic_nominal_rate_hz(),
        cap.drift_compensation(),
    );
    if (rate - cap.mic_nominal_rate_hz()).abs() > 0.5 {
        eprintln!(
            "TWO-CLOCK[{label}]: RATE-MISMATCH between aggregate and mic nominal rates — \
             rate-reconciliation data point (see safety spec § The Two-Clock Complication)"
        );
    }

    let stimulus = build(rate);
    let wav = scratch_wav_path(label);
    write_wav_mono16(&wav, rate as u32, &stimulus.samples);

    // Capture brackets the playback: start draining first (lead-in included),
    // give afplay startup + full stimulus + margin.
    let mut player = std::process::Command::new("afplay")
        .arg(&wav)
        .spawn()
        .expect("spawn afplay");
    let capture = capture_for(&mut cap, stimulus.duration_s(rate) + 4.0);
    let player_status = player.wait().expect("afplay wait");
    assert!(player_status.success(), "afplay failed: {player_status}");
    let _ = std::fs::remove_file(&wav);

    let counters = cap.counters();
    eprintln!(
        "TWO-CLOCK[{label}]: counters: callbacks={} dropped={} invalid={} captured_frames={}",
        counters.callbacks,
        counters.dropped_samples,
        counters.invalid_samples,
        capture.len(),
    );
    assert!(counters.callbacks > 0, "IOProc never engaged");
    if counters.dropped_samples > 0 {
        eprintln!(
            "TWO-CLOCK[{label}]: WARNING: {} dropped samples — the capture timeline has holes; \
             rerun before trusting the numbers",
            counters.dropped_samples
        );
    }
    cap.stop().expect("stop");

    let hits = two_clock::find_marker_train(
        &capture,
        &stimulus.marker,
        stimulus.marker_starts.len(),
        stimulus.min_separation(),
    )
    .expect(
        "markers not found in the capture — check mic placement/level and that the \
         default output is audible to the mic",
    );
    let snrs: Vec<String> = hits.iter().map(|h| format!("{:.1}", h.snr_db)).collect();
    eprintln!("TWO-CLOCK[{label}]: marker SNR dB = [{}]", snrs.join(", "));

    let expected: Vec<f64> = stimulus.marker_starts.iter().map(|&s| s as f64).collect();
    let measured: Vec<f64> = hits.iter().map(|h| h.position).collect();
    let est: SkewEstimate =
        two_clock::estimate_skew(&expected, &measured).expect("skew fit (>= 2 markers)");
    let ms_per_sample = 1_000.0 / rate;
    eprintln!(
        "TWO-CLOCK[{label}]: skew = {:+.3} ppm (capture clock vs playback clock; \
         drift_comp={} means this is the {})",
        est.skew_ppm,
        drift_comp,
        if drift_comp {
            "HAL's residual after drift compensation"
        } else {
            "raw mic-vs-output clock skew"
        },
    );
    eprintln!(
        "TWO-CLOCK[{label}]: t=0 residual rms = {:.3} samples ({:.4} ms), \
         peak = {:.3} samples ({:.4} ms), transport offset = {:.1} samples",
        est.residual_rms_samples,
        est.residual_rms_samples * ms_per_sample,
        est.residual_peak_samples,
        est.residual_peak_samples * ms_per_sample,
        est.intercept_samples,
    );
    eprintln!(
        "TWO-CLOCK[{label}]: per-marker residuals (samples) = {:?}",
        est.residuals_samples
            .iter()
            .map(|r| (r * 1_000.0).round() / 1_000.0)
            .collect::<Vec<_>>(),
    );

    // Sanity only — the magnitude itself is the data being gathered. REW's
    // typical figure is ~12 ppm; 5000 ppm (0.5%) means the harness, not the
    // clocks, is broken.
    assert!(
        est.skew_ppm.abs() < 5_000.0,
        "implausible skew {} ppm — harness fault, not clock data",
        est.skew_ppm
    );
}

/// Lifecycle roundtrip: create (drift-compensated, MS-22 default), formats
/// sane, capture flows, teardown idempotent, capture-after-stop refuses, and
/// a second create works. Needs an input device and the mic TCC grant (a
/// denied grant may deliver zeros — this test asserts flow, not content).
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant"]
fn measure_aggregate_lifecycle_roundtrip() {
    let mut cap = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("MicCapture::create");

    let format = cap.format();
    eprintln!(
        "LIFECYCLE: mic '{}' (nominal {} Hz), aggregate {} Hz, {} frames/block",
        cap.mic_uid(),
        cap.mic_nominal_rate_hz(),
        format.sample_rate_hz,
        format.frames_per_block,
    );
    assert_eq!(format.channels, 1, "mono capture (module contract)");
    assert!(format.sample_rate_hz > 0.0, "sane aggregate rate");
    assert!(format.frames_per_block > 0, "sane buffer frame size");
    assert!(!cap.mic_uid().is_empty(), "mic UID captured");

    // Capture must flow within 10 s (frames counted regardless of content —
    // TCC silent-zeros still delivers frames).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut block = vec![0.0f64; 4800];
    let mut total = 0usize;
    while total == 0 && Instant::now() < deadline {
        total += cap.capture(&mut block).expect("capture");
        if total == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let counters = cap.counters();
    eprintln!(
        "LIFECYCLE: {} frames captured, counters: callbacks={} dropped={} invalid={}",
        total, counters.callbacks, counters.dropped_samples, counters.invalid_samples,
    );
    assert!(total > 0, "no capture frames within 10 s");
    assert!(counters.callbacks > 0, "IOProc never engaged");
    assert_eq!(
        counters.invalid_samples, 0,
        "a real mic must not deliver non-finite samples"
    );

    cap.stop().expect("first stop");
    cap.stop().expect("second stop must be an idempotent no-op");
    assert!(
        cap.capture(&mut block).is_err(),
        "capture after stop must refuse"
    );
    drop(cap);

    // A fresh create -> drop cycle must also work (Drop runs the teardown).
    let cap2 = MicCapture::create(MeasureAggregateConfig::default()).expect("second create");
    assert!(cap2.sample_rate_hz() > 0.0);
    drop(cap2);
}

/// MS-22 refusal: a UID naming no device must be a structured `MicNotFound`,
/// never a degraded aggregate.
#[test]
#[ignore = "requires audio hardware"]
fn measure_aggregate_refuses_unknown_mic_uid() {
    let config = MeasureAggregateConfig {
        mic: MicSelector::Uid("com.paraeq.no-such-device".into()),
        ..MeasureAggregateConfig::default()
    };
    match MicCapture::create(config) {
        Err(MeasureAggregateError::MicNotFound { uid }) => {
            assert_eq!(uid, "com.paraeq.no-such-device");
        }
        Err(other) => panic!("expected MicNotFound, got {other:?}"),
        Ok(_) => panic!("expected MicNotFound, got a live aggregate"),
    }
}

/// measurement-suite/9, part 1: RAW skew — drift compensation OFF, so the
/// capture stays on the mic's own clock. 6 markers across a 5 s span give
/// the ppm figure and the t=0 jitter the specs mark [NEEDS DATA].
#[test]
#[ignore = "requires audio hardware + mic TCC grant; plays audio; run with --release"]
fn two_clock_raw_skew_marker_train() {
    run_two_clock("raw-markers", false, marker_train_stimulus);
}

/// measurement-suite/9, part 2: the SAME train with drift compensation ON
/// (the MS-22 product topology). The printed ppm is the HAL's residual —
/// near zero means the aggregate really does put both devices in one clock
/// domain; the residual jitter feeds the open IR-perturbation question.
#[test]
#[ignore = "requires audio hardware + mic TCC grant; plays audio; run with --release"]
fn two_clock_drift_compensated_residual() {
    run_two_clock("drift-comp", true, marker_train_stimulus);
}

/// measurement-suite/9, part 3: the REW-shaped bracket (decision doc §Q6) —
/// marker, 5 s log sweep, closing marker, drift OFF. Two-point skew plus
/// marker SNR under real sweep-adjacent conditions.
#[test]
#[ignore = "requires audio hardware + mic TCC grant; plays audio (5 s sweep); run with --release"]
fn two_clock_bracketed_sweep_skew() {
    run_two_clock("bracketed-sweep", false, bracketed_sweep_stimulus);
}

// ───────────────────── Stage 5: the one-clock stimulus sink ──────────────────
// The Stage-4 aggregate reserved its output route for this; these exercise it
// on real hardware. Together they are the software side of the stage's end
// state — "a complete coupler measurement is testable end-to-end, headless".
//
// These three run WITHOUT a session or an engine on purpose: they isolate the
// sink (it plays, it paces, play and record share one clock) from everything
// the session layers on top. The full run — a live `TapBackend`, its MS-6
// witness, and a `MeasurementSession` driven begin → solve → acknowledge →
// sweep against it — is
// `a_full_measurement_session_runs_against_the_live_witness` at the bottom of
// this file. The gap the previous comment here described (no `TapStatus`
// witness, `TapSystem` not exposing `self_excluded`) closed with the
// 2026-09-16 integration work: `TapSystem::self_excluded`,
// `TapBackend::exclusion_witness` and `EngineState::self_excluded` all ship.

/// **Owner: listen.** Plays the 300 Hz pilot at its fixed −40 dBFS RMS through
/// the aggregate's own output route and captures it back on the mic, then
/// reports the measured level and the realtime counters.
///
/// This is the first sound ParaEQ has ever emitted through the level
/// interlock, so it emits the quietest thing the design has: the pilot's level
/// is fixed policy, `assemble_pilot` takes no level parameter, and a hot pilot
/// is unrepresentable.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant"]
fn pilot_plays_through_the_aggregate_and_comes_back_on_the_mic() {
    use paraeq_measure::{assemble_pilot, TransducerClass};

    let mut cap = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("MicCapture::create");
    let rate = cap.sample_rate_hz();
    let mut sink = cap
        .take_stimulus_sink()
        .expect("the sink is available once");
    assert!(
        cap.take_stimulus_sink().is_none(),
        "and only once — two producers on one SPSC ring is not a thing"
    );

    let pilot =
        assemble_pilot(1.0, rate as u32, TransducerClass::OverEar).expect("pilot assembles");
    eprintln!(
        "pilot: {} samples at {} dBFS RMS, aggregate rate {rate} Hz",
        pilot.len(),
        pilot.level().dbfs_rms()
    );

    // Drain whatever the mic buffered before playback starts.
    let mut scratch = vec![0.0f64; 4096];
    while cap.capture(&mut scratch).expect("capture") > 0 {}

    let started = Instant::now();
    pilot.emit_to(&mut sink).expect("emit");
    let elapsed = started.elapsed();

    let mut captured = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        let n = cap.capture(&mut scratch).expect("capture");
        captured.extend_from_slice(&scratch[..n]);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let rms = (captured.iter().map(|v| v * v).sum::<f64>() / captured.len().max(1) as f64).sqrt();
    let counters = sink.counters();
    eprintln!(
        "emit took {elapsed:?} for {:.3} s of audio (pacing: the sink must NOT return early)",
        pilot.duration_s()
    );
    eprintln!(
        "captured {} samples, RMS {:.6} ({:.1} dBFS)",
        captured.len(),
        rms,
        20.0 * rms.max(1e-12).log10()
    );
    eprintln!("stimulus counters: {counters:?}");
    eprintln!("capture counters:  {:?}", cap.counters());

    assert!(
        counters.underrun_frames == 0,
        "the emitter fell behind the device — the stimulus had gaps"
    );
    assert!(
        elapsed.as_secs_f64() >= 0.5 * pilot.duration_s(),
        "emit returned in {elapsed:?} for {:.3} s of audio: the sink is not \
         pacing, and MS-14's abort model depends on it",
        pilot.duration_s()
    );
    assert!(
        rms > 1e-5,
        "nothing came back on the mic — check routing, mic placement, and \
         that the output device is the one under test"
    );
}

/// `stop()` drops queued audio rather than playing it out at level, and
/// refuses further emission. The audible half is the point: nothing should be
/// heard after the stop.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant"]
fn stopping_the_sink_drops_queued_audio_rather_than_flushing_it() {
    use paraeq_measure::{assemble_pilot, StimulusSink, TransducerClass};

    let mut cap = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("MicCapture::create");
    let rate = cap.sample_rate_hz();
    let mut sink = cap.take_stimulus_sink().expect("sink");

    let pilot = assemble_pilot(1.0, rate as u32, TransducerClass::OverEar).expect("pilot");
    // Push without pacing so audio really is queued when stop lands.
    let format = StimulusSink::format(&sink);
    let block = format.frames_per_block.max(1);
    sink.emit(&pilot.samples()[..block.min(pilot.len())], pilot.level())
        .expect("first block");
    StimulusSink::stop(&mut sink).expect("stop is infallible here");
    StimulusSink::stop(&mut sink).expect("and idempotent");

    assert!(
        sink.emit(pilot.samples(), pilot.level()).is_err(),
        "a stopped sink must refuse further emission"
    );
    eprintln!("stimulus counters after stop: {:?}", sink.counters());
}

/// The one-clock claim, measured: play the bracketed marker pair through the
/// aggregate's OWN output route (rather than `afplay` on the raw device) and
/// report the skew. If the aggregate really does put play and record in one
/// clock domain, this should read far below the ~12 ppm REW attributes to
/// separate devices.
///
/// Prints rather than asserts a threshold — the number is the experiment's
/// output. Pair it with the `afplay`-driven two-clock runs above: same rig,
/// same markers, different playback route, so the difference between them is
/// exactly what the aggregate buys.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant"]
fn one_clock_skew_through_the_aggregates_own_output() {
    use paraeq_measure::{assemble_sweep, StimulusSink, SweepLevel, TransducerClass};

    let mut cap = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("MicCapture::create");
    let rate = cap.sample_rate_hz();
    let mut sink = cap.take_stimulus_sink().expect("sink");

    let marker = two_clock::default_marker(rate);
    let gap = vec![0.0f64; (0.25 * rate) as usize];
    let level = SweepLevel::new(-20.0, TransducerClass::OverEar).expect("legal level");
    let sweep = assemble_sweep(5.0, rate as u32, 20.0, 20_000.0, level).expect("sweep assembles");

    let mut stimulus = Vec::new();
    stimulus.extend(std::iter::repeat_n(0.0, (0.5 * rate) as usize));
    let first_marker_at = stimulus.len();
    stimulus.extend_from_slice(&marker);
    stimulus.extend_from_slice(&gap);
    stimulus.extend_from_slice(sweep.samples());
    stimulus.extend_from_slice(&gap);
    let second_marker_at = stimulus.len();
    stimulus.extend_from_slice(&marker);
    stimulus.extend(std::iter::repeat_n(0.0, (0.5 * rate) as usize));

    let mut scratch = vec![0.0f64; 8192];
    while cap.capture(&mut scratch).expect("capture") > 0 {}

    // Capture on a second thread so play and record overlap, which is the
    // whole point of a shared aggregate.
    let expected = stimulus.len();
    let capture = std::thread::spawn(move || {
        let mut got = Vec::with_capacity(expected + 8192);
        let deadline = Instant::now() + Duration::from_secs_f64(expected as f64 / rate + 2.0);
        let mut buf = vec![0.0f64; 8192];
        while Instant::now() < deadline {
            let n = cap.capture(&mut buf).expect("capture");
            if n == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            got.extend_from_slice(&buf[..n]);
        }
        (got, cap)
    });

    let block = StimulusSink::format(&sink).frames_per_block.max(1);
    for chunk in stimulus.chunks(block) {
        sink.emit(chunk, level).expect("emit");
    }
    let (captured, cap) = capture.join().expect("capture thread");

    eprintln!(
        "one-clock run: emitted {} samples, captured {} at {rate} Hz",
        stimulus.len(),
        captured.len()
    );
    eprintln!("stimulus counters: {:?}", sink.counters());
    eprintln!("capture counters:  {:?}", cap.counters());

    // The two-point bracket: find both markers in the capture, fit the
    // capture-clock positions against the known playback-clock ones.
    let separation = ((second_marker_at - first_marker_at) / 2).max(1);
    let hits = two_clock::find_marker_train(&captured, &marker, 2, separation);
    match hits.and_then(|hits| {
        let expected = [first_marker_at as f64, second_marker_at as f64];
        let measured: Vec<f64> = hits.iter().map(|h| h.position).collect();
        two_clock::estimate_skew(&expected, &measured)
    }) {
        Some(SkewEstimate {
            skew_ppm,
            residual_peak_samples,
            ..
        }) => eprintln!(
            "ONE-CLOCK SKEW: {skew_ppm:+.2} ppm, residual peak \
             {residual_peak_samples:.2} samples (REW reports ~12 ppm across \
             SEPARATE devices; a shared aggregate should be far below that)"
        ),
        None => eprintln!(
            "no skew estimate could be formed — this is the Warn(TwoClock) \
             fallback path; check marker SNR and mic placement"
        ),
    }
}

/// **The first real headless measurement run**, and the first time both
/// aggregates have ever been up at once.
///
/// The whole chain, live: a `TapBackend` under a real `EngineHandle` →
/// `exclusion_witness()` → `MicCapture::create` → `take_stimulus_sink()` →
/// `DeviceVolume::for_default_output()` → `MeasurementSession::begin` (which
/// polls MS-6 through that witness before it touches the sink or the volume) →
/// `install_solve` → `acknowledge` → `sweep`. Every gate the session enforces
/// is exercised against hardware rather than mocks; `crates/paraeq-measure/
/// tests/test_session.rs` remains the place the gates' *logic* is pinned.
///
/// **A measurement lease is held for the run.** The stimulus plays on the
/// measurement aggregate, which the tap excludes by design (MS-6), so the tap
/// sees nothing but zeros for the whole sweep and the fail-open watchdog would
/// otherwise read that as a TCC silent failure and tear the tap down
/// mid-sweep. That is the wizard's own scenario;
/// `test_hardware.rs::lease_keeps_the_tap_alive_across_a_silent_measurement`
/// is the test that isolates it.
///
/// **This is also plan item B0/E6's experiment**, ahead of schedule and by
/// necessity. `tap.rs::create_aggregate` composes the default output into the
/// tap aggregate and `measure_aggregate.rs` composes the same device again,
/// and nothing has ever run both. A `MicCapture::create` failure here, or a
/// silent mic with a healthy-looking session, is that question's answer — not
/// a measurement bug. Both outcomes print what they mean.
///
/// **The cal and the solve are synthetic, and quiet on purpose.** No cal-file
/// loader exists yet, and no Stage-5 ladder runs here, so the numbers cannot
/// be earned — they are chosen. Choosing them fixed rather than solving them
/// from a fabricated sensitivity is the safety decision: a solve driven by a
/// fake cal against a distant mic asks for a LOUDER level, and the whole
/// point of the caps is that no level reaches a sink unjustified. The emitted
/// level is ~30 dB below the OverEar target, and the printed SPL is fiction —
/// it is what the acknowledgement gate needs to be handed, not a measurement.
///
/// Teardown is RAII on every exit path, panic included:
/// `MeasurementSession::drop` stops the sink and restores the pinned volume,
/// `MicCapture::drop` tears the measurement aggregate down, dropping the lease
/// re-arms fail-open, and `EngineHandle::drop` shuts the controller down,
/// which stops the backend and destroys the tap. Nothing here leaves a tap or
/// a volume change behind.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant; plays a quiet sweep"]
fn a_full_measurement_session_runs_against_the_live_witness() {
    use paraeq_coreaudio::backend::TapBackend;
    use paraeq_coreaudio::volume::DeviceVolume;
    use paraeq_coreaudio::{devices, properties};
    use paraeq_engine::controller::{EngineCommand, EngineConfig, EngineHandle};
    use paraeq_measure::{
        assemble_sweep, CalSensitivity, CalSummary, MeasurementSession, SessionEvent, SessionPhase,
        SessionSeam, SolveOutcome, SweepOutcome, TapStatus, TransducerClass,
    };

    /// Deliberately quiet, deliberately fixed — see the doc comment. The three
    /// numbers are self-consistent (projection = sensitivity + level) so the
    /// log reads like a real run, but only the LEVEL is real: it is what
    /// `SweepLevel::new` checks and what reaches the sink.
    const SOLVE: SolveOutcome = SolveOutcome {
        chain_sensitivity_spl_per_dbfs: 104.0,
        projected_spl_db: 54.0,
        solved_dbfs_rms: -50.0,
    };
    const SWEEP_S: f64 = 2.0;

    // ── the engine, and the witness taken before the backend moves into it ──
    let backend = TapBackend::new();
    let witness = backend.exclusion_witness();
    let engine = EngineHandle::spawn(backend, EngineConfig::default());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !witness.self_excluded() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        witness.self_excluded(),
        "no self-excluding tap within 10 s: MS-6 would refuse this session before it \
         touched the sink, which is the correct behaviour and makes the rest of this \
         test unrunnable. Check the TCC grant and that no other ParaEQ is running."
    );
    let lease = engine
        .acquire_measurement_lease()
        .expect("a fresh engine has no outstanding lease");

    // ── the measurement aggregate, on the same output device (B0/E6) ────────
    let mut cap = match MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    }) {
        Ok(cap) => cap,
        Err(e) => panic!(
            "MicCapture::create failed with the engine's tap aggregate live on the same \
             output device: {e:?}\n\
             This is plan item B0/E6 answering NO — the two aggregates do not coexist. \
             Rerun this test with the engine stopped to confirm the aggregate itself is \
             healthy; if it is, the fallback is a mic-only capture aggregate plus the \
             two-clock marker path for t=0, which redesigns the verification capture side."
        ),
    };
    let rate = cap.sample_rate_hz();
    let sink = cap
        .take_stimulus_sink()
        .expect("the sink is available once");
    let volume = DeviceVolume::for_default_output().expect("default output device");

    // MS-18 wants the device NAMED, not identified by uid.
    let device = properties::default_output_device().expect("default_output_device");
    let uid = properties::device_uid(device).expect("device_uid");
    let device_name = devices::list_output_devices()
        .ok()
        .and_then(|list| list.into_iter().find(|d| d.uid == uid).map(|d| d.name))
        .unwrap_or_else(|| uid.clone());

    // ── the session ─────────────────────────────────────────────────────────
    let cal = CalSummary::validate(
        TransducerClass::OverEar,
        "synthetic (no cal-file loader exists yet)".to_owned(),
        CalSensitivity::Parsed(-18.0),
        1.0,
    )
    .expect("the synthetic cal validates");
    let mut session = MeasurementSession::begin(
        cal,
        1.0,
        SessionSeam {
            sink: Box::new(sink),
            tap: Box::new(witness.clone()),
            volume: Box::new(volume),
        },
    )
    .expect("MS-6 passes over a live self-excluding tap");
    let level = session.install_solve(SOLVE).expect("the solve installs");
    session
        .acknowledge(&device_name, SOLVE.projected_spl_db)
        .expect("the acknowledgement records");

    let f_end_hz = (0.45 * rate).min(20_000.0);
    let stimulus = assemble_sweep(SWEEP_S, rate as u32, 20.0, f_end_hz, level)
        .expect("the sweep assembles at the decided level");
    eprintln!(
        "SESSION: '{device_name}' at {rate} Hz, mic '{}', {:.1} s sweep 20-{f_end_hz:.0} Hz at \
         {:.1} dBFS RMS (projection {} dB SPL is SYNTHETIC)",
        cap.mic_uid(),
        stimulus.duration_s(),
        level.dbfs_rms(),
        SOLVE.projected_spl_db,
    );

    // Drain whatever the mic buffered before the sweep starts.
    let mut scratch = vec![0.0f64; 8192];
    while cap.capture(&mut scratch).expect("capture") > 0 {}

    // Capture on a second thread: `sweep` is paced by the device and does not
    // return until the last block has been consumed.
    let capture_for_s = stimulus.duration_s() + 1.0;
    let recorder = std::thread::spawn(move || {
        let mut got = Vec::new();
        let mut buf = vec![0.0f64; 8192];
        let deadline = Instant::now() + Duration::from_secs_f64(capture_for_s);
        while Instant::now() < deadline {
            let n = cap.capture(&mut buf).expect("capture");
            if n == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            got.extend_from_slice(&buf[..n]);
        }
        (got, cap)
    });
    let outcome = session.sweep(&stimulus).expect("the sweep gate opens");
    let (captured, cap) = recorder.join().expect("capture thread");

    // ── what happened ───────────────────────────────────────────────────────
    let rms = (captured.iter().map(|v| v * v).sum::<f64>() / captured.len().max(1) as f64).sqrt();
    eprintln!(
        "SESSION: captured {} samples, RMS {:.6} ({:.1} dBFS); capture counters {:?}",
        captured.len(),
        rms,
        20.0 * rms.max(1e-12).log10(),
        cap.counters(),
    );
    match &outcome {
        SweepOutcome::Completed { warnings } => {
            eprintln!("SESSION: completed, warnings {warnings:?}");
        }
        SweepOutcome::Aborted {
            diagnostic,
            warnings,
        } => panic!("the sweep aborted: {diagnostic:?} (warnings {warnings:?})"),
    }
    assert_eq!(session.phase(), SessionPhase::Swept);
    assert!(
        witness.self_excluded(),
        "the tap went away during the measurement — the lease did not hold it, and a \
         session that re-polled MS-6 here would refuse"
    );
    assert!(cap.counters().callbacks > 0, "the mic IOProc never engaged");

    let log = session.finish();
    eprintln!("SESSION LOG: {:#?}", log.events());
    assert!(
        matches!(
            log.events().last(),
            Some(SessionEvent::Terminated { diagnostic: None })
        ),
        "a clean run must end with an undiagnosed Terminated: {:?}",
        log.events().last()
    );
    assert!(
        log.events()
            .iter()
            .any(|e| matches!(e, SessionEvent::SweepCompleted)),
        "no SweepCompleted in the log"
    );
    for event in log.events() {
        assert!(
            !matches!(
                event,
                SessionEvent::SinkFault { .. } | SessionEvent::VolumeRestoreFailed { .. }
            ),
            "the teardown collected a fault it did not mask, but a clean run must not \
             produce one: {event:?}"
        );
    }

    // Printed, never asserted: the level that comes back is a property of the
    // rig (mic placement, output routing) AND of whether the tap's
    // MutedWhenTapped posture silences the measurement aggregate as well —
    // which is precisely B0/E6's open question. A threshold here would report
    // an unanswered question as a broken measurement.
    if rms <= 1e-5 {
        eprintln!(
            "SESSION: NOTHING CAME BACK ON THE MIC. Check mic placement and routing first; \
             if `pilot_plays_through_the_aggregate_and_comes_back_on_the_mic` passes with \
             the engine stopped and this does not, that is plan item B0/E6 answering NO — \
             the tap aggregate silences the measurement aggregate's output, and the \
             verification capture side needs redesigning."
        );
    }

    drop(cap);
    drop(lease);
    engine.send(EngineCommand::Disable);
    drop(engine);
}

// ════════════════ Stage 6 (B16): the verification loop on real hardware ══════
//
// Everything below drives the VERIFICATION path — the helper child process, its
// private render aggregate, the tap that must see it, and the closed-loop
// `VerificationPass` — against real devices. `measurement-suite`'s rule,
// verbatim: "Hardware-dependent integration tests stay behind `#[ignore]` and
// run locally (CI has no audio devices). The helper-child-process verification
// loop joins them."
//
// # The rig session, in order
//
// These need a SECOND binary — the helper child — which `cargo test -p
// paraeq-coreaudio` does NOT build, because `paraeq-stimulus` depends on this
// crate rather than the other way round. Build it first, in the same profile:
//
// ```sh
// cargo build --release -p paraeq-stimulus
// cargo build --release -p paraeq-coreaudio --examples
//
// # B0, the spike — three questions, one sitting, PASS/FAIL lines to read:
// cargo run --release -p paraeq-coreaudio --example hal_render_probe -- --bare
// cargo run --release -p paraeq-coreaudio --example hal_render_probe -- --wrapped
// cargo run --release -p paraeq-coreaudio --example hal_render_probe -- --coexist
//
// # B16, the tests — run them one at a time, in this order, and read stderr:
// cargo test -p paraeq-coreaudio --release --test test_measure_hardware -- \
//     --ignored --nocapture --test-threads 1 <one test name>
// ```
//
// Order, and why: `tap_and_measurement_aggregates_coexist_on_one_output_device`
// first (nothing downstream is meaningful if the pair cannot be up at once),
// then `muted_when_tapped_really_mutes_the_helper` (the diagnostic that
// explains a later failure), then `helper_audio_really_is_corrected` (the
// premise of the feature), then the three render-aggregate lifecycle tests,
// then `the_verification_gates_arm_on_a_genuinely_quiet_machine`, and last
// `the_full_verification_pass_runs_end_to_end_on_the_rig`.
//
// `--test-threads 1` is not optional: several of these bring a tap up, and two
// taps on one output device at the same time is not the topology under test.
//
// # How far these reach, and where they stop
//
// The seams `VerificationPass` drives have three different homes:
//
// | seam | production impl | here |
// |---|---|---|
// | `CaptureSource` | `MicCapture` (this crate) | the real one |
// | `VolumeControl` | `DeviceVolume` (this crate) | the real one |
// | `TapStatus` | `ExclusionWitness` (this crate) | the real one |
// | `DeviceFacts` | `DeviceSeam` (`desktop/src-tauri`) | [`RigDeviceFacts`], below — the same two HAL reads |
// | `EngineFacts` / `EngineControl` | `desktop/src-tauri/src/verify_seam.rs` | [`RigEngineSeam`], below |
// | `StimulusHelper` / `HelperProcess` | `desktop/src-tauri` (B14) | [`RigHelper`] / [`HelperChild`], below |
//
// **The last two rows are the compromise, and it is a deliberate one.** The
// production `EngineFacts` impl lives in the desktop crate because those are
// CONTROLLER facts, and `paraeq-coreaudio` may not depend on the desktop crate:
// the dependency runs the other way, and reversing it would put verification
// policy below the controller. So the alternative to a test-side impl is not "a
// better test here", it is "no end-to-end hardware test at all until the desktop
// grows one". The impls below are therefore kept deliberately thin — they map
// one `EngineState` snapshot onto the seam's vocabulary and nothing else — and
// their MAPPING is not what these tests are for. What is under test is the
// physics on the other side of them: that a foreign process's audio is tapped,
// corrected, and audible as corrected. `desktop/src-tauri/src/verify_seam.rs`'s
// own unit tests pin the mapping, headlessly, against the same shipped
// `EngineHandle`.

// These sit here rather than at the top of the file because they serve only
// this section; `PathBuf`, `Duration`, `Instant` and `CaptureSource` are already
// imported above and must not be named twice in one module.
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};

use paraeq_coreaudio::backend::{ExclusionWitness, TapBackend};
use paraeq_coreaudio::render::RenderAggregate;
use paraeq_coreaudio::volume::DeviceVolume;
use paraeq_coreaudio::{devices, properties};
use paraeq_dsp::peq::{EQBand, FilterType};
use paraeq_dsp::two_clock::DEFAULT_MARKER_LAYOUT;
use paraeq_engine::controller::{
    CorrectionConfig, EngineCommand, EngineConfig, EngineHandle, EngineState,
};
use paraeq_engine::preamp;
use paraeq_engine::status::EngineStatus;
use paraeq_measure::seam::{DeviceFacts, HelperLine};
use paraeq_measure::{
    expected_render_device_uid, CalSensitivity, CalSummary, EngineControl, EngineFacts,
    EngineStatusKind, GainPin, HelperExit, HelperProcess, HelperRouting, MeasureError,
    MeasurementLeaseToken, StimulusHelper, StreamFormat, SweepLevel, SweepShape, TapStatus,
    TransducerClass, VerificationPass, VerifyPlan, VerifyRequest, VerifySeam, VerifyTiming,
    VERIFY_MIN_SNR_DB,
};

/// The probe cut: deep, narrow, and at a frequency every transducer on the rig
/// reproduces. Deep so a leak of the helper's RAW (uncorrected) path is
/// unmistakable rather than arguable; narrow so the reference tone beside it is
/// untouched and can cancel the room.
const PROBE_CUT_FC_HZ: f64 = 1_000.0;
const PROBE_CUT_GAIN_DB: f64 = -18.0;
const PROBE_CUT_Q: f64 = 2.0;

/// The reference tone: well below the cut's skirt, so it measures the room, the
/// mic and the transducer WITHOUT the correction, and subtracting it removes all
/// three from the answer.
const PROBE_REFERENCE_HZ: f64 = 300.0;

/// Peak amplitude of each probe tone, linear (≈ −30 dBFS).
///
/// Quiet on purpose, and fixed rather than solved: no cal file and no level
/// ladder run here, so a solved level would be solved from a fabricated
/// sensitivity. These tests ask about SHAPE (is the cut there?), and shape is
/// measured by a ratio, which a quiet tone answers exactly as well as a loud
/// one.
const PROBE_TONE_AMPLITUDE: f64 = 0.03;

/// Seconds per probe tone.
const PROBE_TONE_SECONDS: f64 = 2.0;

/// Raised-cosine fade at each tone's edges, seconds. A hard tone edge is a
/// full-scale click, which is the one thing the whole abort design exists to
/// avoid; it would also smear the single-bin readout below.
const PROBE_FADE_SECONDS: f64 = 0.05;

/// How much of the designed cut must survive to the mic before this counts as
/// "the helper's audio really is corrected", dB.
///
/// A LOOSE bound, and deliberately far from the designed −18 dB: the number that
/// arrives at a microphone has been through a transducer, a room and a mic
/// response. What it falsifies is the failure that matters — the helper's audio
/// reaching the air UNCORRECTED, which reads as 0 dB here, not as 12.
const PROBE_MIN_OBSERVED_CUT_DB: f64 = 6.0;

/// `L_measure` for the end-to-end pass, dBFS RMS. Fixed and quiet; see
/// [`PROBE_TONE_AMPLITUDE`] for why nothing here is solved.
const RIG_L_MEASURE_DBFS: f64 = -40.0;

/// The SPL `L_measure` is declared to project at the mic, dB.
///
/// **This number is fiction**, exactly as it is in
/// `a_full_measurement_session_runs_against_the_live_witness`: it is
/// self-consistent with the synthetic cal (a 104 dB-SPL-per-dBFS chain and a
/// −40 dBFS level project 64 dB) so the log reads like a real run, but only the
/// LEVEL is real. It is what the MS-18 acknowledgement gate needs to be handed,
/// never a measurement.
const RIG_PROJECTED_SPL_DB: f64 = 64.0;

/// Sweep length for the end-to-end pass, seconds. Short: the pass is being
/// exercised, not a transducer.
const RIG_SWEEP_SECONDS: f64 = 2.0;

/// How long to wait for the child's `ready` line before calling it wedged.
const HELPER_READY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for the child's terminating line once it has played out.
const HELPER_DONE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the HAL is given to reap a dead client's aggregate before the
/// device-gone assertions fire. Cleanup after a SIGKILL is the HAL's, and it is
/// asynchronous; a zero-tolerance poll would report a scheduling delay as a
/// leaked device.
const DEVICE_GONE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`WaitingCapture`] waits for a mic that has stopped delivering
/// before it reports the stream ended. Long enough that no scheduling hiccup
/// reaches it, short enough that a dead mic ends the run rather than hanging it.
const CAPTURE_STALL_TIMEOUT: Duration = Duration::from_secs(2);

// ───────────────────────────── the helper binary ─────────────────────────────

/// Where the built `paraeq-stimulus` binary is.
///
/// `env!("CARGO_BIN_EXE_paraeq-stimulus")` — the mechanism
/// `crates/paraeq-stimulus/tests/test_signals.rs` uses — is set by Cargo only
/// for the tests of the package that DECLARES the binary, and this is a
/// different package. So this uses cargo's target-directory convention instead:
/// an integration test binary runs from `<target>/<profile>/deps/`, and every
/// binary target of the workspace is its grandparent's direct child,
/// `<target>/<profile>/<name>`. That holds for a custom `CARGO_TARGET_DIR` too,
/// because the path is derived from this process rather than assumed.
///
/// `PARAEQ_STIMULUS_BIN` overrides it, for a rig session running an installed
/// or bundled helper rather than a freshly built one.
///
/// A missing binary is a HARNESS fault with a one-command fix, not a result, so
/// it panics with the command rather than skipping: a silent skip of the test
/// that proves the feature's premise is the worst outcome available here.
fn stimulus_binary() -> PathBuf {
    if let Ok(path) = std::env::var("PARAEQ_STIMULUS_BIN") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let exe = std::env::current_exe().expect("this test binary's own path");
    let path = exe
        .parent()
        .and_then(Path::parent)
        .expect("<target>/<profile>/deps/<test> has a grandparent")
        .join("paraeq-stimulus");
    assert!(
        path.is_file(),
        "the verification helper is not built at {}. `cargo test -p paraeq-coreaudio` does not \
         build it — paraeq-stimulus depends on this crate, not the other way round. Build it in \
         the SAME profile first:\n    cargo build --release -p paraeq-stimulus\nor point \
         PARAEQ_STIMULUS_BIN at one.",
        path.display()
    );
    path
}

/// One spawned `paraeq-stimulus`, with its stdout pumped onto a channel.
///
/// The pump thread exists for two reasons, and both are safety rather than
/// convenience: a child whose stdout pipe fills up blocks inside `write` and
/// never reaches its abort ramp, and `BufRead::read_line` has no deadline, so a
/// parent reading it directly would hang forever on a wedged child instead of
/// running the teardown ladder.
///
/// `Drop` kills and reaps on EVERY exit path including panic. A test that
/// unwound leaving a helper rendering would leave a private aggregate wrapping
/// the owner's output device.
struct HelperChild {
    child: Option<Child>,
    /// Cached so [`HelperProcess::reap`] and [`HelperProcess::try_reap`] are
    /// idempotent rather than blocking a second time on an already-reaped pid.
    exit: Option<HelperExit>,
    lines: Receiver<String>,
    pid: u32,
    stdin: Option<ChildStdin>,
}

impl HelperChild {
    fn spawn(wav: &Path, device_uid: &str, channel: &str) -> Result<HelperChild, MeasureError> {
        let mut child = Command::new(stimulus_binary())
            .args([
                "--wav",
                wav.to_str().expect("a utf-8 scratch path"),
                "--device-uid",
                device_uid,
                "--channel",
                channel,
                "--json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| {
                MeasureError::Sink(format!("cannot spawn the verification helper: {e}"))
            })?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MeasureError::Sink("the helper has no stdout".to_owned()))?;
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
            // Dropping `tx` here disconnects the channel, which is how EOF
            // reaches `read_deadline` as `HelperLine::Eof`.
        });
        Ok(HelperChild {
            child: Some(child),
            exit: None,
            lines,
            pid,
            stdin,
        })
    }

    /// The reader thread's three outcomes, in the seam's vocabulary — the same
    /// mapping `desktop/src-tauri/src/verify_seam.rs` makes.
    ///
    /// A timeout and an EOF stay APART, which is why [`HelperLine`] has three
    /// variants rather than two: a child that closed its stdout has ended, a
    /// child that has said nothing may still be wedged inside a device open,
    /// and the teardown ladder escalates to SIGTERM only in the second case.
    /// Collapsing them reads a wedged helper as a dead one and leaves it
    /// rendering into the owner's output device.
    ///
    /// Infallible on purpose: `recv_timeout`'s two failure modes are both
    /// answers here, so there is no error left to return.
    fn read_deadline(&mut self, deadline: Duration) -> HelperLine {
        match self.lines.recv_timeout(deadline) {
            Ok(line) => HelperLine::Line(line),
            Err(RecvTimeoutError::Disconnected) => HelperLine::Eof,
            Err(RecvTimeoutError::Timeout) => HelperLine::DeadlineExpired,
        }
    }

    /// Read until a line carrying `"event":"<name>"` arrives, or the deadline
    /// passes. Returns the whole line so the caller can read its fields.
    fn wait_for_event(&mut self, name: &str, deadline: Duration) -> Option<String> {
        let needle = format!(r#""event":"{name}""#);
        let end = Instant::now() + deadline;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match self.read_deadline(remaining) {
                HelperLine::Line(line) => {
                    if line.contains(&needle) {
                        return Some(line);
                    }
                }
                // Both ways of hearing nothing more end the wait: the caller
                // asked for one named event within `deadline`, and neither a
                // closed stdout nor an expired deadline can still produce it.
                HelperLine::DeadlineExpired | HelperLine::Eof => return None,
            }
        }
    }

    fn write_line(&mut self, line: &str) -> Result<(), MeasureError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| MeasureError::Sink("the helper's stdin is closed".to_owned()))?;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| MeasureError::Sink(format!("cannot write '{line}' to the helper: {e}")))
    }

    fn reap_inner(&mut self) -> Result<HelperExit, MeasureError> {
        if let Some(exit) = self.exit {
            return Ok(exit);
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| MeasureError::Sink("the helper is already gone".to_owned()))?;
        let status = child
            .wait()
            .map_err(|e| MeasureError::Sink(format!("cannot reap the helper: {e}")))?;
        Ok(self.record_exit(status))
    }

    /// The NON-BLOCKING twin of [`reap_inner`](Self::reap_inner), over
    /// `Child::try_wait` exactly as `desktop/src-tauri/src/verify_seam.rs` does.
    ///
    /// It must not block: the ladder polls liveness against a deadline while
    /// the helper may still be rendering, and a `wait` there would hang on the
    /// very child the poll exists to escalate against.
    fn try_reap_inner(&mut self) -> Result<Option<HelperExit>, MeasureError> {
        if let Some(exit) = self.exit {
            return Ok(Some(exit));
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| MeasureError::Sink("the helper is already gone".to_owned()))?;
        let polled = child
            .try_wait()
            .map_err(|e| MeasureError::Sink(format!("cannot read the helper's status: {e}")))?;
        Ok(polled.map(|status| self.record_exit(status)))
    }

    /// Cache one observed exit status, in the seam's vocabulary.
    ///
    /// One home for the mapping so the blocking and the non-blocking reap
    /// cannot come to disagree about what `signalled` means.
    fn record_exit(&mut self, status: std::process::ExitStatus) -> HelperExit {
        let exit = HelperExit {
            code: status.code(),
            // `code()` is `None` exactly when the process died OF a signal,
            // which is the fact the teardown ladder branches on: a signal death
            // means no unwind, so the render aggregate's Drop never ran.
            signalled: status.code().is_none(),
        };
        self.exit = Some(exit);
        exit
    }
}

impl Drop for HelperChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl HelperProcess for HelperChild {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn request_abort(&mut self) -> Result<(), MeasureError> {
        self.write_line("abort")
    }

    /// SIGTERM, which `std` cannot send — `Child::kill()` is SIGKILL. The child
    /// installs a handler that arms the SAME abort flag `abort\n` does, so this
    /// is a second RAMP request and not a stop.
    fn request_terminate(&mut self) -> Result<(), MeasureError> {
        // SAFETY: `pid` is a child this process spawned and has not yet reaped,
        // and SIGTERM is a valid signal number on every supported target. The
        // identical call, with the identical argument, is what
        // `crates/paraeq-stimulus/tests/test_signals.rs` already makes.
        let sent = unsafe { libc::kill(self.pid as i32, libc::SIGTERM) };
        if sent == 0 {
            Ok(())
        } else {
            Err(MeasureError::Sink(format!(
                "SIGTERM to the helper (pid {}) failed: {}",
                self.pid,
                std::io::Error::last_os_error()
            )))
        }
    }

    fn kill(&mut self) -> Result<(), MeasureError> {
        match self.child.as_mut() {
            // An already-dead child is not an error: every rung must be safe to
            // call on one, or a rung panics and the ladder stops before the
            // render device is destroyed.
            None => Ok(()),
            Some(child) => child
                .kill()
                .map_err(|e| MeasureError::Sink(format!("cannot SIGKILL the helper: {e}"))),
        }
    }

    fn reap(&mut self) -> Result<HelperExit, MeasureError> {
        self.reap_inner()
    }

    fn try_reap(&mut self) -> Result<Option<HelperExit>, MeasureError> {
        self.try_reap_inner()
    }

    fn send_play(&mut self) -> Result<(), MeasureError> {
        self.write_line("play")
    }

    fn read_line(&mut self, deadline: Duration) -> Result<HelperLine, MeasureError> {
        Ok(self.read_deadline(deadline))
    }
}

/// The `StimulusHelper` half: spawns the real binary. Holds nothing, because
/// the seam deliberately cannot be handed a level or a stimulus — the WAV is
/// the level.
struct RigHelper;

impl StimulusHelper for RigHelper {
    fn spawn(
        &mut self,
        wav_path: &Path,
        device_uid: &str,
        routing: HelperRouting,
    ) -> Result<Box<dyn HelperProcess>, MeasureError> {
        let child = HelperChild::spawn(wav_path, device_uid, &routing.as_channel_arg())?;
        Ok(Box::new(child))
    }
}

// ───────────────────────── the device facts, test-side ───────────────────────

/// The pass's by-UID HAL questions, answered by the real HAL.
///
/// A hardware test must not stub these. The teardown's device-gone check asks
/// whether the helper's private render aggregate is still wrapping the owner's
/// output device, and a stubbed answer would turn the one assertion that can
/// catch a real leak into a tautology. So this calls the same two shipped
/// `properties` wrappers `DeviceSeam` calls in
/// `desktop/src-tauri/src/verify_seam.rs`, with the same conservative bias; it
/// differs only in reporting to stderr, which is where a rig run is read, rather
/// than to the desktop app's log.
struct RigDeviceFacts;

impl DeviceFacts for RigDeviceFacts {
    /// **An unreadable answer is `true`**, which the seam requires: a teardown
    /// that could not establish the device is gone must report a possible leak
    /// rather than assume a clean one. The two errors are not symmetric — a
    /// false report costs the owner a line of stderr, a false clean leaves a
    /// private device on their output with nothing watching it.
    fn device_exists(&self, uid: &str) -> bool {
        match properties::device_exists(uid) {
            Ok(present) => present,
            Err(e) => {
                eprintln!(
                    "RIG: cannot tell whether '{uid}' still exists ({e}); reporting it present"
                );
                true
            }
        }
    }

    /// `None` is "cannot answer", never "0 Hz" — including when no device
    /// carries this UID, which is the benign race the rate fence expects when
    /// the helper has already destroyed its aggregate.
    fn nominal_sample_rate(&self, device_uid: &str) -> Option<f64> {
        match properties::nominal_sample_rate_for_uid(device_uid) {
            Ok(rate) => rate,
            Err(e) => {
                eprintln!("RIG: cannot read '{device_uid}''s nominal rate: {e}");
                None
            }
        }
    }
}

// ────────────────────────── the engine seam, test-side ───────────────────────

/// A handle slot both seam boxes can hold. `EngineHandle` owns an
/// `mpsc::Sender`, which is `Send` but not `Sync`, so the `Mutex` is what makes
/// the shared handle legal to put behind the seams' `Send` bound — the same
/// shape `desktop/src-tauri/src/state.rs` uses for the same reason.
type EngineSlot = Arc<Mutex<Option<EngineHandle>>>;

fn with_engine<T>(slot: &EngineSlot, f: impl FnOnce(&EngineHandle) -> T) -> Option<T> {
    let guard = slot.lock().expect("engine handle lock");
    guard.as_ref().map(f)
}

/// The verification pass's view of the live engine, for the rig.
///
/// See this section's header for why this is not the production impl. It is a
/// straight mapping of one `EngineState` snapshot, with one deliberate
/// difference from nothing at all: `engine_engaged` is NOT `status == Running`.
/// The shipped watchdog sets `Running` only while nonzero input is advancing,
/// and the verification pre-roll REQUIRES a quiet machine, so a gate written
/// against the literal word would refuse a healthy engine on every run.
struct RigEngineSeam {
    engine: EngineSlot,
}

impl RigEngineSeam {
    fn state(&self) -> Option<Arc<EngineState>> {
        with_engine(&self.engine, EngineHandle::state)
    }
}

impl EngineFacts for RigEngineSeam {
    fn bands_dropped(&self) -> usize {
        self.state().map_or(0, |s| s.bands_dropped)
    }

    fn bypass(&self) -> bool {
        self.state().is_some_and(|s| s.bypass)
    }

    fn clipped_samples(&self) -> u64 {
        self.state().map_or(0, |s| s.clipped_samples)
    }

    fn correction_installed(&self) -> bool {
        self.state().is_some_and(|s| s.correction.is_some())
    }

    fn correction_rate_mismatch_hz(&self) -> Option<f64> {
        self.state().and_then(|s| s.correction_rate_mismatch)
    }

    fn engine_engaged(&self) -> bool {
        self.state().is_some_and(|s| {
            s.enabled
                && s.stream.is_some()
                && !matches!(
                    s.status,
                    EngineStatus::AutoDisabledNoInput { .. }
                        | EngineStatus::Failed { .. }
                        | EngineStatus::Stopped
                )
        })
    }

    fn gain_db(&self) -> f32 {
        self.state().map_or(0.0, |s| s.gain_db)
    }

    /// `None` is propagated, never defaulted to `1.0`: a silently-unity preamp
    /// is precisely the failure the verification pass exists to catch.
    fn installed_preamp_lin(&self) -> Option<f32> {
        self.state()
            .and_then(|s| s.auto_preamp_db)
            .map(|db| preamp::preamp_lin(f64::from(db)))
    }

    fn latency_ms(&self) -> Option<f64> {
        self.state().and_then(|s| s.latency_ms)
    }

    fn sections_substituted(&self) -> usize {
        self.state().map_or(0, |s| s.sections_substituted)
    }

    fn status_kind(&self) -> EngineStatusKind {
        let Some(state) = self.state() else {
            return EngineStatusKind::Stopped;
        };
        // Exhaustive on purpose: a ninth `EngineStatus` variant cannot arrive
        // without a decision here.
        match state.status {
            EngineStatus::AutoDisabledNoInput { .. } => EngineStatusKind::AutoDisabledNoInput,
            EngineStatus::Failed { .. } => EngineStatusKind::Failed,
            EngineStatus::Idle { .. } => EngineStatusKind::Idle,
            EngineStatus::InputSilent { .. } => EngineStatusKind::InputSilent,
            EngineStatus::NoInputDetected { .. } => EngineStatusKind::NoInputDetected,
            EngineStatus::Running => EngineStatusKind::Running,
            EngineStatus::Starting { .. } => EngineStatusKind::Starting,
            EngineStatus::Stopped => EngineStatusKind::Stopped,
        }
    }

    fn stream_rate_hz(&self) -> Option<f64> {
        self.state()
            .and_then(|s| s.stream.as_ref().map(|i| i.sample_rate))
    }

    /// Read off the live `RtShared` through the handle's non-publishing
    /// accessor, so polling it through a witness window emits no snapshot.
    /// `None` means "no session, so nothing can be witnessed" — never "nothing
    /// is flowing".
    fn tap_activity(&self) -> Option<paraeq_measure::TapActivity> {
        with_engine(&self.engine, EngineHandle::tap_activity)
            .flatten()
            .map(|a| paraeq_measure::TapActivity {
                callbacks: a.callbacks,
                nonzero_blocks: a.nonzero_blocks,
            })
    }
}

impl EngineControl for RigEngineSeam {
    fn acquire_measurement_lease(
        &mut self,
    ) -> Result<Box<dyn MeasurementLeaseToken>, MeasureError> {
        let lease = with_engine(&self.engine, EngineHandle::acquire_measurement_lease)
            .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_owned()))?
            .ok_or_else(|| {
                MeasureError::Sink(
                    "a measurement lease is already outstanding — one measurement at a time"
                        .to_owned(),
                )
            })?;
        Ok(Box::new(RigLease { _lease: lease }))
    }

    /// Pin the USER's trim and hand back an RAII restore.
    ///
    /// The restore closure captures a CLONE of the slot, not a borrow of
    /// `self`, so it can run from `Drop` during unwinding long after this call
    /// returned.
    fn pin_gain_db(&mut self, db: f32) -> Result<GainPin, MeasureError> {
        let previous_db = with_engine(&self.engine, |handle| handle.state().gain_db)
            .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_owned()))?;
        set_gain_db(&self.engine, db)?;
        let engine = Arc::clone(&self.engine);
        Ok(GainPin::new(
            previous_db,
            db,
            Box::new(move |restore_to| set_gain_db(&engine, restore_to)),
        ))
    }
}

/// Owns the engine's RAII lease, so dropping this re-arms fail-open.
struct RigLease {
    _lease: paraeq_engine::controller::MeasurementLease,
}

impl MeasurementLeaseToken for RigLease {
    fn is_held(&self) -> bool {
        true
    }
}

/// Set the trim and WAIT for the engine to echo it back. `EngineCommand` is
/// fire-and-forget, so "it was set" is only knowable from a published snapshot,
/// and pinning a trim nobody confirmed then measuring through it is the failure
/// the read-back exists to prevent.
fn set_gain_db(slot: &EngineSlot, db: f32) -> Result<(), MeasureError> {
    with_engine(slot, |handle| handle.send(EngineCommand::SetGainDb(db)))
        .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_owned()))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current = with_engine(slot, |handle| handle.state().gain_db)
            .ok_or_else(|| MeasureError::Sink("the engine handle is gone".to_owned()))?;
        if current == db {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(MeasureError::Sink(format!(
                "the engine did not apply a {db} dB trim within 2 s (it still reports {current})"
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ────────────────────────────── the rig scaffolding ──────────────────────────

/// A live engine with its self-exclusion witness, and the slot both seam boxes
/// share.
///
/// Field order is teardown order: the slot's `Option<EngineHandle>` is taken and
/// dropped by [`RigEngine::shut_down`], which sends `Disable` (tap destroyed,
/// device unmuted) before `Shutdown`.
struct RigEngine {
    slot: EngineSlot,
    witness: ExclusionWitness,
}

impl RigEngine {
    /// Spawn the production controller over a real `TapBackend` and wait for a
    /// self-excluding tap.
    ///
    /// The witness is taken BEFORE the backend moves into the handle — the only
    /// moment it can be taken — and it stays live and truthful afterwards.
    fn up() -> RigEngine {
        let backend = TapBackend::new();
        let witness = backend.exclusion_witness();
        let handle = EngineHandle::spawn(backend, EngineConfig::default());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !witness.self_excluded() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            witness.self_excluded(),
            "no self-excluding tap within 10 s. Check the System Audio Recording grant on THIS \
             terminal and that no other ParaEQ is running; without a tap there is nothing for \
             the helper's audio to be corrected by, and every test in this section is unrunnable."
        );
        RigEngine {
            slot: Arc::new(Mutex::new(Some(handle))),
            witness,
        }
    }

    /// The live stream's sample rate, once the backend has reported geometry.
    fn stream_rate_hz(&self) -> f64 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let rate = with_engine(&self.slot, |handle| {
                handle.state().stream.as_ref().map(|s| s.sample_rate)
            })
            .flatten();
            if let Some(rate) = rate {
                return rate;
            }
            assert!(
                Instant::now() < deadline,
                "the engine never reported a stream within 10 s"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn send(&self, command: EngineCommand) {
        let sent = with_engine(&self.slot, |handle| handle.send(command));
        assert!(sent.is_some(), "the engine handle is gone");
    }

    fn state(&self) -> Arc<EngineState> {
        with_engine(&self.slot, EngineHandle::state).expect("the engine handle is live")
    }

    /// Install the probe cut as PEQ INTENT, and wait for the engine to arm it.
    ///
    /// `Peq` rather than `Iir` on purpose: it carries design intent, so the
    /// engine re-derives every band at the LIVE rate and publishes the
    /// cascade-derived `auto_preamp_db`. That is the number the verification
    /// pass's gate 2 recomputes and compares to 1e-6, and baked coefficients
    /// would make the comparison meaningless.
    fn install_probe_cut(&self, rate: f64) -> Vec<Vec<EQBand>> {
        let bands = probe_cut_bands();
        self.send(EngineCommand::SetCorrection(CorrectionConfig::Peq {
            bands: bands.clone(),
            design_rate: rate,
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = self.state();
            if state.correction.is_some() && state.auto_preamp_db.is_some() {
                assert_eq!(
                    state.bands_dropped, 0,
                    "the engine dropped a band from the probe cut at {rate} Hz: {state:?}"
                );
                assert!(
                    state.correction_rate_mismatch.is_none(),
                    "the engine refused the probe cut at the live rate: {state:?}"
                );
                return bands;
            }
            assert!(
                Instant::now() < deadline,
                "the engine never armed the probe cut within 5 s: {state:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn facts(&self) -> Box<dyn EngineFacts> {
        Box::new(RigEngineSeam {
            engine: Arc::clone(&self.slot),
        })
    }

    fn control(&self) -> Box<dyn EngineControl> {
        Box::new(RigEngineSeam {
            engine: Arc::clone(&self.slot),
        })
    }

    /// `Disable` tears the tap down (device unmuted) before the handle drop
    /// sends `Shutdown` and joins the controller. Explicit rather than left to
    /// `Drop` so the two happen in that order and the system is never left
    /// muted by a test that ended early.
    fn shut_down(self) {
        self.send(EngineCommand::Disable);
        let handle = self.slot.lock().expect("engine handle lock").take();
        drop(handle);
    }
}

/// The probe cut, on both channels.
///
/// Identical per channel because the verification pass refuses a `Both` routing
/// over divergent band sets: a `Both` capture heard the SUM of the channels, and
/// a sum of differently-corrected channels is not the response of any one band
/// set.
fn probe_cut_bands() -> Vec<Vec<EQBand>> {
    let band = EQBand {
        filter_type: FilterType::Peaking,
        fc: PROBE_CUT_FC_HZ,
        gain_db: PROBE_CUT_GAIN_DB,
        q: PROBE_CUT_Q,
    };
    vec![vec![band.clone()], vec![band]]
}

/// Minimal 32-bit-float mono WAV writer — the one shape the helper accepts.
///
/// Hand-rolled beside [`write_wav_mono16`] and for the same reason it is: a
/// 44-byte canonical header needs no crate, and `hound` is not a dependency of
/// THIS crate. Format tag 3 (`WAVE_FORMAT_IEEE_FLOAT`) with a 16-byte `fmt`
/// chunk and 32 bits per sample is exactly what the child's reader accepts;
/// anything else it refuses with exit 3 rather than reinterpreting, because a
/// quantizing format silently moves the file's RMS.
fn write_wav_mono_f32(path: &Path, sample_rate: u32, samples: &[f64]) {
    let data_bytes = (samples.len() * 4) as u32;
    let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    bytes.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 4).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&4u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&32u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        bytes.extend_from_slice(&(s as f32).to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write the helper's stimulus wav");
}

/// The two-tone probe: [`PROBE_CUT_FC_HZ`] then [`PROBE_REFERENCE_HZ`], each
/// faded in and out.
///
/// A tone pair rather than a sweep, and the reason is the readout. Both tests
/// that use it ask one question — "how deep is the cut, at the microphone?" — and
/// a tone puts all of its energy in the bin being read, where a sweep merely
/// transits it. The reference tone sits well outside the cut's skirt, so
/// subtracting it removes the transducer, the room and the mic response from the
/// answer, twice over when two runs are differenced.
///
/// The bracketed SWEEP — the product's actual stimulus — is exercised by
/// [`the_full_verification_pass_runs_end_to_end_on_the_rig`], which assembles it
/// through `assemble_bracketed` rather than here.
fn probe_tone_pair(rate: f64) -> Vec<f64> {
    let mut samples = Vec::new();
    for freq in [PROBE_CUT_FC_HZ, PROBE_REFERENCE_HZ] {
        let n = (PROBE_TONE_SECONDS * rate) as usize;
        let fade = ((PROBE_FADE_SECONDS * rate) as usize).clamp(1, (n / 2).max(1));
        let step = std::f64::consts::TAU * freq / rate;
        for i in 0..n {
            let envelope = if i < fade {
                0.5 - 0.5 * (std::f64::consts::PI * i as f64 / fade as f64).cos()
            } else if i >= n - fade {
                0.5 - 0.5 * (std::f64::consts::PI * (n - 1 - i) as f64 / fade as f64).cos()
            } else {
                1.0
            };
            samples.push(PROBE_TONE_AMPLITUDE * envelope * (step * i as f64).sin());
        }
    }
    samples
}

/// The amplitude of a single frequency in `samples`, dBFS.
///
/// A Hann-windowed one-bin DFT — the whole of the analysis these tone tests
/// need. The window matters: without it the bin leaks into its neighbours and a
/// narrow cut reads shallower than it is, which would bias the answer in the
/// direction of "everything is fine".
///
/// The window spans the WHOLE capture, which carries both tones in sequence.
/// That is fair to both by symmetry — a Hann window weights the rising and
/// falling halves identically — and any residual bias from the capture's
/// asymmetric tail is common to both legs of
/// [`helper_audio_really_is_corrected`], where the two are differenced.
///
/// Returns a floor rather than `-inf` on silence so the printed numbers stay
/// readable.
fn tone_level_dbfs(samples: &[f64], sample_rate_hz: f64, freq_hz: f64) -> f64 {
    if samples.is_empty() {
        return -200.0;
    }
    let n = samples.len();
    let step = std::f64::consts::TAU * freq_hz / sample_rate_hz;
    let mut re = 0.0;
    let mut im = 0.0;
    let mut window_sum = 0.0;
    for (i, &s) in samples.iter().enumerate() {
        let window = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
        let phase = step * i as f64;
        re += s * window * phase.cos();
        im -= s * window * phase.sin();
        window_sum += window;
    }
    let amplitude = 2.0 * (re * re + im * im).sqrt() / window_sum.max(1e-12);
    (20.0 * amplitude.max(1e-12).log10()).max(-200.0)
}

/// Broadband RMS of a capture, dBFS.
fn rms_dbfs(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return -200.0;
    }
    let mean_square = samples.iter().map(|v| v * v).sum::<f64>() / samples.len() as f64;
    (10.0 * mean_square.max(1e-20).log10()).max(-200.0)
}

/// What one helper run produced.
struct HelperRun {
    capture: Vec<f64>,
    exit: HelperExit,
    ready: String,
    render_device_uid: String,
}

/// Spawn the real helper on `wav`, capture on `mic` while it plays, and wait for
/// it to end on its own.
///
/// Asserts only harness facts — the child opened its device, it ended cleanly,
/// and its render aggregate is gone afterwards. Everything acoustic is the
/// caller's to measure and, per the harness's discipline, to PRINT.
fn play_through_helper(mic: &mut MicCapture, device_uid: &str, wav: &Path) -> HelperRun {
    let mut child = HelperChild::spawn(wav, device_uid, "both").expect("spawn the helper");
    let pid = child.pid();
    let ready = child
        .wait_for_event("ready", HELPER_READY_TIMEOUT)
        .unwrap_or_else(|| {
            panic!(
                "the helper never emitted `ready` within {HELPER_READY_TIMEOUT:?}. Its device \
                 open failed: exit 3 is a WAV-shape or rate refusal (the WAV is written at the \
                 physical output's nominal rate, so a refusal here is spike S2(d) — the private \
                 wrapper's rate differing from its sub-device's), exit 4 is the device, exit 6 \
                 the level backstop and exit 7 a default-output change mid-run."
            )
        });
    let render_device_uid = expected_render_device_uid(pid);
    assert!(
        ready.contains(&render_device_uid),
        "the helper's `ready` line names a render device this parent did not expect ({render_device_uid}): {ready}"
    );

    // Drain whatever the mic buffered before playback starts, so the capture
    // below is this run's and nothing else's.
    let mut scratch = vec![0.0f64; 8192];
    while cap_read(mic, &mut scratch) > 0 {}

    child.send_play().expect("the parent commits");

    // The child plays autonomously from here; this loop is the capture. It ends
    // when the child says it is done, which is also what bounds it.
    let mut capture = Vec::new();
    let deadline = Instant::now() + HELPER_DONE_TIMEOUT;
    let mut done = None;
    while Instant::now() < deadline && done.is_none() {
        let n = cap_read(mic, &mut scratch);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(2));
        } else {
            capture.extend_from_slice(&scratch[..n]);
        }
        // Non-blocking: the terminating line may already be waiting.
        if let HelperLine::Line(line) = child.read_deadline(Duration::from_millis(1)) {
            if line.contains(r#""event":"done""#) || line.contains(r#""event":"aborted""#) {
                done = Some(line);
            } else if line.contains(r#""event":"error""#) {
                panic!("the helper refused mid-run: {line}");
            }
        }
    }
    let done = done.expect("the helper reported neither `done` nor `aborted` before the deadline");
    eprintln!("HELPER: {done}");

    // Drain the tail: the mic is behind the air by the transport latency, so
    // the last of the stimulus is still arriving when the child exits.
    let tail = Instant::now() + Duration::from_millis(500);
    while Instant::now() < tail {
        let n = cap_read(mic, &mut scratch);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(2));
        } else {
            capture.extend_from_slice(&scratch[..n]);
        }
    }

    let exit = child.reap().expect("reap the helper");
    assert_eq!(
        exit.code,
        Some(0),
        "the helper did not exit cleanly: {exit:?} (2 bad args, 3 WAV/rate, 4 device, 5 stalled, \
         6 level backstop, 7 not the default output)"
    );
    assert_render_device_gone(&render_device_uid);

    HelperRun {
        capture,
        exit,
        ready,
        render_device_uid,
    }
}

/// `CaptureSource::capture` through a `MicCapture`, with the error turned into a
/// panic at the call site rather than at every one of them.
fn cap_read(mic: &mut MicCapture, block: &mut [f64]) -> usize {
    CaptureSource::capture(mic, block).expect("capture")
}

/// A [`CaptureSource`] that WAITS for the device rather than reporting an empty
/// ring as the end of the stream.
///
/// **This adapter exists because of a real gap between two shipped pieces, and
/// it is a FINDING rather than a test convenience.** `paraeq_measure::record`
/// treats a zero-frame read as `SourceExhausted` — its own doc gives the reason,
/// "retrying it forever is how a dead stream becomes a hang" — while
/// `MicCapture::capture` is a non-blocking ring drain that returns 0 whenever
/// the caller outruns the device, which on healthy hardware is most polls.
/// Handed the raw `MicCapture`, a verification pass would refuse
/// `MicDisconnected` microseconds into its first capture span, on every run, on
/// a perfect rig.
///
/// The waiting has to live somewhere, and this is the smallest form of it: poll
/// until at least one frame arrives, and give up after
/// [`CAPTURE_STALL_TIMEOUT`] so a genuinely dead mic still ENDS the run instead
/// of hanging it. Whoever wires the production verification capture owes the
/// same behaviour; it is recorded here so the next reader does not rediscover it
/// on the rig, in the middle of a sweep.
struct WaitingCapture {
    inner: MicCapture,
}

impl CaptureSource for WaitingCapture {
    fn format(&self) -> StreamFormat {
        CaptureSource::format(&self.inner)
    }

    fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError> {
        let deadline = Instant::now() + CAPTURE_STALL_TIMEOUT;
        loop {
            let got = CaptureSource::capture(&mut self.inner, block)?;
            if got > 0 {
                return Ok(got);
            }
            if Instant::now() >= deadline {
                // Genuinely exhausted as far as any caller can tell, which is
                // what `record` needs to hear to stop rather than spin.
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        CaptureSource::stop(&mut self.inner)
    }
}

/// Poll until no device carries `uid`, then assert it.
///
/// Polled rather than read once because the HAL's cleanup after a client's death
/// is asynchronous: a single read would report a scheduling delay as a leaked
/// private aggregate wrapping the user's output device.
fn assert_render_device_gone(uid: &str) {
    let deadline = Instant::now() + DEVICE_GONE_TIMEOUT;
    loop {
        let found = properties::translate_uid_to_device(uid).expect("the lookup itself succeeds");
        if found == 0 {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "a private render aggregate with UID '{uid}' survived the helper (device id \
                 {found}) and is now wrapping the user's output device. Destroy it or reboot \
                 before continuing. If this fires only after a SIGKILL, the teardown ladder's \
                 `RenderDeviceLeaked` diagnostic is correct to be conservative and the kill rung \
                 must stay last."
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The default output is still usable: it resolves, it reports a rate, and a
/// fresh private wrapper round-trips over it.
///
/// The assertion a leak test actually needs beside "the device is gone": a
/// killed helper must leave the user's audio working, not merely leave no
/// named device behind.
fn assert_default_output_still_usable() {
    let device = properties::default_output_device().expect("a default output after the kill");
    let uid = properties::device_uid(device).expect("its UID after the kill");
    let rate = properties::nominal_sample_rate(device).expect("its rate after the kill");
    assert!(
        rate > 0.0,
        "the default output reports {rate} Hz after the kill"
    );
    let mut aggregate = RenderAggregate::create(&uid)
        .expect("a fresh render aggregate over the default output after the kill");
    let errors = aggregate.teardown();
    assert!(
        errors.is_empty(),
        "teardown after the kill errored: {errors:?}"
    );
}

/// A scratch path under cargo's own test temp directory, which is cleaned with
/// the target directory rather than accumulating in `/tmp`.
fn scratch_path(label: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("b16");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir.join(format!("{label}-{}.wav", std::process::id()))
}

// ══════════════════════════════════ the tests ════════════════════════════════

/// **B0-2 / spike S4 — can the tap aggregate and the MS-22 measurement
/// aggregate be up at once on one default output device?**
///
/// `tap.rs::create_aggregate` composes the default output into the tap
/// aggregate and `measure_aggregate.rs` composes the same device again, and
/// nothing had ever run both through the live engine. This is the engine-level
/// form of the question; the HAL-level form is
/// `examples/hal_render_probe.rs --coexist`, which needs no controller and no
/// lease.
///
/// **What the owner should observe.** Four lines beginning `COEXIST:`. The
/// system mutes when the tap comes up and unmutes when it goes away; a system
/// sound loops throughout, because the tap's IOProc does not cycle while the
/// machine is idle (0 cb/s idle, ~94 cb/s during playback) and a coexistence
/// test with nothing playing would read as a failure of the tap rather than a
/// fact about the pair.
///
/// **Which owner question this answers: E6**, and, on a failure, **E12** — if
/// the two cannot coexist, the capture-side ladder is A → B2 (mic-only
/// aggregate + software resample + `Warn(TwoClock)`) → B1 (the mic inside the
/// TAP aggregate: better physics, but it rebuilds a live tap mid-session on the
/// one path whose failure mode is "the user's system is left muted"), and
/// choosing between B2 and B1 is the owner's call.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant; mutes system audio and loops a system sound"]
fn tap_and_measurement_aggregates_coexist_on_one_output_device() {
    let engine = RigEngine::up();
    let lease = with_engine(&engine.slot, EngineHandle::acquire_measurement_lease)
        .flatten()
        .expect("a fresh engine has no outstanding lease");

    // The tap's IOProc only cycles while the system renders audio, so the
    // coexistence question needs something playing to be asked at all.
    let playback = Playback::start();

    let mut mic = match MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    }) {
        Ok(mic) => mic,
        Err(e) => {
            drop(playback);
            drop(lease);
            engine.shut_down();
            panic!(
                "COEXIST: FAIL — MicCapture::create refused with the engine's tap aggregate live \
                 on the same output device: {e:?}\nThis is owner question E6 answering NO. \
                 Confirm the aggregate itself is healthy by running \
                 `measure_aggregate_lifecycle_roundtrip` with the engine stopped; if it passes, \
                 the capture-side fallback ladder is A -> B2 -> B1 and the choice between them \
                 is E12."
            );
        }
    };
    let tapped_device = engine
        .state()
        .stream
        .as_ref()
        .map_or_else(|| "?".to_owned(), |s| s.device_uid.clone());
    eprintln!(
        "COEXIST: both aggregates up — tap on '{tapped_device}', measurement mic '{}' at {} Hz",
        mic.mic_uid(),
        mic.sample_rate_hz(),
    );

    // Both must deliver callbacks, not merely exist.
    let tap_before = with_engine(&engine.slot, EngineHandle::tap_activity).flatten();
    let mic_before = mic.counters().callbacks;
    let mut scratch = vec![0.0f64; 8192];
    while cap_read(&mut mic, &mut scratch) > 0 {}
    std::thread::sleep(Duration::from_secs(3));
    let tap_after = with_engine(&engine.slot, EngineHandle::tap_activity).flatten();
    let mic_after = mic.counters().callbacks;
    eprintln!("COEXIST: tap activity {tap_before:?} -> {tap_after:?}, mic callbacks {mic_before} -> {mic_after}");

    // Tearing down the measurement aggregate must not break the tap.
    drop(mic);
    let tap_pre_teardown = with_engine(&engine.slot, EngineHandle::tap_activity).flatten();
    std::thread::sleep(Duration::from_secs(2));
    let tap_post_teardown = with_engine(&engine.slot, EngineHandle::tap_activity).flatten();
    eprintln!("COEXIST: after destroying the measurement aggregate, tap activity {tap_pre_teardown:?} -> {tap_post_teardown:?}");

    // Tearing down the tap must not break a measurement aggregate.
    let mut mic = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    });
    drop(playback);
    drop(lease);
    engine.shut_down();
    let mic_survived = match mic.as_mut() {
        Ok(mic) => {
            let before = mic.counters().callbacks;
            let deadline = Instant::now() + Duration::from_secs(5);
            while mic.counters().callbacks == before && Instant::now() < deadline {
                let _ = cap_read(mic, &mut scratch);
                std::thread::sleep(Duration::from_millis(20));
            }
            let after = mic.counters().callbacks;
            eprintln!(
                "COEXIST: after destroying the tap aggregate, mic callbacks {before} -> {after}"
            );
            after > before
        }
        Err(e) => {
            eprintln!(
                "COEXIST: the measurement aggregate would not come back a second time: {e:?}"
            );
            false
        }
    };
    drop(mic);

    // Asserted only now, with every device already torn down: a failed assert
    // must never leave a tap up and the system muted.
    let advanced = |before: Option<paraeq_engine::controller::TapActivity>,
                    after: Option<paraeq_engine::controller::TapActivity>| {
        matches!((before, after), (Some(b), Some(a)) if a.callbacks > b.callbacks)
    };
    assert!(
        advanced(tap_before, tap_after),
        "the tap stopped cycling while the measurement aggregate was up — the pair does not \
         coexist (E6/E12)"
    );
    assert!(
        mic_after > mic_before,
        "the measurement aggregate's IOProc never engaged while the tap was up — the pair does \
         not coexist (E6/E12)"
    );
    assert!(
        advanced(tap_pre_teardown, tap_post_teardown),
        "destroying the measurement aggregate stopped the tap: the verification teardown ORDER \
         is load-bearing, not tidy (E12)"
    );
    assert!(
        mic_survived,
        "destroying the tap aggregate stopped the measurement aggregate (E12)"
    );
}

/// **B0-3 / HW-2 — does `MutedWhenTapped` really mute the helper's RAW path?**
///
/// The only unknown that can kill the feature SILENTLY. The tap mutes the tapped
/// device and ParaEQ re-renders the corrected audio; if the raw path leaks
/// instead, the microphone hears raw **plus** corrected. The residual is then
/// meaningless and the acoustic level is up to **+6 dB over the solve** — a
/// safety miss, not a measurement miss.
///
/// One number, unambiguous: with a −18 dB cut at 1 kHz installed, play a 1 kHz
/// tone and a 300 Hz reference from the helper and report
/// `level(1 kHz) − level(300 Hz)` at the microphone. Mute working ⇒ roughly the
/// designed cut. Leaking ⇒ near the same difference the reference run has, i.e.
/// no cut at all.
///
/// **What the owner should observe.** One `HW-2:` line carrying the two tone
/// levels and their difference. [`helper_audio_really_is_corrected`] FAILS under
/// a leak but cannot say why; this is the diagnostic that says why, which is why
/// the plan runs them in the same sitting.
///
/// **This test prints its number and does not assert it**, and that is the
/// harness's own discipline rather than timidity. One run cannot separate the
/// correction from the transducer's own response between 1 kHz and 300 Hz, so a
/// room whose 1 kHz happens to sit quiet would report a total raw-path leak as a
/// pass. [`helper_audio_really_is_corrected`] differences two runs, which
/// cancels exactly that, and it is where the assertion lives.
///
/// **Which owner question this answers: E6** — it is the other half of "can the
/// two aggregates be up at once", namely "and is what comes out of the pair the
/// corrected signal".
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant; plays two quiet tones"]
fn muted_when_tapped_really_mutes_the_helper() {
    let engine = RigEngine::up();
    let lease = with_engine(&engine.slot, EngineHandle::acquire_measurement_lease)
        .flatten()
        .expect("a fresh engine has no outstanding lease");
    let rate = engine.stream_rate_hz();
    engine.install_probe_cut(rate);

    let device = properties::default_output_device().expect("a default output");
    let device_uid = properties::device_uid(device).expect("its UID");
    let device_rate = properties::nominal_sample_rate(device).expect("its rate");
    let mut mic = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("the measurement aggregate (see tap_and_measurement_aggregates_coexist...)");

    let wav = scratch_path("hw2-tones");
    write_wav_mono_f32(&wav, device_rate as u32, &probe_tone_pair(device_rate));
    let run = play_through_helper(&mut mic, &device_uid, &wav);
    let _ = std::fs::remove_file(&wav);

    let capture_rate = mic.sample_rate_hz();
    let at_cut = tone_level_dbfs(&run.capture, capture_rate, PROBE_CUT_FC_HZ);
    let at_reference = tone_level_dbfs(&run.capture, capture_rate, PROBE_REFERENCE_HZ);

    drop(mic);
    drop(lease);
    engine.shut_down();

    eprintln!(
        "HW-2: {} captured samples at {capture_rate} Hz; {PROBE_CUT_FC_HZ} Hz = {at_cut:.1} dBFS, \
         {PROBE_REFERENCE_HZ} Hz = {at_reference:.1} dBFS, difference = {:.1} dB against a \
         designed cut of {PROBE_CUT_GAIN_DB} dB.",
        run.capture.len(),
        at_cut - at_reference,
    );
    eprintln!(
        "HW-2: a difference near 0 dB means the tap did NOT mute the helper's raw path — the mic \
         is hearing raw + corrected, the residual is meaningless, and the acoustic level can sit \
         up to +6 dB over the solve. A difference near {PROBE_CUT_GAIN_DB} dB means the mute works. \
         Read it against `helper_audio_really_is_corrected`, which differences two runs and so \
         carries the ASSERTION; this number carries the room's own 1 kHz-versus-300 Hz tilt as \
         well as the cut, which is why it is printed rather than asserted."
    );
    // Asserted: only that a capture came back at all, which is a harness fact.
    // The DEPTH is not asserted here on purpose — see the doc comment: a single
    // run cannot separate the correction from the transducer's own response
    // between the two tones, and asserting it would let a room whose 1 kHz is
    // naturally quiet report a total raw-path leak as a pass.
    assert!(
        run.capture.len() > capture_rate as usize,
        "less than a second of capture came back ({} samples) — check mic placement and routing \
         before reading the numbers above",
        run.capture.len()
    );
}

/// **The premise of the entire verification feature, as a HAL fact rather than
/// a computation: the helper child's audio really is tapped and really is
/// corrected.**
///
/// ParaEQ's tap excludes ParaEQ's own process, so a sweep ParaEQ plays is never
/// corrected. Verification's whole design rests on a different pid not being in
/// that exclusion list. Nothing in this repo had tested it end to end against a
/// real transducer.
///
/// The same stimulus twice — once with the engine BYPASSED, once with the probe
/// cut armed — differing by the designed correction to a loose bound. Each leg
/// is read as `level(1 kHz) − level(300 Hz)`, so the transducer, the room and
/// the mic cancel within a leg; the two legs are then differenced, so anything
/// that drifted between them cancels again.
///
/// Note what the bypassed leg is NOT: it is not the helper playing untapped. The
/// tap is up in both legs and the raw path is muted in both; only the chain
/// differs. Under the REJECTED `SetGainDb` preamp carrier the bypassed leg would
/// have been attenuated too and this test would have been corrupted — one more
/// independent argument for putting the computed preamp inside the installed
/// correction.
///
/// **What the owner should observe.** Two `CORRECTED:` lines, one per leg, and a
/// third carrying their difference against the designed −18 dB.
///
/// **Which owner question this answers: E6.** A failure here with
/// [`muted_when_tapped_really_mutes_the_helper`] also failing means the raw path
/// leaks; a failure here with HW-2 passing means the helper is not being tapped
/// at all, which is spike S2's fallback (an AudioQueue client) rather than a bug.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant; plays two quiet tones twice"]
fn helper_audio_really_is_corrected() {
    let engine = RigEngine::up();
    let lease = with_engine(&engine.slot, EngineHandle::acquire_measurement_lease)
        .flatten()
        .expect("a fresh engine has no outstanding lease");
    let rate = engine.stream_rate_hz();
    engine.install_probe_cut(rate);

    let device = properties::default_output_device().expect("a default output");
    let device_uid = properties::device_uid(device).expect("its UID");
    let device_rate = properties::nominal_sample_rate(device).expect("its rate");
    let mut mic = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("the measurement aggregate");

    let wav = scratch_path("corrected-tones");
    write_wav_mono_f32(&wav, device_rate as u32, &probe_tone_pair(device_rate));
    let capture_rate = mic.sample_rate_hz();

    // Leg 1 — BYPASSED. The chain is flat; the tap is still up and the raw path
    // is still muted, so this measures the route and not the correction.
    engine.send(EngineCommand::SetBypass(true));
    std::thread::sleep(Duration::from_millis(500));
    assert!(engine.state().bypass, "the engine did not enter bypass");
    let bypassed = play_through_helper(&mut mic, &device_uid, &wav);
    let bypassed_shape = tone_level_dbfs(&bypassed.capture, capture_rate, PROBE_CUT_FC_HZ)
        - tone_level_dbfs(&bypassed.capture, capture_rate, PROBE_REFERENCE_HZ);

    // Leg 2 — CORRECTED. Only the chain changed.
    engine.send(EngineCommand::SetBypass(false));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!engine.state().bypass, "the engine did not leave bypass");
    let corrected = play_through_helper(&mut mic, &device_uid, &wav);
    let corrected_shape = tone_level_dbfs(&corrected.capture, capture_rate, PROBE_CUT_FC_HZ)
        - tone_level_dbfs(&corrected.capture, capture_rate, PROBE_REFERENCE_HZ);

    let _ = std::fs::remove_file(&wav);
    drop(mic);
    drop(lease);
    engine.shut_down();

    let delta = corrected_shape - bypassed_shape;
    eprintln!(
        "CORRECTED: bypassed leg — 1 kHz minus 300 Hz = {bypassed_shape:.1} dB ({} samples)",
        bypassed.capture.len()
    );
    eprintln!(
        "CORRECTED: corrected leg — 1 kHz minus 300 Hz = {corrected_shape:.1} dB ({} samples)",
        corrected.capture.len()
    );
    eprintln!(
        "CORRECTED: the correction moved the mic by {delta:.1} dB against a designed \
         {PROBE_CUT_GAIN_DB} dB cut. This is the premise of the verification feature, measured."
    );
    assert!(
        delta <= -PROBE_MIN_OBSERVED_CUT_DB,
        "the helper's audio is NOT being corrected: bypassing the engine changed the microphone \
         by only {delta:.1} dB against a {PROBE_CUT_GAIN_DB} dB cut. If \
         `muted_when_tapped_really_mutes_the_helper` also failed, the tap is not muting the \
         helper's raw path; if it passed, the tap is not capturing the helper at all, which is \
         spike S2's AudioQueue fallback rather than a bug."
    );
}

/// The helper's private render aggregate round-trips across one real run: it is
/// named in `ready`, it resolves while the child is playing, and it is gone once
/// the child has exited.
///
/// `tests/test_render.rs` round-trips [`RenderAggregate`] in-process, where
/// `Drop` is a local. This is the round trip that matters to the product: the
/// aggregate is created in ANOTHER process, and the parent's teardown asserts it
/// is gone by a UID it computes rather than by a handle it holds.
///
/// **What the owner should observe.** One `ROUNDTRIP:` line naming the render
/// device and the child's exit code, and a clean pass.
///
/// **Which owner question this answers: none directly** — it is the regression
/// fence that makes the E8 and E12 answers from the other tests trustworthy, by
/// proving that the CLEAN path leaks nothing.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant; plays two quiet tones"]
fn the_render_aggregate_round_trips_across_a_real_helper_run() {
    let engine = RigEngine::up();
    let lease = with_engine(&engine.slot, EngineHandle::acquire_measurement_lease)
        .flatten()
        .expect("a fresh engine has no outstanding lease");

    let device = properties::default_output_device().expect("a default output");
    let device_uid = properties::device_uid(device).expect("its UID");
    let device_rate = properties::nominal_sample_rate(device).expect("its rate");
    let mut mic = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("the measurement aggregate");

    let wav = scratch_path("roundtrip-tones");
    write_wav_mono_f32(&wav, device_rate as u32, &probe_tone_pair(device_rate));
    let run = play_through_helper(&mut mic, &device_uid, &wav);
    let _ = std::fs::remove_file(&wav);

    drop(mic);
    drop(lease);
    engine.shut_down();

    eprintln!(
        "ROUNDTRIP: render device '{}' was named in `ready`, carried the run, and is gone. \
         Child exit {:?}. Capture {} samples, {:.1} dBFS RMS.",
        run.render_device_uid,
        run.exit,
        run.capture.len(),
        rms_dbfs(&run.capture),
    );
    // `play_through_helper` already asserted the exit code and the device-gone
    // check; what is left is the fact only this test names.
    assert!(
        run.ready.contains(r#""event":"ready""#),
        "the ready line is not a ready event: {}",
        run.ready
    );
    assert!(
        !run.exit.signalled,
        "a clean run must not end in a signal death: {:?}",
        run.exit
    );
}

/// **The acoustic half of teardown step 1d: SIGKILL the helper mid-play and
/// assert nothing of it survives.**
///
/// `tests/test_render.rs::render_aggregate_is_destroyed_on_drop_and_on_panic`
/// proves the in-process `Drop`. This proves the LADDER's last rung, which
/// `Drop` cannot reach: SIGKILL bypasses unwinding entirely, so the child's
/// render-aggregate RAII never runs and the only thing that can clean up is the
/// HAL. The verification teardown treats that case as
/// `RenderDeviceLeaked` — conservatively, because nobody had measured it.
///
/// **What the owner should observe.** One `KILLED:` line. A PASS says the HAL
/// reaps a dead client's private aggregate and the default output is immediately
/// usable again. A FAIL is serious and the message says what to do: a private
/// aggregate is wrapping the output device and will stay there.
///
/// **Which owner question this answers: E8** — it prices the worst rung of the
/// abort ladder. If the aggregate survives a kill, the SIGKILL rung is not a
/// last resort but a hazard, and the ramp/SIGTERM deadlines above it must be
/// generous rather than tight.
#[test]
#[ignore = "requires audio hardware; plays a quiet tone briefly"]
fn the_render_aggregate_does_not_survive_a_killed_helper() {
    let device = properties::default_output_device().expect("a default output");
    let device_uid = properties::device_uid(device).expect("its UID");
    let device_rate = properties::nominal_sample_rate(device).expect("its rate");

    let wav = scratch_path("killed-tones");
    write_wav_mono_f32(&wav, device_rate as u32, &probe_tone_pair(device_rate));

    let mut child = HelperChild::spawn(&wav, &device_uid, "both").expect("spawn the helper");
    let pid = child.pid();
    let render_device_uid = expected_render_device_uid(pid);
    child
        .wait_for_event("ready", HELPER_READY_TIMEOUT)
        .expect("the helper opened its device");
    assert_ne!(
        properties::translate_uid_to_device(&render_device_uid).expect("the lookup succeeds"),
        0,
        "the helper said `ready` but no device carries '{render_device_uid}' — the parent's \
         teardown assertion is looking for the wrong name"
    );

    child.send_play().expect("the parent commits");
    std::thread::sleep(Duration::from_millis(500));

    // Rung 1c, alone and out of order on purpose: this test is about what the
    // LAST rung leaves behind, so the polite rungs above it are skipped.
    HelperProcess::kill(&mut child).expect("SIGKILL the helper");
    let exit = child.reap().expect("reap the killed helper");
    let _ = std::fs::remove_file(&wav);

    eprintln!("KILLED: child {pid} exit {exit:?} (a signal death means no unwind, so the child's own Drop never ran)");
    assert!(
        exit.signalled,
        "the child was SIGKILLed but did not die of a signal: {exit:?}"
    );

    assert_render_device_gone(&render_device_uid);
    assert_default_output_still_usable();
    eprintln!(
        "KILLED: PASS — no device carries '{render_device_uid}' and the default output still \
         opens a fresh private wrapper. The HAL reaps a dead client's aggregate, so \
         RenderDeviceLeaked is conservative rather than wrong."
    );
}

/// **The SIGTERM rung is a second RAMP request, not a stop — and its cost is
/// recorded separately, which is what owner question E8 needs.**
///
/// `crates/paraeq-stimulus/tests/test_signals.rs` pins the child's half: with a
/// handler installed, SIGTERM fades rather than killing. This pins the same
/// mechanism from the PARENT's side of the seam — through
/// [`HelperProcess::request_terminate`], the method the teardown ladder actually
/// calls — and measures the wall time the rung adds.
///
/// **What the owner should observe.** One `SIGTERM:` line carrying the
/// milliseconds from the signal to the child's exit. That number is the SIGTERM
/// rung's contribution to the abort budget, which the ladder pays once between
/// the polite `abort` write and the kill, and it is reported separately from the
/// rest of the budget so the two can be priced independently.
///
/// **Which owner question this answers: E8** — "what wall-clock figure from
/// `abort` to mic silence is acceptable on your rig, and does it differ for IEMs
/// versus speakers?" cannot be answered without knowing what each rung costs.
#[test]
#[ignore = "requires audio hardware; plays a quiet tone briefly"]
fn the_sigterm_rung_ramps_the_child_and_its_cost_is_recorded() {
    let device = properties::default_output_device().expect("a default output");
    let device_uid = properties::device_uid(device).expect("its UID");
    let device_rate = properties::nominal_sample_rate(device).expect("its rate");

    let wav = scratch_path("sigterm-tones");
    write_wav_mono_f32(&wav, device_rate as u32, &probe_tone_pair(device_rate));

    let mut child = HelperChild::spawn(&wav, &device_uid, "both").expect("spawn the helper");
    let pid = child.pid();
    let render_device_uid = expected_render_device_uid(pid);
    child
        .wait_for_event("ready", HELPER_READY_TIMEOUT)
        .expect("the helper opened its device");
    child.send_play().expect("the parent commits");
    std::thread::sleep(Duration::from_millis(500));

    let signalled_at = Instant::now();
    child.request_terminate().expect("SIGTERM the helper");
    let aborted = child.wait_for_event("aborted", Duration::from_secs(5));
    let exit = child.reap().expect("reap the helper");
    let rung_ms = signalled_at.elapsed().as_secs_f64() * 1000.0;
    let _ = std::fs::remove_file(&wav);

    assert_render_device_gone(&render_device_uid);

    eprintln!("SIGTERM: rung cost {rung_ms:.1} ms from the signal to the child's exit (pid {pid}, exit {exit:?})");
    eprintln!("SIGTERM: child said {aborted:?}");
    eprintln!(
        "SIGTERM: this is ONE rung. The full acoustic budget the pass reports is one helper \
         block + the 5 ms ramp + the engine's own latency + slack; the two are recorded \
         separately so E8 can price them separately."
    );
    assert_eq!(
        exit.code,
        Some(0),
        "SIGTERM must be HANDLED, not fatal: a signal death means the child had no handler, which \
         means it did not ramp and its render aggregate's Drop never ran"
    );
    assert!(
        aborted.is_some(),
        "the child exited 0 but never reported `aborted` — it must FADE on SIGTERM, because a \
         hard stop is itself a full-scale click"
    );
}

/// **R15's acoustic half, and rig step S9: the verification gates arm on a
/// genuinely quiet machine.**
///
/// Gate 2 reads `engine_engaged()` rather than `status == Running`, and the
/// argument for that is made from two source files: the watchdog sets `Running`
/// only while nonzero input is advancing, while the verification pre-roll
/// REQUIRES stationary input, so a gate written against the literal word would
/// refuse a healthy engine on every single run. This is the one-minute
/// experiment that confirms the argument against the real watchdog, whose status
/// at this moment will in fact be `Idle` or `InputSilent`.
///
/// It arms and then finishes: no helper is spawned and no sample is emitted, by
/// construction — `arm()` runs gates 1–6 and the pre-spawn routing fence and
/// stops there. Run it BEFORE believing the gate order in
/// `crates/paraeq-measure/tests/test_verify.rs`, which proves the same sequence
/// against mocks.
///
/// **What the owner should observe.** One `ARMED:` line carrying the engine's
/// status at the moment the gates ran and the level they decided. If this fails,
/// `engine_engaged()`'s definition is wrong and everything downstream is
/// untestable — which is exactly why it is cheap and early.
///
/// **Which owner question this answers: E6's precondition**, and it is the
/// evidence behind the escalation that `wizard`'s "Engine is Running" cannot be
/// read literally.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant; mutes system audio, plays nothing"]
fn the_verification_gates_arm_on_a_genuinely_quiet_machine() {
    let engine = RigEngine::up();
    let rate = engine.stream_rate_hz();
    let bands = engine.install_probe_cut(rate);
    let status_at_arm = format!("{:?}", engine.state().status);

    let device = properties::default_output_device().expect("a default output");
    let device_uid = properties::device_uid(device).expect("its UID");
    let mic = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("the measurement aggregate");
    let volume = DeviceVolume::for_default_output().expect("default output device");

    // The noise floor handed to gate 6 is FABRICATED quiet, and deliberately:
    // this test is about gate 2 — whether `engine_engaged()` admits a healthy
    // engine on a silent machine — and a room too noisy for the SNR budget
    // would refuse at gate 6 first and mask the answer. The end-to-end test
    // below measures the real floor, because there the number is load-bearing.
    const FABRICATED_QUIET_FLOOR_DBFS: f64 = -90.0;

    let request = rig_verify_request(
        &device_uid,
        rate,
        &bands,
        FABRICATED_QUIET_FLOOR_DBFS,
        scratch_path("armed-sweep"),
    );
    let armed = VerificationPass::arm(
        request,
        VerifySeam {
            capture: Box::new(WaitingCapture { inner: mic }),
            control: engine.control(),
            devices: Box::new(RigDeviceFacts),
            engine: engine.facts(),
            helper: Box::new(RigHelper),
            tap: Box::new(engine.witness.clone()),
            volume: Box::new(volume),
        },
    );

    // Finish (or drop) BEFORE asserting: a failed assert must never leave a
    // lease held, the user's trim pinned or the system volume where the pass
    // put it.
    let outcome = match armed {
        Ok(pass) => {
            let level = pass.level();
            let projected = pass.projected_spl_db();
            let log = pass.finish();
            Ok((level, projected, log))
        }
        Err(failure) => Err(failure),
    };
    engine.shut_down();

    match outcome {
        Ok((level, projected, log)) => {
            eprintln!(
                "ARMED: the gates passed with the engine reporting {status_at_arm}. L_verify = \
                 {level:?} dBFS RMS, projected {projected:.1} dB SPL (SYNTHETIC). Log: {:#?}",
                log.events()
            );
        }
        Err(failure) => panic!(
            "ARMED: FAIL — the verification gates refused on a quiet machine with the engine \
             reporting {status_at_arm}: {}\nIf the refusal is EngineNotRunning, `engine_engaged()` \
             is wrong and every gate order downstream is re-derived from here. Log: {:#?}",
            failure.error,
            failure.log.events(),
        ),
    }
}

/// **The whole verification loop, end to end, through the real seams.**
///
/// Engine engaged with a known correction; the gates arm; the MS-18
/// acknowledgement is recorded; `paraeq-stimulus` plays the bracketed sweep from
/// a DIFFERENT pid, so the tap captures it and `RealtimeChain` corrects it; the
/// mic records; the four markers locate `t = 0`; deconvolution yields the
/// corrected impulse response. The residual is **printed, not asserted** — its
/// threshold is an owner question, and the number that comes back is a property
/// of the rig.
///
/// Every seam is the production one except `EngineFacts`/`EngineControl` and
/// `StimulusHelper`, whose production impls live in the desktop crate; see this
/// section's header for why that is a compromise and not a choice.
///
/// **What the owner should observe.** A `PASS:` line per stage, then a summary
/// carrying: the capture's peak and clip counts (MS-21, the FAR side of the
/// transducer), the engine's armed preamp, `L_verify`, the two-clock fit
/// (skew ppm and residual samples), the abort acoustic budget the pass computed,
/// and every non-blocking warning. If the SNR budget refuses before arming, the
/// room is too noisy for a fixed quiet level and the test SKIPs with the numbers
/// — raising the level to fix it is exactly what MS-8 forbids ("remedies never
/// touch output level").
///
/// **Which owner questions this answers: E6** (the pair coexists under a real
/// pass, not just a probe), **E8** (it prints the computed
/// `abort_acoustic_budget_ms` for a real rig) and, through the printed two-clock
/// fit, the retune data behind **E11**.
#[test]
#[ignore = "requires audio hardware + input device + mic TCC grant; plays a quiet sweep"]
fn the_full_verification_pass_runs_end_to_end_on_the_rig() {
    let engine = RigEngine::up();
    let rate = engine.stream_rate_hz();
    let bands = engine.install_probe_cut(rate);

    let device = properties::default_output_device().expect("a default output");
    let device_uid = properties::device_uid(device).expect("its UID");
    let device_name = devices::list_output_devices()
        .ok()
        .and_then(|list| {
            list.into_iter()
                .find(|d| d.uid == device_uid)
                .map(|d| d.name)
        })
        .unwrap_or_else(|| device_uid.clone());
    let mut mic = MicCapture::create(MeasureAggregateConfig {
        mic: mic_selector_from_env(),
        ..MeasureAggregateConfig::default()
    })
    .expect("the measurement aggregate");
    eprintln!(
        "PASS: engine up at {rate} Hz on '{device_name}', mic '{}'",
        mic.mic_uid()
    );

    // The noise floor, measured rather than assumed: gate 6 refuses a level that
    // cannot produce a meaningful residual, and the honest input to that gate is
    // this room's floor right now.
    let mut scratch = vec![0.0f64; 8192];
    while cap_read(&mut mic, &mut scratch) > 0 {}
    let mut floor_samples = Vec::new();
    let until = Instant::now() + Duration::from_millis(1_000);
    while Instant::now() < until {
        let n = cap_read(&mut mic, &mut scratch);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(2));
        } else {
            floor_samples.extend_from_slice(&scratch[..n]);
        }
    }
    let noise_floor_dbfs = rms_dbfs(&floor_samples);
    let projected_snr_db = RIG_L_MEASURE_DBFS - noise_floor_dbfs;
    eprintln!("PASS: noise floor {noise_floor_dbfs:.1} dBFS, so L_verify projects {projected_snr_db:.1} dB of SNR");
    if projected_snr_db < VERIFY_MIN_SNR_DB {
        drop(mic);
        engine.shut_down();
        eprintln!(
            "SKIP: this room's noise floor ({noise_floor_dbfs:.1} dBFS) cannot support the fixed \
             quiet level this test uses ({RIG_L_MEASURE_DBFS} dBFS needs a floor at or below \
             {:.1} dBFS). Quieten the room or move the mic closer. Raising the level instead is \
             what MS-8 forbids — remedies never touch output level.",
            RIG_L_MEASURE_DBFS - VERIFY_MIN_SNR_DB
        );
        return;
    }

    let volume = DeviceVolume::for_default_output().expect("default output device");
    let wav_path = scratch_path("verify-sweep");
    let request = rig_verify_request(
        &device_uid,
        rate,
        &bands,
        noise_floor_dbfs,
        wav_path.clone(),
    );
    let mut pass = VerificationPass::arm(
        request,
        VerifySeam {
            capture: Box::new(WaitingCapture { inner: mic }),
            control: engine.control(),
            devices: Box::new(RigDeviceFacts),
            engine: engine.facts(),
            helper: Box::new(RigHelper),
            tap: Box::new(engine.witness.clone()),
            volume: Box::new(volume),
        },
    )
    .unwrap_or_else(|failure| {
        engine.send(EngineCommand::Disable);
        panic!(
            "PASS: the gates refused before anything was spawned: {}\nLog: {:#?}",
            failure.error,
            failure.log.events()
        )
    });
    eprintln!(
        "PASS: armed — L_verify {:?}, projected {:.1} dB SPL (SYNTHETIC)",
        pass.level(),
        pass.projected_spl_db()
    );

    let acknowledged = pass.acknowledge(&device_name, pass.projected_spl_db());
    let outcome = acknowledged.and_then(|()| pass.run());
    let log = pass.finish();
    let _ = std::fs::remove_file(&wav_path);
    engine.shut_down();

    match outcome {
        Ok(outcome) => {
            eprintln!(
                "PASS: completed. level {:.2} dBFS RMS, installed preamp {:.3} dB, trim {:.1} dB, \
                 running rate {} Hz, IR {} samples",
                outcome.level_dbfs,
                outcome.installed_preamp_db,
                outcome.gain_db,
                outcome.running_rate_hz,
                outcome.ir.samples.len(),
            );
            eprintln!(
                "PASS: capture (MS-21, the FAR side of the transducer) peak {:.1} dBFS, rms \
                 {:.1} dBFS, {} clipped samples",
                outcome.capture.peak_dbfs,
                outcome.capture.rms_dbfs,
                outcome.capture.clipped_samples,
            );
            eprintln!("PASS: two-clock fit {:?}", outcome.two_clock);
            eprintln!(
                "PASS: abort acoustic budget {:.1} ms (computed, never asserted — the acceptance \
                 bound is owner question E8)",
                outcome.abort_acoustic_budget_ms
            );
            eprintln!("PASS: warnings {:?}", outcome.warnings);
        }
        Err(e) => panic!(
            "PASS: the verification run refused: {e}\nRead the log below; \
             TapSilentDuringVerification means the helper's audio never reached the tap (spike \
             S2), SystemAudioNotQuiet means something else was playing, and \
             VerificationChainClipped means the engine's own output clamp fired during the \
             sweep.\nLog: {:#?}",
            log.events()
        ),
    }
}

/// The request both `VerificationPass` tests build, in one place so the two
/// cannot drift about the level, the cal or the layout.
///
/// **Everything here except the level and the bands is synthetic**, exactly as
/// it is in `a_full_measurement_session_runs_against_the_live_witness`: there is
/// no cal-file loader and no ladder run, so a solved level would be solved from
/// a fabricated sensitivity — and a solve driven by a fake cal against a distant
/// mic asks for a LOUDER level, which is the whole reason the caps exist.
/// Choosing the numbers fixed and quiet is the safety decision.
fn rig_verify_request(
    device_uid: &str,
    stream_rate_hz: f64,
    bands: &[Vec<EQBand>],
    noise_floor_dbfs: f64,
    wav_path: PathBuf,
) -> VerifyRequest {
    let cal = CalSummary::validate(
        TransducerClass::OverEar,
        "synthetic (no cal-file loader exists yet)".to_owned(),
        CalSensitivity::Parsed(-18.0),
        1.0,
    )
    .expect("the synthetic cal validates");
    let l_measure = SweepLevel::new(RIG_L_MEASURE_DBFS, TransducerClass::OverEar)
        .expect("the fixed quiet level is legal for the class");
    let f_end_hz = (0.45 * stream_rate_hz).min(20_000.0);
    VerifyRequest {
        // `Both` on both sides, which gate 9 requires to be equal. It is legal
        // here only because the probe cut is identical per channel: a `Both`
        // capture hears the SUM of the channels, and a sum of
        // differently-corrected channels is not the response of any band set.
        baseline_routing: HelperRouting::Both,
        cal,
        device_uid: device_uid.to_owned(),
        input_gain_read_back: 1.0,
        l_measure,
        l_measure_projected_spl_db: RIG_PROJECTED_SPL_DB,
        layout: DEFAULT_MARKER_LAYOUT,
        noise_floor_dbfs,
        plan: VerifyPlan {
            bands: bands.to_vec(),
            design_rate_hz: stream_rate_hz,
            // Provenance only. Gate 2 recomputes the preamp at the LIVE rate
            // over these bands and compares THAT against the engine's armed
            // number; this field is never what the level book uses.
            preamp_db: 0.0,
        },
        position_index: 0,
        routing: HelperRouting::Both,
        sweep: SweepShape {
            duration_s: RIG_SWEEP_SECONDS,
            f_end_hz,
            f_start_hz: 20.0,
            sample_rate_hz: stream_rate_hz as u32,
        },
        timing: VerifyTiming::default(),
        wav_path,
    }
}

/// Keeps the system rendering audio for the guard's lifetime by looping
/// `afplay` on a builtin sound — the same shape `tests/test_hardware.rs` uses,
/// and for the same reason: the tap aggregate's IOProc delivers NO callbacks
/// while the system is idle (0 cb/s idle, ~94 cb/s during playback), so any test
/// that counts tap callbacks must drive playback itself.
struct Playback {
    playing: Arc<AtomicBool>,
    player: Option<std::thread::JoinHandle<()>>,
}

impl Playback {
    fn start() -> Playback {
        let playing = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&playing);
        let player = std::thread::spawn(move || {
            while flag.load(Ordering::Relaxed) {
                let _ = Command::new("afplay")
                    .arg("/System/Library/Sounds/Submarine.aiff")
                    .status();
            }
        });
        Playback {
            playing,
            player: Some(player),
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        if let Some(player) = self.player.take() {
            let _ = player.join();
        }
    }
}
