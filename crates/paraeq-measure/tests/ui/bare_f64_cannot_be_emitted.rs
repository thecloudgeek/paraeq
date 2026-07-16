//! The unguarded playback wiring the spec exists to prevent: a level chosen at
//! the call site, unchecked against any cap. -3.01 dBFS RMS is the unscaled
//! sweep and REW's absolute maximum; on an IEM chain it is the 122 dB SPL
//! scenario. It must not compile.

use paraeq_measure::{MeasureError, StimulusSink, StreamFormat, SweepLevel};

struct NullSink;

impl StimulusSink for NullSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 2,
            frames_per_block: 512,
            sample_rate_hz: 48_000.0,
        }
    }

    fn emit(&mut self, _block: &[f64], _level: SweepLevel) -> Result<(), MeasureError> {
        Ok(())
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        Ok(())
    }
}

fn main() {
    let mut sink = NullSink;
    let _ = sink.emit(&[0.0; 4], -3.01);
}
