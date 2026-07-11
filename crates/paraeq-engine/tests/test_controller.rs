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
const SILENCE_MS: u64 = 150;
const STALL_MS: u64 = 150;
const TICK_MS: u64 = 10;

/// Generous per-wait budget; each wait normally completes in tens of ms.
const WAIT: Duration = Duration::from_secs(5);

fn fast_config() -> EngineConfig {
    EngineConfig {
        requested_buffer_frames: None,
        ring_capacity: 4,
        tick_ms: TICK_MS,
        watchdog: WatchdogConfig {
            engage_tolerance_ms: ENGAGE_MS,
            silence_window_ms: SILENCE_MS,
            stall_window_ms: STALL_MS,
        },
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
fn iir_gain(g: f64, channels: usize) -> CorrectionConfig {
    CorrectionConfig::Iir {
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
    // are indistinguishable), never Running/Stalled/Failed.
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.0);
        let status = handle.state().status.clone();
        assert!(
            !matches!(
                status,
                EngineStatus::Running | EngineStatus::Stalled { .. } | EngineStatus::Failed { .. }
            ),
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

    // Correction doubles: 0.2 -> 0.4.
    handle.send(EngineCommand::SetCorrection(iir_gain(2.0, 2)));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.4)));

    // -6.0206 dB is 0.5 linear: corrected 0.4 * 0.5 = 0.2.
    handle.send(EngineCommand::SetGainDb(-6.020_6));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.2)));

    // Bypass skips correction only; gain still applies: 0.2 * 0.5 = 0.1.
    handle.send(EngineCommand::SetBypass(true));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.1)));

    // And back: corrected output returns.
    handle.send(EngineCommand::SetBypass(false));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.2)));
}

#[test]
fn set_correction_swaps_and_retires_off_thread() {
    let (backend, handle) = spawn_engine();

    // More successful swaps than the ring capacity (4): if the controller
    // did not drain retired corrections, the retire ring would fill and
    // later swaps would defer forever (a swap is gated on retire space).
    for i in 0..6 {
        let g = if i % 2 == 0 { 2.0 } else { 0.5 };
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

    // Retained config: correction x2, gain 0.5 -> 0.2 in, 0.2 out.
    handle.send(EngineCommand::SetCorrection(iir_gain(2.0, 2)));
    handle.send(EngineCommand::SetGainDb(-6.020_6));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.2)));
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
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.2)));
    assert_eq!(backend.start_count(), 2, "one rebuild, no rebuild loop");
}

#[test]
fn stall_triggers_rebuild_then_failed() {
    let (backend, handle) = spawn_engine();

    // Get callbacks flowing (arms stall detection), reach Running.
    assert!(wait_until(WAIT, || {
        backend.pump(512, 0.5);
        handle.state().status == EngineStatus::Running
    }));

    // Freeze the callback counter (stop pumping) with start failures
    // injected: stall -> rebuild attempt fails, retry fails -> Failed.
    backend.fail_next_starts(2);
    assert!(wait_until(Duration::from_secs(10), || matches!(
        handle.state().status,
        EngineStatus::Failed { .. }
    )));

    // Initial start + exactly 2 failed rebuild attempts...
    assert_eq!(backend.start_count(), 3);
    // ...and no rebuild-loop on a dead device afterward.
    assert!(!wait_until(Duration::from_millis(STALL_MS * 3), || {
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

    // Exactly one stop+start renegotiation cycle, request preserved.
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    assert_eq!(
        backend.calls()[..3],
        [
            Call::Start {
                requested_buffer_frames: Some(128)
            },
            Call::Stop,
            Call::Start {
                requested_buffer_frames: Some(128)
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
        firs: vec![vec![2.0]; 2],
    }));
    assert!(wait_until(WAIT, || pump_matches(&backend, 256, 0.2, 0.4)));
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
    assert!(wait_until(WAIT, || pump_matches(&backend, 256, 0.2, 0.4)));
}

#[test]
fn channel_count_renegotiation() {
    let backend = MockBackend::new();
    backend.set_reported_channels(1); // != provisional stereo chain
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    // One stop+start cycle down to the mono chain.
    assert!(wait_until(WAIT, || backend.start_count() == 2));

    handle.send(EngineCommand::SetCorrection(iir_gain(2.0, 1)));
    assert!(wait_until(WAIT, || {
        backend
            .pump(512, 0.2)
            .is_some_and(|out| out.len() == 1 && (out[0][0] - 0.4).abs() < 1e-5)
    }));
    assert_eq!(backend.start_count(), 2, "renegotiation must not loop");
    assert!(handle
        .state()
        .stream
        .as_ref()
        .is_some_and(|s| s.channels == 1));
}

#[test]
fn slow_first_callback_does_not_rebuild() {
    let (backend, handle) = spawn_engine();

    // No pumping at all: callbacks frozen at 0 well past stall_window. The
    // spike measured ~4 s with NO callbacks before a healthy tap engages,
    // so this must be treated as a slow engage, never a stall.
    assert!(wait_until(WAIT, || matches!(
        handle.state().status,
        EngineStatus::NoInputDetected { .. }
    )));
    assert!(!wait_until(Duration::from_millis(STALL_MS * 3), || {
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

    handle.send(EngineCommand::SetCorrection(iir_gain(2.0, 2)));
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.4)));

    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || {
        !backend.is_running() && handle.state().status == EngineStatus::Stopped
    }));
    assert!(backend.pump(512, 0.2).is_none(), "disabled == no processor");

    handle.send(EngineCommand::Enable);
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    // Stored correction re-applied to the fresh chain after Enable.
    assert!(wait_until(WAIT, || pump_matches(&backend, 512, 0.2, 0.4)));
}
