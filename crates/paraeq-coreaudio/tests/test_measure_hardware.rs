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
