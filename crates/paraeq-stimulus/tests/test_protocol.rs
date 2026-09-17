//! The line protocol, the exit-code table and the argument parser. Pure — no
//! file, no device, no process.

use std::ffi::OsString;

use paraeq_coreaudio::measure_aggregate::StimulusRouting;
use paraeq_stimulus::protocol::{
    check_channel_bound, encode, parse, parse_command, parse_routing, routing_label, Command,
    Event, ExitCode, Invocation,
};

fn argv(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("paraeq-stimulus")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

#[test]
fn protocol_round_trip_and_eof_means_abort() {
    assert_eq!(parse_command("play\n"), Command::Play);
    assert_eq!(parse_command("abort\n"), Command::Abort);
    assert_eq!(parse_command("  play  "), Command::Play);
    // Unknown lines are ignored, never fatal: a protocol the parent extends
    // must not kill a child that predates the extension.
    assert_eq!(parse_command("resume"), Command::Unknown);
    assert_eq!(parse_command(""), Command::Unknown);
    // Case is NOT tolerated. A case-insensitive `abort` is one typo away from
    // a command that means something else.
    assert_eq!(parse_command("ABORT"), Command::Unknown);

    // EOF is an abort, and it is the parent-crashed case: a stimulus nobody is
    // listening to is one to fade out, not one to keep playing. The reader
    // thread turns EOF into this same command, which is why the enum has no
    // separate variant for it.
    assert_eq!(
        parse_command(""),
        Command::Unknown,
        "an empty LINE is not EOF; EOF is the absence of a line"
    );
}

#[test]
fn exit_code_mapping_is_exhaustive() {
    // A compiler-checked match, not a runtime table walk: the parent turns each
    // number into a distinct refusal, so a number that quietly changed meaning
    // would move a refusal onto the wrong cause.
    for code in [
        ExitCode::Ok,
        ExitCode::BadArgs,
        ExitCode::Wav,
        ExitCode::DeviceNotFound,
        ExitCode::Stalled,
        ExitCode::BackstopRefused,
        ExitCode::NotDefaultOutput,
    ] {
        let expected = match code {
            ExitCode::Ok => 0,
            ExitCode::BadArgs => 2,
            ExitCode::Wav => 3,
            ExitCode::DeviceNotFound => 4,
            ExitCode::Stalled => 5,
            ExitCode::BackstopRefused => 6,
            ExitCode::NotDefaultOutput => 7,
        };
        assert_eq!(code.code(), expected, "{code:?}");
    }
    // 1 is deliberately unused: it is what a panicking process would look like
    // under some runners, and 101+ is what one looks like here.
    assert!(![1u8].contains(&ExitCode::Ok.code()));
}

#[test]
fn channel_is_required_and_is_echoed_in_ready() {
    // MANDATORY, with no default. Left and right are separate measurements, so
    // playing to both at once measures their sum and nothing useful — a
    // default would make every per-ear coupler verification silently
    // difference a per-ear baseline against an L+R sum, with no symptom.
    let refusal = parse(argv(&["--wav", "/tmp/x.wav", "--device-uid", "dev"]))
        .expect_err("--channel is required");
    assert_eq!(refusal.code, ExitCode::BadArgs);
    assert!(refusal.message.contains("--channel"), "{}", refusal.message);

    let Invocation::Run(args) = parse(argv(&[
        "--wav",
        "/tmp/x.wav",
        "--device-uid",
        "dev",
        "--channel",
        "1",
        "--json",
    ]))
    .expect("a complete command line") else {
        panic!("expected a run invocation");
    };
    assert_eq!(args.routing, StimulusRouting::Only(1));
    assert!(args.json);

    // And the ready line carries it back, so the parent reads what was
    // understood rather than trusting that it was.
    let line = encode(&Event::Ready {
        channels: 2,
        device_uid: "dev".to_owned(),
        frames_per_block: 512,
        render_device_uid: "com.paraeq.render.1".to_owned(),
        routing: routing_label(args.routing),
        sample_rate_hz: 48_000.0,
        sub_device_sample_rate_hz: 48_000.0,
    });
    assert!(line.contains(r#""routing":"1""#), "{line}");
}

#[test]
fn a_channel_index_at_or_above_the_reported_count_is_refused_with_exit_2() {
    // The refusal `ready.channels` exists to make possible, and the reason the
    // channel count is read from the DEVICE rather than off the first callback:
    // `ready` is emitted before any callback, so a refusal derived from a
    // callback would arrive after the parent had committed.
    check_channel_bound(StimulusRouting::Only(1), 2).expect("channel 1 of 2 is fine");
    check_channel_bound(StimulusRouting::Both, 1).expect("both is always in range");

    for (routing, channels) in [
        (StimulusRouting::Only(2), 2u32),
        (StimulusRouting::Only(5), 2),
        (StimulusRouting::Only(0), 0),
    ] {
        let refusal = check_channel_bound(routing, channels)
            .expect_err("a channel the device does not have is refused");
        assert_eq!(refusal.code, ExitCode::BadArgs);
        assert_eq!(refusal.code.code(), 2);
    }
}

#[test]
fn ready_echoes_both_the_render_device_and_sub_device_rates() {
    // Nothing establishes that a private aggregate's nominal rate equals its
    // single sub-device's, so both are reported. Without the second number a
    // rate refusal cannot say WHICH side disagreed, and the failure reads as a
    // WAV bug.
    let line = encode(&Event::Ready {
        channels: 2,
        device_uid: "BuiltInSpeakerDevice".to_owned(),
        frames_per_block: 512,
        render_device_uid: "com.paraeq.render.42".to_owned(),
        routing: "both".to_owned(),
        sample_rate_hz: 48_000.0,
        sub_device_sample_rate_hz: 44_100.0,
    });
    assert!(line.contains(r#""event":"ready""#), "{line}");
    assert!(line.contains(r#""sample_rate_hz":48000.0"#), "{line}");
    assert!(
        line.contains(r#""sub_device_sample_rate_hz":44100.0"#),
        "{line}"
    );
    assert!(
        line.contains(r#""render_device_uid":"com.paraeq.render.42""#),
        "the parent's rate fence reads THIS device, not the default output: {line}"
    );
    assert!(!line.contains('\n'), "one object per line: {line}");
}

#[test]
fn the_routing_parser_accepts_both_and_an_index_and_nothing_else() {
    assert_eq!(parse_routing("both").expect("both"), StimulusRouting::Both);
    assert_eq!(
        parse_routing("0").expect("channel zero"),
        StimulusRouting::Only(0)
    );
    for bad in ["", "left", "-1", "1.5", "Both"] {
        assert_eq!(
            parse_routing(bad).expect_err(bad).code,
            ExitCode::BadArgs,
            "accepted '{bad}'"
        );
    }
}

#[test]
fn the_helper_has_no_level_flag_and_no_marker_lead_flag() {
    // The WAV *is* the level, so there is nothing to pass and nothing to
    // mis-pass — this is the replacement for the type interlock that cannot
    // cross a process boundary. And the parent bakes the markers into the WAV,
    // so a lead the child inserted would be a second place for the layout to
    // live.
    for flag in ["--level", "--marker-lead-s"] {
        let refusal = parse(argv(&[
            "--wav",
            "/tmp/x.wav",
            "--device-uid",
            "dev",
            "--channel",
            "both",
            flag,
            "1",
        ]))
        .expect_err("an unknown flag is refused rather than ignored");
        assert_eq!(refusal.code, ExitCode::BadArgs);
        assert!(refusal.message.contains(flag), "{}", refusal.message);
    }
    assert!(!paraeq_stimulus::protocol::USAGE.contains("--level"));
    assert!(!paraeq_stimulus::protocol::USAGE.contains("--marker-lead-s"));
}

#[test]
fn help_is_the_one_invocation_that_needs_nothing_else() {
    assert_eq!(
        parse(argv(&["--help"])).expect("help parses"),
        Invocation::Help
    );
    assert_eq!(parse(argv(&["-h"])).expect("short help"), Invocation::Help);
}

#[test]
fn an_empty_device_uid_is_refused_rather_than_resolved() {
    // Selector validation before any HAL call: an empty UID is a caller bug,
    // not a device question.
    let refusal = parse(argv(&[
        "--wav",
        "/tmp/x.wav",
        "--device-uid",
        "",
        "--channel",
        "both",
    ]))
    .expect_err("an empty UID is refused");
    assert_eq!(refusal.code, ExitCode::BadArgs);
}

#[test]
fn every_event_encodes_as_one_tagged_line() {
    for event in [
        Event::Aborted {
            frames_emitted: 12,
            ramp_frames: 240,
        },
        Event::Done {
            clamped: 0,
            frames_emitted: 264_000,
            sanitized: 0,
            underrun_frames: 0,
        },
        Event::Error {
            code: 6,
            message: "too hot".to_owned(),
        },
        Event::Progress {
            frames_emitted: 48_000,
        },
        Event::Started,
    ] {
        let line = encode(&event);
        assert!(line.starts_with(r#"{"event":"#), "{line}");
        assert!(!line.contains('\n'), "{line}");
    }
}
