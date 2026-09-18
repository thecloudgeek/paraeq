//! Controller lifecycle tests against a scriptable MockBackend: engage
//! gating, command plumbing, correction swaps, rebuild-on-change,
//! renegotiation, and fail-safe stop. Real threads are involved, so every
//! assertion polls with a timeout (no bare sleeps as assertions); watchdog
//! windows are shortened via `EngineConfig` to keep the suite fast.

mod common;

use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use common::mock_backend::{Call, MockBackend};
use paraeq_dsp::peq::{EQBand, FilterType};
use paraeq_engine::backend::BackendEvent;
use paraeq_engine::controller::{
    CorrectionConfig, EngineCommand, EngineConfig, EngineHandle, EngineState,
};
use paraeq_engine::status::{EngineStatus, WatchdogConfig};

const ENGAGE_MS: u64 = 200;
const FAIL_OPEN_MS: u64 = 300;
const IDLE_MS: u64 = 150;
const SILENCE_MS: u64 = 150;
const TICK_MS: u64 = 10;

/// Generous per-wait budget; each wait normally completes in tens of ms.
const WAIT: Duration = Duration::from_secs(5);

/// Baseline config: fail-open disabled so the pre-existing lifecycle tests
/// keep their timing semantics (several deliberately sit in
/// `NoInputDetected` longer than a shortened fail-open window). The
/// `fail_open_*` tests opt in via [`fail_open_config`].
fn fast_config() -> EngineConfig {
    EngineConfig {
        enabled: true,
        fail_open_after_ms: None,
        requested_buffer_frames: None,
        ring_capacity: 4,
        tick_ms: TICK_MS,
        watchdog: WatchdogConfig {
            engage_tolerance_ms: ENGAGE_MS,
            idle_window_ms: IDLE_MS,
            silence_window_ms: SILENCE_MS,
        },
    }
}

fn fail_open_config() -> EngineConfig {
    EngineConfig {
        fail_open_after_ms: Some(FAIL_OPEN_MS),
        ..fast_config()
    }
}

fn spawn_engine() -> (MockBackend, EngineHandle) {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fast_config());
    (backend, handle)
}

/// Poll `pred` every couple of ms until it holds or `timeout` elapses.
fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A memoryless gain-`g` IIR correction (`y = g * x`) for `channels`.
///
/// Every caller here passes `g <= 1`, and that is load-bearing: R1-1 computes
/// an auto-preamp for the baked arm from the realized cascade's magnitude, so
/// a boosting `g` would be attenuated straight back to unity and these tests
/// could no longer tell a correction from pass-through. A pure cut gets a
/// preamp of exactly 0 dB (DIVERGENCES.md #14 -- ParaEQ's preamp is clamped
/// at <= 0), which keeps the arithmetic about the plumbing under test.
fn iir_gain(g: f64, channels: usize) -> CorrectionConfig {
    CorrectionConfig::Iir {
        // The MockBackend reports 48 kHz, so the baked arm's R1-6 rate
        // compare passes. Rate independence itself is covered in
        // `test_rate_independence.rs`.
        design_rate: 48_000.0,
        sos_per_channel: vec![vec![[g, 0.0, 0.0, 1.0, 0.0, 0.0]]; channels],
    }
}

fn peaking(fc: f64, gain_db: f64, q: f64) -> EQBand {
    EQBand {
        filter_type: FilterType::Peaking,
        fc,
        gain_db,
        q,
    }
}

/// A `Peq` config at the MockBackend's 48 kHz, one band set broadcast across
/// channels -- the shape the desktop sends.
fn peq(bands: Vec<EQBand>) -> CorrectionConfig {
    CorrectionConfig::Peq {
        bands: vec![bands],
        design_rate: 48_000.0,
    }
}

/// `pump` and check the first output sample of channel 0 against `expect`.
fn pump_matches(backend: &MockBackend, frames: usize, amplitude: f32, expect: f32) -> bool {
    backend
        .pump(frames, amplitude)
        .is_some_and(|out| (out[0][0] - expect).abs() < 1e-5)
}

#[test]
fn engages_on_first_nonzero_input() {
    let (backend, handle) = spawn_engine();
    let snapshots = handle.subscribe();

    // Silence keeps callbacks flowing but never engages: Starting ->
    // NoInputDetected (informational -- TCC denial and "no music playing"
    // are indistinguishable), never Running or Failed. (A transient Idle is
    // tolerated here: it only means this test thread paused pumping long
    // enough for the idle window to elapse -- informational, not a bug.)
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.0);
        let status = handle.state().status.clone();
        assert!(
            !matches!(status, EngineStatus::Running | EngineStatus::Failed { .. }),
            "silence must not reach {status:?}"
        );
        matches!(status, EngineStatus::NoInputDetected { .. })
    }));
    assert_eq!(backend.start_count(), 1, "silence must not trigger rebuild");

    // First nonzero input -> Running.
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.5);
        handle.state().status == EngineStatus::Running
    }));

    // The subscribe channel saw the published transition.
    let saw_running =
        std::iter::from_fn(|| snapshots.try_recv().ok()).any(|s| s.status == EngineStatus::Running);
    assert!(saw_running, "subscriber missed the Running snapshot");
}

#[test]
fn bypass_and_gain_commands_reach_the_rt_side() {
    let (backend, handle) = spawn_engine();

    // Correction halves: 0.2 -> 0.1.
    handle.send(EngineCommand::SetCorrection(iir_gain(0.5, 2)));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.1)));

    // -6.0206 dB is 0.5 linear: corrected 0.1 * 0.5 = 0.05.
    handle.send(EngineCommand::SetGainDb(-6.020_6));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.05)));

    // Bypass skips correction only; gain still applies: 0.2 * 0.5 = 0.1.
    handle.send(EngineCommand::SetBypass(true));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.1)));

    // And back: corrected output returns.
    handle.send(EngineCommand::SetBypass(false));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.05)));
}

