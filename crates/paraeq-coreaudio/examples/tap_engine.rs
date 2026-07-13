//! `tap_engine` — the stage-3 acceptance artifact and the tap spike's
//! successor, running the PRODUCTION stack end to end: `EngineHandle`
//! (controller + watchdog + snapshots) driving `TapBackend` (tap +
//! aggregate + IOProc), with the correction designed by `paraeq-dsp`.
//!
//! ```text
//! cargo run -p paraeq-coreaudio --example tap_engine --release -- \
//!     [--fc HZ] [--gain DB] [--q Q]    peaking band (default 80 / +9 / 1)
//!     [--fir]                          min-phase FIR instead of the IIR biquad
//!     [--frames N]                     requested buffer frame size
//!     [--bypass-every SECS]            A/B toggle bypass on a timer
//!     [--secs N]                       exit after N seconds (0 = Ctrl-C)
//! ```
//!
//! Run from a TCC-granted terminal (System Audio Recording) to hear the EQ;
//! without the grant the tap delivers silent zeros (`input_peak` stays 0.0
//! and the status hints `NoInputDetected`) but the stream still cycles, so
//! latency/CPU numbers remain valid. The IOProc only cycles while the
//! system renders audio — play something (hardware finding 2026-07-11).
//! `RUST_LOG=paraeq_coreaudio=debug` shows backend lifecycle logs.
//!
//! ## Measured latency + CPU (2026-07-11)
//!
//! MacBook (Darwin 24.6.0), BuiltInSpeakerDevice @ 48 kHz stereo, release
//! build, `afplay` driving playback, TCC-silent terminal (input zeros run
//! the full DSP path, so latency/CPU stay representative). Latency is the
//! engine's `latency_ms` (out/in sample-time delta); %CPU is steady-state
//! `ps -o %cpu= -p <pid>` for the whole process, sampled at 1 Hz over an
//! 8 s run. Spike baseline: 62.3 ms at the 512-frame default.
//!
//! | frames | latency_ms | %CPU (iir) | %CPU (fir, 4096-tap min-phase) |
//! |--------|------------|------------|--------------------------------|
//! | 512    | 62.3       | ~0.0       | 0.2-0.4                        |
//! | 256    | 51.6       | ~0.0       | —                              |
//! | 128    | 46.3       | ~0.0       | —                              |
//!
//! Each frame-size halving removes 2x the buffer delta (input + output
//! legs), extrapolating to a ~41 ms fixed floor for the tap -> aggregate ->
//! output path itself; the 20-30 ms spec budget is not reachable by buffer
//! sizing alone (recorded honestly per the spike doc). CPU is negligible on
//! both paths: the IIR runs read 0.0% on every sample (one 0.1% blip), the
//! 4096-tap FIR at 512 frames reads 0.2-0.4%.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};

use paraeq_coreaudio::backend::TapBackend;
use paraeq_dsp::biquad;
use paraeq_dsp::fir::{design_fir_correction, FirPhase};
use paraeq_engine::controller::{
    CorrectionConfig, EngineCommand, EngineConfig, EngineHandle, EngineState,
};
use paraeq_engine::status::EngineStatus;

/// Points on the linear 0..Nyquist grid the peaking response is sampled
/// onto before FIR frequency-sampling design.
const FIR_GRID_POINTS: usize = 2048;

/// Minimum-phase FIR length (the prototype's default `n_taps`); big enough
/// to exercise the overlap-add convolver for real.
const FIR_TAPS: usize = 4096;

struct Cli {
    bypass_every: Option<f64>,
    fc: f64,
    fir: bool,
    frames: Option<usize>,
    gain: f64,
    q: f64,
    secs: u64,
}

fn parse_cli(args: &[String]) -> Cli {
    let mut c = Cli {
        bypass_every: None,
        fc: 80.0,
        fir: false,
        frames: None,
        gain: 9.0,
        q: 1.0,
        secs: 0,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bypass-every" => {
                i += 1;
                c.bypass_every = Some(args[i].parse().expect("--bypass-every SECS"));
            }
            "--fc" => {
                i += 1;
                c.fc = args[i].parse().expect("--fc HZ");
            }
            "--fir" => c.fir = true,
            "--frames" => {
                i += 1;
                c.frames = Some(args[i].parse().expect("--frames N"));
            }
            "--gain" => {
                i += 1;
                c.gain = args[i].parse().expect("--gain DB");
            }
            "--help" => {
                println!(
                    "usage: tap_engine [--fc HZ] [--gain DB] [--q Q] [--fir] \
                     [--frames N] [--bypass-every SECS] [--secs N]"
                );
                std::process::exit(0);
            }
            "--q" => {
                i += 1;
                c.q = args[i].parse().expect("--q Q");
            }
            "--secs" => {
                i += 1;
                c.secs = args[i].parse().expect("--secs N");
            }
            other => eprintln!("ignoring arg: {other}"),
        }
        i += 1;
    }
    c
}

