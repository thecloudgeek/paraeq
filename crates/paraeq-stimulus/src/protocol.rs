//! The parent <-> child line protocol, the argument parser, and the exit-code
//! table. Pure: nothing here opens a file, a device or a socket.
//!
//! Two phases, and the split is the point. The child opens its device and emits
//! `ready` BEFORE any audio, so a device that cannot be opened becomes a clean
//! exit code instead of five seconds of silence the parent has to time out. The
//! parent then commits with `play`.

use std::ffi::OsString;
use std::path::PathBuf;

use paraeq_coreaudio::measure_aggregate::StimulusRouting;

/// Every way this process can end, with the number it exits with.
///
/// A closed enum rather than bare integers so the mapping is a compiler-checked
/// match: the parent reads these numbers and turns each into a distinct
/// refusal, and a number that quietly changed meaning would move a refusal onto
/// the wrong cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitCode {
    /// Played to the end, or aborted cleanly. An abort is NOT a failure: it
    /// ramped, it tore down, and it destroyed its render device.
    Ok,
    /// Bad arguments — including a `--channel N` at or above the device's
    /// reported channel count, which is refused before a sample is emitted.
    BadArgs,
    /// The WAV is missing, is not mono 32-bit float, or its rate disagrees with
    /// the device. Never resampled: the parent generated the file at the
    /// device's own rate and a silent resample would move the level.
    Wav,
    /// No device by that UID, or the device could not be opened.
    DeviceNotFound,
    /// The device stopped cycling. Bounded by the same stall constant the
    /// in-process path uses.
    Stalled,
    /// The level backstop refused the file, BEFORE any device was opened.
    BackstopRefused,
    /// The device it opened is not the current default output. The tap watches
    /// the default output, so rendering anywhere else measures a route the
    /// engine is not on.
    NotDefaultOutput,
}

impl ExitCode {
    /// The process exit status. 101 and above are panics and signals, which is
    /// why nothing here uses them.
    pub fn code(self) -> u8 {
        match self {
            ExitCode::Ok => 0,
            ExitCode::BadArgs => 2,
            ExitCode::Wav => 3,
            ExitCode::DeviceNotFound => 4,
            ExitCode::Stalled => 5,
            ExitCode::BackstopRefused => 6,
            ExitCode::NotDefaultOutput => 7,
        }
    }
}

/// A refusal: the exit code and the line a human reads on stderr.
///
/// One error type for the whole crate. No `thiserror`, deliberately — this
/// process has six dependencies and a hand-written `Display` is three lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub code: ExitCode,
    pub message: String,
}

impl Refusal {
    pub fn new(code: ExitCode, message: impl Into<String>) -> Refusal {
        Refusal {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (exit {})", self.message, self.code.code())
    }
}

/// What the parent said on stdin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// `abort\n`, or EOF. EOF is an abort because a parent that went away
    /// cannot be waited for, and the safe thing to do with a stimulus nobody
    /// is listening to is to fade it out.
    Abort,
    /// `play\n`.
    Play,
    /// Anything else. Ignored, never fatal: a protocol the parent extends must
    /// not kill a child that predates the extension.
    Unknown,
}

/// Parse one stdin line. Trailing newlines and surrounding whitespace are
/// tolerated; case is not, because a case-insensitive `abort` is one typo away
/// from a command that means something else.
pub fn parse_command(line: &str) -> Command {
    match line.trim() {
        "abort" => Command::Abort,
        "play" => Command::Play,
        _ => Command::Unknown,
    }
}

/// One line of stdout, when `--json` is set. Internally tagged on `event`, so
/// the parent reads a discriminated union keyed on one field.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "event")]
pub enum Event {
    /// The stimulus was faded out and the device torn down. Exit 0.
    Aborted {
        frames_emitted: u64,
        ramp_frames: u64,
    },
    /// Played to the end. The guard counts are the MS-4 ones; a verified
    /// stimulus produces zeros, and anything else is a defect the parent turns
    /// into its own warnings.
    Done {
        clamped: u64,
        frames_emitted: u64,
        sanitized: u64,
        underrun_frames: u64,
    },
    Error {
        code: u8,
        message: String,
    },
    Progress {
        frames_emitted: u64,
    },
    /// Emitted after the device is open and BEFORE any audio.
    Ready {
        /// Output channels the render device reports. `--channel N` is refused
        /// against this, before `play`.
        channels: u32,
        /// The PHYSICAL output device's UID — the route the parent taps.
        device_uid: String,
        frames_per_block: usize,
        /// The render wrapper's UID. The parent's rate fence reads THIS device,
        /// not the default output, or the two sides are talking about different
        /// devices and the child exits 3 on every run.
        render_device_uid: String,
        routing: String,
        /// The render wrapper's nominal rate.
        sample_rate_hz: f64,
        /// The physical output's own nominal rate. Both are echoed because
        /// nothing establishes that a private aggregate's nominal rate equals
        /// its single sub-device's, and a rate refusal has to be able to say
        /// WHICH one disagreed.
        sub_device_sample_rate_hz: f64,
    },
    /// Advisory only. Nothing in the timing path reads it: `t = 0` comes from
    /// markers the parent baked into the WAV, never from an IPC timestamp.
    Started,
}

