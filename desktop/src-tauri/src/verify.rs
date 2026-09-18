//! The desktop's half of the closed-loop verification pass: the type mappings
//! nobody else can write, the capture adapter the two shipped contracts need
//! between them, and the lift from a finished pass into a `decide()` bundle.
//!
//! # Why these live HERE and nowhere else
//!
//! Each item below names two crates that may not name each other:
//!
//! - `CaptureRouting` is `paraeq-decide`'s and `StimulusRouting` is
//!   `paraeq-coreaudio`'s, and neither crate depends on the other.
//! - `CorrectionPlan` is `paraeq-decide`'s and `CorrectionConfig` is
//!   `paraeq-engine`'s, and `paraeq-decide` may not depend on `paraeq-engine`.
//! - `VerifyOutcome` is `paraeq-measure`'s and `bundle::Verification` is
//!   `paraeq-decide`'s, and those two may not depend on each other either --
//!   which is why they are documented twins, field for field, rather than one
//!   re-export.
//!
//! The desktop is the one crate that can see every side, so the mappings are
//! written once, here, with the tests that pin them.

use paraeq_decide::bundle::{
    CaptureRouting, CaptureStats, ImpulseResponse as BundleIr, TwoClockFit, Verification,
};
use paraeq_decide::outcome::CorrectionPlan;
use paraeq_engine::controller::CorrectionConfig;
use paraeq_measure::seam::HelperRouting;
use paraeq_measure::{CaptureSource, MeasureError, StreamFormat, VerifyOutcome};
use std::time::{Duration, Instant};

/// How long [`WaitingCapture`] waits for a mic that has stopped delivering
/// before it reports the stream ended.
///
/// Long enough that no scheduling hiccup reaches it, short enough that a dead
/// mic ENDS the run rather than hanging it. Taken verbatim from the hardware
/// harness's own `CAPTURE_STALL_TIMEOUT`, so the rig and the app wait the same
/// amount of time for the same fact.
const CAPTURE_STALL_TIMEOUT: Duration = Duration::from_secs(2);

// ─────────────────────────── the capture adapter ───────────────────────────

/// A [`CaptureSource`] that WAITS for the device rather than reporting an empty
/// ring as the end of the stream.
///
/// # Why this exists — two shipped contracts that disagree about zero
///
/// `paraeq_measure::record` treats a zero-frame read as
/// `CaptureEnd::SourceExhausted`, and its own doc gives the reason: "retrying
/// it forever is how a dead stream becomes a hang". That is the right rule for
/// a capture loop, which must not spin, and it is deliberately not changed
/// here.
///
/// `paraeq_coreaudio::MicCapture::capture` is a NON-BLOCKING ring drain. It
/// returns 0 frames whenever the caller outruns the device, which on healthy
/// hardware is most polls — the reader is a control-plane loop and the device
/// fills the ring one IOProc callback at a time.
///
/// Wired to each other directly, a verification pass refuses `MicDisconnected`
/// microseconds into its first capture span, **on every run, on a perfect
/// rig**. The waiting has to live somewhere, and it may not live in `record`
/// (whose no-spin rule is the safety property) nor in `MicCapture` (whose
/// non-blocking drain is what keeps it usable from a realtime-adjacent
/// caller). So it lives in the adapter between them: poll until at least one
/// frame arrives, and give up after [`CAPTURE_STALL_TIMEOUT`] so a genuinely
/// dead mic still reaches `SourceExhausted` instead of hanging the app.
///
/// Found by the hardware-test lane, which closed it test-side with an adapter
/// of exactly this shape in `crates/paraeq-coreaudio/tests/`. This is the
/// production twin it said the wiring owed.
pub struct WaitingCapture {
    inner: Box<dyn CaptureSource>,
    /// Overridable so the unit tests below are milliseconds rather than
    /// seconds. Production always uses [`CAPTURE_STALL_TIMEOUT`].
    stall_timeout: Duration,
}

impl WaitingCapture {
    pub fn new(inner: Box<dyn CaptureSource>) -> WaitingCapture {
        WaitingCapture {
            inner,
            stall_timeout: CAPTURE_STALL_TIMEOUT,
        }
    }

    /// For tests: the same adapter with a shorter patience.
    #[cfg(test)]
    fn with_timeout(inner: Box<dyn CaptureSource>, stall_timeout: Duration) -> WaitingCapture {
        WaitingCapture {
            inner,
            stall_timeout,
        }
    }
}

impl CaptureSource for WaitingCapture {
    fn format(&self) -> StreamFormat {
        self.inner.format()
    }

    fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError> {
        let deadline = Instant::now() + self.stall_timeout;
        loop {
            let got = self.inner.capture(block)?;
            if got > 0 {
                return Ok(got);
            }
            if Instant::now() >= deadline {
                // Genuinely exhausted as far as any caller can tell, which is
                // what `record` needs to hear in order to STOP rather than
                // spin. Passing the zero through is the whole point: this
                // adapter delays that verdict, it never suppresses it.
                return Ok(0);
            }
            // A millisecond is well under one IOProc block at any supported
            // rate, so no frame waits long, and it is long enough that this
            // loop is a sleep rather than a spin on a control-plane thread.
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        self.inner.stop()
    }
}

// ───────────────────────────── routing mappings ────────────────────────────

/// A routing that cannot be represented on the other side.
///
/// Hand-written `Display` rather than a `thiserror` derive: this crate carries
/// no error-derive dependency today, and one enum with one variant does not
/// earn one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutingError {
    ChannelTooWide { channel: usize },
}

impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingError::ChannelTooWide { channel } => write!(
                f,
                "channel index {channel} does not fit the wire's u32 -- refusing rather than \
                 truncating, because a truncated index re-routes the sweep to another ear with \
                 no symptom"
            ),
        }
    }
}

impl std::error::Error for RoutingError {}

/// `CaptureRouting` -> `StimulusRouting`. TOTAL, and infallible on every
/// supported target: `u32` -> `usize` widens.
///
/// This is the direction the product uses -- the plan says which channel the
/// baseline measured, and the helper is told to play there.
pub fn capture_routing_to_stimulus(
    routing: CaptureRouting,
) -> paraeq_coreaudio::measure_aggregate::StimulusRouting {
    use paraeq_coreaudio::measure_aggregate::StimulusRouting;
    match routing {
        CaptureRouting::Both => StimulusRouting::Both,
        CaptureRouting::Only(channel) => StimulusRouting::Only(channel as usize),
    }
}

