//! The ParaEQ verification stimulus helper — the library half.
//!
//! # Why this process exists
//!
//! ParaEQ's tap excludes ParaEQ's own process, so a sweep ParaEQ plays never
//! reaches `RealtimeChain` and is never corrected. That exclusion is the design
//! goal for the BASELINE measurement — it is what keeps correction state out of
//! the measurement — and it is exactly the obstacle for verification, which has
//! to measure the CORRECTED output. measurement-safety's resolution is this
//! binary: a different pid is not in the exclusion list, so its audio is
//! tapped, corrected, and played to the device. Same sweep, two processes, and
//! the difference between the two captures is the correction the engine is
//! actually applying.
//!
//! # What it deliberately does not know
//!
//! No level policy, no transducer classes, no session state. The WAV *is* the
//! level: the parent solved it, wrote it, and this process plays those bytes.
//! There is no `--level` flag and never will be, so there is nothing to pass
//! and nothing to mis-pass. What this process does carry is one independent
//! interlock — a windowed level backstop that runs BEFORE any device is opened
//! (see [`backstop`]) — and its own MS-4 emit guard, because the stimulus path
//! cannot borrow anyone else's clamps.
//!
//! # The invariant every exit path keeps
//!
//! Ramp, then destroy the render device. Never a hard stop, which is itself a
//! full-scale click, and never a leaked device — a private aggregate left
//! behind is one wrapping the user's output. Three triggers (`abort` on stdin,
//! EOF, SIGTERM) all write ONE flag, so all three fade.
//!
//! Everything lives in this library so `tests/` can drive it with no process
//! and no device; `main.rs` is a shim. Nothing here is `pub(crate)`: an
//! integration test is a separate crate and sees only `pub`.
//!
//! This crate carries exactly ONE `unsafe` block — the `libc::signal` call in
//! [`signals::arm`] — and the lint below makes sure a second one cannot arrive
//! undocumented. Installing a POSIX signal handler is not CoreAudio FFI, which
//! is what the standing "only `paraeq-coreaudio` has unsafe CoreAudio FFI" rule
//! is about.

#![warn(clippy::undocumented_unsafe_blocks)]

pub mod backstop;
pub mod player;
pub mod protocol;
pub mod ramp;
pub mod signals;
pub mod wav;

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use paraeq_coreaudio::render::{DeviceRenderer, RenderConfig, RenderError, RenderTarget};

use crate::player::Outcome;
use crate::protocol::{Args, Command, Event, ExitCode, Invocation, Refusal};

/// The whole program, minus process setup.
///
/// Returns the exit code instead of calling `std::process::exit`, so a test can
/// assert on it without spawning anything.
pub fn run(args: impl IntoIterator<Item = OsString>) -> u8 {
    let args = match protocol::parse(args) {
        Ok(Invocation::Help) => {
            print!("{}", protocol::USAGE);
            return ExitCode::Ok.code();
        }
        Ok(Invocation::Run(args)) => args,
        Err(refusal) => return refuse(&refusal, false),
    };
    match play(&args) {
        Ok(code) => code,
        Err(refusal) => refuse(&refusal, args.json),
    }
}

/// Report a refusal on stderr, and on stdout too when the parent asked for
/// JSON — the parent reads one stream and must not have to correlate two.
fn refuse(refusal: &Refusal, json: bool) -> u8 {
    eprintln!("paraeq-stimulus: {refusal}");
    if json {
        emit(&Event::Error {
            code: refusal.code.code(),
            message: refusal.message.clone(),
        });
    }
    refusal.code.code()
}

fn emit(event: &Event) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", protocol::encode(event));
    let _ = stdout.flush();
}

/// The two-phase sequence, in order. Each step is a gate, and the ORDER is the
/// interesting part: the backstop runs before any device is opened, and `ready`
/// is emitted before any audio.
fn play(args: &Args) -> Result<u8, Refusal> {
    // SIGTERM must be handled before anything can be interrupted. Without a
    // handler the default disposition terminates without unwinding, so the
    // render device's `Drop` never runs.
    signals::arm();

    // 1. The file, refused for shape rather than reinterpreted.
    let file = wav::read(&args.wav)?;

    // 2. The independent level interlock, BEFORE a device exists. A hot file
    //    must be refused by a process that has opened nothing, or the refusal
    //    arrives after the hazard.
    backstop::check(&file.samples, file.sample_rate_hz)?;

    // 3. The device: a private wrapper around the named physical output.
    let renderer = DeviceRenderer::open(RenderConfig {
        target: RenderTarget::Uid(args.device_uid.clone()),
        routing: args.routing,
        ..RenderConfig::default()
    })
    .map_err(open_refusal)?;
    let format = paraeq_coreaudio::RenderSink::format(&renderer);
    let channels = format.channels as u32;

    // 4. The routing bound, refused before `ready` and before `play`.
    protocol::check_channel_bound(args.routing, channels)?;

    // 5. The rate, never reconciled by resampling.
    wav::check_rate(
        file.sample_rate_hz,
        format.sample_rate_hz,
        renderer.sub_device_sample_rate_hz(),
    )?;

    // 6. The route: the tap watches the default output, so rendering anywhere
    //    else measures a path the engine is not on.
    check_default_output(renderer.device_uid())?;

    if args.json {
        emit(&Event::Ready {
            channels,
            device_uid: renderer.device_uid().to_owned(),
            frames_per_block: format.frames_per_block,
            render_device_uid: renderer.render_device_uid().to_owned(),
            routing: protocol::routing_label(args.routing),
            sample_rate_hz: format.sample_rate_hz,
            sub_device_sample_rate_hz: renderer.sub_device_sample_rate_hz(),
        });
    }

    // 7. Wait for the parent to commit. A dedicated blocking thread, never a
    //    poll on the render path: `abort` latency should be a scheduler hop,
    //    not a block period.
    let commands = spawn_stdin_reader();
    let mut player = player::Player::new(Box::new(renderer));
    match commands.recv() {
        Ok(Command::Play) => {}
        Ok(Command::Abort) | Err(_) => {
            // Nothing has played, so there is nothing to fade — but the device
            // is open, and it must not outlive this decision.
            player.stop();
            if args.json {
                emit(&Event::Aborted {
                    frames_emitted: 0,
                    ramp_frames: 0,
                });
            }
            return Ok(ExitCode::Ok.code());
        }
        Ok(Command::Unknown) => {
            // Unknown lines are ignored, never fatal. Wait again rather than
            // guessing what the parent meant. The VETTED file goes with it —
            // see `wait_then_play`.
            return wait_then_play(args, &file, player, commands);
        }
    }
    emit_started(args);
    let outcome = player.play(&file.samples, &abort_poll(&commands), |frames| {
        if args.json {
            emit(&Event::Progress {
                frames_emitted: frames,
            });
        }
    })?;
    finish(args, outcome)
}

