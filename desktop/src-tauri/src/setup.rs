//! First-launch setup wizard support: the deterministic chime probe that
//! verifies audio *capture* actually works, plus the pure verdict logic the
//! wizard renders from.
//!
//! WHY a chime probe at all: `AudioHardwareCreateProcessTap` succeeds and
//! delivers *silent zeros* when the System Audio Recording (TCC) grant is
//! missing -- there is no query API, so a missing grant is indistinguishable
//! from "no audio is playing" (status.rs:4-12). The probe removes the ambiguity
//! by GUARANTEEING the system is rendering audio: it spawns a SEPARATE child
//! process looping a builtin chime. A helper process is REQUIRED because
//! ParaEQ's own process is excluded from its own tap by design, and the child's
//! playback needs no TCC grant. With audio provably playing, `Running` proves
//! capture works; `NoInputDetected` persisting past [`PROBE_TIMEOUT`] (kept
//! deliberately BELOW the engine's 15 s fail-open so we verdict before it
//! auto-disables) means the tap is capturing silence -> the grant is missing.
//!
//! SAFETY: the probe must never orphan an `afplay` child looping forever, and
//! must never leave the system muted. [`ProbeState::stop`] kills the live child
//! and joins the loop thread; [`ProbeState::start`] first stops any prior probe
//! so only one loop is ever alive; and the loop caps itself at [`MAX_PROBE`]
//! even if `stop` is never called. Completing/cancelling the wizard both call
//! `stop`.

use paraeq_engine::status::EngineStatus;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The builtin system sound the probe loops. Short (~1.5 s) and always present
/// on macOS; playing it needs no permission.
const CHIME_PATH: &str = "/System/Library/Sounds/Glass.aiff";

/// Hard cap on the probe loop: even if `stop` is never called (a crash between
/// start and stop, say), the loop exits and the child stops. Comfortably longer
/// than the wizard's ~10 s verdict window so a legitimately slow engage still
/// hears the chime.
const MAX_PROBE: Duration = Duration::from_secs(12);

/// How long the wizard waits in `NoInputDetected` before verdicting `TimedOut`.
/// Deliberately BELOW the engine's 15 s fail-open (`AutoDisabledNoInput`) so the
/// wizard shows guidance while the engine is still trying, not after it gave up.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Poll interval while waiting on the current chime playback (keeps `stop`
/// responsive without a blocking `wait`, which would consume the `Child` out of
/// the shared slot and defeat kill-on-stop).
const POLL: Duration = Duration::from_millis(50);

/// Small gap between chime plays so the loop is an audible pulse, not a
/// continuous drone.
const GAP: Duration = Duration::from_millis(150);

/// The URL that opens System Settings -> Privacy & Security -> Screen & System
/// Audio Recording (the pane that hosts the "System Audio Recording" grant a
/// process tap needs on macOS 14/15). VERIFY at owner acceptance by running
/// `open "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"`;
/// if it lands on the wrong pane, fall back to the Privacy & Security root
/// (`x-apple.systempreferences:com.apple.preference.security`).
pub const PRIVACY_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// The wizard's three-way read of the probe, computed by [`probe_verdict`].
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeVerdict {
    /// Capture works: the engine reached `Running` with the chime playing.
    Running,
    /// Keep waiting: still starting, or `NoInputDetected` but under the timeout.
    StillProbing,
    /// The engine never captured a nonzero sample within the window (or already
    /// auto-disabled) -> the TCC grant is likely missing. Show guidance.
    TimedOut,
}

/// Pure verdict logic: given the live engine status, how long the probe has been
/// running, and the timeout, decide what the wizard should show. `Failed` maps
/// to `StillProbing` here because the wizard handles hard failures on their own
/// branch (surfacing `reason`); this function only answers the capture question.
pub fn probe_verdict(status: &EngineStatus, elapsed: Duration, timeout: Duration) -> ProbeVerdict {
    match status {
        EngineStatus::Running => ProbeVerdict::Running,
        EngineStatus::AutoDisabledNoInput { .. } => ProbeVerdict::TimedOut,
        EngineStatus::NoInputDetected { .. } if elapsed >= timeout => ProbeVerdict::TimedOut,
        _ => ProbeVerdict::StillProbing,
    }
}