#[test]
fn set_correction_swaps_and_retires_off_thread() {
    let (backend, handle) = spawn_engine();

    // More successful swaps than the ring capacity (4): if the controller
    // did not drain retired corrections, the retire ring would fill and
    // later swaps would defer forever (a swap is gated on retire space).
    for i in 0..6 {
        let g = if i % 2 == 0 { 0.5 } else { 0.25 };
        handle.send(EngineCommand::SetCorrection(iir_gain(g, 2)));
        let expect = 0.2 * g as f32;
        assert!(
            wait_until(WAIT, || pump_matches(&backend, 512, 0.2, expect)),
            "swap {i} never landed (retire ring not drained?)"
        );
    }

    // ClearCorrection goes through the same ring: back to pass-through.
    handle.send(EngineCommand::ClearCorrection);
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.2)));
    assert!(handle.state().correction.is_none());
}

#[test]
fn backend_event_triggers_stop_start_rebuild() {
    let (backend, handle) = spawn_engine();

    // Retained config: correction x0.5, gain 0.5 -> 0.2 in, 0.05 out.
    handle.send(EngineCommand::SetCorrection(iir_gain(0.5, 2)));
    handle.send(EngineCommand::SetGainDb(-6.020_6));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.05)));
    assert_eq!(backend.start_count(), 1);

    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(wait_until(WAIT, || backend.start_count() == 2));

    // stop-then-start order: a Stop precedes the second Start.
    let calls = backend.calls();
    let second_start = calls
        .iter()
        .rposition(|c| matches!(c, Call::Start { .. }))
        .unwrap();
    assert!(
        calls[..second_start].contains(&Call::Stop),
        "rebuild must stop before restarting: {calls:?}"
    );

    // Fresh RtShared + chain after rebuild: stored correction AND gain
    // re-applied (watchdog re-baselines to zeroed counters, so everything
    // must come back from controller-retained state).
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.05)));
    assert_eq!(backend.start_count(), 2, "one rebuild, no rebuild loop");
}

#[test]
fn idle_does_not_rebuild() {
    let (backend, handle) = spawn_engine();

    // Get callbacks flowing, reach Running.
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.5);
        handle.state().status == EngineStatus::Running
    }));

    // Playback pauses: the tap aggregate's IOProc stops cycling entirely
    // (hardware finding 2026-07-11), so the callback counter freezes. That
    // is normal idling, NOT IOProc death: status goes Idle and NO rebuild
    // may happen (rebuilding would loop forever while the system is idle).
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::Idle { .. }
    )));
    assert!(!wait_until(Duration::from_millis(IDLE_MS * 3), || {
        backend.start_count() > 1
    }));
    assert_eq!(backend.start_count(), 1, "idle must never trigger rebuild");

    // Playback resumes -> Running again, still the same single session.
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.5);
        handle.state().status == EngineStatus::Running
    }));
    assert_eq!(backend.start_count(), 1);
}

#[test]
fn event_rebuild_start_failures_latch_failed() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));

    // The device dies; both rebuild start attempts fail -> Failed latch.
    backend.fail_next_starts(2);
    backend.queue_event(BackendEvent::DeviceDied);
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::Failed { .. }
    )));

    // Initial start + exactly 2 failed rebuild attempts...
    assert_eq!(backend.start_count(), 3);
    // ...and no rebuild-loop on a dead device afterward.
    assert!(!wait_until(Duration::from_millis(IDLE_MS * 3), || {
        backend.start_count() > 3
    }));
    assert!(!backend.is_running());
}

#[test]
fn disable_stops_backend_and_drop_is_clean() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));

    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || {
        !backend.is_running() && handle.state().status == EngineStatus::Stopped
    }));
    let stops_after_disable = backend.stop_count();
    assert!(stops_after_disable >= 1);

    // Drop joins the controller thread; the exit-path guard may issue at
    // most one more (idempotent) stop.
    drop(handle);
    assert!(backend.stop_count() <= stops_after_disable + 1);
    assert!(!backend.is_running());
}

#[test]
fn buffer_frames_renegotiation() {
    let backend = MockBackend::new();
    backend.set_reported_buffer_frames(256); // != requested 128
    let mut config = fast_config();
    config.requested_buffer_frames = Some(128);
    let handle = EngineHandle::spawn(backend.clone(), config);

    // Exactly one stop+start renegotiation cycle; the retry requests the
    // REPORTED size (256), not the original request (asking for 128 again
    // would just reproduce the mismatch).
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    assert_eq!(
        backend.calls()[..3],
        [
            Call::Start {
                requested_buffer_frames: Some(128)
            },
            Call::Stop,
            Call::Start {
                requested_buffer_frames: Some(256)
            },
        ]
    );
    assert!(wait_until(WAIT, || {
        handle
            .state()
            .stream
            .as_ref()
            .is_some_and(|s| s.buffer_frames == 256)
    }));

    // FIR requires exactly block_size frames: corrected output at 256
    // proves the chain was rebuilt for the effective size, not the request.
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Fir {
        design_rate: 48_000.0,
        firs: vec![vec![0.5]; 2],
    }));
    assert!(wait_until(WAIT, || pump_matches(&backend, 256, 0.2, 0.1)));
    assert_eq!(backend.start_count(), 2, "renegotiation must not loop");

    // SetBufferFrames while running: stored request updated + one clean
    // restart (new request matches the reported size, so no extra cycle).
    handle.send(EngineCommand::SetBufferFrames(256));
    assert!(wait_until(WAIT, || backend.start_count() == 3));
    assert_eq!(
        backend.calls().last(),
        Some(&Call::Start {
            requested_buffer_frames: Some(256)
        })
    );
    assert!(wait_until(WAIT, || pump_matches(&backend, 256, 0.2, 0.1)));
}

