//! The dependency surface, asserted by the compiler.
//!
//! This crate must not depend on the parent measurement crate — the
//! architecture spec says so in a manifest, and an owner decision re-pins it —
//! so every item it needs from there is re-exported by `paraeq-coreaudio` and
//! named through that crate. A missing re-export is therefore a COMPILE
//! failure, here, rather than a quiet re-addition of the forbidden dependency,
//! and this file is what makes the surface's completeness a checked property
//! instead of a claim.
//!
//! It exists at all only because this crate has a library target: an
//! integration test is its own crate and links the package's library, and a
//! bin-only package has nothing for it to link.

use paraeq_coreaudio::{
    abort_envelope, abort_ramp_len, MeasureError, RenderSink, StreamFormat, ABORT_RAMP_MS,
    ABSOLUTE_MAX_DBFS_RMS,
};

/// A sink is the shape the child owns, so holding one behind the re-exported
/// trait is the strongest form of "this really is the same trait".
struct NullSink;

impl RenderSink for NullSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: 512,
            sample_rate_hz: 48_000.0,
        }
    }

    fn ramp_out(&mut self) {}

    fn stop(&mut self) {}

    fn write(&mut self, _block: &[f32]) -> Result<(), MeasureError> {
        Ok(())
    }
}

#[test]
fn the_child_names_the_render_seam_through_paraeq_coreaudio() {
    // Bind all SEVEN, and USE each: a re-export that exists but resolves to the
    // wrong thing would compile and then fail here.
    let mut sink: Box<dyn RenderSink> = Box::new(NullSink);
    let format: StreamFormat = sink.format();
    assert_eq!(format.channels, 1);
    sink.write(&[0.0; 8])
        .expect("the null sink accepts a block");
    sink.ramp_out();
    sink.stop();

    let error = MeasureError::Sink("a sink fault the child reports".to_owned());
    match error {
        MeasureError::Sink(message) => assert!(message.contains("sink fault")),
        other => panic!("the re-exported error is the parent's: {other}"),
    }

    assert_eq!(ABSOLUTE_MAX_DBFS_RMS, -3.0);
    assert_eq!(ABORT_RAMP_MS, 5.0);

    // BOTH arguments. A one-argument regression is a compile failure here, not
    // a panic on the abort path at 48 kHz with a 512-frame block.
    let ramp_len = abort_ramp_len(48_000.0);
    assert_eq!(ramp_len, 240);
    let env = abort_envelope(ramp_len, ramp_len * 2);
    assert_eq!(env.len(), ramp_len * 2);
    assert_eq!(env[env.len() - 1], 0.0);
}

#[test]
fn the_helper_crate_exposes_its_modules_to_its_tests() {
    // The failure this test exists to prevent: a bin-only crate, or a
    // `pub(crate)` anywhere in it, makes every other test in this directory
    // uncompilable.
    use paraeq_stimulus::{backstop, player, protocol, ramp, signals, wav};

    let _ = backstop::BACKSTOP_WINDOW_MS;
    let _ = ramp::ramp_len(48_000.0);
    let _ = signals::aborting();
    let _ = protocol::parse_command("play");
    // Name the two remaining modules' public items so the `use` above is not
    // the only thing holding them.
    let _: fn(&std::path::Path) -> Result<wav::Wav, protocol::Refusal> = wav::read;
    let _: fn(Box<dyn RenderSink>) -> player::Player = player::Player::new;

    assert_eq!(
        paraeq_stimulus::run([std::ffi::OsString::from("paraeq-stimulus"), "--help".into()]),
        0,
        "--help is the one path that reaches no file and no device"
    );
}
