//! The audio half, driven through a mock sink — no device, no process.
//!
//! What is pinned here is the teardown invariant this whole binary exists to
//! keep: **every exit path ramps and then destroys the render device.** A
//! player that returned early, or that reported an error before tearing down,
//! would leave a private aggregate wrapping the user's output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use paraeq_coreaudio::{abort_envelope, abort_ramp_len, MeasureError, RenderSink, StreamFormat};
use paraeq_stimulus::player::{Outcome, Player};
use paraeq_stimulus::protocol::ExitCode;

const BLOCK: usize = 64;
const RATE: f64 = 48_000.0;

#[derive(Default)]
struct Recorded {
    /// Every sample handed to the sink, in order. (Accepted, which since R-B1
    /// is no longer the same thing as rendered — see `rendered`.)
    written: Vec<f32>,
    /// Accepted but not yet taken by the device: the mock's ONE block of
    /// queue. A real paced sink returns from `write` while up to one block is
    /// still in flight, which is why the abort ramp could be lost.
    queued: Vec<f32>,
    /// Every sample the DEVICE actually rendered, in order.
    rendered: Vec<f32>,
    /// Accepted samples `stop` discarded instead of rendering.
    dropped: usize,
    /// `ramp_out` was called, and at which sample offset.
    ramped_at: Option<usize>,
    stops: usize,
    /// Calls after `ramp_out` (the sink refuses a second one; the player must
    /// not try).
    writes_after_ramp: usize,
}

/// A sink with a REAL queue, so the MS-14 acceptance tests can see a ramp be
/// lost.
///
/// The old mock appended every block straight to a `Vec`, which made it
/// structurally incapable of dropping anything — so `RenderSink::stop`'s
/// pre-R-B1 flush-first order passed every test here while discarding the
/// whole 5 ms ramp on a real device. This one holds a block back, exactly as
/// the pacing loop does, and `drain_on_stop` chooses which contract it keeps.
#[derive(Clone)]
struct MockSink {
    /// R-B1's contract: `stop` lets everything already queued render. False
    /// models the OLD sink, which dropped it — see
    /// `a_sink_that_drops_its_queue_at_stop_loses_the_whole_abort_ramp`.
    drain_on_stop: Arc<AtomicBool>,
    fail_next_write: Arc<AtomicBool>,
    log: Arc<Mutex<Recorded>>,
}

impl Default for MockSink {
    fn default() -> Self {
        MockSink {
            drain_on_stop: Arc::new(AtomicBool::new(true)),
            fail_next_write: Arc::default(),
            log: Arc::default(),
        }
    }
}

impl MockSink {
    /// Keep the pre-R-B1 contract: drop whatever is still queued at `stop`.
    fn dropping_stop() -> MockSink {
        let sink = MockSink::default();
        sink.drain_on_stop.store(false, Ordering::SeqCst);
        sink
    }
}

impl RenderSink for MockSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: BLOCK,
            sample_rate_hz: RATE,
        }
    }

    fn ramp_out(&mut self) {
        let mut log = self.log.lock().expect("uncontended");
        let at = log.written.len();
        log.ramped_at = Some(at);
    }

    fn stop(&mut self) {
        let mut log = self.log.lock().expect("uncontended");
        log.stops += 1;
        let queued = std::mem::take(&mut log.queued);
        if self.drain_on_stop.load(Ordering::SeqCst) {
            log.rendered.extend_from_slice(&queued);
        } else {
            log.dropped += queued.len();
        }
    }

    fn write(&mut self, block: &[f32]) -> Result<(), MeasureError> {
        if self.fail_next_write.swap(false, Ordering::SeqCst) {
            return Err(MeasureError::Sink(
                "the device stopped consuming".to_owned(),
            ));
        }
        let mut log = self.log.lock().expect("uncontended");
        if log.ramped_at.is_some() {
            log.writes_after_ramp += 1;
        }
        log.written.extend_from_slice(block);
        log.queued.extend_from_slice(block);
        // Pace: the device takes everything but the last block, which is what
        // a paced `write` returning "within about one block of caught up"
        // means.
        let keep = BLOCK.min(log.queued.len());
        let take = log.queued.len() - keep;
        let taken: Vec<f32> = log.queued.drain(..take).collect();
        log.rendered.extend_from_slice(&taken);
        Ok(())
    }
}

fn stimulus(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 * ((i % 7) as f32 / 7.0)).collect()
}