/// Owns the chime-probe child process and its loop thread. Lives in `AppShared`.
/// All fields are `Arc`/`Mutex` so the loop thread and the `stop` caller share
/// them safely; the struct itself is `Send + Sync`.
pub struct ProbeState {
    /// Set to request the loop stop; checked between (and during) plays.
    stop_flag: Arc<AtomicBool>,
    /// The currently-playing chime child, so `stop` can kill it mid-play.
    child: Arc<Mutex<Option<Child>>>,
    /// The loop thread handle, joined by `stop`.
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl ProbeState {
    pub fn new() -> ProbeState {
        ProbeState {
            stop_flag: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            thread: Mutex::new(None),
        }
    }

    /// Start the chime loop. Idempotent by construction: it first `stop`s any
    /// prior probe (killing its child and joining its thread), so exactly one
    /// loop is ever alive.
    pub fn start(&self) {
        self.stop();
        self.stop_flag.store(false, Ordering::Relaxed);

        let stop_flag = Arc::clone(&self.stop_flag);
        let child_slot = Arc::clone(&self.child);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + MAX_PROBE;
            while !stop_flag.load(Ordering::Relaxed) && Instant::now() < deadline {
                match Command::new("afplay").arg(CHIME_PATH).spawn() {
                    Ok(child) => *child_slot.lock().unwrap() = Some(child),
                    // afplay missing / spawn failed: nothing to loop.
                    Err(_) => break,
                }
                // Wait for this play to finish, polling so `stop` stays snappy.
                loop {
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    let done = {
                        let mut guard = child_slot.lock().unwrap();
                        match guard.as_mut() {
                            // try_wait reaps a finished child (no zombie); an
                            // error means we can't track it -> stop waiting.
                            Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
                            None => true, // stop() took the child from under us
                        }
                    };
                    if done {
                        break;
                    }
                    thread::sleep(POLL);
                }
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                thread::sleep(GAP);
            }
            // Never leave a straggler child playing when the loop ends.
            kill_child(&child_slot);
        });
        *self.thread.lock().unwrap() = Some(handle);
    }

    /// Stop the probe: signal the loop, kill any live child, and join the
    /// thread. Safe to call when no probe is running (a no-op) and idempotent.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        kill_child(&self.child);
        if let Some(handle) = self.thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

impl Default for ProbeState {
    fn default() -> Self {
        ProbeState::new()
    }
}

/// Kill and reap the child in `slot`, if any, leaving the slot empty. Killing an
/// already-exited child is harmless; `wait` reaps it either way (no zombie).
fn kill_child(slot: &Mutex<Option<Child>>) {
    if let Some(mut child) = slot.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn running_is_success_regardless_of_elapsed() {
        assert_eq!(
            probe_verdict(&EngineStatus::Running, Duration::ZERO, TIMEOUT),
            ProbeVerdict::Running
        );
        assert_eq!(
            probe_verdict(&EngineStatus::Running, Duration::from_secs(30), TIMEOUT),
            ProbeVerdict::Running
        );
    }

    #[test]
    fn no_input_under_timeout_keeps_probing() {
        assert_eq!(
            probe_verdict(
                &EngineStatus::NoInputDetected { since_ms: 500 },
                Duration::from_secs(9),
                TIMEOUT
            ),
            ProbeVerdict::StillProbing
        );
    }

    #[test]
    fn no_input_at_or_past_timeout_times_out() {
        assert_eq!(
            probe_verdict(
                &EngineStatus::NoInputDetected { since_ms: 500 },
                TIMEOUT,
                TIMEOUT
            ),
            ProbeVerdict::TimedOut
        );
        assert_eq!(
            probe_verdict(
                &EngineStatus::NoInputDetected { since_ms: 500 },
                Duration::from_secs(11),
                TIMEOUT
            ),
            ProbeVerdict::TimedOut
        );
    }

    #[test]
    fn auto_disabled_times_out_immediately() {
        assert_eq!(
            probe_verdict(
                &EngineStatus::AutoDisabledNoInput { after_ms: 15_000 },
                Duration::ZERO,
                TIMEOUT
            ),
            ProbeVerdict::TimedOut
        );
    }

    #[test]
    fn starting_and_transient_states_keep_probing() {
        for status in [
            EngineStatus::Starting { since_ms: 0 },
            EngineStatus::Idle { since_ms: 0 },
            EngineStatus::InputSilent { since_ms: 0 },
            EngineStatus::Stopped,
            EngineStatus::Failed {
                reason: "boom".into(),
            },
        ] {
            assert_eq!(
                probe_verdict(&status, Duration::from_secs(30), TIMEOUT),
                ProbeVerdict::StillProbing,
                "{status:?} should keep probing (wizard owns the failed branch)"
            );
        }
    }

    #[test]
    fn verdict_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ProbeVerdict::TimedOut).unwrap(),
            "\"timed_out\""
        );
        assert_eq!(
            serde_json::to_string(&ProbeVerdict::StillProbing).unwrap(),
            "\"still_probing\""
        );
        assert_eq!(
            serde_json::to_string(&ProbeVerdict::Running).unwrap(),
            "\"running\""
        );
    }

    #[test]
    fn new_probe_has_no_child_and_stop_is_a_safe_noop() {
        let probe = ProbeState::new();
        assert!(probe.child.lock().unwrap().is_none());
        // stop() with nothing running must not panic and must set the flag.
        probe.stop();
        assert!(probe.stop_flag.load(Ordering::Relaxed));
        assert!(probe.child.lock().unwrap().is_none());
        // Idempotent: a second stop is still a clean no-op.
        probe.stop();
        assert!(probe.child.lock().unwrap().is_none());
    }

    /// Exercises the kill/reap bookkeeping on a REAL (non-audio) child so we
    /// verify the mechanism without spawning `afplay`. Uses `sleep` as a
    /// stand-in long-lived process.
    #[test]
    fn kill_child_terminates_and_clears_the_slot() {
        let slot: Mutex<Option<Child>> = Mutex::new(Some(
            Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn sleep"),
        ));
        kill_child(&slot);
        assert!(
            slot.lock().unwrap().is_none(),
            "kill_child must empty the slot"
        );
        // A second call on an empty slot is a no-op (does not panic).
        kill_child(&slot);
    }
}