/// Keep waiting for a decision after an unrecognized line.
///
/// **`file` is the already-vetted `Wav` from [`play`], and it is a parameter
/// for exactly that reason (R-B5).** This used to call `wav::read` again and
/// play whatever came back: no [`backstop::check`], no [`wav::check_rate`],
/// and the device already open — so the one code path in this process that
/// makes sound had a branch with the level interlock missing from in front of
/// it. Handing the checked value in makes the vetted bytes the only bytes
/// there are. The second read's `?` was a second defect in the same line: it
/// returned past `player.stop()`, leaving the render device to `Drop` instead
/// of the explicit teardown every other exit path runs.
///
/// `pub` because the crate's integration tests are a separate crate — the
/// module header's own rule — and this is where a regression would live.
pub fn wait_then_play(
    args: &Args,
    file: &wav::Wav,
    mut player: player::Player,
    commands: Receiver<Command>,
) -> Result<u8, Refusal> {
    loop {
        match commands.recv() {
            Ok(Command::Unknown) => continue,
            Ok(Command::Play) => break,
            Ok(Command::Abort) | Err(_) => {
                player.stop();
                if args.json {
                    emit(&Event::Aborted {
                        frames_emitted: 0,
                        ramp_frames: 0,
                    });
                }
                return Ok(ExitCode::Ok.code());
            }
        }
    }
    emit_started(args);
    let outcome = player.play(&file.samples, &abort_poll(&commands), |frames| {
        if args.json {
            emit(&Event::Progress {
                frames_emitted: frames,
            });
        }
    })?;
    finish(args, outcome)
}

fn emit_started(args: &Args) {
    if args.json {
        // Advisory only. Nothing in the timing path reads it: `t = 0` comes
        // from markers the parent baked into the WAV.
        emit(&Event::Started);
    }
}

fn finish(args: &Args, outcome: Outcome) -> Result<u8, Refusal> {
    if args.json {
        match outcome {
            Outcome::Aborted {
                frames_emitted,
                ramp_frames,
            } => emit(&Event::Aborted {
                frames_emitted,
                ramp_frames,
            }),
            Outcome::Done {
                clamped,
                frames_emitted,
                sanitized,
            } => emit(&Event::Done {
                clamped,
                frames_emitted,
                sanitized,
                underrun_frames: 0,
            }),
        }
    }
    Ok(ExitCode::Ok.code())
}

/// The abort predicate the render loop polls: the stdin reader's lines and the
/// signal handler converge on ONE flag, so this reads one cell.
fn abort_poll(commands: &Receiver<Command>) -> impl Fn() -> bool + '_ {
    move || {
        loop {
            match commands.try_recv() {
                Ok(Command::Abort) => signals::request_abort(),
                Ok(_) => continue,
                // The reader thread ends on EOF, and EOF is an abort — it has
                // already armed the flag before dropping the sender.
                Err(TryRecvError::Disconnected) | Err(TryRecvError::Empty) => break,
            }
        }
        signals::aborting()
    }
}

/// A dedicated blocking thread on stdin. It blocks in `read_line` and never
/// polls, so an `abort` costs a scheduler hop rather than a block period.
fn spawn_stdin_reader() -> Receiver<Command> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let command = protocol::parse_command(&line);
            if command == Command::Abort {
                // Arm the flag HERE, not on the receiving side: the render loop
                // may be inside a paced write, and the flag is what it polls.
                signals::request_abort();
            }
            if tx.send(command).is_err() {
                return;
            }
        }
        // EOF: the parent went away. That is an abort — a stimulus nobody is
        // listening to is one to fade out, not one to keep playing.
        signals::request_abort();
        let _ = tx.send(Command::Abort);
    });
    rx
}

/// Refuse when the device we opened is not the current default output.
fn check_default_output(device_uid: &str) -> Result<(), Refusal> {
    let not_default = |message: String| Refusal::new(ExitCode::NotDefaultOutput, message);
    let current = paraeq_coreaudio::properties::default_output_device()
        .and_then(paraeq_coreaudio::properties::device_uid)
        .map_err(|e| not_default(format!("cannot read the default output device: {e}")))?;
    if current != device_uid {
        return Err(not_default(format!(
            "rendering to '{device_uid}' but the default output is now '{current}' — the tap \
             watches the default output, so this would measure a route the engine is not on"
        )));
    }
    Ok(())
}

fn open_refusal(error: RenderError) -> Refusal {
    Refusal::new(
        match error {
            // A caller bug, refused before any HAL call.
            RenderError::EmptyUid => ExitCode::BadArgs,
            _ => ExitCode::DeviceNotFound,
        },
        error.to_string(),
    )
}