#[test]
fn channel_count_renegotiation() {
    let backend = MockBackend::new();
    backend.set_reported_channels(1); // != provisional stereo chain
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    // One stop+start cycle down to the mono chain.
    assert!(wait_until(WAIT, || backend.start_count() == 2));

    handle.send(EngineCommand::SetCorrection(iir_gain(0.5, 1)));
    assert!(wait_until(WAIT, || {
        backend
            .pump(512, 0.2)
            .is_some_and(|out| out.len() == 1 && (out[0][0] - 0.1).abs() < 1e-5)
    }));
    assert_eq!(backend.start_count(), 2, "renegotiation must not loop");
    assert!(handle
        .state()
        .stream
        .as_ref()
        .is_some_and(|s| s.channels == 1));
}

#[test]
fn shifting_geometry_renegotiation_converges_on_final_report() {
    let backend = MockBackend::new();
    // Start 1 reports (2 ch, 256) -- differs from the provisional chain;
    // start 2 (retrying at 2 ch / 256) reports (1 ch, 128) -- differs
    // AGAIN; start 3 (retrying at 1 ch / 128) reports the sticky (1, 128)
    // and converges exactly at the 3-start cap.
    backend.queue_report(2, 256);
    backend.queue_report(1, 128);
    backend.set_reported_channels(1);
    backend.set_reported_buffer_frames(128);
    let mut config = fast_config();
    config.requested_buffer_frames = Some(512);
    let handle = EngineHandle::spawn(backend.clone(), config);

    assert!(wait_until(WAIT, || backend.start_count() == 3));
    // Each retry requests the LAST-reported size.
    let starts: Vec<Call> = backend
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::Start { .. }))
        .collect();
    assert_eq!(
        starts,
        [
            Call::Start {
                requested_buffer_frames: Some(512)
            },
            Call::Start {
                requested_buffer_frames: Some(256)
            },
            Call::Start {
                requested_buffer_frames: Some(128)
            },
        ]
    );

    // The final chain matches the FINAL report: a FIR (which demands
    // exactly block_size frames) corrects a mono 128-frame block.
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Fir {
        design_rate: 48_000.0,
        firs: vec![vec![0.5]],
    }));
    assert!(wait_until(WAIT, || {
        backend
            .pump(128, 0.2)
            .is_some_and(|out| out.len() == 1 && (out[0][0] - 0.1).abs() < 1e-5)
    }));
    assert_eq!(backend.start_count(), 3, "renegotiation must stop at cap");
    assert!(handle
        .state()
        .stream
        .as_ref()
        .is_some_and(|s| s.channels == 1 && s.buffer_frames == 128));
}

#[test]
fn command_flood_does_not_starve_tick_work() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));

    // A queued backend event is only noticed by tick work (poll_event).
    backend.queue_event(BackendEvent::DefaultOutputChanged);

    // Flood commands faster than tick_ms: recv_timeout keeps returning Ok,
    // so without the in-command tick check the rebuild would never happen.
    let deadline = Instant::now() + WAIT;
    while backend.start_count() < 2 {
        assert!(
            Instant::now() < deadline,
            "tick work starved by command flood: rebuild never happened"
        );
        handle.send(EngineCommand::SetGainDb(0.0));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(backend.start_count() >= 2, "rebuild happened under flood");
}

#[test]
fn malformed_correction_is_ignored_not_fatal() {
    let (backend, handle) = spawn_engine();

    // Degenerate configs would panic build_correction; the controller must
    // validate and ignore them (no state change, thread stays alive).
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Fir {
        design_rate: 48_000.0,
        firs: vec![],
    }));
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Fir {
        design_rate: 48_000.0,
        firs: vec![vec![]],
    }));
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Iir {
        design_rate: 48_000.0,
        sos_per_channel: vec![],
    }));

    // The controller thread survived and a subsequent valid command works.
    handle.send(EngineCommand::SetCorrection(iir_gain(0.5, 2)));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.1)));
    assert!(handle
        .state()
        .correction
        .as_ref()
        .is_some_and(|c| c.starts_with("iir")));
}

/// The other half of `validate_correction`: bad band VALUES, which the
/// owner-decided relocation (D-13,
/// `docs/decisions/2026-07-22-owner-value-calls.md`) moved out of
/// `desktop/src-tauri/src/eq.rs` into the engine, so the daemon seam
/// inherits the guard. The rate-independent checks (finite `fc`/`q`/
/// `gain_db`, `fc > 0`, `q` in range, `|gain_db| <= GAIN_LIMIT_DB`) run at
/// COMMAND time and warn-and-ignore the whole config; the Nyquist half is
/// rate-dependent and belongs at build time, where it drops band by band.
///
/// The distinction is the point, and nothing exercised it: the existing
/// malformed-correction test sends only STRUCTURAL defects (which
/// `build_correction` rejects independently), and every `SetCorrection` with
/// `Peq` bands elsewhere in the suite carries valid ones. Deleting the value
/// pre-screen left the whole workspace green while changing the contract --
/// the junk config would be ACCEPTED, `build_correction` would drop its only
/// band, nothing would survive, and the engine would replace a healthy live
/// correction with flat pass-through AND publish a rate-mismatch refusal
/// that blames the sample rate for a NaN.
#[test]
fn a_band_value_defect_is_ignored_and_leaves_the_live_correction_alone() {
    let (backend, handle) = spawn_engine();

    // A good two-band Peq goes live, with a boost so it owns a preamp.
    handle.send(EngineCommand::SetCorrection(peq(vec![
        peaking(1_000.0, 9.0, 1.0),
        peaking(4_000.0, -3.0, 1.0),
    ])));
    assert!(
        wait_until(WAIT, || handle.state().correction.as_deref()
            == Some("peq:2-band")),
        "the good correction never installed"
    );
    let live = handle.state();
    let preamp = live
        .auto_preamp_db
        .expect("a +9 dB band must carry an auto-preamp");

    for bad in [
        peaking(1_000.0, 3.0, f64::NAN),
        peaking(1_000.0, 3.0, 0.0),
        peaking(1_000.0, f64::INFINITY, 1.0),
        peaking(1_000.0, 1_000.0, 1.0),
    ] {
        handle.send(EngineCommand::SetCorrection(peq(vec![bad])));
    }
    // A command that IS accepted, to order the assertions behind the ignored
    // ones: the controller handles commands in the order they were sent.
    handle.send(EngineCommand::SetBypass(true));
    assert!(
        wait_until(WAIT, || handle.state().bypass),
        "the ordering command never landed"
    );

    let after = handle.state();
    assert_eq!(
        after.correction.as_deref(),
        Some("peq:2-band"),
        "an ignored SetCorrection must not replace the retained config"
    );
    assert_eq!(
        after.auto_preamp_db,
        Some(preamp),
        "the live chain (and its headroom) must be untouched"
    );
    assert_eq!(
        after.correction_rate_mismatch, None,
        "a value defect is not a rate refusal and must not be reported as one"
    );

    // The thread survived all four and still accepts work.
    handle.send(EngineCommand::SetBypass(false));
    handle.send(EngineCommand::SetCorrection(iir_gain(0.5, 2)));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.1)));
}