/// `StimulusRouting` -> `CaptureRouting`. FALLIBLE, and the failure is the
/// point.
///
/// `StimulusRouting::Only` carries a platform-width `usize` and `CaptureRouting`
/// carries a `u32`, because the latter is a frozen wire shape in
/// `fixtures/decide/` and a platform-width integer does not belong in one. So
/// the narrowing can lose information, and losing it silently would re-route
/// audio: `Only(2^32)` truncates to `Only(0)`, which is a wrong-ear
/// measurement that produces a plausible-looking residual and no symptom
/// whatsoever. It refuses instead.
pub fn stimulus_routing_to_capture(
    routing: paraeq_coreaudio::measure_aggregate::StimulusRouting,
) -> Result<CaptureRouting, RoutingError> {
    use paraeq_coreaudio::measure_aggregate::StimulusRouting;
    match routing {
        StimulusRouting::Both => Ok(CaptureRouting::Both),
        StimulusRouting::Only(channel) => u32::try_from(channel)
            .map(CaptureRouting::Only)
            .map_err(|_| RoutingError::ChannelTooWide { channel }),
    }
}

/// `CaptureRouting` -> the helper seam's own spelling.
///
/// A third enum for the same idea would be wrong, which is why
/// `HelperRouting::Only` is already the same width as `CaptureRouting::Only`:
/// this map is total in both directions and carries no refusal.
pub fn capture_routing_to_helper(routing: CaptureRouting) -> HelperRouting {
    match routing {
        CaptureRouting::Both => HelperRouting::Both,
        CaptureRouting::Only(channel) => HelperRouting::Only(channel),
    }
}

/// The helper seam's spelling -> `CaptureRouting`. Total, same widths.
pub fn helper_routing_to_capture(routing: HelperRouting) -> CaptureRouting {
    match routing {
        HelperRouting::Both => CaptureRouting::Both,
        HelperRouting::Only(channel) => CaptureRouting::Only(channel),
    }
}

// ────────────────────────── plan -> engine correction ──────────────────────

/// `decide()`'s plan as the engine's install command.
///
/// **`design_rate` is provenance, not an instruction.** Since R1-6 the engine
/// re-derives every band at whatever rate its stream is actually running, so
/// this field records the rate the bands were FITTED at and never the rate the
/// coefficients are designed at. Carrying it is what lets the engine report a
/// `correction_rate_mismatch` instead of silently running coefficients from
/// another rate.
///
/// **`preamp_db` is deliberately NOT mapped.** It reaches the engine as
/// `Correction.preamp_lin`, computed by the engine itself from the bands that
/// survived at the live rate, and applied on the corrected path only. Passing
/// the plan's number through as a separate `SetGainDb` would apply it on BOTH
/// chain paths, leaving the bypassed side of an A/B quieter by the whole
/// preamp -- and it would be the wrong number besides, because `decide()`
/// computed it at `design_rate` over all bands. The verification gate's whole
/// job is to check that the engine's own number agrees with a recomputation,
/// so handing it ours would be marking our own homework.
///
/// `clamps` and `dropped` are the drawer's evidence and mean nothing to the
/// realtime chain, so they do not cross.
pub fn correction_plan_to_peq_config(plan: &CorrectionPlan) -> CorrectionConfig {
    CorrectionConfig::Peq {
        bands: plan.bands.as_slice().to_vec(),
        design_rate: plan.design_rate,
    }
}

// ─────────────────────── outcome -> bundle verification ────────────────────

