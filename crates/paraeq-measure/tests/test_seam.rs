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

// ------------------------------------------------- the verification seams

#[test]
fn helper_routing_maps_to_the_child_channel_argument() {
    // ONE mapping, in one place. A third spelling of "which channel carries
    // the mono stimulus" — the parent's enum, the platform crate's enum, and
    // an ad-hoc `format!` at the spawn site — is three places for a per-ear
    // coupler verification to silently difference a per-ear baseline against
    // an L+R sum.
    use paraeq_measure::HelperRouting;

    assert_eq!(HelperRouting::Both.as_channel_arg(), "both");
    assert_eq!(HelperRouting::Only(0).as_channel_arg(), "0");
    assert_eq!(HelperRouting::Only(1).as_channel_arg(), "1");
    assert_eq!(HelperRouting::Only(7).as_channel_arg(), "7");
}

#[test]
fn the_helper_seam_exposes_no_level_and_no_stimulus_type() {
    // API shape, in the grep form the routing-interpretation test uses: the
    // WAV *is* the level, so the MS-2 mistake must be unspellable on this
    // seam — there is nothing to pass and nothing to mis-pass. A signature
    // taking a level or an assembled stimulus would be a second way to decide
    // how loud the verification sweep plays, which is the one decision this
    // whole design keeps in a single place.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/seam.rs"))
        .expect("seam.rs is beside this test");
    let helper_seam = src
        .split_once("pub trait StimulusHelper")
        .expect("the helper seam is declared in seam.rs")
        .1;
    // Stop at the routing mirror: everything between is the two helper traits.
    let helper_seam = helper_seam
        .split_once("pub enum HelperRouting")
        .expect("HelperRouting follows the helper traits")
        .0;

    // Signatures only. The doc comments above them say the words "level" and
    // "stimulus" on purpose — explaining why they are absent is the whole
    // point of the paragraph — so strip the prose and read what compiles.
    let signatures: String = helper_seam
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    for forbidden in ["SweepLevel", "AssembledStimulus", "dbfs", "level"] {
        assert!(
            !signatures.contains(forbidden),
            "`{forbidden}` appears in a helper-seam signature; the WAV is the level"
        );
    }
}

#[test]
fn a_gain_pin_restores_once_and_is_idempotent() {
    // The RAII contract `EngineControl::pin_gain_db` promises. `Drop` is the
    // backstop; an explicit `restore()` is how a caller SEES the failure. A
    // second call must not touch the engine again — a teardown ladder that
    // restored twice would fight whatever ran between the two calls.
    use paraeq_measure::GainPin;
    use std::sync::{Arc, Mutex};

    let calls: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    let mut pin = GainPin::new(
        -6.0,
        0.0,
        Box::new(move |db| {
            sink.lock().expect("uncontended").push(db);
            Ok(())
        }),
    );
    assert_eq!(pin.pinned_db(), 0.0);
    assert_eq!(pin.previous_db(), -6.0);

    pin.restore().expect("the first restore reaches the engine");
    pin.restore().expect("the second is a no-op");
    drop(pin);

    assert_eq!(
        *calls.lock().expect("uncontended"),
        vec![-6.0],
        "the engine is told exactly once, with the value it held before the pin"
    );
}

#[test]
fn a_gain_pin_restores_on_drop_without_an_explicit_call() {
    // The path that matters: an unwinding teardown never gets to call
    // `restore()`. "The system must never be left at measurement volume" has
    // a trim-stage twin, and this is it.
    use paraeq_measure::GainPin;
    use std::sync::{Arc, Mutex};

    let calls: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    {
        let _pin = GainPin::new(
            3.5,
            0.0,
            Box::new(move |db| {
                sink.lock().expect("uncontended").push(db);
                Ok(())
            }),
        );
    }
    assert_eq!(*calls.lock().expect("uncontended"), vec![3.5]);
}

#[test]
fn a_failing_restore_on_drop_does_not_panic() {
    // A `Drop` that panicked would abort the teardown ladder behind it, and
    // the ladder's last rung is the one that destroys the render device.
    use paraeq_measure::{GainPin, MeasureError};

    let pin = GainPin::new(
        -2.0,
        0.0,
        Box::new(|_| Err(MeasureError::Sink("engine went away".to_owned()))),
    );
    drop(pin);
}
