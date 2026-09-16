//! MS-6/MS-14/MS-18/MS-23 (+ the session half of MS-5 and MS-11): the
//! `MeasurementSession` sequencing state machine, mock-driven.
//!
//! Tier 3 (analytic/policy) per the four-tier convention: sequencing, refusal
//! order and teardown order are product policy with no oracle. What is
//! pinned: the MS-6 refusal fires before any sample is emitted (a mock sink
//! counts emit calls), the sweep phase is unreachable without the recorded
//! acknowledgement (MS-18), the abort ramp is monotone to exactly 0.0 within
//! ~5 ms of block time starting the first block after the trigger and is
//! followed by the restore sequence in order (MS-14), the pre-measurement
//! volume is restored on every exit path including panic (MS-5's RAII half),
//! and the session log after a synthetic run is complete, in order, and
//! carries one event per rung (MS-23).

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use paraeq_dsp::targets::TransducerClass;
use paraeq_measure::{
    assemble_sweep, AbortHandle, AbortReason, CalSensitivity, CalSummary, MeasureError,
    MeasurementDiagnostic as D, MeasurementSession, SessionError, SessionEvent, SessionPhase,
    SessionSeam, SolveOutcome, StimulusSink, StreamFormat, SweepOutcome, TapStatus, VolumeControl,
    ABORT_RAMP_MS, CAL_ERROR_MARGIN_DB,
};

const RATE: f64 = 48_000.0;
const BLOCK: usize = 512;
const PRE_VOLUME: f64 = 0.4;

// ── Mock kit ──────────────────────────────────────────────────────────────
// Shared interior state (Arc) so every mock stays observable after the
// session takes ownership — including across a panic boundary.

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Cross-mock call-order recorder: the MS-14 restore sequence is an *order*
/// ("sink stop → volume restore → state teardown"), so one shared journal
/// records every seam call in arrival order.
#[derive(Clone, Default)]
struct Journal(Arc<Mutex<Vec<String>>>);

impl Journal {
    fn record(&self, call: &str) {
        lock(&self.0).push(call.to_owned());
    }

    fn calls(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
}

#[derive(Clone)]
struct MockTap {
    excluded: Arc<AtomicBool>,
}

impl MockTap {
    fn new(excluded: bool) -> Self {
        Self {
            excluded: Arc::new(AtomicBool::new(excluded)),
        }
    }
}

impl TapStatus for MockTap {
    fn self_excluded(&self) -> bool {
        self.excluded.load(Ordering::SeqCst)
    }
}

/// Records every emitted block; optionally trips an [`AbortHandle`] *during*
/// the n-th emit call, the way a real trigger (Esc, SPL metering, a device
/// listener) fires while audio is flowing.
#[derive(Clone, Default)]
struct MockSink {
    blocks: Arc<Mutex<Vec<Vec<f64>>>>,
    fail_stop: Arc<AtomicBool>,
    journal: Journal,
    trip: Arc<Mutex<Option<(usize, AbortHandle, AbortReason)>>>,
}

impl MockSink {
    fn with_journal(journal: Journal) -> Self {
        Self {
            journal,
            ..Self::default()
        }
    }

    fn trip_on_call(&self, nth: usize, handle: AbortHandle, reason: AbortReason) {
        *lock(&self.trip) = Some((nth, handle, reason));
    }

    /// Make `stop` return `Err` — the teardown must collect the fault and still
    /// run the volume restore behind it (the `TapSystem` never-mask posture).
    fn fail_stop(&self) {
        self.fail_stop.store(true, Ordering::SeqCst);
    }

    fn blocks(&self) -> Vec<Vec<f64>> {
        lock(&self.blocks).clone()
    }

    fn emit_calls(&self) -> usize {
        lock(&self.blocks).len()
    }
}

impl StimulusSink for MockSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: BLOCK,
            sample_rate_hz: RATE,
        }
    }

    fn emit(
        &mut self,
        block: &[f64],
        _level: paraeq_measure::SweepLevel,
    ) -> Result<(), MeasureError> {
        self.journal.record("emit");
        let mut blocks = lock(&self.blocks);
        blocks.push(block.to_vec());
        let n = blocks.len();
        drop(blocks);
        if let Some((nth, handle, reason)) = lock(&self.trip).clone() {
            if n == nth {
                handle.trigger(reason);
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        self.journal.record("stop");
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(MeasureError::Sink("injected stop failure".to_owned()));
        }
        Ok(())
    }
}

/// Panics on the first emit — the MS-5 panic-injection vehicle. Panics before
/// touching any lock so the unwind cannot poison the journal the assertions
/// need afterwards.
#[derive(Clone, Default)]
struct PanickingSink {
    journal: Journal,
}