/// Lift a finished pass into the bundle block `decide()` grades.
///
/// # The mono-to-per-channel lift, which is the only real decision here
///
/// [`VerifyOutcome::ir`] is MONO: one microphone measured one acoustic path,
/// so there is exactly one row of samples. `bundle::Verification::ir` is
/// per-capture-channel, and `decide()`'s verification gate refuses outright
/// unless it has the same number of rows as
/// `bundle.positions[position_index].ir` -- "differencing captures of different
/// width is not a residual".
///
/// So the row is replicated to `capture_channels`, which the caller reads off
/// the BASELINE position this pass re-measured. That is the honest lift rather
/// than a convenient one: the baseline at that position was recorded through
/// the same single mic and is per-channel for the same reason, so the two sides
/// are the same measurement replicated the same way, and the gate's per-channel
/// residual is then comparing like with like. Under `Only(n)` every row is
/// predicted by `installed.bands[n]` and under `Both` every row is predicted by
/// the common band set, so the prediction is row-independent either way -- the
/// replication cannot smuggle in a per-channel difference that was not
/// measured.
///
/// # What `installed` is, and why it is an argument
///
/// The plan the engine was RUNNING when this pass captured -- the ARMED plan,
/// supplied by the caller that armed it. It is not derivable from the outcome:
/// the pass deliberately carries the engine's own preamp
/// ([`VerifyOutcome::installed_preamp_db`]) and not the plan, because those are
/// two different numbers whenever the design rate and the live rate differ, and
/// `decide()` refuses when they disagree by more than it allows. Passing the
/// plan in keeps both on the record.
pub fn lift_verification(
    outcome: &VerifyOutcome,
    installed: CorrectionPlan,
    capture_channels: usize,
) -> Verification {
    Verification {
        capture: CaptureStats {
            clipped_samples: outcome.capture.clipped_samples,
            peak_dbfs: outcome.capture.peak_dbfs,
            rms_dbfs: outcome.capture.rms_dbfs,
        },
        gain_db: f64::from(outcome.gain_db),
        installed,
        installed_preamp_db: outcome.installed_preamp_db,
        ir: BundleIr {
            // `peak` is a fractional, parabolic-refined index on the measure
            // side and a whole sample on the wire. `peak_index()` is the
            // rounding the DSP crate itself defines, so the two sides cannot
            // round differently.
            peak: outcome.ir.peak_index(),
            sample_rate: outcome.ir.sample_rate,
            samples: vec![outcome.ir.samples.clone(); capture_channels.max(1)],
        },
        level_dbfs: outcome.level_dbfs,
        position_index: outcome.position_index,
        routing: helper_routing_to_capture(outcome.routing),
        running_rate_hz: outcome.running_rate_hz,
        two_clock: outcome.two_clock.map(|fit| TwoClockFit {
            intercept_samples: fit.intercept_samples,
            residual_peak_samples: fit.residual_peak_samples,
            residual_rms_samples: fit.residual_rms_samples,
            skew_ppm: fit.skew_ppm,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paraeq_coreaudio::measure_aggregate::StimulusRouting;
    use paraeq_dsp::peq::{EQBand, FilterType};
    use paraeq_dsp::PerChannel;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ── the capture adapter ────────────────────────────────────────────────

    /// A source that returns 0 frames `empties` times and then delivers.
    struct Stuttering {
        delivered: Arc<AtomicUsize>,
        empties: usize,
        polls: Arc<AtomicUsize>,
    }

    impl CaptureSource for Stuttering {
        fn format(&self) -> StreamFormat {
            StreamFormat {
                channels: 1,
                frames_per_block: 512,
                sample_rate_hz: 48_000.0,
            }
        }

        fn capture(&mut self, block: &mut [f64]) -> Result<usize, MeasureError> {
            let n = self.polls.fetch_add(1, Ordering::SeqCst);
            if n < self.empties {
                return Ok(0);
            }
            block[0] = 0.5;
            self.delivered.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }

        fn stop(&mut self) -> Result<(), MeasureError> {
            Ok(())
        }
    }

    /// The healthy rig. A non-blocking ring drain returns 0 whenever the reader
    /// outruns the device, which is MOST polls -- so without the wait, a
    /// verification pass refuses `MicDisconnected` microseconds into its first
    /// span on every run.
    #[test]
    fn an_empty_ring_is_waited_out_rather_than_reported_as_the_end_of_the_stream() {
        let delivered = Arc::new(AtomicUsize::new(0));
        let polls = Arc::new(AtomicUsize::new(0));
        let mut capture = WaitingCapture::with_timeout(
            Box::new(Stuttering {
                delivered: Arc::clone(&delivered),
                empties: 3,
                polls: Arc::clone(&polls),
            }),
            Duration::from_millis(500),
        );

        let mut block = [0.0f64; 8];
        assert_eq!(
            capture.capture(&mut block).expect("no error"),
            1,
            "the adapter must hand back the frame that eventually arrived"
        );
        assert_eq!(block[0], 0.5, "and the samples with it");
        assert!(
            polls.load(Ordering::SeqCst) > 3,
            "it polled through the empties"
        );
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
    }

    /// The dead mic. The adapter DELAYS the verdict; it must never suppress it,
    /// or a disconnected microphone hangs the app instead of ending the run.
    #[test]
    fn a_source_that_never_delivers_still_ends_as_exhausted_after_the_deadline() {
        struct Silent;
        impl CaptureSource for Silent {
            fn format(&self) -> StreamFormat {
                StreamFormat {
                    channels: 1,
                    frames_per_block: 512,
                    sample_rate_hz: 48_000.0,
                }
            }

            fn capture(&mut self, _block: &mut [f64]) -> Result<usize, MeasureError> {
                Ok(0)
            }

            fn stop(&mut self) -> Result<(), MeasureError> {
                Ok(())
            }
        }

        let mut capture = WaitingCapture::with_timeout(Box::new(Silent), Duration::from_millis(50));
        let started = Instant::now();
        let mut block = [0.0f64; 8];
        assert_eq!(
            capture.capture(&mut block).expect("no error"),
            0,
            "a zero must still reach `record`, which is what ends the run"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "and only after the deadline, not on the first empty poll"
        );
    }

    /// An error is not an empty ring. It propagates immediately rather than
    /// being retried for two seconds -- a mic that has gone away reports itself.
    #[test]
    fn a_capture_error_is_not_waited_out() {
        struct Broken;
        impl CaptureSource for Broken {
            fn format(&self) -> StreamFormat {
                StreamFormat {
                    channels: 1,
                    frames_per_block: 512,
                    sample_rate_hz: 48_000.0,
                }
            }

            fn capture(&mut self, _block: &mut [f64]) -> Result<usize, MeasureError> {
                Err(MeasureError::Capture("the mic went away".to_owned()))
            }

            fn stop(&mut self) -> Result<(), MeasureError> {
                Ok(())
            }
        }

        let mut capture = WaitingCapture::with_timeout(Box::new(Broken), Duration::from_secs(60));
        let started = Instant::now();
        let mut block = [0.0f64; 8];
        capture.capture(&mut block).expect_err("errors propagate");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    // ── the routing mappings ───────────────────────────────────────────────

    /// TOTAL: every `CaptureRouting` has an image, including the widths a
    /// `usize` payload makes reachable from the other side.
    #[test]
    fn capture_routing_maps_totally_to_stimulus_routing() {
        assert_eq!(
            capture_routing_to_stimulus(CaptureRouting::Both),
            StimulusRouting::Both
        );
        for channel in [0u32, 1, 7, 255, u32::MAX] {
            assert_eq!(
                capture_routing_to_stimulus(CaptureRouting::Only(channel)),
                StimulusRouting::Only(channel as usize),
                "channel {channel} must survive the widening"
            );
            // And it round-trips, which is what makes "total" a useful claim
            // rather than a statement about one direction.
            assert_eq!(
                stimulus_routing_to_capture(StimulusRouting::Only(channel as usize)),
                Ok(CaptureRouting::Only(channel))
            );
        }
        assert_eq!(
            stimulus_routing_to_capture(StimulusRouting::Both),
            Ok(CaptureRouting::Both)
        );
    }

    /// The narrowing REFUSES. Truncation would silently re-route the sweep to
    /// channel 0 -- a wrong-ear measurement that differences a per-ear baseline
    /// against the other ear's response and produces no symptom at all.
    ///
    /// Only reachable on a 64-bit target, which is every target this app ships
    /// on; on a 32-bit one the conversion is already infallible and there is
    /// nothing to refuse.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn stimulus_routing_narrowing_refuses_rather_than_truncating() {
        let too_wide = u32::MAX as usize + 1;
        assert_eq!(
            stimulus_routing_to_capture(StimulusRouting::Only(too_wide)),
            Err(RoutingError::ChannelTooWide { channel: too_wide }),
            "the refusal names the channel it could not carry"
        );
        // The specific accident this guards: `too_wide as u32` is 0.
        assert_eq!(too_wide as u32, 0);
        assert_ne!(
            stimulus_routing_to_capture(StimulusRouting::Only(too_wide)),
            Ok(CaptureRouting::Only(0)),
            "truncating to channel 0 is the failure, not the fallback"
        );
    }

    // ── plan -> engine correction ──────────────────────────────────────────

    fn plan() -> CorrectionPlan {
        CorrectionPlan {
            bands: PerChannel::new(vec![
                vec![EQBand {
                    fc: 120.0,
                    filter_type: FilterType::Peaking,
                    gain_db: 6.0,
                    q: 1.5,
                }],
                vec![EQBand {
                    fc: 3200.0,
                    filter_type: FilterType::LowShelf,
                    gain_db: -2.0,
                    q: 0.7,
                }],
            ])
            .expect("two channels"),
            clamps: vec![Vec::new(), Vec::new()],
            design_rate: 48_000.0,
            dropped: Vec::new(),
            preamp_db: -6.0,
        }
    }

    /// The bands and the design rate cross; nothing else does.
    #[test]
    fn correction_plan_maps_to_peq_config() {
        let plan = plan();
        match correction_plan_to_peq_config(&plan) {
            CorrectionConfig::Peq { bands, design_rate } => {
                assert_eq!(bands, plan.bands.as_slice().to_vec());
                assert_eq!(
                    bands.len(),
                    2,
                    "per ENGINE channel index, in the plan's own order"
                );
                assert_eq!(design_rate, 48_000.0);
            }
            other => panic!("a PEQ plan must install as PEQ, got {other:?}"),
        }
    }

    /// The preamp does NOT ride the config. It is the engine's own number,
    /// recomputed at the live rate over the surviving bands and applied inside
    /// `Correction` on the corrected path -- and the verification gate exists
    /// to check that number against a recomputation, so handing it ours would
    /// be marking our own homework.
    #[test]
    fn the_plans_preamp_does_not_cross_into_the_engine_config() {
        let plan = plan();
        assert_eq!(plan.preamp_db, -6.0, "the plan carries a real preamp");
        let json = format!("{:?}", correction_plan_to_peq_config(&plan));
        assert!(
            !json.contains("preamp"),
            "no preamp field may reach the engine config: {json}"
        );
    }

    // ── outcome -> bundle verification ─────────────────────────────────────

    fn outcome() -> VerifyOutcome {
        VerifyOutcome {
            abort_acoustic_budget_ms: 21.7,
            capture: paraeq_measure::VerifyCaptureStats {
                clipped_samples: 0,
                peak_dbfs: -12.5,
                rms_dbfs: -31.0,
            },
            gain_db: 0.0,
            installed_preamp_db: -6.0,
            ir: paraeq_dsp::gating::ImpulseResponse {
                peak: 2304.4,
                sample_rate: 48_000,
                samples: vec![0.0, 1.0, 0.25, -0.125],
            },
            level_dbfs: -21.0,
            position_index: 2,
            routing: HelperRouting::Only(1),
            running_rate_hz: 44_100.0,
            two_clock: Some(paraeq_measure::VerifyTwoClockFit {
                intercept_samples: 2304.5,
                residual_peak_samples: 0.8,
                residual_rms_samples: 0.31,
                skew_ppm: 12.5,
            }),
            warnings: Vec::new(),
        }
    }

    /// Every field of the bundle block comes from the pass, and the mono IR is
    /// replicated to the baseline's channel count -- which is what makes the
    /// gate's per-channel precondition satisfiable.
    #[test]
    fn a_finished_pass_lifts_into_the_bundles_per_channel_shape() {
        let outcome = outcome();
        let lifted = lift_verification(&outcome, plan(), 2);

        assert_eq!(lifted.capture.clipped_samples, 0);
        assert_eq!(lifted.capture.peak_dbfs, -12.5);
        assert_eq!(lifted.capture.rms_dbfs, -31.0);
        assert_eq!(lifted.gain_db, 0.0);
        assert_eq!(lifted.installed.design_rate, 48_000.0);
        assert_eq!(lifted.installed_preamp_db, -6.0);
        assert_eq!(lifted.level_dbfs, -21.0);
        assert_eq!(lifted.position_index, 2);
        assert_eq!(lifted.routing, CaptureRouting::Only(1));
        assert_eq!(lifted.running_rate_hz, 44_100.0);
        assert_eq!(lifted.two_clock.expect("carried").skew_ppm, 12.5);

        assert_eq!(
            lifted.ir.peak, 2304,
            "the fractional peak is rounded by the DSP crate's own rule"
        );
        assert_eq!(lifted.ir.sample_rate, 48_000);
        assert_eq!(lifted.ir.samples.len(), 2, "one row per capture channel");
        assert_eq!(lifted.ir.samples[0], outcome.ir.samples);
        assert_eq!(
            lifted.ir.samples[0], lifted.ir.samples[1],
            "one mic measured one path: the rows are the same measurement"
        );
    }

    /// The gate refuses on a zero-width IR before it can say anything useful,
    /// so a caller that reads a channel count off an empty baseline must not be
    /// able to produce one.
    #[test]
    fn the_lift_never_produces_a_zero_channel_impulse_response() {
        let lifted = lift_verification(&outcome(), plan(), 0);
        assert_eq!(lifted.ir.samples.len(), 1);
    }

    /// The running rate is the pass's, never the plan's `design_rate`. `H(f)`
    /// is evaluated at the rate the engine RAN at, and the two legitimately
    /// differ -- this fixture has them 3.9 kHz apart on purpose.
    #[test]
    fn the_lift_carries_the_live_rate_not_the_design_rate() {
        let lifted = lift_verification(&outcome(), plan(), 1);
        assert_eq!(lifted.running_rate_hz, 44_100.0);
        assert_eq!(lifted.installed.design_rate, 48_000.0);
        assert_ne!(lifted.running_rate_hz, lifted.installed.design_rate);
    }

    // ── R-B4: a panicking worker must not wedge the slot ───────────────────

    /// The worker's publish used to be its last statement, so a panic in
    /// `run()`/`finish()`/`grade()` skipped it: the slot stayed `Running` with
    /// an abort handle for the life of the process, `arm` refused forever, and
    /// `abort` could not reset it. The user's only recovery was to restart
    /// ParaEQ.
    #[test]
    fn a_panicking_worker_publishes_failed_instead_of_staying_running() {
        let state = super::worker_state(|| panic!("injected: the pass blew up mid-grade"));
        match state {
            VerifyState::Failed {
                code,
                remedy,
                summary,
            } => {
                assert_eq!(code, None, "a panic carries no diagnostic number");
                assert!(
                    summary.contains("injected: the pass blew up mid-grade"),
                    "the panic's own message must survive into the summary: {summary}"
                );
                assert!(
                    remedy
                        .expect("a panic still gets a remedy")
                        .contains("ParaEQ"),
                    "and the remedy must say it is our bug, not theirs"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_worker_that_returns_normally_publishes_what_it_returned() {
        let state = super::worker_state(|| VerifyState::Failed {
            code: Some(42),
            remedy: None,
            summary: "a refusal, not a panic".to_owned(),
        });
        assert!(
            matches!(state, VerifyState::Failed { code: Some(42), .. }),
            "the guard must be transparent on the normal path, got {state:?}"
        );
    }

    /// `Running` is published before the worker starts and cleared by the
    /// worker's own last act. A `Running` whose worker has finished therefore
    /// means the clearing never happened — stale, not live.
    #[test]
    fn a_running_state_whose_worker_has_finished_is_cleared() {
        let mut runtime = super::VerifyRuntime {
            state: VerifyState::Running,
            worker: Some(std::thread::spawn(|| {})),
            ..Default::default()
        };
        // Wait for the thread to actually be finished, not merely spawned.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !runtime.worker.as_ref().expect("a worker").is_finished() {
            assert!(Instant::now() < deadline, "the empty worker never finished");
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(super::clear_stale_running(&mut runtime));
        assert!(matches!(runtime.state, VerifyState::Idle));
        assert!(runtime.worker.is_none());
    }

    /// The other side of the same rule: a pass that really is running must not
    /// be cleared out from under itself, or `arm` would happily start a second
    /// helper against the same device.
    #[test]
    fn a_running_state_with_a_live_worker_is_left_alone() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut runtime = super::VerifyRuntime {
            state: VerifyState::Running,
            worker: Some(std::thread::spawn(move || {
                let _ = rx.recv();
            })),
            ..Default::default()
        };

        assert!(!super::clear_stale_running(&mut runtime));
        assert!(matches!(runtime.state, VerifyState::Running));
        assert!(runtime.worker.is_some());

        drop(tx);
        runtime
            .worker
            .take()
            .expect("a worker")
            .join()
            .expect("it ends");
    }

    /// A slot that is not `Running` at all is never touched — an `Armed` pass
    /// waiting on MS-18 must survive an `arm` refusal, and a `Complete` report
    /// must survive being looked at.
    #[test]
    fn a_slot_that_is_not_running_is_never_cleared() {
        for state in [
            VerifyState::Idle,
            VerifyState::Armed {
                device_name: "AirPods Max".to_owned(),
                level_dbfs: -21.0,
                projected_spl_db: 78.0,
            },
        ] {
            let mut runtime = super::VerifyRuntime {
                state: state.clone(),
                ..Default::default()
            };
            assert!(!super::clear_stale_running(&mut runtime));
            assert_eq!(
                format!("{:?}", runtime.state),
                format!("{state:?}"),
                "the slot must be untouched"
            );
        }
    }
}

// ══════════════════════════ the verification runtime ═══════════════════════
//
// Arming, running and aborting one pass, with the result graded by `decide()`
// and published on the existing `app-state` event.

use crate::state::{AppShared, VerifyDiagnostic, VerifyReport, VerifyState};
use paraeq_decide::bundle::MeasurementBundle;
use paraeq_decide::outcome::{DecisionSet, Severity, Verdict};
use paraeq_dsp::two_clock::DEFAULT_MARKER_LAYOUT;
use paraeq_measure::{
    AbortHandle, AbortReason, CalSensitivity, CalSummary, MeasurementDiagnostic, SweepLevel,
    SweepShape, VerificationPass, VerifyError, VerifyPlan, VerifyRequest, VerifySeam, VerifyTiming,
};

/// Everything the Verify screen must supply to arm a pass.
///
/// **The baseline bundle is the source for almost all of it**, deliberately: it
/// is the frozen record of the Direct session this pass re-measures, so the
/// sweep shape, the level, the class, the cal file, the noise floor and the
/// routing all come from the one artefact that recorded them. Re-supplying any
/// of those over IPC would be a second place for them to disagree with the
/// baseline, and a verification that re-measures a different sweep is not a
/// verification.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct VerifyArmRequest {
    /// The frozen Direct session this pass re-measures.
    pub bundle: MeasurementBundle,
    /// The output device's human name, for the MS-18 acknowledgement.
    pub device_name: String,
    /// The physical output device the helper renders to.
    pub device_uid: String,
    /// The mic input-gain read-back at the time the cal was pinned.
    pub input_gain_read_back: f64,
    /// `L_measure`'s projected SPL, from the Direct session's solve.
    /// `L_verify`'s projection is this plus the engine's armed preamp.
    pub l_measure_projected_spl_db: f64,
    /// The plan the engine is RUNNING.
    pub plan: paraeq_decide::outcome::CorrectionPlan,
    /// Which baseline position to re-measure. Auto mode verifies one.
    pub position_index: usize,
}

/// What arming produced, for the acknowledgement dialog.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct VerifyArmed {
    pub device_name: String,
    pub level_dbfs: f64,
    pub projected_spl_db: f64,
}

/// One pass, armed and waiting on MS-18.
struct ArmedPass {
    bundle: MeasurementBundle,
    /// Rows of `positions[position_index].ir.samples`. The lift replicates the
    /// mono capture to this width, and `decide()` refuses if the two disagree.
    capture_channels: usize,
    pass: VerificationPass,
    plan: paraeq_decide::outcome::CorrectionPlan,
}

/// The app's single verification slot.
///
/// **There is at most one pass, ever.** Two passes would mean two helpers
/// rendering to the same device and two leases contending, and the second would
/// refuse anyway -- but refusing early is cheaper than refusing after a spawn.
#[derive(Default)]
pub struct VerifyRuntime {
    /// The trigger for a pass currently RUNNING. Held here rather than on the
    /// pass because the pass has moved onto the worker thread by then, and an
    /// abort that cannot reach a running sweep is not an abort.
    abort: Option<AbortHandle>,
    /// The armed pass, until `run` takes it. Dropping it runs the full MS-14
    /// teardown, which is why `abort` and `shutdown` take it rather than
    /// clearing it.
    armed: Option<ArmedPass>,
    pub state: VerifyState,
    /// The worker running a pass, so shutdown can wait for its teardown.
    worker: Option<std::thread::JoinHandle<()>>,
}

impl VerifyRuntime {
    /// A snapshot for the UI.
    pub fn state(&self) -> VerifyState {
        self.state.clone()
    }
}

/// Build the measurement crate's request from the bundle and the plan.
fn build_request(
    request: &VerifyArmRequest,
    wav_path: std::path::PathBuf,
) -> Result<(VerifyRequest, usize), String> {
    let bundle = &request.bundle;
    let position = bundle
        .positions
        .get(request.position_index)
        .ok_or_else(|| format!("position {} is not in this bundle", request.position_index))?;

    // MS-9: the sensitivity is the sole path to an SPL, and a bundle with no
    // cal file cannot produce one. `CalSummary::validate` is the gate; it
    // refuses rather than defaulting, which is the whole of MS-9.
    let cal_file = bundle
        .cal
        .as_ref()
        .ok_or_else(|| "this measurement carries no calibration file".to_owned())?;
    let sensitivity = match cal_file.sensitivity_db {
        Some(db) => CalSensitivity::Parsed(db),
        None => CalSensitivity::Unparseable,
    };
    let cal = CalSummary::validate(
        bundle.class,
        cal_file
            .serial
            .clone()
            .unwrap_or_else(|| "<unidentified cal file>".to_owned()),
        sensitivity,
        cal_file.gain_db.unwrap_or(request.input_gain_read_back),
    )
    .map_err(|refusal| {
        let diagnostic = refusal.diagnostic();
        format!("{diagnostic:?}: {}", diagnostic.fix_easy())
    })?;

    let l_measure = SweepLevel::new(bundle.capture.sweep.level_dbfs, bundle.class)
        .map_err(|e| format!("the baseline's own level is not legal: {e}"))?;

    // The broadband floor, as the WORST channel. A mean would let a quiet ear
    // pay for a noisy one, and the SNR budget exists to refuse a pass that
    // cannot produce a meaningful residual on the ear that is being measured.
    let noise_floor_dbfs = bundle
        .noise_floor
        .rms_dbfs
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    if !noise_floor_dbfs.is_finite() {
        return Err("this measurement carries no usable noise floor".to_owned());
    }

    let routing = capture_routing_to_helper(position.routing);
    let capture_channels = position.ir.samples.len();

    Ok((
        VerifyRequest {
            baseline_routing: routing,
            cal,
            device_uid: request.device_uid.clone(),
            input_gain_read_back: request.input_gain_read_back,
            l_measure,
            l_measure_projected_spl_db: request.l_measure_projected_spl_db,
            // The baseline's own layout, which the bundle does not yet carry:
            // the Direct path has no layout selector, so there is exactly one
            // and it is this one. **When a layout choice lands, the bundle must
            // record it and this line must read it** -- two captures aligned by
            // different means cannot be subtracted.
            layout: DEFAULT_MARKER_LAYOUT,
            noise_floor_dbfs,
            plan: VerifyPlan {
                bands: request.plan.bands.as_slice().to_vec(),
                design_rate_hz: request.plan.design_rate,
                preamp_db: request.plan.preamp_db,
            },
            position_index: request.position_index,
            routing,
            sweep: SweepShape {
                duration_s: bundle.capture.sweep.duration_s,
                f_end_hz: bundle.capture.sweep.f_end_hz,
                f_start_hz: bundle.capture.sweep.f_start_hz,
                sample_rate_hz: bundle.capture.sweep_rate,
            },
            timing: VerifyTiming::default(),
            wav_path,
        },
        capture_channels,
    ))
}

/// Compose the six seams from the live app.
fn build_seam(
    shared: &AppShared,
    app: tauri::AppHandle,
    mic_uid: &str,
) -> Result<VerifySeam, String> {
    use paraeq_coreaudio::measure_aggregate::{MeasureAggregateConfig, MicSelector};

    let mic = paraeq_coreaudio::measure_aggregate::MicCapture::create(MeasureAggregateConfig {
        drift_compensation: true,
        mic: if mic_uid.is_empty() {
            MicSelector::DefaultInput
        } else {
            MicSelector::Uid(mic_uid.to_owned())
        },
        // The stimulus side of this aggregate stays SILENT: the helper renders
        // to the physical device in its own process, which is the whole point.
        // `take_stimulus_sink` is never called.
        routing: paraeq_coreaudio::measure_aggregate::StimulusRouting::Both,
    })
    .map_err(|e| format!("cannot open the measurement microphone: {e}"))?;

    let volume = paraeq_coreaudio::volume::DeviceVolume::for_default_output()
        .map_err(|e| format!("cannot reach the output device's volume: {e}"))?;

    Ok(VerifySeam {
        // WRAPPED, always. A raw `MicCapture` is a non-blocking ring drain and
        // `record` reads its zero as the end of the stream -- see
        // `WaitingCapture`'s own doc.
        capture: Box::new(WaitingCapture::new(Box::new(mic))),
        control: Box::new(crate::verify_seam::EngineSeam::managed(app.clone())),
        devices: Box::new(crate::verify_seam::DeviceSeam),
        engine: Box::new(crate::verify_seam::EngineSeam::managed(app)),
        helper: Box::new(crate::verify_seam::HelperSpawner::new()),
        tap: crate::verify_seam::tap_status(shared),
        volume: Box::new(volume),
    })
}

/// Gates 1-6. No process is spawned and no sample is emitted.
pub fn arm(app: &tauri::AppHandle, request: VerifyArmRequest) -> Result<VerifyArmed, String> {
    use tauri::Manager;
    let shared = app.state::<AppShared>();

    {
        let mut runtime = shared.verify.lock().unwrap();
        // R-B4: a `Running` whose worker has finished is a stale label, not a
        // live pass. Clearing it here is what keeps a worker panic from
        // refusing every later arm for the life of the process.
        clear_stale_running(&mut runtime);
        if runtime.armed.is_some() || matches!(runtime.state, VerifyState::Running) {
            return Err("a verification pass is already armed or running".to_owned());
        }
    }

    let wav_path = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("verify")
        .join(format!("verify-{}.wav", std::process::id()));
    let mic_uid = request.bundle.capture.input_uid.clone();
    let (measure_request, capture_channels) = build_request(&request, wav_path)?;
    let seam = build_seam(&shared, app.clone(), &mic_uid)?;

    let pass = match VerificationPass::arm(measure_request, seam) {
        Ok(pass) => pass,
        Err(failure) => {
            let state = failed_state(&failure.error);
            let summary = match &state {
                VerifyState::Failed { summary, .. } => summary.clone(),
                _ => failure.to_string(),
            };
            set_state(app, state);
            return Err(summary);
        }
    };

    let armed = VerifyArmed {
        device_name: request.device_name.clone(),
        level_dbfs: pass.level().map(|l| l.dbfs_rms()).unwrap_or(f64::NAN),
        projected_spl_db: pass.projected_spl_db(),
    };
    {
        let mut runtime = shared.verify.lock().unwrap();
        runtime.abort = Some(pass.abort_handle());
        runtime.armed = Some(ArmedPass {
            bundle: request.bundle,
            capture_channels,
            pass,
            plan: request.plan,
        });
        runtime.state = VerifyState::Armed {
            device_name: armed.device_name.clone(),
            level_dbfs: armed.level_dbfs,
            projected_spl_db: armed.projected_spl_db,
        };
    }
    crate::engine_bridge::publish_current(app);
    Ok(armed)
}

/// MS-18, then the sweep.
///
/// The acknowledgement is not a parameter for form's sake: it carries the SPL
/// the user was actually shown, and the pass refuses a stale one. The run
/// itself moves to a worker thread -- it plays a multi-second sweep and then
/// runs an O(N·M) matched filter over the capture, and a command that did that
/// inline would freeze the window for the whole of it.
pub fn run(
    app: &tauri::AppHandle,
    device_name: String,
    acknowledged_spl_db: f64,
) -> Result<(), String> {
    use tauri::Manager;
    let shared = app.state::<AppShared>();

    let mut armed = {
        let mut runtime = shared.verify.lock().unwrap();
        runtime
            .armed
            .take()
            .ok_or_else(|| "no verification pass is armed".to_owned())?
    };

    if let Err(e) = armed.pass.acknowledge(&device_name, acknowledged_spl_db) {
        // Dropping `armed` here runs the full teardown: helper down, capture
        // stopped, volume and trim restored, lease released.
        let state = failed_state(&e);
        set_state(app, state);
        return Err(e.to_string());
    }

    {
        let mut runtime = shared.verify.lock().unwrap();
        runtime.state = VerifyState::Running;
    }
    crate::engine_bridge::publish_current(app);

    let worker_app = app.clone();
    let worker = std::thread::Builder::new()
        .name("paraeq-verify".into())
        .spawn(move || {
            // R-B4. The publish used to BE the worker's last statement, so a
            // panic in run()/finish()/grade() skipped it and left the slot at
            // `Running` with an abort handle, for the life of the process:
            // `arm` refused forever and `abort` could not reset it. Everything
            // that matters for safety already runs on unwind — dropping
            // `armed` tears the pass down and releases the lease — so only the
            // published state lied. Now a panic publishes `Failed`.
            let state = worker_state(move || {
                let outcome = armed.pass.run();
                // `finish` runs teardown again (idempotent) and hands back the
                // MS-23 log by value, so the pass is fully terminated before
                // the state is published.
                let ArmedPass {
                    bundle,
                    capture_channels,
                    pass,
                    plan,
                } = armed;
                let _log = pass.finish();
                match outcome {
                    Ok(outcome) => VerifyState::Complete {
                        report: grade(&outcome, bundle, plan, capture_channels),
                    },
                    Err(e) => failed_state(&e),
                }
            });
            {
                let shared = worker_app.state::<AppShared>();
                let mut runtime = shared.verify.lock().unwrap();
                runtime.abort = None;
                runtime.state = state;
            }
            crate::engine_bridge::publish_current(&worker_app);
        })
        .map_err(|e| format!("cannot start the verification worker: {e}"))?;

    shared.verify.lock().unwrap().worker = Some(worker);
    Ok(())
}

/// Run the verification worker's body and publish a state whatever happens.
///
/// R-B4. A panic here is not a safety event — the pass's own `Drop` runs on
/// unwind, so the helper is stopped and reaped, the capture stopped, the trim
/// restored and the lease released — but it IS a liveness event: without this,
/// the slot keeps `Running` forever and the user cannot verify again without
/// restarting ParaEQ.
///
/// `AssertUnwindSafe` is honest here rather than a silencer: the closure owns
/// the `ArmedPass` outright, and nothing it touches is observed afterwards
/// except through the `VerifyState` this returns.
fn worker_state(body: impl FnOnce() -> VerifyState) -> VerifyState {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(state) => state,
        Err(payload) => VerifyState::Failed {
            // No `MeasurementDiagnostic`, so no canned remedy: a panic is a
            // ParaEQ bug, and the only honest advice is to say so.
            code: None,
            remedy: Some(
                "This is a fault in ParaEQ itself, not something you did. Try the verification \
                 again; if it keeps happening, the message above is what to report."
                    .to_owned(),
            ),
            summary: format!("the verification worker panicked: {}", panic_text(&payload)),
        },
    }
}

/// The panic's own message, when it left one. `panic!("...")` payloads are a
/// `&str` or a `String`; anything else has no text to show.
fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_owned();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "no message".to_owned()
}

/// Is the slot's `Running` state STALE — nothing left to wait for?
///
/// R-B4's second half. `Running` is published before the worker starts and
/// cleared by the worker's own last act, so a `Running` whose worker has
/// already finished means the clearing never happened. Treat that as reset-able
/// rather than as a live pass: the alternative is the wedge itself, where
/// `arm` refuses forever because a thread that no longer exists is "running".
///
/// A finished handle is dropped rather than joined — `is_finished` is already
/// the answer, and `shutdown` is where joining belongs.
fn clear_stale_running(runtime: &mut VerifyRuntime) -> bool {
    if !matches!(runtime.state, VerifyState::Running) {
        return false;
    }
    if runtime.worker.as_ref().is_some_and(|w| !w.is_finished()) {
        return false;
    }
    runtime.worker = None;
    runtime.abort = None;
    runtime.state = VerifyState::Idle;
    true
}

/// Abort whatever is armed or running, through the MS-14 ladder.
///
/// Two paths, because there are two states to be in. A RUNNING pass is reached
/// through its abort handle -- the capture polls it once per block and the
/// helper is asked to RAMP, never hard-stopped. An ARMED pass has no helper
/// yet, so taking it and dropping it is the abort: `Drop` runs the same
/// teardown, which restores the volume and the trim and releases the lease.
pub fn abort(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let shared = app.state::<AppShared>();
    let armed = {
        let mut runtime = shared.verify.lock().unwrap();
        if let Some(abort) = runtime.abort.as_ref() {
            abort.trigger(AbortReason::UserRequest);
        }
        // R-B4: the same stale-`Running` reset, so the Stop button can clear a
        // slot a panicked worker left behind instead of doing nothing visible.
        clear_stale_running(&mut runtime);
        runtime.armed.take()
    };
    if let Some(armed) = armed {
        // Explicit rather than relying on the drop order of a `let`: this is
        // the line that tears the pass down, and it should read like it.
        drop(armed);
        let mut runtime = shared.verify.lock().unwrap();
        runtime.abort = None;
        runtime.state = VerifyState::Idle;
    }
    crate::engine_bridge::publish_current(app);
    Ok(())
}

/// The app is quitting: abort, and WAIT for the teardown to finish.
///
/// Waiting is the point. A helper rendering into a private aggregate outlives
/// this process if nobody tears it down, and it would be left wrapping the
/// user's output device with nothing watching it. The wait is bounded by the
/// pass's own deadlines -- the ramp rung, the SIGTERM rung and the kill rung
/// all carry one -- so this cannot hang the quit path indefinitely.
///
/// Called BEFORE the engine teardown, so the app's existing never-leave-muted
/// exit path still runs afterwards exactly as it did.
pub fn shutdown(shared: &AppShared) {
    let (armed, worker) = {
        let mut runtime = shared.verify.lock().unwrap();
        if let Some(abort) = runtime.abort.as_ref() {
            abort.trigger(AbortReason::UserRequest);
        }
        (runtime.armed.take(), runtime.worker.take())
    };
    drop(armed);
    if let Some(worker) = worker {
        let _ = worker.join();
    }
}

/// Grade a finished pass: lift it into the bundle and run `decide()`.
fn grade(
    outcome: &paraeq_measure::VerifyOutcome,
    mut bundle: MeasurementBundle,
    plan: paraeq_decide::outcome::CorrectionPlan,
    capture_channels: usize,
) -> VerifyReport {
    bundle.verification = Some(lift_verification(outcome, plan, capture_channels));
    let decided: DecisionSet = paraeq_decide::decide(&bundle);
    // B17's third leg. `decide()` cannot log at all -- its determinism test
    // forbids I/O inside it -- so the crate that CAN log does it, at the one
    // place a `DecisionSet` reaches this process.
    crate::engine_bridge::log_transition_source(&decided.decisions);
    let report = decided.verification.as_ref();
    VerifyReport {
        abort_acoustic_budget_ms: outcome.abort_acoustic_budget_ms,
        diagnostics: decided
            .diagnostics
            .iter()
            .map(|d| VerifyDiagnostic {
                code: d.code.code(),
                remedy: d.remedy.clone(),
                severity: match d.severity {
                    Severity::Refuse => "refuse".to_owned(),
                    Severity::Warn => "warn".to_owned(),
                },
                summary: match d.value {
                    Some(value) => format!("{:?} ({value})", d.code),
                    None => format!("{:?}", d.code),
                },
            })
            // The pass's own non-blocking findings ride along: they are
            // capture-layer facts `decide()` never sees, and dropping them
            // would hide (say) a clamped emit behind a clean residual.
            .chain(outcome.warnings.iter().map(|w| VerifyDiagnostic {
                code: w.code(),
                remedy: w.fix_easy(),
                severity: "warn".to_owned(),
                summary: format!("{w:?}"),
            }))
            .collect(),
        gate_db: report.map(|r| r.gate_db),
        installed_preamp_db: outcome.installed_preamp_db,
        level_dbfs: outcome.level_dbfs,
        residual_rms_db: report.map(|r| r.residual_rms_db),
        verdict: match decided.verdict {
            Verdict::Proceed => "proceed".to_owned(),
            Verdict::ProceedWithWarnings => "proceed_with_warnings".to_owned(),
            Verdict::Refuse => "refuse".to_owned(),
        },
    }
}

/// A pass failure as the Verify screen shows it.
///
/// A refusal carries a numbered `MeasurementDiagnostic` and therefore a remedy;
/// everything else is a seam or protocol failure, which has a message but no
/// code and no canned fix. Inventing one for those would be worse than saying
/// nothing -- a remedy the user follows that cannot help is a remedy that
/// teaches them to ignore remedies.
fn failed_state(error: &VerifyError) -> VerifyState {
    match error {
        VerifyError::Refused(refusal) => {
            let diagnostic: MeasurementDiagnostic = (*refusal).diagnostic();
            VerifyState::Failed {
                code: Some(diagnostic.code()),
                remedy: Some(diagnostic.fix_easy()),
                summary: format!("{diagnostic:?}"),
            }
        }
        other => VerifyState::Failed {
            code: None,
            remedy: None,
            summary: other.to_string(),
        },
    }
}

fn set_state(app: &tauri::AppHandle, state: VerifyState) {
    use tauri::Manager;
    app.state::<AppShared>().verify.lock().unwrap().state = state;
    crate::engine_bridge::publish_current(app);
}