#[test]
fn a_clean_play_emits_every_sample_once_and_then_tears_down() {
    let sink = MockSink::default();
    let log = Arc::clone(&sink.log);
    let mut player = Player::new(Box::new(sink));
    let samples = stimulus(BLOCK * 3 + 11);

    let outcome = player
        .play(&samples, &|| false, |_| {})
        .expect("a clean play");

    match outcome {
        Outcome::Done {
            clamped,
            frames_emitted,
            sanitized,
        } => {
            assert_eq!(frames_emitted as usize, samples.len());
            assert_eq!((clamped, sanitized), (0, 0), "a verified stimulus is clean");
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let log = log.lock().expect("uncontended");
    assert_eq!(log.written, samples, "every sample, once, in order");
    assert_eq!(log.stops, 1, "the device is destroyed exactly once");
    assert!(log.ramped_at.is_none(), "a clean play never arms the abort");
    // R-B1's second-order half: the CLEAN end of file was truncated too. The
    // last block in flight is the tail of MS-4's 50 ms fade-out — quieter than
    // an abort ramp, but discarding it is the same defect one order down.
    assert_eq!(log.dropped, 0);
    assert_eq!(
        log.rendered, samples,
        "the file's own fade-out reached the device as well"
    );
}

#[test]
fn an_abort_arms_the_sink_first_then_writes_exactly_one_faded_block() {
    // Arming FIRST is what guarantees nothing at level can follow the fade.
    // The alternative — stopping — is itself the full-scale click the fade
    // exists to prevent.
    let sink = MockSink::default();
    let log = Arc::clone(&sink.log);
    let mut player = Player::new(Box::new(sink));
    let samples = stimulus(BLOCK * 20);

    // Abort from the very first poll, so the fade starts at sample 0 and the
    // expected coefficients are unambiguous.
    let outcome = player
        .play(&samples, &|| true, |_| {})
        .expect("an abort is not a failure");

    let ramp_len = abort_ramp_len(RATE);
    match outcome {
        Outcome::Aborted {
            frames_emitted,
            ramp_frames,
        } => {
            assert_eq!(frames_emitted, 0, "nothing played before the abort");
            assert_eq!(ramp_frames as usize, ramp_len);
        }
        other => panic!("expected Aborted, got {other:?}"),
    }

    let log = log.lock().expect("uncontended");
    assert_eq!(log.ramped_at, Some(0), "armed before the fade was written");
    assert_eq!(log.writes_after_ramp, 1, "exactly one block after arming");
    assert_eq!(log.written.len(), ramp_len);
    assert_eq!(log.stops, 1, "the device is destroyed");
    // R-B1: handed to the sink is not enough — every ramp sample must reach
    // the DEVICE. `stop` follows the ramp write immediately, and the sink is
    // paced, so a stop that dropped its queue would drop the ramp entire.
    assert_eq!(log.dropped, 0, "no accepted sample was discarded");
    assert_eq!(
        log.rendered, log.written,
        "the ramp was rendered, not flushed into a counter"
    );

    // And the fade is the SHARED envelope applied to the stimulus's own next
    // samples — not a re-render, and not a second shape.
    let env = abort_envelope(ramp_len, ramp_len);
    for (i, (&written, &coefficient)) in log.written.iter().zip(&env).enumerate() {
        assert_eq!(
            written,
            (f64::from(samples[i]) * coefficient) as f32,
            "index {i}"
        );
    }
    assert_eq!(
        *log.written.last().expect("a ramp"),
        0.0,
        "the fade ends at exactly zero"
    );
}

#[test]
fn an_abort_past_the_end_of_the_stimulus_fades_silence_rather_than_reaching_past_it() {
    // The stimulus runs out mid-fade: fading from where the signal actually is
    // means the tail is silence, and the read must not run off the buffer.
    let sink = MockSink::default();
    let log = Arc::clone(&sink.log);
    let mut player = Player::new(Box::new(sink));
    let ramp_len = abort_ramp_len(RATE);
    let samples = stimulus(BLOCK);

    let armed = AtomicBool::new(false);
    let outcome = player
        .play(&samples, &|| armed.swap(true, Ordering::SeqCst), |_| {})
        .expect("an abort after the first block");

    assert!(matches!(outcome, Outcome::Aborted { .. }));
    let log = log.lock().expect("uncontended");
    assert_eq!(
        log.written.len(),
        BLOCK + ramp_len,
        "one full block, then one fade block"
    );
    assert!(
        log.written[BLOCK..].iter().all(|&v| v == 0.0),
        "past the end of the stimulus the fade has silence to fade"
    );
    assert_eq!(log.stops, 1);
}

#[test]
fn a_sink_failure_still_destroys_the_device_before_it_is_reported() {
    // The rung that matters: reporting the error first and tearing down
    // afterwards would leave a private aggregate behind on exactly the path
    // where something has already gone wrong.
    let sink = MockSink::default();
    let log = Arc::clone(&sink.log);
    let fail = Arc::clone(&sink.fail_next_write);
    let mut player = Player::new(Box::new(sink));
    fail.store(true, Ordering::SeqCst);

    let refusal = player
        .play(&stimulus(BLOCK * 4), &|| false, |_| {})
        .expect_err("a device that stopped consuming is a refusal");
    assert_eq!(refusal.code, ExitCode::Stalled);
    assert_eq!(refusal.code.code(), 5);

    // The write that failed was an ordinary one, so `play` returned through
    // `?` — and the caller's own `stop` is what runs. Assert the player can
    // still be stopped and that stopping is idempotent from here.
    player.stop();
    player.stop();
    assert!(
        log.lock().expect("uncontended").stops >= 1,
        "the device is destroyed on the failure path too"
    );
}

#[test]
fn a_failed_fade_tears_down_before_it_reports() {
    // The same rule one rung deeper: if the FADE's write fails, the device
    // still has to be destroyed, and the error is reported after.
    let sink = MockSink::default();
    let log = Arc::clone(&sink.log);
    let fail = Arc::clone(&sink.fail_next_write);
    let mut player = Player::new(Box::new(sink));
    fail.store(true, Ordering::SeqCst);

    let refusal = player
        .play(&stimulus(BLOCK * 4), &|| true, |_| {})
        .expect_err("the fade's write failed");
    assert_eq!(refusal.code, ExitCode::Stalled);

    let log = log.lock().expect("uncontended");
    assert!(log.ramped_at.is_some(), "the sink was armed");
    assert_eq!(
        log.stops, 1,
        "and the device was destroyed even though the fade could not be written"
    );
}

#[test]
fn the_guard_counts_reach_the_caller() {
    // The MS-4 guard is belt-and-braces, not a processing stage — a verified
    // stimulus produces zeros — so a nonzero count is a defect the parent turns
    // into its own warning, and it has to travel.
    let sink = MockSink::default();
    let log = Arc::clone(&sink.log);
    let mut player = Player::new(Box::new(sink));
    let mut samples = stimulus(BLOCK);
    samples[3] = f32::NAN;
    samples[5] = 1.5;

    let outcome = player
        .play(&samples, &|| false, |_| {})
        .expect("the guard fixes the block rather than refusing it");
    match outcome {
        Outcome::Done {
            clamped, sanitized, ..
        } => {
            assert_eq!(sanitized, 1, "the NaN was zeroed");
            assert_eq!(clamped, 1, "the over-scale sample was clamped");
        }
        other => panic!("expected Done, got {other:?}"),
    }
    let log = log.lock().expect("uncontended");
    assert_eq!(log.written[3], 0.0, "no NaN reaches the device");
    assert_eq!(log.written[5], 1.0);
}

#[test]
fn progress_is_reported_without_a_clock_on_the_render_path() {
    let sink = MockSink::default();
    let mut player = Player::new(Box::new(sink));
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink_reports = Arc::clone(&reports);

    player
        .play(&stimulus(RATE as usize), &|| false, move |frames| {
            sink_reports.lock().expect("uncontended").push(frames);
        })
        .expect("a one-second play");

    let reports = reports.lock().expect("uncontended");
    assert!(
        reports.len() >= 4,
        "roughly four times a second, got {reports:?}"
    );
    assert!(
        reports.windows(2).all(|pair| pair[1] > pair[0]),
        "frame counts only rise: {reports:?}"
    );
}

#[test]
fn a_sink_that_drops_its_queue_at_stop_loses_the_whole_abort_ramp() {
    // The defect R-B1 fixes, kept as a live witness rather than a paragraph.
    // A paced sink returns from `write` with up to one block still in flight;
    // after `abort_from` that block IS the ramp. A `stop` that discards it —
    // which is what arming the flush before stopping the IOProc did — leaves
    // the DEVICE's last rendered sample at level: "a hard stop is itself a
    // full-scale click", on every abort.
    let sink = MockSink::dropping_stop();
    let log = Arc::clone(&sink.log);
    let mut player = Player::new(Box::new(sink));
    let samples = stimulus(BLOCK * 20);

    player
        .play(&samples, &|| true, |_| {})
        .expect("an abort is not a failure");

    let log = log.lock().expect("uncontended");
    let ramp_len = abort_ramp_len(RATE);
    assert_eq!(
        log.written.len(),
        ramp_len,
        "the ramp was handed over in full"
    );
    assert_eq!(
        log.dropped, BLOCK,
        "the block still in flight was discarded — and it is the ramp's tail"
    );
    assert_eq!(log.rendered.len(), ramp_len - BLOCK);
    // The acoustic consequence, stated as an assertion: the fade is cut off
    // while its envelope is still well above zero — about -16 dB of the
    // ramp's starting level here — so the device's last rendered sample is a
    // step to silence, which is the click. (Asserting on the ENVELOPE rather
    // than on the sample: the stimulus itself crosses zero, so an individual
    // sample near the cut can be 0.0 by coincidence.)
    let env = abort_envelope(ramp_len, ramp_len);
    assert!(
        env[log.rendered.len() - 1] > 0.1,
        "the fade never got near zero before the device stopped: envelope {}",
        env[log.rendered.len() - 1]
    );
}