#[test]
fn slow_first_callback_does_not_rebuild() {
    let (backend, handle) = spawn_engine();

    // No pumping at all: callbacks frozen at 0 well past idle_window. The
    // spike measured ~4 s with NO callbacks before a healthy tap engages,
    // so this must be treated as a slow engage, never as Idle.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    )));
    assert!(!wait_until(Duration::from_millis(IDLE_MS * 3), || {
        backend.start_count() > 1
    }));
    assert_eq!(backend.start_count(), 1, "slow engage must not rebuild");
    assert!(matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    ));
}

#[test]
fn disable_enable_roundtrip() {
    let (backend, handle) = spawn_engine();

    handle.send(EngineCommand::SetCorrection(iir_gain(0.5, 2)));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.1)));

    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || {
        !backend.is_running() && handle.state().status == EngineStatus::Stopped
    }));
    assert!(backend.pump(512, 0.2).is_none(), "disabled == no processor");

    handle.send(EngineCommand::Enable);
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    // Stored correction re-applied to the fresh chain after Enable.
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.1)));
}

#[test]
fn fail_open_auto_disables_when_input_never_arrives() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());

    // Zeros only (the TCC silent-failure signature): Starting ->
    // NoInputDetected -> past the fail-open window -> AutoDisabledNoInput.
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.0);
        matches!(
            handle.state().status,
            EngineStatus::AutoDisabledNoInput { .. }
        )
    }));

    // The auto-disable took the Disable path: backend stopped (tap
    // destroyed -> device unmuted) after waiting out the full window.
    assert!(!backend.is_running(), "auto-disable must stop the backend");
    assert!(backend.stop_count() >= 1);
    match handle.state().status {
        EngineStatus::AutoDisabledNoInput { after_ms } => assert!(
            after_ms >= FAIL_OPEN_MS,
            "after_ms {after_ms} < window {FAIL_OPEN_MS}"
        ),
        ref other => panic!("expected AutoDisabledNoInput, got {other:?}"),
    }

    // Sticky: the status must not be overwritten by the watchdog's
    // post-stop `Stopped` on later ticks, and there is no auto-retry
    // (exactly one start total -- re-engaging would re-mute the system).
    assert!(
        !wait_until(Duration::from_millis(TICK_MS * 20), || {
            !matches!(
                handle.state().status,
                EngineStatus::AutoDisabledNoInput { .. }
            )
        }),
        "AutoDisabledNoInput was overwritten after the auto-disable"
    );
    assert_eq!(backend.start_count(), 1, "auto-disable must never retry");
}

#[test]
fn fail_open_never_fires_after_running() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());

    // Real input engages the engine: fail-open is now off the table for
    // this session (NoInputDetected can only precede the first nonzero
    // input; the post-Running states are Idle / InputSilent).
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.5);
        handle.state().status == EngineStatus::Running
    }));

    // Playback pauses: callbacks freeze -> Idle. Sitting there well past
    // the fail-open window must NOT disable anything -- pausing music must
    // never disable the EQ.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::Idle { .. }
    )));
    assert!(!wait_until(Duration::from_millis(FAIL_OPEN_MS * 2), || {
        matches!(
            handle.state().status,
            EngineStatus::AutoDisabledNoInput { .. }
        )
    }));
    assert!(backend.is_running());

    // Rendering resumes with silent zeros -> InputSilent; same rule, again
    // well past the fail-open window.
    let deadline = Instant::now() + Duration::from_millis(FAIL_OPEN_MS * 2);
    let mut saw_input_silent = false;
    while Instant::now() < deadline {
        backend.pump(512, 0.0);
        let status = handle.state().status.clone();
        assert!(
            !matches!(status, EngineStatus::AutoDisabledNoInput { .. }),
            "fail-open fired from a post-Running state: {status:?}"
        );
        saw_input_silent |= matches!(status, EngineStatus::InputSilent { .. });
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        saw_input_silent,
        "zeros after Idle should report InputSilent"
    );
    assert!(backend.is_running());
    assert_eq!(backend.stop_count(), 0);
    assert_eq!(
        backend.start_count(),
        1,
        "fail-open must never fire after Running"
    );
}

#[test]
fn enable_after_fail_open_restarts() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());

    // No pumping at all also reaches NoInputDetected (the slow-engage
    // path: callbacks frozen at 0) and then the fail-open latch.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::AutoDisabledNoInput { .. }
    )));
    assert_eq!(backend.start_count(), 1);

    // Explicit Enable clears the latch and starts again, exactly like
    // Enable-after-Disable. The auto-disable did NOT count toward the
    // 2-consecutive-start-failure Failed latch, so this start proceeds.
    handle.send(EngineCommand::Enable);
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.5);
        handle.state().status == EngineStatus::Running
    }));
}

