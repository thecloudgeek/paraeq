//! Watchdog state-machine tests: synthetic ms timelines, no sleeping, no
//! real clock (the watchdog is pure -- the caller supplies `now_ms`).

use paraeq_engine::status::{EngineStatus, Watchdog, WatchdogConfig};

fn watchdog() -> Watchdog {
    // Defaults: engage 5000 ms, silence 3000 ms, idle 2000 ms.
    Watchdog::new(WatchdogConfig::default())
}

#[test]
fn defaults_match_spec() {
    let c = WatchdogConfig::default();
    assert_eq!(c.engage_tolerance_ms, 5000);
    assert_eq!(c.idle_window_ms, 2000);
    assert_eq!(c.silence_window_ms, 3000);
}

#[test]
fn slow_engage_within_tolerance_reaches_running() {
    let mut w = watchdog();
    w.started(0);
    // Callbacks flow but input is all zeros for 4 s -- still Starting
    // (within the 5 s engage tolerance), then first nonzero -> Running.
    assert_eq!(
        *w.observe(1_000, 100, 0),
        EngineStatus::Starting { since_ms: 0 }
    );
    assert_eq!(
        *w.observe(3_900, 390, 0),
        EngineStatus::Starting { since_ms: 0 }
    );
    assert_eq!(*w.observe(4_000, 400, 1), EngineStatus::Running);
}

#[test]
fn zeros_past_engage_tolerance_hint_no_input() {
    let mut w = watchdog();
    w.started(0);
    assert_eq!(
        *w.observe(4_999, 499, 0),
        EngineStatus::Starting { since_ms: 0 }
    );
    // TCC failure is indistinguishable from silence: informational hint,
    // never a hard failure.
    assert_eq!(
        *w.observe(5_000, 500, 0),
        EngineStatus::NoInputDetected { since_ms: 5_000 }
    );
    // Stays informational.
    assert_eq!(
        *w.observe(60_000, 6_000, 0),
        EngineStatus::NoInputDetected { since_ms: 5_000 }
    );
}

#[test]
fn no_input_recovers_to_running_on_nonzero() {
    let mut w = watchdog();
    w.started(0);
    w.observe(5_000, 500, 0);
    assert!(matches!(w.observe(5_250, 525, 3), EngineStatus::Running));
}

#[test]
fn running_to_silent_and_back() {
    let mut w = watchdog();
    w.started(0);
    assert_eq!(*w.observe(250, 25, 10), EngineStatus::Running);
    // Nonzero counter freezes (music stopped); callbacks keep flowing.
    assert_eq!(*w.observe(2_000, 200, 10), EngineStatus::Running);
    assert_eq!(
        *w.observe(3_250, 325, 10),
        EngineStatus::InputSilent { since_ms: 3_250 }
    );
    assert_eq!(
        *w.observe(10_000, 1_000, 10),
        EngineStatus::InputSilent { since_ms: 3_250 }
    );
    // Music resumes.
    assert_eq!(*w.observe(10_250, 1_025, 11), EngineStatus::Running);
}

#[test]
fn frozen_callbacks_before_first_callback_is_slow_engage_not_idle() {
    let mut w = watchdog();
    w.started(0);
    // The spike measured up to ~4 s with NO callbacks at all on a healthy
    // first launch: NEVER Idle before the first observed callback.
    assert_eq!(
        *w.observe(2_500, 0, 0),
        EngineStatus::Starting { since_ms: 0 }
    );
    assert_eq!(
        *w.observe(4_000, 0, 0),
        EngineStatus::Starting { since_ms: 0 }
    );
}

#[test]
fn frozen_callbacks_past_engage_tolerance_is_no_input_not_idle() {
    let mut w = watchdog();
    w.started(0);
    w.observe(2_500, 0, 0);
    assert_eq!(
        *w.observe(5_000, 0, 0),
        EngineStatus::NoInputDetected { since_ms: 5_000 }
    );
    // Still no callbacks, way past every window: still informational only.
    assert_eq!(
        *w.observe(20_000, 0, 0),
        EngineStatus::NoInputDetected { since_ms: 5_000 }
    );
}

