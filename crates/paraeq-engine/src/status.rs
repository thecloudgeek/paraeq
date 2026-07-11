//! Silence watchdog: a pure state machine over the realtime telemetry
//! counters (spike obligations, doc lines 26-27).
//!
//! The watchdog holds NO clock -- the caller supplies `now_ms` -- so every
//! transition is unit-testable with synthetic timelines. It never hard-fails
//! on silence alone: `AudioHardwareCreateProcessTap` succeeds and delivers
//! silent zeros when the TCC permission is missing, and there is no query
//! API, so missing permission is *indistinguishable* from "no music
//! playing". [`EngineStatus::NoInputDetected`] is therefore informational
//! ("check the System Audio Recording permission or play some audio"), and
//! [`EngineStatus::Failed`] is only ever set by the controller (backend
//! errors), never in here.
//!
//! Every state this machine produces is informational -- NONE is a rebuild
//! trigger. Hardware finding 2026-07-11: the private tap aggregate's IOProc
//! only cycles while the system is actually rendering audio (0 cb/s idle,
//! ~94 cb/s during playback), so a frozen callback counter is normal idling
//! ([`EngineStatus::Idle`]), never an IOProc-death signal. Genuine device
//! death arrives as backend listener events, handled by the controller.

/// Engine lifecycle status, serialized into every state snapshot.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub enum EngineStatus {
    Stopped,
    Starting {
        since_ms: u64,
    },
    NoInputDetected {
        since_ms: u64,
    },
    Running,
    InputSilent {
        since_ms: u64,
    },
    /// The system is not rendering audio: the callback counter froze after
    /// callbacks had been seen. Normal when playback is paused (the tap
    /// aggregate's IOProc only cycles during playback -- hardware finding
    /// 2026-07-11). Informational; never a rebuild trigger.
    Idle {
        since_ms: u64,
    },
    Failed {
        reason: String,
    },
}

/// Watchdog windows (milliseconds).
#[derive(Clone, Copy, Debug)]
pub struct WatchdogConfig {
    /// How long `Starting` tolerates all-zero input (callbacks flowing or
    /// not) before hinting `NoInputDetected`. Spike: healthy first launches
    /// engage within ~4-5 s.
    pub engage_tolerance_ms: u64,
    /// How long the callback counter may freeze -- AFTER callbacks have been
    /// observed at least once since `started()` -- before `Idle`.
    pub idle_window_ms: u64,
    /// How long `Running` tolerates no new nonzero blocks before
    /// `InputSilent`.
    pub silence_window_ms: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            engage_tolerance_ms: 5000,
            idle_window_ms: 2000,
            silence_window_ms: 3000,
        }
    }
}

/// Pure watchdog state machine. Drive it with [`Watchdog::observe`] from
/// the controller tick; it reads the cumulative `callbacks` /
/// `nonzero_blocks` counters (fresh -- zeroed -- after every `started()`,
/// since the controller rebuilds `RtShared` on each start).
pub struct Watchdog {
    /// First `callbacks` increase observed since `started()`. Idle
    /// detection is gated on this: the spike measured up to ~4 s with NO
    /// callbacks at all before a healthy tap engages, so a frozen counter
    /// before the first callback is a slow engage, never `Idle`.
    callbacks_seen: bool,
    config: WatchdogConfig,
    last_callback_change_ms: u64,
    last_callbacks: u64,
    last_nonzero: u64,
    last_nonzero_change_ms: u64,
    status: EngineStatus,
}

impl Watchdog {
    pub fn new(config: WatchdogConfig) -> Watchdog {
        Watchdog {
            callbacks_seen: false,
            config,
            last_callback_change_ms: 0,
            last_callbacks: 0,
            last_nonzero: 0,
            last_nonzero_change_ms: 0,
            status: EngineStatus::Stopped,
        }
    }

    /// The backend (re)started: enter `Starting` and re-baseline against
    /// fresh (zeroed) counters. Re-arms the slow-engage grace: idle
    /// detection stays off until callbacks are observed again.
    pub fn started(&mut self, now_ms: u64) {
        self.callbacks_seen = false;
        self.last_callback_change_ms = now_ms;
        self.last_callbacks = 0;
        self.last_nonzero = 0;
        self.last_nonzero_change_ms = now_ms;
        self.status = EngineStatus::Starting { since_ms: now_ms };
    }

    /// The backend stopped (disable/teardown): enter `Stopped`.
    pub fn stopped(&mut self) {
        self.status = EngineStatus::Stopped;
    }

    /// Current status without feeding a new observation (for snapshot
    /// publication between observes).
    pub fn status(&self) -> &EngineStatus {
        &self.status
    }

    /// Feed one telemetry sample; returns the (possibly updated) status.
    ///
    /// `callbacks` / `nonzero_blocks` are the cumulative counters from
    /// `RtShared`. Transition priority: nonzero input always wins
    /// (-> `Running`), then the idle window, then engage-tolerance /
    /// silence windows.
    pub fn observe(&mut self, now_ms: u64, callbacks: u64, nonzero_blocks: u64) -> &EngineStatus {
        // Not running: nothing to watch (`Failed` is controller-owned).
        if matches!(
            self.status,
            EngineStatus::Stopped | EngineStatus::Failed { .. }
        ) {
            return &self.status;
        }

        let callbacks_advanced = callbacks > self.last_callbacks;
        if callbacks_advanced {
            self.callbacks_seen = true;
            self.last_callback_change_ms = now_ms;
            self.last_callbacks = callbacks;
        }

        // Nonzero input is the ground truth for "audio is flowing": gate
        // Running on it, never on start() having returned.
        if nonzero_blocks > self.last_nonzero {
            self.last_nonzero = nonzero_blocks;
            self.last_nonzero_change_ms = now_ms;
            self.status = EngineStatus::Running;
            return &self.status;
        }

        // Idle: callbacks were flowing and then froze for a full window --
        // the system stopped rendering audio. Normal when playback is
        // paused (hardware finding 2026-07-11: the tap aggregate's IOProc
        // only cycles during playback); informational, never a rebuild
        // trigger (device death arrives as backend events).
        if self.callbacks_seen
            && now_ms.saturating_sub(self.last_callback_change_ms) >= self.config.idle_window_ms
        {
            if !matches!(self.status, EngineStatus::Idle { .. }) {
                self.status = EngineStatus::Idle { since_ms: now_ms };
            }
            return &self.status;
        }

        match self.status {
            EngineStatus::Starting { since_ms }
                if now_ms.saturating_sub(since_ms) >= self.config.engage_tolerance_ms =>
            {
                // Informational only -- TCC denial and "no music playing"
                // look identical; never auto-fail on silence.
                self.status = EngineStatus::NoInputDetected { since_ms: now_ms };
            }
            EngineStatus::Running
                if now_ms.saturating_sub(self.last_nonzero_change_ms)
                    >= self.config.silence_window_ms =>
            {
                self.status = EngineStatus::InputSilent { since_ms: now_ms };
            }
            // Rendering resumed but with all-zero samples: IO cycles are
            // back, input is silent.
            EngineStatus::Idle { .. } if callbacks_advanced => {
                self.status = EngineStatus::InputSilent { since_ms: now_ms };
            }
            _ => {}
        }
        &self.status
    }
}