#[test]
fn fail_open_none_disables_the_behavior() {
    // fast_config: fail_open_after_ms = None.
    let (backend, handle) = spawn_engine();

    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.0);
        matches!(handle.state().status, EngineStatus::NoInputDetected { .. })
    }));

    // Zeros forever, far past the window that WOULD have fired: the engine
    // stays in the informational NoInputDetected state and never stops.
    let deadline = Instant::now() + Duration::from_millis(FAIL_OPEN_MS * 3);
    while Instant::now() < deadline {
        backend.pump(512, 0.0);
        assert!(
            !matches!(
                handle.state().status,
                EngineStatus::AutoDisabledNoInput { .. }
            ),
            "fail-open fired despite fail_open_after_ms = None"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    ));
    assert_eq!(backend.stop_count(), 0, "None must mean no auto-disable");
    assert_eq!(backend.start_count(), 1);
    assert!(backend.is_running());
}

// --- MeasurementLease (wizard/1) -------------------------------------------
//
// wizard-design.md:418, verbatim: "the wizard acquires a `MeasurementLease`
// from the controller for the duration of a session. The lease suspends the
// fail-open watchdog and is released on **every** exit path including panic
// -- the same invariant, and the same discipline, as tap teardown. The lease
// must not suppress the watchdog's *reporting*: a genuine TCC silent failure
// during a measurement session is still a `Refuse`, it just must not race the
// wizard to the teardown."
//
// These tests park the watchdog in `NoInputDetected` by NOT pumping at all --
// callbacks frozen at 0, the slow-engage signature `slow_first_callback_does_
// not_rebuild` and `enable_after_fail_open_restarts` already use, and the
// latter proves this exact configuration DOES auto-disable without a lease.
//
// Pumping silent zeros is the more faithful wizard signature (a `Direct`
// capture is tap-excluded BY DESIGN, so callbacks flow and every sample is
// zero), but it cannot be used for a long sit here: one stall of this thread
// past `idle_window_ms` takes the watchdog Idle -> InputSilent, which the
// fail-open branch can never fire from at all, and the falsifier below would
// then pass for entirely the wrong reason. Frozen callbacks gate Idle off
// (`Watchdog::callbacks_seen`), so `NoInputDetected` is stable for as long as
// a test needs it.

#[test]
fn measurement_lease_suspends_fail_open() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());
    let lease = handle
        .acquire_measurement_lease()
        .expect("a fresh engine hands out the first lease");

    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    )));

    // Sit far past the window. Without the lease this is exactly
    // `enable_after_fail_open_restarts`, which latches AutoDisabledNoInput.
    let deadline = Instant::now() + Duration::from_millis(FAIL_OPEN_MS * 3);
    while Instant::now() < deadline {
        assert!(
            !matches!(
                handle.state().status,
                EngineStatus::AutoDisabledNoInput { .. }
            ),
            "fail-open fired while a measurement lease was held"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // The tap is still up and the session was never torn down mid-run --
    // wizard:416's named failure ("the controller tears the tap down
    // mid-session, and the subsequent Helper verification capture has no
    // engine to run through").
    assert_eq!(
        backend.start_count(),
        1,
        "the lease must not restart anything"
    );
    assert_eq!(
        backend.stop_count(),
        0,
        "a held lease must never reach stop_session"
    );
    assert!(backend.is_running());
    assert!(handle.state().enabled, "the lease must not clear `enabled`");
    assert!(matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    ));

    drop(lease);
}

#[test]
fn lease_does_not_suppress_reporting() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());
    let snapshots = handle.subscribe();
    let lease = handle.acquire_measurement_lease().expect("first lease");

    // wizard:418's second clause. The status machine keeps producing
    // `NoInputDetected` under the lease, and it keeps being PUBLISHED -- a
    // genuine TCC denial mid-measurement is still visible to the wizard, which
    // refuses on its own evidence. Only the teardown is deferred.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    )));
    let mut published = false;
    assert!(
        wait_until(WAIT, || {
            published |= std::iter::from_fn(|| snapshots.try_recv().ok())
                .any(|s| matches!(s.status, EngineStatus::NoInputDetected { .. }));
            published
        }),
        "NoInputDetected was never published while a lease was held"
    );

    // Still reported well past the window it is suspending.
    std::thread::sleep(Duration::from_millis(FAIL_OPEN_MS * 2));
    assert!(matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    ));

    drop(lease);
}

#[test]
fn lease_release_rearms_fail_open_from_zero() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());
    let lease = handle.acquire_measurement_lease().expect("first lease");

    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    )));
    // Accrue three full windows under the lease.
    std::thread::sleep(Duration::from_millis(FAIL_OPEN_MS * 3));
    assert!(matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    ));

    let released_at = Instant::now();
    drop(lease);

    // Fail-open re-arms FROM ZERO, not from the accrued time: the tap is
    // supposed to see zeros during a measurement (`Direct` captures are
    // tap-excluded by design), so that time is not evidence of a TCC failure.
    // A fire-on-release implementation auto-disables the engine immediately
    // after every successful measurement -- exactly backwards -- and fails
    // here on both assertions.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::AutoDisabledNoInput { .. }
    )));
    let elapsed = released_at.elapsed();
    assert!(
        elapsed >= Duration::from_millis(FAIL_OPEN_MS),
        "fired {elapsed:?} after release; a full {FAIL_OPEN_MS} ms window must elapse first"
    );
    match handle.state().status {
        // The window the controller reports is the re-baselined one (~one
        // window), not the ~four windows that actually elapsed in
        // NoInputDetected.
        EngineStatus::AutoDisabledNoInput { after_ms } => assert!(
            (FAIL_OPEN_MS..FAIL_OPEN_MS * 2).contains(&after_ms),
            "after_ms {after_ms} should be about one window ({FAIL_OPEN_MS} ms); \
             the time accrued under the lease must not count"
        ),
        ref other => panic!("expected AutoDisabledNoInput, got {other:?}"),
    }
    assert!(
        !backend.is_running(),
        "the re-armed fail-open must stop the backend"
    );
}

