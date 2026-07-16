//! MS-1: "Sinks/sources are traits". The point of the seam is that the level
//! policy is exercisable with no hardware and no `unsafe` in sight, so this
//! test is also the proof that Stage 4's `paraeq-coreaudio` impls have a shape
//! to implement — and that mocks can stand in for them (MS-5/MS-6/MS-14 all
//! specify mock-driven tests against these traits).

use paraeq_dsp::targets::TransducerClass;
use paraeq_measure::{CaptureSource, MeasureError, StimulusSink, StreamFormat, SweepLevel};

#[derive(Default)]
struct MockSink {
    emitted: Vec<(usize, f64)>,
    stopped: bool,
}

impl StimulusSink for MockSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 2,
            frames_per_block: 512,
            sample_rate_hz: 48_000.0,
        }
    }

    fn emit(&mut self, block: &[f64], level: SweepLevel) -> Result<(), MeasureError> {
        self.emitted.push((block.len(), level.dbfs_rms()));
        Ok(())
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        self.stopped = true;
        Ok(())
    }
}

struct MockSource {
    rate_hz: f64,
}

impl CaptureSource for MockSource {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: 512,
            sample_rate_hz: self.rate_hz,
        }
    }

    fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError> {
        block.fill(0.0);
        Ok(block.len())
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        Ok(())
    }
}

#[test]
fn a_sink_receives_the_level_that_survived_the_caps_table() {
    let mut sink = MockSink::default();
    let level = SweepLevel::new(-20.0, TransducerClass::InEar).unwrap();
    sink.emit(&[0.0; 4], level).unwrap();
    sink.stop().unwrap();
    assert_eq!(sink.emitted, vec![(4, -20.0)]);
    assert!(sink.stopped);
}

/// Both traits are object-safe: a session holds one of each without being
/// generic over the platform, which is what keeps the `paraeq-coreaudio`
/// dependency pointing *into* this crate (the `AudioBackend` seam's shape).
#[test]
fn the_seam_traits_are_object_safe() {
    let mut sink: Box<dyn StimulusSink> = Box::new(MockSink::default());
    let mut source: Box<dyn CaptureSource> = Box::new(MockSource { rate_hz: 48_000.0 });
    let level = SweepLevel::new(-40.0, TransducerClass::Bookshelf).unwrap();
    sink.emit(&[0.0; 4], level).unwrap();
    let mut block = [1.0; 4];
    assert_eq!(source.capture(&mut block).unwrap(), 4);
    assert_eq!(block, [0.0; 4]);
}

/// § The Two-Clock Complication: a UMIK-1 runs on its own crystal at its own
/// fixed rate against an output device on another. The seam reports both rates
/// rather than assuming one, so the mismatch is visible to the session that has
/// to reconcile it (MS-22) instead of being discovered in a deconvolution.
#[test]
fn the_two_stream_formats_are_independent() {
    let sink = MockSink::default();
    let source = MockSource { rate_hz: 44_100.0 };
    assert_ne!(sink.format().sample_rate_hz, source.format().sample_rate_hz);
}
