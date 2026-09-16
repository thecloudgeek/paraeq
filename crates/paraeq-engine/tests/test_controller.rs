//! Controller lifecycle tests against a scriptable MockBackend: engage
//! gating, command plumbing, correction swaps, rebuild-on-change,
//! renegotiation, and fail-safe stop. Real threads are involved, so every
//! assertion polls with a timeout (no bare sleeps as assertions); watchdog
//! windows are shortened via `EngineConfig` to keep the suite fast.

mod common;

use std::time::{Duration, Instant};

use common::mock_backend::{Call, MockBackend};
use paraeq_engine::backend::BackendEvent;
use paraeq_engine::controller::{CorrectionConfig, EngineCommand, EngineConfig, EngineHandle};
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