/// Build the correction for the live stream geometry. IIR: the peaking SOS
/// per channel. FIR: sample the SAME peaking response onto a linear
/// 0..Nyquist grid (`sos_frequency_response_db`) and design a min-phase FIR
/// from it (`design_fir_correction`) — the convolver path, same audible EQ.
fn correction_for(cli: &Cli, sample_rate: f64, channels: usize) -> CorrectionConfig {
    let sos = biquad::peaking(cli.fc, cli.gain, cli.q, sample_rate);
    if cli.fir {
        let nyquist = sample_rate / 2.0;
        let freqs: Vec<f64> = (0..FIR_GRID_POINTS)
            .map(|i| i as f64 * nyquist / (FIR_GRID_POINTS - 1) as f64)
            .collect();
        let response_db = biquad::sos_frequency_response_db(&[sos], &freqs, sample_rate);
        let fir = design_fir_correction(&response_db, FIR_TAPS, FirPhase::Minimum);
        // One FIR: the convolver reuses the last filter for extra channels.
        CorrectionConfig::Fir { firs: vec![fir] }
    } else {
        CorrectionConfig::Iir {
            sos_per_channel: vec![vec![sos]; channels],
        }
    }
}

fn fmt_latency(latency_ms: Option<f64>) -> String {
    match latency_ms {
        Some(v) => format!("{v:.1}"),
        None => "n/a".to_string(),
    }
}

fn snapshot_line(s: &EngineState) -> String {
    let stream = match &s.stream {
        Some(i) => format!(
            "{}ch@{}Hz/{}f ({})",
            i.channels, i.sample_rate, i.buffer_frames, i.device_uid
        ),
        None => "none".to_string(),
    };
    format!(
        "status={:?} bypass={} gain_db={:.1} correction={:?} stream={} latency_ms={} peak={:.4}",
        s.status,
        s.bypass,
        s.gain_db,
        s.correction,
        stream,
        fmt_latency(s.latency_ms),
        s.input_peak,
    )
}

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = parse_cli(&args);

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .expect("install Ctrl-C handler");
    }

    println!(
        "tap_engine: peaking fc={} Hz gain={} dB q={} path={} frames={:?} \
         bypass_every={:?} secs={} (0 = until Ctrl-C)",
        cli.fc,
        cli.gain,
        cli.q,
        if cli.fir { "fir(min-phase)" } else { "iir" },
        cli.frames,
        cli.bypass_every,
        cli.secs,
    );

    // Spawn auto-starts the backend; correction is sent once the stream
    // geometry (sample rate) is known, and the controller retains it across
    // rebuilds after that.
    let handle = EngineHandle::spawn(
        TapBackend::new(),
        EngineConfig {
            requested_buffer_frames: cli.frames,
            ..EngineConfig::default()
        },
    );
    let snapshots = handle.subscribe();
    println!("[state] {}", snapshot_line(&handle.state()));

    let started = Instant::now();
    let mut last_telemetry = Instant::now();
    let mut last_bypass_flip = Instant::now();
    let mut bypass = false;
    let mut correction_sent = false;

    while !stop.load(Ordering::SeqCst) {
        if cli.secs > 0 && started.elapsed() >= Duration::from_secs(cli.secs) {
            break;
        }

        // Print every published snapshot change.
        match snapshots.recv_timeout(Duration::from_millis(100)) {
            Ok(snap) => {
                println!("[change] {}", snapshot_line(&snap));
                if let EngineStatus::AutoDisabledNoInput { after_ms } = snap.status {
                    println!(
                        "[hint] no system audio captured in {:.0}s -- engine disabled itself \
                         (audio restored). If music WAS playing, grant System Audio Recording \
                         to this terminal (System Settings -> Privacy & Security -> Screen & \
                         System Audio Recording), fully relaunch it, and re-run.",
                        after_ms as f64 / 1000.0,
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if !correction_sent {
            let state = handle.state();
            if let Some(stream) = &state.stream {
                let config = correction_for(&cli, stream.sample_rate, stream.channels);
                let what = match &config {
                    CorrectionConfig::Fir { firs } => {
                        format!("min-phase FIR, {} taps", firs[0].len())
                    }
                    CorrectionConfig::Iir { sos_per_channel } => {
                        format!("IIR, {} band(s) per channel", sos_per_channel[0].len())
                    }
                };
                println!(
                    "[correction] designed for {} Hz: {what}",
                    stream.sample_rate
                );
                handle.send(EngineCommand::SetCorrection(config));
                correction_sent = true;
            }
        }

        if let Some(every) = cli.bypass_every {
            if last_bypass_flip.elapsed() >= Duration::from_secs_f64(every) {
                bypass = !bypass;
                println!("[bypass] {bypass}");
                handle.send(EngineCommand::SetBypass(bypass));
                last_bypass_flip = Instant::now();
            }
        }

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            let s = handle.state();
            println!(
                "[telemetry] status={:?} input_peak={:.4} latency_ms={}",
                s.status,
                s.input_peak,
                fmt_latency(s.latency_ms),
            );
            last_telemetry = Instant::now();
        }
    }

    // Disable tears the tap down (device unmuted) before the handle drop
    // sends Shutdown and joins the controller.
    println!("shutting down: Disable -> Shutdown");
    handle.send(EngineCommand::Disable);
    drop(handle);
}
