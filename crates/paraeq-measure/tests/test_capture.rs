//! Tier 3 (analytic/policy) for the capture runtime: MS-21's metering and the
//! record loop's outcomes. Mock-driven — no hardware, no `unsafe`, per MS-1.

use paraeq_measure::capture::{
    record, CaptureEnd, CaptureMeter, CLIP_BLOCK_FRACTION, PEAK_DECAY_DB_PER_BLOCK,
};
use paraeq_measure::seam::{CaptureSource, StreamFormat};
use paraeq_measure::session::{AbortHandle, AbortReason};
use paraeq_measure::{MeasureError, MeasurementDiagnostic};

/// A source that hands out a scripted sample stream one block at a time, and
/// can be told to fail or to dry up.
struct MockSource {
    fail_after: Option<usize>,
    frames_per_block: usize,
    position: usize,
    samples: Vec<f64>,
    served_blocks: usize,
}

impl MockSource {
    fn new(samples: Vec<f64>, frames_per_block: usize) -> Self {
        Self {
            fail_after: None,
            frames_per_block,
            position: 0,
            samples,
            served_blocks: 0,
        }
    }
}

impl CaptureSource for MockSource {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: self.frames_per_block,
            sample_rate_hz: 48_000.0,
        }
    }

    fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError> {
        if self.fail_after == Some(self.served_blocks) {
            return Err(MeasureError::Capture("mock source died".to_owned()));
        }
        self.served_blocks += 1;
        let available = self.samples.len().saturating_sub(self.position);
        let n = block.len().min(available);
        block[..n].copy_from_slice(&self.samples[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        Ok(())
    }
}

// ─────────────────────────────── the meter ───────────────────────────────────

#[test]
fn the_peak_decays_between_blocks_unlike_the_engines() {
    // The whole reason MS-21 exists: `RtShared::peak_in` is a monotonic
    // session maximum with no decay, which cannot answer "how hot is it now".
    let mut meter = CaptureMeter::new();
    meter.observe(&[0.5, -0.5]);
    let loud = meter.peak_dbfs();
    for _ in 0..4 {
        meter.observe(&[0.0, 0.0]);
    }
    let quiet = meter.peak_dbfs();
    assert!(quiet < loud, "the peak must fall: {loud} -> {quiet}");
    // Four silent blocks of decay, plus the one applied on the loud block
    // itself: five in total.
    let expected = 20.0 * 0.5f64.log10() - 4.0 * PEAK_DECAY_DB_PER_BLOCK;
    assert!(
        (quiet - expected).abs() < 1e-9,
        "expected {expected}, got {quiet}"
    );
}

#[test]
fn digital_silence_reads_as_negative_infinity_not_a_floor() {
    let mut meter = CaptureMeter::new();
    meter.observe(&[0.0; 64]);
    assert_eq!(meter.peak_dbfs(), f64::NEG_INFINITY);
    assert_eq!(meter.peak_linear(), 0.0);
}

#[test]
fn the_thirty_percent_rule_fires_strictly_above_thirty_percent() {
    // REW's rule as worded: *more than* 30%. Exactly 30% passes.
    let block_len = 100;
    let at_threshold = (CLIP_BLOCK_FRACTION * block_len as f64) as usize;

    let mut meter = CaptureMeter::new();
    let mut block = vec![0.1; block_len];
    block[..at_threshold].fill(1.0);
    assert_eq!(meter.observe(&block), None, "exactly 30% must not fire");

    let mut meter = CaptureMeter::new();
    let mut block = vec![0.1; block_len];
    block[..at_threshold + 1].fill(1.0);
    assert_eq!(
        meter.observe(&block),
        Some(MeasurementDiagnostic::InputClipping),
        "31% must fire"
    );
    assert_eq!(meter.clipped_blocks(), 1);
}

#[test]
fn clipping_is_blocking_not_a_warning() {
    // "the measurement is invalid regardless" — so it must refuse, and the
    // diagnostic vocabulary has to agree.
    assert!(MeasurementDiagnostic::InputClipping.is_blocking());
}

#[test]
fn non_finite_samples_count_as_clipped_rather_than_reading_as_silence() {
    // The defect this meter exists not to inherit: `if a > peak` is false for
    // NaN, so a NaN storm reads to the engine's watchdog as silence.
    let mut meter = CaptureMeter::new();
    let block = vec![f64::NAN; 64];
    assert_eq!(
        meter.observe(&block),
        Some(MeasurementDiagnostic::InputClipping)
    );
    assert_eq!(meter.clipped_samples(), 64);
    assert_eq!(
        meter.peak_dbfs(),
        f64::NEG_INFINITY,
        "and no NaN leaks into the peak"
    );
}

#[test]
fn the_clip_counter_rises_and_resets_while_the_peak_does_not() {
    let mut meter = CaptureMeter::new();
    meter.observe(&[1.0, 1.0, 0.1, 0.1]);
    assert_eq!(meter.clipped_samples(), 2);
    meter.observe(&[1.0, 0.1, 0.1, 0.1]);
    assert_eq!(meter.clipped_samples(), 3, "cumulative until reset");
    let peak = meter.peak_linear();
    meter.reset_clips();
    assert_eq!(meter.clipped_samples(), 0);
    assert_eq!(meter.clipped_blocks(), 0);
    assert_eq!(
        meter.peak_linear(),
        peak,
        "resetting clips must not put a false floor under the next block"
    );
}

#[test]
fn full_scale_exactly_counts_as_clipped() {
    // A converter that rails reports +-1.0; treating that as un-clipped is how
    // a railed capture passes for a hot one.
    let mut meter = CaptureMeter::new();
    meter.observe(&[1.0, -1.0, 0.9]);
    assert_eq!(meter.clipped_samples(), 2);
}

#[test]
fn an_empty_block_still_counts_as_a_block() {
    let mut meter = CaptureMeter::new();
    assert_eq!(meter.observe(&[]), None);
    assert_eq!(meter.blocks(), 1);
}

// ──────────────────────────── the record loop ────────────────────────────────

#[test]
fn a_clean_run_captures_exactly_what_was_asked_for() {
    let samples: Vec<f64> = (0..1000).map(|i| (i as f64 / 1000.0) * 0.5).collect();
    let mut source = MockSource::new(samples.clone(), 128);
    let mut meter = CaptureMeter::new();
    let run = record(&mut source, 512, &mut meter, &AbortHandle::new()).expect("clean run");
    assert_eq!(run.ended_early, None);
    assert_eq!(run.samples, samples[..512]);
    assert_eq!(run.sanitized, 0);
    assert_eq!(meter.blocks(), 4);
}

#[test]
fn a_capture_shorter_than_a_block_is_not_an_error() {
    let mut source = MockSource::new(vec![0.25; 1000], 512);
    let mut meter = CaptureMeter::new();
    let run = record(&mut source, 10, &mut meter, &AbortHandle::new()).expect("short run");
    assert_eq!(run.samples.len(), 10);
    assert_eq!(run.ended_early, None);
}

#[test]
fn an_exhausted_source_ends_the_run_rather_than_spinning() {
    let mut source = MockSource::new(vec![0.25; 300], 128);
    let mut meter = CaptureMeter::new();
    let run = record(&mut source, 4096, &mut meter, &AbortHandle::new()).expect("run");
    assert_eq!(run.ended_early, Some(CaptureEnd::SourceExhausted));
    assert_eq!(run.samples.len(), 300);
}

#[test]
fn non_finite_input_is_zeroed_and_counted_before_the_caller_sees_it() {
    // MS-4's boundary 2: a NaN in a recorded IR propagates through
    // `deconvolve` into NaN correction coefficients, and one NaN poisons the
    // DF2T feedback state permanently.
    let mut samples = vec![0.25; 128];
    samples[3] = f64::NAN;
    samples[7] = f64::INFINITY;
    let mut source = MockSource::new(samples, 128);
    let mut meter = CaptureMeter::new();
    let run = record(&mut source, 128, &mut meter, &AbortHandle::new()).expect("run");
    assert!(run.samples.iter().all(|v| v.is_finite()));
    assert_eq!(run.samples[3], 0.0);
    assert_eq!(run.samples[7], 0.0);
    assert_eq!(run.sanitized, 2);
    assert_eq!(
        meter.clipped_samples(),
        2,
        "the meter must see the NaN before it is zeroed"
    );
}

#[test]
fn a_clipping_block_ends_the_run() {
    let mut samples = vec![0.1; 256];
    samples[128..].fill(1.0);
    let mut source = MockSource::new(samples, 128);
    let mut meter = CaptureMeter::new();
    let run = record(&mut source, 256, &mut meter, &AbortHandle::new()).expect("run");
    assert_eq!(run.ended_early, Some(CaptureEnd::Clipping));
    assert_eq!(
        run.samples.len(),
        256,
        "the offending block is still returned — the diagnostic explains it"
    );
}

#[test]
fn an_armed_abort_stops_the_capture_between_blocks() {
    let mut source = MockSource::new(vec![0.25; 4096], 128);
    let mut meter = CaptureMeter::new();
    let abort = AbortHandle::new();
    abort.trigger(AbortReason::MicDisconnected);
    let run = record(&mut source, 4096, &mut meter, &abort).expect("run");
    assert_eq!(
        run.ended_early,
        Some(CaptureEnd::Aborted(AbortReason::MicDisconnected))
    );
    assert!(run.samples.is_empty(), "armed before the first block");
}

#[test]
fn a_source_failure_propagates_rather_than_being_swallowed() {
    let mut source = MockSource::new(vec![0.25; 4096], 128);
    source.fail_after = Some(2);
    let mut meter = CaptureMeter::new();
    assert!(record(&mut source, 4096, &mut meter, &AbortHandle::new()).is_err());
}

#[test]
fn non_finite_samples_are_counted_apart_from_hot_ones() {
    // Folding NaN into the clip count is what stops a NaN storm reading as
    // silence, but the two are not the same condition and the clipping
    // diagnostic's advice ("turn the input gain down") does nothing about a
    // NaN. A caller must be able to tell them apart before rendering it.
    let mut meter = CaptureMeter::new();
    meter.observe(&[f64::NAN, 1.0, 0.1, 0.1]);
    assert_eq!(meter.clipped_samples(), 2, "both count as clipped");
    assert_eq!(meter.non_finite_samples(), 1, "only one is a defect");
    meter.reset_clips();
    assert_eq!(meter.non_finite_samples(), 0);
}