impl StimulusSink for PanickingSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 1,
            frames_per_block: BLOCK,
            sample_rate_hz: RATE,
        }
    }

    fn emit(
        &mut self,
        _block: &[f64],
        _level: paraeq_measure::SweepLevel,
    ) -> Result<(), MeasureError> {
        panic!("injected: realtime callback blew up mid-sweep");
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        self.journal.record("stop");
        Ok(())
    }
}

#[derive(Clone)]
struct MockVolume {
    fail_set: Arc<AtomicBool>,
    journal: Journal,
    scalar: f64,
    sets: Arc<Mutex<Vec<f64>>>,
}

impl MockVolume {
    fn new(scalar: f64, journal: Journal) -> Self {
        Self {
            fail_set: Arc::default(),
            journal,
            scalar,
            sets: Arc::default(),
        }
    }

    fn sets(&self) -> Vec<f64> {
        lock(&self.sets).clone()
    }

    /// Make `set_volume` return `Err` — the restore failure must be logged
    /// (`VolumeRestoreFailed`), never swallowed and never masking teardown.
    fn fail_set(&self) {
        self.fail_set.store(true, Ordering::SeqCst);
    }
}

impl VolumeControl for MockVolume {
    fn volume(&self) -> Result<f64, MeasureError> {
        Ok(self.scalar)
    }

    fn set_volume(&mut self, scalar: f64) -> Result<(), MeasureError> {
        self.journal.record("set_volume");
        lock(&self.sets).push(scalar);
        if self.fail_set.load(Ordering::SeqCst) {
            return Err(MeasureError::Sink("injected volume failure".to_owned()));
        }
        Ok(())
    }
}

// ── Fixture helpers ───────────────────────────────────────────────────────

fn healthy_cal(class: TransducerClass) -> CalSummary {
    CalSummary::validate(
        class,
        "UMIK-1 #7001234 (umik-7001234.txt)".to_owned(),
        CalSensitivity::Parsed(-18.0),
        1.0,
    )
    .expect("healthy cal validates")
}

/// The standard Stage-5-shaped solve for an OverEar run: in-envelope chain,
/// projection at the 84 dB target, solved level exactly at the class cap.
fn solve() -> SolveOutcome {
    SolveOutcome {
        chain_sensitivity_spl_per_dbfs: 104.0,
        projected_spl_db: 84.0,
        solved_dbfs_rms: -20.0,
    }
}

fn seam(sink: MockSink, tap: MockTap, volume: MockVolume) -> SessionSeam {
    SessionSeam {
        sink: Box::new(sink),
        tap: Box::new(tap),
        volume: Box::new(volume),
    }
}

/// begin → install_solve → acknowledge, on a healthy OverEar chain with a
/// matching gain read-back: the shortest legal path to the sweep gate.
fn acknowledged_session(sink: MockSink, tap: MockTap, volume: MockVolume) -> MeasurementSession {
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(sink, tap, volume),
    )
    .expect("begin succeeds");
    session.install_solve(solve()).expect("solve installs");
    session
        .acknowledge("External Headphone Amp", 84.0)
        .expect("ack records");
    session
}

// ── MS-6: no sweep without self-exclusion ─────────────────────────────────

/// MS-6, with the refusal timing the requirement demands: the mock sink
/// proves **zero** emit calls happened, and the volume was never touched —
/// the refusal is cheaper than any side effect.
#[test]
fn ms6_refuses_before_any_sample_is_emitted() {
    let sink = MockSink::default();
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let refusal = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(sink.clone(), MockTap::new(false), volume.clone()),
    )
    .unwrap_err();
    assert_eq!(refusal.diagnostic(), D::SelfExclusionUnavailable);
    assert_eq!(sink.emit_calls(), 0, "no sample may be emitted");
    assert!(volume.sets().is_empty(), "volume must not be touched");
}