#[test]
fn lease_does_not_block_rebuild() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());
    let lease = handle.acquire_measurement_lease().expect("first lease");
    assert!(wait_until(WAIT, || backend.start_count() == 1));

    // A device change must still rebuild under a lease. The measurement
    // aborts on it on its own evidence (AbortReason::OutputDeviceChanged),
    // so suppressing the rebuild would only leave the engine on a dead device.
    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(
        wait_until(WAIT, || backend.start_count() == 2),
        "a held lease blocked the rebuild"
    );
    assert!(wait_until(WAIT, || backend.is_running()));
    assert_eq!(
        backend.calls(),
        vec![
            Call::Start {
                requested_buffer_frames: None
            },
            Call::Stop,
            Call::Start {
                requested_buffer_frames: None
            },
        ],
        "rebuild must still be stop-then-start under a lease"
    );

    drop(lease);
}

#[test]
fn lease_does_not_block_disable_or_shutdown() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());
    let lease = handle.acquire_measurement_lease().expect("first lease");
    assert!(wait_until(WAIT, || backend.is_running()));

    // Explicit user intent outranks a measurement.
    handle.send(EngineCommand::Disable);
    assert!(
        wait_until(WAIT, || {
            !backend.is_running() && handle.state().status == EngineStatus::Stopped
        }),
        "a held lease blocked EngineCommand::Disable"
    );

    // ...and so does app exit: dropping the handle sends Shutdown and JOINS
    // the controller thread. A lease that blocked Shutdown would hang quit.
    let started = Instant::now();
    drop(handle);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "dropping the EngineHandle under a lease must still join the controller thread"
    );
    assert!(backend.stop_count() >= 1);

    drop(lease);
}

#[test]
fn second_lease_is_refused() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());

    let first = handle.acquire_measurement_lease().expect("first lease");
    assert!(
        handle.acquire_measurement_lease().is_none(),
        "one measurement at a time: a second lease must be refused, not queued"
    );

    drop(first);
    let second = handle
        .acquire_measurement_lease()
        .expect("the cell is free again once the first lease is released");
    drop(second);
}

#[test]
fn lease_is_released_on_panic() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());

    let unwind = panic::catch_unwind(AssertUnwindSafe(|| {
        let _lease = handle.acquire_measurement_lease().expect("first lease");
        panic!("wizard run blew up mid-measurement");
    }));
    assert!(unwind.is_err(), "the injected panic must propagate");

    // Drop ran during unwinding, so the cell is free...
    let again = handle
        .acquire_measurement_lease()
        .expect("a panicked holder must not strand the lease");
    drop(again);

    // ...and the safety net is genuinely back on, not merely the flag: the
    // configuration that sat suspended in `measurement_lease_suspends_fail_open`
    // now fails open.
    assert!(
        wait_until(WAIT, || matches!(
            handle.state().status,
            EngineStatus::AutoDisabledNoInput { .. }
        )),
        "fail-open must be re-armed after a panicked lease holder"
    );
}

#[test]
fn lease_outliving_the_handle_is_harmless() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fail_open_config());
    let lease = handle.acquire_measurement_lease().expect("first lease");
    assert!(wait_until(WAIT, || backend.is_running()));

    // App exit with a lease still outstanding. The handle drops first
    // (Shutdown + join + backend stop), the token after -- the reverse of the
    // wizard's own drop order, and the order a caller gets wrong by accident.
    drop(handle);
    assert!(
        !backend.is_running(),
        "shutdown must stop the backend even with a lease outstanding"
    );

    // The token holds only the shared cell, never the handle, so this is a
    // plain store into an `Arc<AtomicBool>` nothing reads any more.
    let started = Instant::now();
    drop(lease);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn spawn_disabled_does_not_start_until_enable() {
    let backend = MockBackend::default();
    let probe = backend.clone();
    let handle = EngineHandle::spawn(
        backend,
        EngineConfig {
            enabled: false,
            tick_ms: TICK_MS,
            ..fast_config()
        },
    );
    // Disabled spawn: the backend is never touched, and the snapshot reports
    // enabled: false, status: Stopped.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        probe.start_count(),
        0,
        "disabled spawn must not touch the backend"
    );
    let s = handle.state();
    assert!(!s.enabled);
    assert_eq!(s.status, EngineStatus::Stopped);

    // Enable starts the backend and flips enabled: true.
    handle.send(EngineCommand::Enable);
    assert!(wait_until(WAIT, || probe.start_count() == 1));
    assert!(wait_until(WAIT, || handle.state().enabled));

    // Disable flips it back and tears the session down.
    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || {
        !probe.is_running() && !handle.state().enabled
    }));
}

