//! `RenderSink` is the documented exception to "a sink is not the engine's
//! output path", not a way around the MS-2 interlock. It carries no
//! `SweepLevel` because a child process receives BYTES, not types — and that
//! is only safe while the two sinks stay unsubstitutable in BOTH directions.
//!
//! This is the direction the compiler can prove: a `RenderSink` may not stand
//! in where a `StimulusSink` is required, so nothing that discharged the level
//! interlock by writing a WAV can be handed the in-process measurement path,
//! where the level is a type and the assembly pipeline expects to supply it.
//! `SessionSeam` has no `RenderSink` slot for the same reason.

use paraeq_measure::{
    assemble_sweep, MeasureError, RenderSink, StreamFormat, SweepLevel, TransducerClass,
};

struct Renderer;

impl RenderSink for Renderer {
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

fn main() {
    let level = SweepLevel::new(-20.0, TransducerClass::InEar).expect("under every cap");
    let stimulus =
        assemble_sweep(1.0, 48_000, 20.0, 20_000.0, level).expect("a legal one-second sweep");
    let mut renderer = Renderer;
    let _ = stimulus.emit_to(&mut renderer);
}