/// Encode one event as a single line of JSON (no trailing newline).
pub fn encode(event: &Event) -> String {
    // A serialization failure here would mean a non-finite float in a field
    // the child computed, which is a bug worth seeing rather than hiding.
    serde_json::to_string(event).unwrap_or_else(|e| {
        format!(r#"{{"event":"error","code":1,"message":"encode failed: {e}"}}"#)
    })
}

/// Everything the child was asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Args {
    /// Which output channel carries the mono stimulus. MANDATORY: left and
    /// right are separate measurements, so playing to both at once measures
    /// their sum and nothing useful — a default here would make every per-ear
    /// coupler verification silently difference a per-ear baseline against an
    /// L+R sum, with no symptom.
    pub routing: StimulusRouting,
    pub device_uid: String,
    pub json: bool,
    pub wav: PathBuf,
}

/// What the command line asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invocation {
    Help,
    Run(Args),
}

/// The usage text. Note what is NOT here: no `--level` (the WAV *is* the
/// level, so there is nothing to pass and nothing to mis-pass), no
/// `--marker-lead-s` (the parent bakes markers into the WAV; a lead the child
/// inserted would be a second place for the layout to live), and no buffer-size
/// or rate setter (`set_buffer_frame_size` is device-GLOBAL, so calling it
/// would renegotiate the engine's geometry mid-verification).
pub const USAGE: &str = "\
paraeq-stimulus — the ParaEQ verification stimulus helper.

USAGE:
    paraeq-stimulus --wav <path> --device-uid <uid> --channel <both|N> [--json]

    --wav <path>          Mono 32-bit-float WAV, already at the device's rate
                          and already at level.
    --device-uid <uid>    The physical output device to render to.
    --channel <both|N>    Which output channel carries the stimulus. Required.
    --json                One JSON object per line on stdout.
    --help                This text.

The stimulus is played as-is: this process applies no gain, ever, and never
resamples.
";

/// Parse `argv` (including argv[0]).
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, Refusal> {
    let bad = |message: String| Refusal::new(ExitCode::BadArgs, message);
    let mut argv = args.into_iter().skip(1);
    let mut channel: Option<String> = None;
    let mut device_uid: Option<String> = None;
    let mut json = false;
    let mut wav: Option<PathBuf> = None;

    while let Some(arg) = argv.next() {
        let arg = arg.to_string_lossy().into_owned();
        match arg.as_str() {
            "--channel" => {
                channel = Some(next_value(&mut argv, "--channel")?);
            }
            "--device-uid" => {
                device_uid = Some(next_value(&mut argv, "--device-uid")?);
            }
            "--help" | "-h" => return Ok(Invocation::Help),
            "--json" => json = true,
            "--wav" => {
                wav = Some(PathBuf::from(next_value(&mut argv, "--wav")?));
            }
            other => return Err(bad(format!("unknown argument '{other}'"))),
        }
    }

    let channel = channel.ok_or_else(|| bad("--channel is required".to_owned()))?;
    let device_uid = device_uid.ok_or_else(|| bad("--device-uid is required".to_owned()))?;
    let wav = wav.ok_or_else(|| bad("--wav is required".to_owned()))?;
    if device_uid.is_empty() {
        return Err(bad("--device-uid is empty".to_owned()));
    }

    Ok(Invocation::Run(Args {
        routing: parse_routing(&channel)?,
        device_uid,
        json,
        wav,
    }))
}

/// `both`, or a non-negative channel index.
pub fn parse_routing(value: &str) -> Result<StimulusRouting, Refusal> {
    if value == "both" {
        return Ok(StimulusRouting::Both);
    }
    match value.parse::<u32>() {
        Ok(n) => Ok(StimulusRouting::Only(n as usize)),
        Err(_) => Err(Refusal::new(
            ExitCode::BadArgs,
            format!("--channel expects 'both' or a channel index, got '{value}'"),
        )),
    }
}

/// The routing as it appears in the `ready` line, so the parent can read back
/// what it asked for rather than trusting that it was understood.
pub fn routing_label(routing: StimulusRouting) -> String {
    match routing {
        StimulusRouting::Both => "both".to_owned(),
        StimulusRouting::Only(ch) => ch.to_string(),
    }
}

/// Refuse a channel index the device cannot honour.
///
/// This is why the channel count is read from the device rather than off the
/// first callback: `ready` is emitted before any callback, and a refusal that
/// arrived after `ready` would arrive after the parent had already committed.
/// Exit 2, not silence — the render path itself would play silence on an
/// out-of-range channel, and silence is a measurement the parent would have to
/// diagnose rather than a refusal it can report.
pub fn check_channel_bound(routing: StimulusRouting, channels: u32) -> Result<(), Refusal> {
    if let StimulusRouting::Only(ch) = routing {
        if ch as u64 >= u64::from(channels) {
            return Err(Refusal::new(
                ExitCode::BadArgs,
                format!("--channel {ch} but the render device reports {channels} output channels"),
            ));
        }
    }
    Ok(())
}

fn next_value(argv: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<String, Refusal> {
    match argv.next() {
        Some(value) => Ok(value.to_string_lossy().into_owned()),
        None => Err(Refusal::new(
            ExitCode::BadArgs,
            format!("{flag} expects a value"),
        )),
    }
}