#[test]
fn enabled_flip_alone_publishes() {
    // Guards the hand-rolled `effectively_equal`: spawn disabled, make the
    // Enable-time start fail so `status` stays Stopped and EVERY other
    // compared field is unchanged -- only `enabled` flips to true. If the
    // comparison omits `enabled`, publish() returns early and no subscriber
    // ever sees the flip.
    //
    // The tick is set far beyond the test's runtime on purpose. The
    // Enable-time start is only DEFERRED by `fail_next_starts(1)`: the next
    // `on_tick` retries it and, on success, advances Stopped -> Starting.
    // `run`'s tick-starvation guard runs `on_tick` in the SAME loop iteration
    // as `handle(Enable)` -- before the flip's `publish` -- whenever
    // `last_tick.elapsed() >= tick`, which would coalesce the transient
    // {enabled: true, status: Stopped} frame this test waits for into a
    // {enabled: true, status: Starting} one. A tick that cannot elapse before
    // Enable arrives keeps the guard from ever firing here, so the
    // lone-`enabled` publish is emitted deterministically. (The deferred
    // start's retry is pushed past teardown; the handle's Shutdown wakes the
    // blocked `recv_timeout` immediately, so the long tick never delays
    // anything.)
    let backend = MockBackend::default();
    let probe = backend.clone();
    let handle = EngineHandle::spawn(
        backend,
        EngineConfig {
            enabled: false,
            // Far larger than the whole test; see the note above.
            tick_ms: 600_000,
            ..fast_config()
        },
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(!handle.state().enabled);
    assert_eq!(handle.state().status, EngineStatus::Stopped);

    probe.fail_next_starts(1);
    let snapshots = handle.subscribe();
    handle.send(EngineCommand::Enable);

    assert!(wait_until(WAIT, || handle.state().enabled));
    let saw_flip = std::iter::from_fn(|| snapshots.try_recv().ok())
        .any(|s| s.enabled && s.status == EngineStatus::Stopped);
    assert!(saw_flip, "enabled flip alone did not publish a snapshot");
}

#[test]
fn frame_mismatch_blocks_reach_snapshots() {
    let (backend, handle) = spawn_engine();

    // A FIR is built for the session's block size (the mock reports 512) and
    // corrects ONLY exact-block_size frames, so pumping half-blocks (256)
    // flags frame_mismatch on every block and grows the snapshot counter.
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Fir {
        design_rate: 48_000.0,
        firs: vec![vec![1.0]; 2],
    }));
    assert!(wait_until(WAIT, || {
        backend.pump(256, 0.5);
        handle.state().frame_mismatch_blocks > 0
    }));

    // A rebuild builds a fresh RtShared (counter zeroed); conforming
    // full-block pumps flag no new mismatch, so the snapshot re-zeroes.
    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.5);
        handle.state().frame_mismatch_blocks == 0
    }));
}

// ---------------------------------------------------------------------------
// wizard/1, half 1: the MS-6 self-exclusion witness on the wire.
//
// `EngineState.self_excluded` answers one question -- is the audio ParaEQ's own
// process emits kept out of ParaEQ's own capture right now? The measurement
// wizard refuses to begin a `Direct` capture when it is false
// (`docs/specs/2026-07-15-wizard-design.md:412`), because the "uncorrected"
// baseline would silently be a corrected one and feedback would be live into a
// coupler that may be on someone's head.
//
// The controller invents nothing here: it publishes exactly what the backend
// reports, so the field and `paraeq-measure`'s live `TapStatus` witness cannot
// disagree about the same tap.
// ---------------------------------------------------------------------------

/// The snapshot follows the backend, in both directions. A live stream with
/// `self_excluded == false` is the fail-open signature the wizard refuses on
/// (`tap.rs`'s empty exclusion list after `translate_pid` returned 0 twice).
#[test]
fn self_excluded_is_published_from_the_backend() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));
    // The mock reports an excluding tap by default -- a healthy `TapSystem`.
    assert!(wait_until(WAIT, || handle.state().self_excluded));

    backend.set_self_excluded(false);
    assert!(wait_until(WAIT, || !handle.state().self_excluded));
    assert!(
        handle.state().stream.is_some(),
        "false WITH a live stream is the fail-open signature, not 'engine off'"
    );
}

/// A safety witness must reach the UI on EVERY change, so `effectively_equal`
/// compares it exactly -- unlike `input_peak` (quantized at 1e-3) or
/// `latency_ms` (quantized at 0.1 ms), whose deltas are meter noise.
#[test]
fn self_excluded_change_always_publishes() {
    let (backend, handle) = spawn_engine();
    // Settle into a steady state where nothing else moves: never pumping
    // parks the watchdog in `NoInputDetected` (fast_config disables
    // fail-open, and with zero callbacks it never reaches `Idle`), and every
    // metered field stays at its start value.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    )));
    let before = (*handle.state()).clone();
    let snapshots = handle.subscribe();

    backend.set_self_excluded(false);

    // Take snapshots until the change arrives, rather than asserting on the
    // first one: a snapshot IDENTICAL to `before` can legitimately arrive
    // first, because `publish` stores the new snapshot into the `state()`
    // cell BEFORE it locks the subscriber list. A `state()` read followed by
    // a `subscribe()` can straddle one publish and see it twice -- once as
    // `before`, once on the channel. That is a delivery race in the test's
    // own setup, not a change in the engine, and it fails a loaded
    // `cargo test --workspace` often enough to matter.
    //
    // The teeth are unchanged: every snapshot that arrives BEFORE the change
    // must still equal `before` EXACTLY, and the changed one must differ in
    // `self_excluded` and NOTHING else -- so any tolerance on the compare
    // would still be caught, and the wizard could still not read a stale
    // safety fact.
    let expected = EngineState {
        self_excluded: false,
        ..before.clone()
    };
    loop {
        let published = snapshots
            .recv_timeout(WAIT)
            .expect("a self_excluded change must publish a snapshot");
        if published.self_excluded {
            assert_eq!(
                *published, before,
                "nothing but `self_excluded` may move in this steady state"
            );
            continue;
        }
        assert_eq!(*published, expected);
        break;
    }
}

/// No session, nothing witnessed. The `(self_excluded, stream)` PAIR is what
/// lets the UI tell "the engine is off" (`false`, `None`) from "the fail-open
/// path fired" (`false`, `Some`) -- which is why the spec's literal `bool` is
/// enough and no `Option<bool>` is needed.
#[test]
fn self_excluded_is_false_when_no_session() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || handle.state().self_excluded));

    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || {
        let state = handle.state();
        !state.self_excluded && state.stream.is_none()
    }));
    assert!(!backend.is_running());
}

// ---------------------------------------------------------------------------
// B18: the two `BuildReport` facts on the wire, and the non-publishing
// `tap_activity()` accessor.
//
// `BuildReport { bands_dropped, preamp_db, sections_substituted }` records the
// only two ways the INSTALLED cascade differs from the one the plan describes.
// Until B18 both counts were logged at `warn` and thrown away, which was
// defensible only while the Advanced drawer (Stage 7) was their sole consumer.
// Stage 6's verification gate is a second consumer: it refuses on
// `bands_dropped > 0`, because predicting the plan's response while the chain
// runs a subset would blame the chain for our own prediction fault.
//
// `tap_activity()` goes the other way -- deliberately NOT on `EngineState`.
// It is a monotonic per-block counter, so putting it in `effectively_equal`'s
// compare set would emit a snapshot plus a Tauri event every tick, which is
// the documented reason `frame_mismatch_blocks` is compared with `> 0`.
// ---------------------------------------------------------------------------