/// Exclusion can vanish mid-session (device change ⇒ tap rebuild), so the
/// sweep gate re-checks it — still before any sample.
#[test]
fn ms6_rechecks_at_the_sweep_gate() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let tap = MockTap::new(true);
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let mut session = acknowledged_session(sink.clone(), tap.clone(), volume.clone());

    tap.excluded.store(false, Ordering::SeqCst);
    let level = session.emit_level().expect("level installed");
    let stim = assemble_sweep(0.25, 48_000, 20.0, 20_000.0, level).expect("stimulus assembles");
    let err = session.sweep(&stim).unwrap_err();
    match err {
        SessionError::Refused(refusal) => {
            assert_eq!(refusal.diagnostic(), D::SelfExclusionUnavailable);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(sink.emit_calls(), 0, "no sample may be emitted");
    // The refusal terminated the session: full restore sequence, in order.
    assert_eq!(journal.calls(), vec!["stop", "set_volume"]);
    assert_eq!(volume.sets(), vec![PRE_VOLUME]);
    assert_eq!(session.phase(), SessionPhase::Terminated);
}

// ── MS-18: the sweep is unreachable without the acknowledgement ───────────

/// The state-machine proof: from every phase except `Acknowledged`, `sweep`
/// is a wrong-phase error and the sink counts zero emits. The only path into
/// `Acknowledged` is [`MeasurementSession::acknowledge`] — an explicit call
/// carrying the device name and projected SPL, recorded in the log.
#[test]
fn ms18_sweep_is_unreachable_without_the_acknowledgement() {
    let sink = MockSink::default();
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(sink.clone(), MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    let stim = assemble_sweep(
        0.25,
        48_000,
        20.0,
        20_000.0,
        paraeq_measure::SweepLevel::new(-20.0, TransducerClass::OverEar).expect("legal level"),
    )
    .expect("stimulus assembles");

    // Preflight: no solve, no ack — unreachable.
    assert!(matches!(
        session.sweep(&stim).unwrap_err(),
        SessionError::WrongPhase { .. }
    ));
    // Solved but unacknowledged — still unreachable. This is the MS-18 edge.
    session.install_solve(solve()).expect("solve installs");
    assert!(matches!(
        session.sweep(&stim).unwrap_err(),
        SessionError::WrongPhase { .. }
    ));
    assert_eq!(sink.emit_calls(), 0, "no sample before the acknowledgement");

    // The explicit acknowledgement opens the gate, and is logged verbatim.
    session
        .acknowledge("External Headphone Amp", 84.0)
        .expect("ack records");
    assert!(session
        .log()
        .events()
        .contains(&SessionEvent::Acknowledged {
            device_name: "External Headphone Amp".to_owned(),
            projected_spl_db: 84.0,
        }));
    match session.sweep(&stim).expect("sweep runs after the ack") {
        SweepOutcome::Completed { warnings } => assert!(warnings.is_empty()),
        aborted => panic!("expected completion, got {aborted:?}"),
    }
    assert!(sink.emit_calls() > 0);

    // Swept is not Acknowledged: the ack authorizes exactly one sweep.
    assert!(matches!(
        session.sweep(&stim).unwrap_err(),
        SessionError::WrongPhase { .. }
    ));
}

/// The acknowledgement itself is gated: it cannot be recorded before a solve
/// exists (there is no projected SPL to acknowledge yet).
#[test]
fn an_acknowledgement_needs_a_solve_to_acknowledge() {
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(MockSink::default(), MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    assert!(matches!(
        session
            .acknowledge("External Headphone Amp", 84.0)
            .unwrap_err(),
        SessionError::WrongPhase { .. }
    ));
}

/// An acknowledgement that names no device names nothing — rejected, no
/// transition.
#[test]
fn an_acknowledgement_must_name_a_device() {
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(MockSink::default(), MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    session.install_solve(solve()).expect("solve installs");
    for unnamed in ["", "   "] {
        assert!(matches!(
            session.acknowledge(unnamed, 84.0).unwrap_err(),
            SessionError::AckNamesNoDevice
        ));
    }
    assert_eq!(session.phase(), SessionPhase::Solved, "no transition");
}

/// An acknowledgement carrying a different SPL than the installed solve is
/// stale — the user acknowledged a number that is not this run's projection.
#[test]
fn a_stale_acknowledgement_is_rejected() {
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(MockSink::default(), MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    session.install_solve(solve()).expect("solve installs");
    assert!(matches!(
        session
            .acknowledge("External Headphone Amp", 90.0)
            .unwrap_err(),
        SessionError::AckSplStale { .. }
    ));
    assert_eq!(session.phase(), SessionPhase::Solved, "no transition");
}

/// The ACK_SPL_TOLERANCE_DB boundary, pinned so a widened (or NaN-permissive)
/// tolerance can't slip through: the projection is 84.0, so 84.0 + tolerance is
/// still fresh, 84.0 + 2·tolerance is stale, and a NaN acknowledged SPL — which
/// passes no comparison — is stale, not accepted.
#[test]
fn acknowledgement_freshness_is_pinned_at_the_tolerance_boundary() {
    use paraeq_measure::ACK_SPL_TOLERANCE_DB as TOL;
    let fresh_session = || {
        let mut s = MeasurementSession::begin(
            healthy_cal(TransducerClass::OverEar),
            1.0,
            seam(
                MockSink::default(),
                MockTap::new(true),
                MockVolume::new(PRE_VOLUME, Journal::default()),
            ),
        )
        .expect("begin succeeds");
        s.install_solve(solve()).expect("solve installs");
        s
    };

    // At the tolerance: still this run's projection.
    fresh_session()
        .acknowledge("Amp", 84.0 + TOL)
        .expect("within tolerance is fresh");
    // Past twice the tolerance: stale.
    assert!(matches!(
        fresh_session()
            .acknowledge("Amp", 84.0 + 2.0 * TOL)
            .unwrap_err(),
        SessionError::AckSplStale { .. }
    ));
    // NaN acknowledges nothing.
    assert!(matches!(
        fresh_session().acknowledge("Amp", f64::NAN).unwrap_err(),
        SessionError::AckSplStale { .. }
    ));
}

// ── Refusals at the solve boundary ────────────────────────────────────────

/// The spec's cheapest possible refusal: a projection over the class cap
/// refuses before a single sample is emitted, and terminates the session
/// through the full restore sequence. A NaN projection lands in the same
/// refusal arm (it cannot be shown under the cap).
#[test]
fn a_projection_over_the_cap_refuses_before_a_single_sample() {
    for bad_projection in [100.1, f64::NAN] {
        let journal = Journal::default();
        let sink = MockSink::with_journal(journal.clone());
        let volume = MockVolume::new(PRE_VOLUME, journal.clone());
        let mut session = MeasurementSession::begin(
            healthy_cal(TransducerClass::OverEar),
            1.0,
            seam(sink.clone(), MockTap::new(true), volume.clone()),
        )
        .expect("begin succeeds");
        let err = session
            .install_solve(SolveOutcome {
                projected_spl_db: bad_projection,
                ..solve()
            })
            .unwrap_err();
        match err {
            SessionError::Refused(refusal) => {
                assert_eq!(refusal.diagnostic(), D::ProjectedSplOverCap);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(sink.emit_calls(), 0, "no sample may be emitted");
        assert_eq!(journal.calls(), vec!["stop", "set_volume"]);
        assert_eq!(session.phase(), SessionPhase::Terminated);
        assert_eq!(
            session.log().events().last(),
            Some(&SessionEvent::Terminated {
                diagnostic: Some(D::ProjectedSplOverCap)
            })
        );
    }
}

/// The sweep gate verifies stimulus provenance: a stimulus assembled at any
/// level other than the session's margined level is refused unplayed.
#[test]
fn a_stimulus_at_the_wrong_level_is_refused_unplayed() {
    let sink = MockSink::default();
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = acknowledged_session(sink.clone(), MockTap::new(true), volume);
    let wrong = assemble_sweep(
        0.25,
        48_000,
        20.0,
        20_000.0,
        paraeq_measure::SweepLevel::new(-26.0, TransducerClass::OverEar).expect("legal level"),
    )
    .expect("stimulus assembles");
    assert!(matches!(
        session.sweep(&wrong).unwrap_err(),
        SessionError::StimulusLevelMismatch { .. }
    ));
    assert_eq!(sink.emit_calls(), 0);
}

// ── MS-11 at the session boundary ─────────────────────────────────────────

/// The session applies the cal-error margin at its level decision: with a
/// gain read-back that disagrees with the cal reference, the emitted level is
/// ≤ solved − 6 dB, and the decision is logged with both numbers.
#[test]
fn ms11_a_gain_mismatch_derates_the_session_level() {
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        0.6, // read back ≠ the cal's 1.0 reference
        seam(MockSink::default(), MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    let level = session.install_solve(solve()).expect("solve installs");
    assert!(
        level.dbfs_rms() <= solve().solved_dbfs_rms - 6.0,
        "emitted {} dBFS must be ≤ solved − 6 dB on a gain mismatch",
        level.dbfs_rms()
    );
    assert!(session
        .log()
        .events()
        .contains(&SessionEvent::SolveInstalled {
            chain_sensitivity_spl_per_dbfs: 104.0,
            emitted_dbfs_rms: solve().solved_dbfs_rms - CAL_ERROR_MARGIN_DB,
            margin_db: CAL_ERROR_MARGIN_DB,
            projected_spl_db: 84.0,
            solved_dbfs_rms: -20.0,
        }));
}

// ── MS-14: abort ramp, then the restore sequence in order ─────────────────

/// The MS-14 unit test against a mock sink. The trigger fires *during* emit
/// call 2; the ramp must occupy the first block after the trigger: a
/// raised-cosine on the sink's output buffer, monotone non-increasing,
/// reaching exactly 0.0 within ~5 ms of block time, zeros to the block edge,
/// and then — in order — sink stop, volume restore, state teardown.
#[test]
fn ms14_abort_ramps_to_zero_then_restores_in_order() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let mut session = acknowledged_session(sink.clone(), MockTap::new(true), volume.clone());
    sink.trip_on_call(2, session.abort_handle(), AbortReason::UserRequest);

    let level = session.emit_level().expect("level installed");
    let stim = assemble_sweep(0.25, 48_000, 20.0, 20_000.0, level).expect("stimulus assembles");
    match session
        .sweep(&stim)
        .expect("an abort is an outcome, not an error")
    {
        SweepOutcome::Aborted {
            diagnostic,
            warnings,
        } => {
            assert_eq!(diagnostic, D::UserAborted);
            assert!(warnings.is_empty());
        }
        completed => panic!("expected an abort, got {completed:?}"),
    }

    // Block anatomy: two full-level blocks (the trigger fired during the
    // second), then exactly one ramp block — the ramp starts on the first
    // block after the trigger and 5 ms fits inside one 512-frame block.
    let blocks = sink.blocks();
    assert_eq!(
        blocks.len(),
        3,
        "ramp must start on the first block after the trigger"
    );
    let source = &stim.samples()[2 * BLOCK..3 * BLOCK];
    let ramp = &blocks[2];
    assert_eq!(ramp.len(), BLOCK);
    let ramp_len = (ABORT_RAMP_MS / 1000.0 * RATE).round() as usize; // 240 @ 48 kHz

    // Reaches exactly 0.0 within ~5 ms of block time, and stays there.
    assert!(
        ramp[..ramp_len].iter().any(|v| *v != 0.0),
        "the ramp carries signal before reaching zero"
    );
    for (i, v) in ramp[ramp_len - 1..].iter().enumerate() {
        assert_eq!(
            *v,
            0.0,
            "sample {} after the ramp end must be exactly 0.0",
            ramp_len - 1 + i
        );
    }

    // The ramp is the sink's buffer × a monotone non-increasing envelope < 1
    // — an attenuation of the very samples the sweep would have played, not a
    // re-render and not a hard stop.
    let mut previous = f64::INFINITY;
    for i in 0..ramp_len - 1 {
        if source[i].abs() > 1e-6 {
            let env = ramp[i] / source[i];
            assert!(
                env > 0.0 && env < 1.0,
                "sample {i}: envelope {env} must attenuate"
            );
            assert!(
                env <= previous + 1e-9,
                "sample {i}: envelope must be monotone non-increasing"
            );
            previous = env;
        }
    }

    // The restore sequence, in order, exactly once: stop, then volume.
    assert_eq!(
        journal.calls(),
        vec!["emit", "emit", "emit", "stop", "set_volume"]
    );
    assert_eq!(volume.sets(), vec![PRE_VOLUME]);
    // State teardown last: the phase is terminal and the log closes with the
    // terminating diagnostic.
    assert_eq!(session.phase(), SessionPhase::Terminated);
    assert_eq!(
        session.log().events().last(),
        Some(&SessionEvent::Terminated {
            diagnostic: Some(D::UserAborted)
        })
    );
}

/// Every abort reason maps onto its § Abort Guards diagnostic, so the
/// terminating log entry names the true trigger.
#[test]
fn abort_reasons_map_to_their_trigger_diagnostics() {
    for (reason, diagnostic) in [
        (AbortReason::EngineFailed, D::EngineFailed),
        (AbortReason::EngineStopped, D::EngineNotRunning),
        (AbortReason::InputClipping, D::InputClipping),
        (AbortReason::MicDisconnected, D::MicDisconnected),
        (AbortReason::OutputDeviceChanged, D::OutputDeviceChanged),
        (AbortReason::SplOverCap, D::SplOverCap),
        (AbortReason::UserRequest, D::UserAborted),
    ] {
        assert_eq!(reason.diagnostic(), diagnostic);
    }
}

/// The engine being switched off mid-run is **not** an engine failure. A
/// `Disable` from the user, or the fail-open watchdog auto-disabling after
/// `NoInputDetected` (wizard § The fail-open watchdog: "15 s in, the
/// controller tears the tap down mid-session"), tears the tap down while the
/// session is live — but nothing is broken, so reporting `EngineFailed` would
/// send the user off to restart the app instead of switching the EQ back on.
/// The terminating log entry must name `EngineNotRunning`.
#[test]
fn a_mid_run_engine_stop_terminates_as_not_running_not_as_failed() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let mut session = acknowledged_session(sink.clone(), MockTap::new(true), volume.clone());

    session.abort_now(AbortReason::EngineStopped);

    assert_eq!(sink.emit_calls(), 0);
    assert_eq!(session.phase(), SessionPhase::Terminated);
    let log = session.finish();
    assert_eq!(
        log.events().last(),
        Some(&SessionEvent::Terminated {
            diagnostic: Some(D::EngineNotRunning)
        }),
        "a mid-run Disable/auto-disable must not be misreported as EngineFailed"
    );
    // The restore sequence still ran, in order, exactly once.
    assert_eq!(journal.calls(), vec!["stop", "set_volume"]);
    assert_eq!(volume.sets(), vec![PRE_VOLUME]);
}

/// First trigger wins: a second trigger cannot re-label the abort.
#[test]
fn the_first_abort_trigger_wins() {
    let handle = AbortHandle::new();
    handle.trigger(AbortReason::SplOverCap);
    handle.trigger(AbortReason::UserRequest);
    assert_eq!(handle.triggered(), Some(AbortReason::SplOverCap));
}

// ── MS-5 (session half): RAII volume restore on every exit path ───────────

/// The panic-injection test: the sink panics mid-sweep, and the pre-
/// measurement volume is still restored — with the sink stopped first, in
/// teardown order — while the panic propagates.
#[test]
fn raii_volume_restore_survives_a_panic() {
    let journal = Journal::default();
    let sink = PanickingSink {
        journal: journal.clone(),
    };
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let volume_probe = volume.clone();

    let unwind = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut session = MeasurementSession::begin(
            healthy_cal(TransducerClass::OverEar),
            1.0,
            SessionSeam {
                sink: Box::new(sink),
                tap: Box::new(MockTap::new(true)),
                volume: Box::new(volume),
            },
        )
        .expect("begin succeeds");
        session.install_solve(solve()).expect("solve installs");
        session
            .acknowledge("External Headphone Amp", 84.0)
            .expect("ack records");
        let level = session.emit_level().expect("level installed");
        let stim = assemble_sweep(0.25, 48_000, 20.0, 20_000.0, level).expect("stimulus assembles");
        let _ = session.sweep(&stim); // panics inside the sink
    }));

    assert!(unwind.is_err(), "the injected panic must propagate");
    assert_eq!(
        volume_probe.sets(),
        vec![PRE_VOLUME],
        "the pre-measurement volume must be restored during unwinding"
    );
    assert_eq!(journal.calls(), vec!["stop", "set_volume"]);
}

/// A session dropped without `finish` — a window close, an early return —
/// still restores. Idempotently: a finished session's drop restores nothing
/// twice.
#[test]
fn drop_without_finish_restores_exactly_once() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    {
        let _session = acknowledged_session(sink, MockTap::new(true), volume.clone());
        // dropped here, mid-session
    }
    assert_eq!(volume.sets(), vec![PRE_VOLUME]);
    assert_eq!(journal.calls(), vec!["stop", "set_volume"]);
}

/// `abort_now` outside a sweep (nothing is playing, so no ramp) still runs
/// the restore sequence in order and logs the trigger's diagnostic.
#[test]
fn abort_outside_a_sweep_still_restores_in_order() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let mut session = acknowledged_session(sink.clone(), MockTap::new(true), volume.clone());
    session.abort_now(AbortReason::MicDisconnected);
    assert_eq!(sink.emit_calls(), 0);
    assert_eq!(journal.calls(), vec!["stop", "set_volume"]);
    assert_eq!(session.phase(), SessionPhase::Terminated);
    let log = session.finish();
    assert_eq!(
        log.events().last(),
        Some(&SessionEvent::Terminated {
            diagnostic: Some(D::MicDisconnected)
        })
    );
    // finish() after the abort tears down nothing twice.
    assert_eq!(volume.sets(), vec![PRE_VOLUME]);
}

// ── MS-23: the structured session log ─────────────────────────────────────

/// The synthetic full run: every event, in order, exactly once — cal identity
/// with the gain read-back, the volume pin, the level decision, one event per
/// rung, the acknowledgement, the sweep bracket, and the clean termination.
#[test]
fn ms23_the_log_is_complete_and_in_order_after_a_synthetic_run() {
    let sink = MockSink::default();
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(sink, MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    let level = session.install_solve(solve()).expect("solve installs");
    // Stage 5 will drive the real ≤6 dB climb; the session records each rung.
    for spl in [74.2, 79.9, 83.8] {
        session.record_rung(spl).expect("rung records");
    }
    session
        .acknowledge("External Headphone Amp", 84.0)
        .expect("ack records");
    let stim = assemble_sweep(0.25, 48_000, 20.0, 20_000.0, level).expect("stimulus assembles");
    match session.sweep(&stim).expect("sweep completes") {
        SweepOutcome::Completed { warnings } => assert!(warnings.is_empty()),
        aborted => panic!("expected completion, got {aborted:?}"),
    }
    let log = session.finish();

    assert_eq!(
        log.events(),
        &[
            SessionEvent::CalLoaded {
                class: TransducerClass::OverEar,
                file_identity: "UMIK-1 #7001234 (umik-7001234.txt)".to_owned(),
                gain_matches: true,
                input_gain_read_back: 1.0,
                reference_input_gain: 1.0,
                sens_factor_dbfs: -18.0,
                spl_cap_db: 100.0,
            },
            SessionEvent::VolumePinned {
                pre_measurement_scalar: Some(PRE_VOLUME),
            },
            SessionEvent::SolveInstalled {
                chain_sensitivity_spl_per_dbfs: 104.0,
                emitted_dbfs_rms: -20.0,
                margin_db: 0.0,
                projected_spl_db: 84.0,
                solved_dbfs_rms: -20.0,
            },
            SessionEvent::RungMeasured {
                index: 0,
                measured_spl_db: 74.2,
            },
            SessionEvent::RungMeasured {
                index: 1,
                measured_spl_db: 79.9,
            },
            SessionEvent::RungMeasured {
                index: 2,
                measured_spl_db: 83.8,
            },
            SessionEvent::Acknowledged {
                device_name: "External Headphone Amp".to_owned(),
                projected_spl_db: 84.0,
            },
            SessionEvent::SweepStarted {
                level_dbfs_rms: -20.0,
            },
            SessionEvent::SweepCompleted,
            SessionEvent::Terminated { diagnostic: None },
        ]
    );
}

/// One event per rung, by construction: the session assigns the index, so
/// three recordings are exactly three `RungMeasured` events, indexed 0, 1, 2
/// — a caller cannot record two events for one rung or skip an index.
#[test]
fn one_event_per_rung_by_construction() {
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(MockSink::default(), MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    session.install_solve(solve()).expect("solve installs");
    assert_eq!(session.record_rung(74.2).expect("records"), 0);
    assert_eq!(session.record_rung(79.9).expect("records"), 1);
    assert_eq!(session.record_rung(83.8).expect("records"), 2);
    let rungs: Vec<u32> = session
        .log()
        .events()
        .iter()
        .filter_map(|e| match e {
            SessionEvent::RungMeasured { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(rungs, vec![0, 1, 2]);
}

/// Rungs belong to the level check: before a solve exists there is nothing to
/// have measured, and after termination there is nothing to record into.
#[test]
fn a_rung_cannot_be_recorded_outside_the_level_check() {
    let volume = MockVolume::new(PRE_VOLUME, Journal::default());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(MockSink::default(), MockTap::new(true), volume),
    )
    .expect("begin succeeds");
    assert!(matches!(
        session.record_rung(74.2).unwrap_err(),
        SessionError::WrongPhase { .. }
    ));
}

// ── Review-added hardening (Stage-4 review) ───────────────────────────────

/// The sweep gate must reject a non-Sweep stimulus even when its level matches
/// the session's decided level — the vulnerable case, since the pilot's fixed
/// −40 dBFS can coincide with a margined level. Here the solve is arranged so
/// the session's level is exactly −40 dBFS, matching the pilot, so only the
/// kind check (which runs before the level check) can catch it.
#[test]
fn sweep_rejects_a_pilot_even_at_a_matching_level() {
    // solved −40 dBFS with a matching gain pin ⇒ emitted level −40 dBFS, the
    // pilot's fixed level; projected 64 dB is well under the OverEar cap.
    let matching_solve = SolveOutcome {
        chain_sensitivity_spl_per_dbfs: 104.0,
        projected_spl_db: 64.0,
        solved_dbfs_rms: -40.0,
    };
    let sink = MockSink::default();
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(
            sink.clone(),
            MockTap::new(true),
            MockVolume::new(PRE_VOLUME, Journal::default()),
        ),
    )
    .expect("begin succeeds");
    let level = session
        .install_solve(matching_solve)
        .expect("solve installs");
    session.acknowledge("Amp", 64.0).expect("ack records");

    let pilot = paraeq_measure::assemble_pilot(0.25, 48_000, TransducerClass::OverEar)
        .expect("pilot assembles");
    assert_eq!(
        pilot.level().dbfs_rms(),
        level.dbfs_rms(),
        "the test only bites if the levels genuinely match"
    );
    assert!(matches!(
        session.sweep(&pilot).unwrap_err(),
        SessionError::StimulusNotSweep { .. }
    ));
    assert_eq!(
        sink.emit_calls(),
        0,
        "no pilot sample plays under the sweep gate"
    );
}

/// An abort armed during the FINAL block must not be lost: the run must end
/// `Aborted` with the terminating diagnostic in the log, not a clean
/// `Completed`. The trigger fires during the last emit, after which the loop
/// exits — the post-loop re-check is the only thing that can catch it.
#[test]
fn abort_on_the_final_block_is_not_lost() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let mut session = acknowledged_session(sink.clone(), MockTap::new(true), volume.clone());

    let level = session.emit_level().expect("level installed");
    let stim = assemble_sweep(0.25, 48_000, 20.0, 20_000.0, level).expect("stimulus assembles");
    let total_blocks = stim.samples().len().div_ceil(BLOCK);
    sink.trip_on_call(
        total_blocks,
        session.abort_handle(),
        AbortReason::UserRequest,
    );

    match session.sweep(&stim).expect("an abort is an outcome") {
        SweepOutcome::Aborted { diagnostic, .. } => assert_eq!(diagnostic, D::UserAborted),
        completed => panic!("a final-block abort was lost as {completed:?}"),
    }
    // Every block emitted (the trigger was on the last), no extra ramp block —
    // the stimulus was exhausted, so there is nothing to fade.
    assert_eq!(
        sink.emit_calls(),
        total_blocks,
        "no ramp block: stimulus exhausted"
    );
    let log = session.finish();
    assert_eq!(
        log.events().last(),
        Some(&SessionEvent::Terminated {
            diagnostic: Some(D::UserAborted),
        }),
        "the log must end aborted, not SweepCompleted"
    );
    assert!(
        !log.events().contains(&SessionEvent::SweepCompleted),
        "a lost abort would have logged SweepCompleted"
    );
    assert_eq!(volume.sets(), vec![PRE_VOLUME], "restore still runs");
}

/// A solve that clears the SPL projection cap but whose margined output level
/// is illegal (over the class dBFS cap / −3 dBFS) must terminate through a
/// refusal with a diagnostic — never `?`-propagate a bare Level error leaving
/// the log without a terminating event (MS-23).
#[test]
fn an_illegal_solved_level_refuses_with_a_diagnostic() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let mut session = MeasurementSession::begin(
        healthy_cal(TransducerClass::OverEar),
        1.0,
        seam(sink.clone(), MockTap::new(true), volume.clone()),
    )
    .expect("begin succeeds");
    // In-cap SPL projection (64 dB ≪ OverEar cap) but a solved level of 0 dBFS
    // — above the unconditional −3 dBFS RMS ceiling SweepLevel::new refuses.
    let illegal = SolveOutcome {
        chain_sensitivity_spl_per_dbfs: 64.0,
        projected_spl_db: 64.0,
        solved_dbfs_rms: 0.0,
    };
    match session.install_solve(illegal).unwrap_err() {
        SessionError::Refused(refusal) => assert_eq!(refusal.diagnostic(), D::SolvedLevelIllegal),
        other => panic!("expected a SolvedLevelIllegal refusal, got {other:?}"),
    }
    assert_eq!(
        sink.emit_calls(),
        0,
        "no sample emitted at an illegal level"
    );
    assert_eq!(session.phase(), SessionPhase::Terminated);
    // MS-23: the log carries the terminating diagnostic, not a silent finish.
    let log = session.finish();
    assert_eq!(
        log.events().last(),
        Some(&SessionEvent::Terminated {
            diagnostic: Some(D::SolvedLevelIllegal),
        }),
    );
    assert_eq!(
        volume.sets(),
        vec![PRE_VOLUME],
        "restore runs on the refusal"
    );
}

/// The teardown never-mask posture: a failing sink `stop()` is collected as a
/// `SinkFault` but must NOT block the volume restore behind it.
#[test]
fn a_failing_stop_still_restores_the_volume() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    sink.fail_stop();
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    let mut session = acknowledged_session(sink.clone(), MockTap::new(true), volume.clone());
    // Abort at the sweep gate to force a teardown through the failing stop.
    session.abort_handle().trigger(AbortReason::UserRequest);
    let level = session.emit_level().expect("level installed");
    let stim = assemble_sweep(0.25, 48_000, 20.0, 20_000.0, level).expect("stimulus assembles");
    let _ = session.sweep(&stim).expect("abort is an outcome");

    // stop failed, yet the volume was still restored after it.
    assert_eq!(
        volume.sets(),
        vec![PRE_VOLUME],
        "volume restored despite a failing stop"
    );
    let log = session.finish();
    assert!(
        log.events()
            .iter()
            .any(|e| matches!(e, SessionEvent::SinkFault { .. })),
        "the stop fault must be collected, not swallowed"
    );
    // The restore ran in its normal slot in the order (stop attempted first).
    assert_eq!(journal.calls().last(), Some(&"set_volume".to_owned()));
}

/// A failing volume restore is logged (`VolumeRestoreFailed`) and surfaced,
/// never swallowed — the other half of the never-mask posture.
#[test]
fn a_failing_volume_restore_is_logged() {
    let journal = Journal::default();
    let sink = MockSink::with_journal(journal.clone());
    let volume = MockVolume::new(PRE_VOLUME, journal.clone());
    volume.fail_set();
    let mut session = acknowledged_session(sink.clone(), MockTap::new(true), volume.clone());
    session.abort_handle().trigger(AbortReason::UserRequest);
    let level = session.emit_level().expect("level installed");
    let stim = assemble_sweep(0.25, 48_000, 20.0, 20_000.0, level).expect("stimulus assembles");
    let _ = session.sweep(&stim).expect("abort is an outcome");

    // The restore was attempted (recorded) and its failure logged.
    assert_eq!(volume.sets(), vec![PRE_VOLUME], "restore attempted");
    let log = session.finish();
    assert!(
        log.events()
            .iter()
            .any(|e| matches!(e, SessionEvent::VolumeRestoreFailed { .. })),
        "the restore failure must be logged"
    );
}