#[test]
fn callbacks_flowing_then_frozen_is_idle() {
    let mut w = watchdog();
    w.started(0);
    assert_eq!(*w.observe(250, 25, 5), EngineStatus::Running);
    assert_eq!(*w.observe(500, 50, 10), EngineStatus::Running);
    // Playback pauses: the tap aggregate's IOProc stops cycling entirely
    // (hardware finding 2026-07-11) -- callbacks freeze at 50. Normal
    // idling, informational only.
    assert_eq!(*w.observe(1_500, 50, 10), EngineStatus::Running);
    assert_eq!(
        *w.observe(2_500, 50, 10),
        EngineStatus::Idle { since_ms: 2_500 }
    );
    // Stays informational, indefinitely.
    assert_eq!(
        *w.observe(60_000, 50, 10),
        EngineStatus::Idle { since_ms: 2_500 }
    );
}

#[test]
fn idle_recovers_to_running_on_nonzero() {
    let mut w = watchdog();
    w.started(0);
    w.observe(250, 25, 5); // Running
    w.observe(2_500, 25, 5); // Idle (callbacks frozen)

    // Playback resumes: callbacks and nonzero input flow again.
    assert_eq!(*w.observe(9_000, 30, 8), EngineStatus::Running);
}

#[test]
fn idle_with_resumed_zero_callbacks_is_input_silent() {
    let mut w = watchdog();
    w.started(0);
    w.observe(250, 25, 5); // Running
    assert_eq!(
        *w.observe(2_500, 25, 5),
        EngineStatus::Idle { since_ms: 2_500 }
    );
    // IO cycles resume but samples are all zero: the system is rendering
    // silence -- InputSilent, not Idle.
    assert_eq!(
        *w.observe(9_000, 30, 5),
        EngineStatus::InputSilent { since_ms: 9_000 }
    );
    // Then real audio -> Running.
    assert_eq!(*w.observe(9_250, 35, 6), EngineStatus::Running);
}

#[test]
fn idle_detection_also_gates_from_starting_once_callbacks_were_seen() {
    let mut w = watchdog();
    w.started(0);
    // Callbacks seen (zeros only), then rendering stops while Starting.
    assert_eq!(
        *w.observe(250, 25, 0),
        EngineStatus::Starting { since_ms: 0 }
    );
    assert_eq!(
        *w.observe(2_250, 25, 0),
        EngineStatus::Idle { since_ms: 2_250 }
    );
}

#[test]
fn stopped_from_anywhere() {
    // From Starting.
    let mut w = watchdog();
    w.started(0);
    w.stopped();
    assert_eq!(*w.observe(1_000, 100, 100), EngineStatus::Stopped);

    // From Running.
    let mut w = watchdog();
    w.started(0);
    w.observe(250, 25, 5);
    w.stopped();
    assert_eq!(*w.observe(1_000, 100, 100), EngineStatus::Stopped);

    // From Idle.
    let mut w = watchdog();
    w.started(0);
    w.observe(250, 25, 5);
    w.observe(2_500, 25, 5);
    w.stopped();
    assert_eq!(*w.observe(10_000, 25, 5), EngineStatus::Stopped);
}

#[test]
fn restart_after_stop_regains_slow_engage_grace() {
    let mut w = watchdog();
    w.started(0);
    w.observe(250, 25, 5); // Running, callbacks seen
    w.stopped();
    // Restart with fresh (zeroed) counters: the slow-engage grace applies
    // again -- frozen callbacks are NOT Idle before the first callback.
    w.started(10_000);
    assert_eq!(
        *w.observe(14_000, 0, 0),
        EngineStatus::Starting { since_ms: 10_000 }
    );
}

#[test]
fn status_serializes_to_json() {
    let json = serde_json::to_string(&EngineStatus::NoInputDetected { since_ms: 5000 }).unwrap();
    assert!(json.contains("NoInputDetected"), "got: {json}");
    assert!(json.contains("5000"), "got: {json}");
    let json = serde_json::to_string(&EngineStatus::Running).unwrap();
    assert!(json.contains("Running"), "got: {json}");
}