/// A band above the live Nyquist is dropped, counted, and PUBLISHED. The count
/// rides the snapshot because the verification gate reads it through the
/// engine-facts seam, and a seam can only carry what the wire carries.
#[test]
fn bands_dropped_reaches_the_published_snapshot() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));

    // 30 kHz is above the mock's 24 kHz Nyquist; 1 kHz survives, so the
    // correction still installs and only the one band is dropped.
    handle.send(EngineCommand::SetCorrection(peq(vec![
        peaking(1_000.0, -3.0, 1.0),
        peaking(30_000.0, -3.0, 1.0),
    ])));
    assert!(wait_until(WAIT, || handle.state().bands_dropped == 1));
    assert!(
        handle.state().correction.is_some(),
        "one dropped band must not refuse the whole set (D-10)"
    );

    // ...and it is session-scoped, like `auto_preamp_db`: with nothing
    // installed there is no partially-installed cascade to report.
    handle.send(EngineCommand::ClearCorrection);
    assert!(wait_until(WAIT, || {
        let s = handle.state();
        s.bands_dropped == 0 && s.auto_preamp_db.is_none()
    }));
}

/// The identity-substitution count reaches the wire too. It does NOT gate --
/// `preamp_db` and the realized response both substitute identically, so the
/// prediction already models it -- but a surprising residual needs a visible
/// first thing to look at.
#[test]
fn sections_substituted_reaches_the_published_snapshot() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));

    // `a2 = 2.0` puts a pole outside the unit circle, so `is_stable` rejects
    // the row and `build_iir` swaps in the identity section.
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Iir {
        design_rate: 48_000.0,
        sos_per_channel: vec![vec![[1.0, 0.0, 0.0, 1.0, 0.0, 2.0]]; 2],
    }));
    assert!(wait_until(WAIT, || handle.state().sections_substituted == 2));

    handle.send(EngineCommand::ClearCorrection);
    assert!(wait_until(WAIT, || handle.state().sections_substituted == 0));
}

/// `None` before the first start and after a teardown: there is no `RtShared`
/// to read, and a witness window must be able to tell "nothing is flowing"
/// from "I cannot see whether anything is flowing" and refuse on the second.
#[test]
fn tap_activity_is_none_before_start_and_after_teardown() {
    let backend = MockBackend::new();
    let probe = backend.clone();
    let handle = EngineHandle::spawn(
        backend,
        EngineConfig {
            enabled: false,
            ..fast_config()
        },
    );
    assert!(
        handle.tap_activity().is_none(),
        "a never-started engine has no realtime block to read"
    );

    handle.send(EngineCommand::Enable);
    assert!(wait_until(WAIT, || probe.is_running()));
    assert!(wait_until(WAIT, || handle.tap_activity().is_some()));

    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || !probe.is_running()));
    assert!(
        wait_until(WAIT, || handle.tap_activity().is_none()),
        "a teardown must retract the cell, not leave it naming a dead session"
    );
}

/// The falsifier for "do not put it on `EngineState`": reading the activity
/// must publish NOTHING. A monotonic per-block counter inside
/// `effectively_equal` would emit a snapshot -- and a Tauri event, and a React
/// re-render -- on every tick of every session.
#[test]
fn tap_activity_emits_no_snapshot_and_no_event() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));
    // Park in the same steady state `self_excluded_change_always_publishes`
    // uses: never pumping freezes the callback counter, so the watchdog sits
    // in `NoInputDetected` and every metered field stays at its start value.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    )));
    let snapshots = handle.subscribe();
    // `publish` stores the new snapshot (which is what `wait_until` just
    // observed) BEFORE it notifies subscribers, so a subscription taken between
    // those two steps receives the `NoInputDetected` transition itself and this
    // test would blame `tap_activity` for it. Let that in-flight notification
    // land and drain it, then measure from a quiet baseline.
    std::thread::sleep(Duration::from_millis(TICK_MS * 2));
    while snapshots.try_recv().is_ok() {}
    let before = handle.state();

    for _ in 0..100 {
        let _ = handle.tap_activity();
    }
    // Give the controller several ticks to publish anything these reads might
    // have provoked.
    std::thread::sleep(Duration::from_millis(TICK_MS * 5));

    assert!(
        snapshots.try_recv().is_err(),
        "reading tap activity published a snapshot"
    );
    assert!(
        std::sync::Arc::ptr_eq(&before, &handle.state()),
        "reading tap activity replaced the published snapshot"
    );
}

/// The falsifier a `None` test cannot provide. Every rebuild -- device change,
/// rate change, correction-triggered restart -- builds a FRESH `RtShared`, and
/// a handle still holding the previous one reports `Some` and never advances.
/// To a verification witness that reads as a dead tap, and it refuses every
/// run.
#[test]
fn tap_activity_follows_a_rebuild() {
    let (backend, handle) = spawn_engine();
    assert!(wait_until(WAIT, || backend.is_running()));
    backend.pump(512, 0.5);
    assert!(wait_until(WAIT, || handle
        .tap_activity()
        .is_some_and(|a| a.nonzero_blocks > 0)));

    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    assert!(wait_until(WAIT, || backend.is_running()));

    let after_rebuild = handle.tap_activity().expect("a live session publishes");
    for _ in 0..3 {
        backend.pump(512, 0.5);
    }
    let advanced = handle.tap_activity().expect("a live session publishes");
    assert!(
        advanced.callbacks > after_rebuild.callbacks
            && advanced.nonzero_blocks > after_rebuild.nonzero_blocks,
        "the handle is holding a previous session's RtShared: \
         {after_rebuild:?} -> {advanced:?}"
    );
}
