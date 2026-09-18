//! `MeasurementSession` (MS-6, MS-14, MS-18, MS-23, and the session half of
//! MS-5/MS-11): the sequencing state machine around the Stage-5 solve.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**, per the four-tier
//! convention and the `spline.rs:1-5` precedent. Sequencing, refusal timing
//! and teardown order are product policy; `tests/test_session.rs` pins them
//! mock-driven — no hardware, no `unsafe`, per MS-1.
//!
//! # What this module is, and is not
//!
//! The session enforces the spec's **sequencing guarantees**: nothing is
//! emitted without self-exclusion (MS-6), the sweep is unreachable without a
//! recorded acknowledgement naming device and projected SPL (MS-18), an
//! abort ramps — never hard-stops — and then restores in the tap-teardown
//! order (MS-14), the pre-measurement volume is restored on **every** exit
//! path including panic (MS-5's RAII half, the `TapSystem` shape: a
//! `torn_down` guard, failures collected not masked, a `Drop` impl), and the
//! whole run leaves a structured, ordered log (MS-23).
//!
//! The **level ladder solve** — noise floor → pilot → solve → envelope →
//! ≤6 dB rungs with per-rung SPL re-verification (MS-7/MS-8/MS-17) — is
//! **Stage 5** and is deliberately absent: it enters through the
//! [`SolveOutcome`] boundary type below, and its rungs are *recorded* here
//! ([`MeasurementSession::record_rung`]), never computed.
//!
//! # State machine
//!
//! A checked enum ([`SessionPhase`]), one direction only:
//!
//! ```text
//! begin ──▶ Preflight ──install_solve──▶ Solved ──acknowledge──▶ Acknowledged
//!               │                          │                        │
//!               │                          │ (record_rung loops)    │ sweep
//!               ▼                          ▼                        ▼
//!           Terminated ◀───────── refusal / abort / drop ◀──────── Swept
//! ```
//!
//! Every transition method checks the phase and refuses out-of-order calls
//! with [`SessionError::WrongPhase`]; every terminal path funnels through one
//! private `teardown` that runs the § Abort Guards restore sequence in order.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::cal::{margined_emit_dbfs, CalSummary, PinnedGain};
use crate::diagnostic::{MeasurementDiagnostic, Refusal};
use crate::level::{caps_for, LevelError, SweepLevel};
use crate::seam::{StimulusSink, TapStatus, VolumeControl};
use crate::stimulus::{emit_guard, AssembledStimulus, GuardCounts, StimulusKind};
use crate::MeasureError;
use paraeq_dsp::targets::TransducerClass;

/// The abort ramp duration (§ Abort Guards): ~5 ms of block time, a raised
/// cosine applied to the sink's output buffer — never a re-render of the
/// stimulus, and never a hard stop, which would itself be the full-scale
/// click § Fade exists to prevent.
pub const ABORT_RAMP_MS: f64 = 5.0;

/// How far an acknowledged SPL may sit from the solved projection and still
/// be an acknowledgement *of this run*: half a display-rounding step (the UI
/// shows 0.1 dB). Beyond it the ack is stale — it names a number that is not
/// this run's projection — and is rejected.
pub const ACK_SPL_TOLERANCE_DB: f64 = 0.05;

/// Poison-tolerant lock: the abort flag and mocks are read during unwinding,
/// and a teardown that panics on a poisoned mutex would abort the process
/// mid-restore.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why a run stopped — the § Abort Guards trigger table, minus panic (which
/// arrives through unwinding, not through a flag), plus [`Self::EngineStopped`]
/// for the engine leaving `Running` by request (the spec's table has only the
/// `Failed` row; the wizard's fail-open watchdog section supplies the other
/// way a live session loses its engine).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbortReason {
    /// The engine entered `Failed` (`EngineStatus::Failed`, controller-owned).
    EngineFailed,
    /// The engine left `Running` *by request* — a user `Disable`, or the
    /// fail-open watchdog auto-disabling after `NoInputDetected` (wizard
    /// § The fail-open watchdog: the tap is torn down mid-session). The run
    /// cannot continue without the engine, but nothing failed, so this
    /// publishes [`MeasurementDiagnostic::EngineNotRunning`] and **not**
    /// [`MeasurementDiagnostic::EngineFailed`] — a "restart ParaEQ" remedy
    /// for a switch the user flipped is a wrong answer, not a vaguer one.
    EngineStopped,
    /// >30% of samples in an input block clipped (capture metering).
    InputClipping,
    /// The measurement mic disconnected (`paraeq-coreaudio` listener).
    MicDisconnected,
    /// The output device changed or died (backend listener events).
    OutputDeviceChanged,
    /// Measured SPL exceeded the cap mid-sweep (capture metering).
    SplOverCap,
    /// Esc / Space, or window close (UI).
    UserRequest,
}

impl AbortReason {
    /// The terminating [`MeasurementDiagnostic`] this trigger publishes.
    pub fn diagnostic(self) -> MeasurementDiagnostic {
        match self {
            Self::EngineFailed => MeasurementDiagnostic::EngineFailed,
            Self::EngineStopped => MeasurementDiagnostic::EngineNotRunning,
            Self::InputClipping => MeasurementDiagnostic::InputClipping,
            Self::MicDisconnected => MeasurementDiagnostic::MicDisconnected,
            Self::OutputDeviceChanged => MeasurementDiagnostic::OutputDeviceChanged,
            Self::SplOverCap => MeasurementDiagnostic::SplOverCap,
            Self::UserRequest => MeasurementDiagnostic::UserAborted,
        }
    }
}

/// A cloneable trigger for the § Abort Guards rows: the UI, the capture
/// metering and the device listeners each hold a clone; the session polls it
/// between blocks. First trigger wins — a second cannot re-label the abort.
#[derive(Clone, Debug, Default)]
pub struct AbortHandle {
    reason: Arc<Mutex<Option<AbortReason>>>,
}

impl AbortHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm the abort. Idempotent; the first reason is the one published.
    pub fn trigger(&self, reason: AbortReason) {
        let mut slot = lock(&self.reason);
        if slot.is_none() {
            *slot = Some(reason);
        }
    }

    /// The armed reason, if any.
    pub fn triggered(&self) -> Option<AbortReason> {
        *lock(&self.reason)
    }
}

/// Where in the one-way state machine a session is. The declaration order is
/// the machine's order; [`SessionPhase::Terminated`] is absorbing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPhase {
    /// Cal validated, gain pinned, volume read: waiting on the Stage-5 solve.
    Preflight,
    /// A [`SolveOutcome`] validated and a level decided; rungs may record.
    Solved,
    /// The MS-18 acknowledgement is on record; the sweep gate is open once.
    Acknowledged,
    /// The sweep completed; capture/analysis (later stages) proceed outside.
    Swept,
    /// Torn down: stimulus stopped, volume restored. Absorbing.
    Terminated,
}

/// One typed entry of the MS-23 session log, in the order events happened.
///
/// Machine-parseable **without serde, by shape**: every field is a scalar, a
/// `String`, a [`TransducerClass`], or a [`MeasurementDiagnostic`] (itself
/// wire-numbered via `code()`), so each variant maps 1:1 onto a tagged
/// record; when a serde consumer arrives it derives with no re-modelling.
/// Wire-format stability is the diagnostic numbering's job, not this enum's —
/// the log is per-run data, not a cross-version protocol.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    /// The MS-18 acknowledgement, verbatim as acknowledged.
    Acknowledged {
        device_name: String,
        projected_spl_db: f64,
    },
    /// Cal identity and the MS-11 gain pin (reference vs read-back), plus the
    /// class and its hard cap so the log is self-describing.
    CalLoaded {
        class: TransducerClass,
        file_identity: String,
        gain_matches: bool,
        input_gain_read_back: f64,
        reference_input_gain: f64,
        sens_factor_dbfs: f64,
        spl_cap_db: f64,
    },
    /// One rung of the Stage-5 climb — one event per rung by construction:
    /// the session assigns `index` monotonically at the sole recording call.
    RungMeasured { index: u32, measured_spl_db: f64 },
    /// A seam failure (emit or stop) that the teardown collected rather than
    /// masked — the `TapSystem` posture.
    SinkFault { error: String },
    /// The session's level decision at the solve boundary (MS-11): what the
    /// ladder solved, what margin applied, what will be emitted.
    SolveInstalled {
        chain_sensitivity_spl_per_dbfs: f64,
        emitted_dbfs_rms: f64,
        margin_db: f64,
        projected_spl_db: f64,
        solved_dbfs_rms: f64,
    },
    /// The sweep ran to its last sample.
    SweepCompleted,
    /// The sweep gate opened and emission began at this level.
    SweepStarted { level_dbfs_rms: f64 },
    /// The terminal event, always last: the terminating diagnostic, or `None`
    /// for a clean finish.
    Terminated {
        diagnostic: Option<MeasurementDiagnostic>,
    },
    /// The verification pass's level decision — MS-23's "final level, cap,
    /// class" for a pass that runs no rungs and no solve, so
    /// [`Self::SolveInstalled`] does not apply and [`Self::RungMeasured`]
    /// never appears.
    ///
    /// `l_verify_dbfs_rms` is SWEEP-SPAN RMS, the same convention
    /// [`Self::SweepStarted`]'s `level_dbfs_rms` already uses — the bracketed
    /// file's own RMS is lower and is never reported anywhere.
    ///
    /// No margin field: the MS-11 cal-error margin is applied exactly once,
    /// upstream, and [`Self::CalLoaded`]'s `gain_matches` already records
    /// which case fired while [`Self::SolveInstalled`]'s `margin_db` records
    /// its size on the baseline side.
    VerifyLevelled {
        class: TransducerClass,
        gain_db_pinned: f32,
        l_measure_dbfs_rms: f64,
        l_verify_dbfs_rms: f64,
        peak_correction_gain_db: f64,
        spl_cap_db: f64,
    },
    /// The pre-measurement volume READ at `begin`. A snapshot, not a change:
    /// nothing in the shipped product writes the output volume, so this
    /// records what the system was at rather than what ParaEQ set it to, and
    /// teardown restores it only if a later stage actually wrote it
    /// ([`volume_restore_target`]). `None`: unreadable.
    VolumePinned { pre_measurement_scalar: Option<f64> },
    /// Restoring the volume failed; collected, surfaced, never masked.
    VolumeRestoreFailed { error: String },
    /// A non-blocking diagnostic surfaced mid-run (MS-4 guard counts, etc.).
    Warning { diagnostic: MeasurementDiagnostic },
}

/// The MS-23 log: an append-only, ordered record of one session. Obtainable
/// by reference at any time ([`MeasurementSession::log`]) and by value from
/// [`MeasurementSession::finish`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionLog {
    events: Vec<SessionEvent>,
}

impl SessionLog {
    /// Every event, oldest first.
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// `pub(crate)` so the verification pass ([`crate::verify`]) writes into
    /// the same MS-23 log rather than minting a second one. Without it a
    /// sibling module cannot reach `events` — which is private — and an
    /// implementer invents a parallel log type, which is exactly the
    /// duplication MS-23 exists to prevent.
    pub(crate) fn push(&mut self, event: SessionEvent) {
        self.events.push(event);
    }
}

// ────────────────────────── STAGE-5 SOLVE BOUNDARY ──────────────────────────
// Everything the level ladder computes crosses into the session through this
// one type. The ladder itself — noise floor → pilot (300 Hz, −40 dBFS RMS)
// → solve → envelope check → ≤6 dB rungs with per-rung SPL re-verification
// (MS-7/MS-8/MS-17) — is Stage 5 and does NOT live in this crate yet. The
// session VALIDATES and SEQUENCES a solve; it never performs one. Fields are
// public and this type is plain data precisely because it is the boundary:
// Stage 5 fills it, the session judges it (cap check, MS-11 margin, the sole
// SweepLevel constructor) before anything can be emitted at it.

/// What the Stage-5 level ladder hands the session (spec § The Level Ladder,
/// steps 3–4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolveOutcome {
    /// Chain sensitivity `S = SPL_measured − L_pilot`, dB SPL per dBFS RMS.
    pub chain_sensitivity_spl_per_dbfs: f64,
    /// The SPL the solved level projects at the mic — the number the user
    /// acknowledges (MS-18) and the caps are checked against.
    pub projected_spl_db: f64,
    /// The solved output level `L = SPL_target − S`, dBFS RMS, pre-margin.
    pub solved_dbfs_rms: f64,
}

// ────────────────────────────────────────────────────────────────────────────

/// The three platform seams a session drives, bundled so
/// [`MeasurementSession::begin`] cannot be handed a partial set. Mocks in
/// tests; `paraeq-coreaudio` impls in Stage 4/5.
pub struct SessionSeam {
    pub sink: Box<dyn StimulusSink>,
    pub tap: Box<dyn TapStatus>,
    pub volume: Box<dyn VolumeControl>,
}

/// Everything that can go wrong at the session's gates. Refusals travel as
/// [`SessionError::Refused`] and have already terminated the session
/// (restore sequence run, terminating diagnostic logged) by the time the
/// caller sees them.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("acknowledgement names no device")]
    AckNamesNoDevice,
    #[error(
        "acknowledged SPL {acknowledged_spl_db} dB is not this run's projection \
         {projected_spl_db} dB — a stale acknowledgement authorizes nothing"
    )]
    AckSplStale {
        acknowledged_spl_db: f64,
        projected_spl_db: f64,
    },
    #[error("the solve produced an illegal level: {0}")]
    Level(#[from] LevelError),
    #[error("refused: {0:?}")]
    Refused(Refusal),
    #[error("seam failure: {0}")]
    Seam(#[from] MeasureError),
    #[error(
        "stimulus is scaled to {stimulus_dbfs_rms} dBFS RMS but the session's \
         decided level is {emit_dbfs_rms} dBFS RMS"
    )]
    StimulusLevelMismatch {
        emit_dbfs_rms: f64,
        stimulus_dbfs_rms: f64,
    },
    #[error("sweep() was handed a {kind:?} stimulus, not a Sweep")]
    StimulusNotSweep { kind: StimulusKind },
    #[error("{method}() requires phase {required}; session is {actual:?}")]
    WrongPhase {
        actual: SessionPhase,
        method: &'static str,
        required: &'static str,
    },
}

/// How a sweep ended. An abort is an *outcome*, not an error: the ramp and
/// the restore sequence already ran, and the diagnostic says why.
#[derive(Clone, Debug, PartialEq)]
pub enum SweepOutcome {
    Aborted {
        diagnostic: MeasurementDiagnostic,
        warnings: Vec<MeasurementDiagnostic>,
    },
    Completed {
        warnings: Vec<MeasurementDiagnostic>,
    },
}

/// The measurement session runtime. See the module header for the machine;
/// see `tests/test_session.rs` for the pinned guarantees.
pub struct MeasurementSession {
    abort: AbortHandle,
    cal: CalSummary,
    emit_level: Option<SweepLevel>,
    log: SessionLog,
    next_rung: u32,
    phase: SessionPhase,
    pin: PinnedGain,
    pre_volume: Option<f64>,
    projected_spl_db: Option<f64>,
    sink: Box<dyn StimulusSink>,
    tap: Box<dyn TapStatus>,
    torn_down: bool,
    volume: Box<dyn VolumeControl>,
    /// Did THIS session ever write the output volume? R-B2: teardown restores
    /// only what it actually changed. Nothing in the shipped product sets the
    /// volume — MS-5's case 1 is unbuilt — so this is `false` for the whole of
    /// a run today, and the flag is what makes that fact explicit rather than
    /// implicit in an unconditional `set_volume`.
    volume_written: bool,
}

impl MeasurementSession {
    /// Open a session on a validated cal (MS-9/MS-10 already refused at
    /// [`CalSummary::validate`]).
    ///
    /// Order is load-bearing:
    ///
    /// 1. **MS-6 first** — no self-exclusion, no session: refuse
    ///    `SelfExclusionUnavailable` before the sink or the volume is so much
    ///    as touched, so the refusal provably precedes any sample.
    /// 2. Pin the input gain (MS-11): the read-back is compared against the
    ///    cal's reference and both go in the log verbatim.
    /// 3. Read the pre-measurement volume — the value every exit path
    ///    restores. Unreadable is recorded as `None` (nothing will be
    ///    changed, so nothing needs restoring; the volume-policy rows of the
    ///    spec are the caller's to arbitrate with `FixedMaxVolume` /
    ///    `VolumeUncontrollable` at a later stage).
    pub fn begin(
        cal: CalSummary,
        input_gain_read_back: f64,
        seam: SessionSeam,
    ) -> Result<Self, Refusal> {
        let SessionSeam { sink, tap, volume } = seam;
        // MS-6, before anything else: with no self-exclusion there is no
        // validated stimulus topology, and no session to leave side effects.
        if !tap.self_excluded() {
            return Err(Refusal::new(
                MeasurementDiagnostic::SelfExclusionUnavailable,
            ));
        }
        let pin = cal.pin_gain(input_gain_read_back);
        let pre_volume = volume.volume().ok();
        let mut log = SessionLog::default();
        log.push(SessionEvent::CalLoaded {
            class: cal.class(),
            file_identity: cal.file_identity().to_owned(),
            gain_matches: pin.matches(),
            input_gain_read_back,
            reference_input_gain: cal.reference_input_gain(),
            sens_factor_dbfs: cal.sensitivity().sens_factor_dbfs(),
            spl_cap_db: caps_for(cal.class()).spl_refuse_db,
        });
        log.push(SessionEvent::VolumePinned {
            pre_measurement_scalar: pre_volume,
        });
        Ok(Self {
            abort: AbortHandle::new(),
            cal,
            emit_level: None,
            log,
            next_rung: 0,
            phase: SessionPhase::Preflight,
            pin,
            pre_volume,
            projected_spl_db: None,
            sink,
            tap,
            torn_down: false,
            volume,
            volume_written: false,
        })
    }

    /// Install the Stage-5 solve (the boundary type's docs above). Validates
    /// the projection against the class cap — the spec's cheapest possible
    /// refusal, `ProjectedSplOverCap`, before a single sample — then applies
    /// the MS-11 margin and constructs the level through `SweepLevel::new`,
    /// the sole constructor. Returns the decided level for stimulus assembly.
    pub fn install_solve(&mut self, solve: SolveOutcome) -> Result<SweepLevel, SessionError> {
        self.require(SessionPhase::Preflight, "install_solve")?;
        // The spec's cheapest possible refusal. Written as a positive
        // requirement so a NaN projection — which passes no comparison —
        // lands in the refusal arm, never under an unwitnessed cap.
        let cap_db = caps_for(self.cal.class()).spl_refuse_db;
        let under_cap = solve.projected_spl_db.is_finite() && solve.projected_spl_db <= cap_db;
        if !under_cap {
            return Err(self.refuse(MeasurementDiagnostic::ProjectedSplOverCap));
        }
        // MS-11: the margin decides the number; the sole constructor decides
        // whether it is a level at all. A projection can clear the SPL cap yet
        // still demand an illegal *level* (over the class dBFS cap or −3 dBFS
        // after the margin) — that must terminate through a refusal, not
        // `?`-propagate a bare `Level` error and leave the log without a
        // terminating diagnostic (MS-23).
        let emitted_dbfs_rms = margined_emit_dbfs(solve.solved_dbfs_rms, self.pin);
        let level = match SweepLevel::new(emitted_dbfs_rms, self.cal.class()) {
            Ok(level) => level,
            Err(_) => return Err(self.refuse(MeasurementDiagnostic::SolvedLevelIllegal)),
        };
        self.log.push(SessionEvent::SolveInstalled {
            chain_sensitivity_spl_per_dbfs: solve.chain_sensitivity_spl_per_dbfs,
            emitted_dbfs_rms,
            margin_db: solve.solved_dbfs_rms - emitted_dbfs_rms,
            projected_spl_db: solve.projected_spl_db,
            solved_dbfs_rms: solve.solved_dbfs_rms,
        });
        self.emit_level = Some(level);
        self.projected_spl_db = Some(solve.projected_spl_db);
        self.phase = SessionPhase::Solved;
        Ok(level)
    }

    /// Record one rung of the Stage-5 climb (MS-23): one event per rung by
    /// construction — the session assigns the index monotonically and this is
    /// the only way a rung enters the log. Returns the index assigned.
    pub fn record_rung(&mut self, measured_spl_db: f64) -> Result<u32, SessionError> {
        // Rungs happen during the level check: after a solve exists, before
        // or after the ack (the spec sequences the ack at step 6; where the
        // climb sits relative to it is Stage 5's call), never after
        // termination.
        if !matches!(
            self.phase,
            SessionPhase::Solved | SessionPhase::Acknowledged
        ) {
            return Err(SessionError::WrongPhase {
                actual: self.phase,
                method: "record_rung",
                required: "Solved or Acknowledged",
            });
        }
        let index = self.next_rung;
        self.next_rung += 1;
        self.log.push(SessionEvent::RungMeasured {
            index,
            measured_spl_db,
        });
        Ok(index)
    }

    /// Record the MS-18 acknowledgement: an explicit, deliberate call naming
    /// the output device and the projected SPL the user saw. The only door to
    /// [`SessionPhase::Acknowledged`], and therefore to the sweep. The UI
    /// must present it as a deliberate action, never a default-focused
    /// button (spec § headphones-not-on-head).
    pub fn acknowledge(
        &mut self,
        device_name: &str,
        projected_spl_db: f64,
    ) -> Result<(), SessionError> {
        self.require(SessionPhase::Solved, "acknowledge")?;
        if device_name.trim().is_empty() {
            return Err(SessionError::AckNamesNoDevice);
        }
        let solved_projection = self
            .projected_spl_db
            .expect("Solved phase implies a projection");
        // Positive requirement (NaN is stale): the acknowledged number must
        // be *this run's* projection, or the ack authorizes nothing.
        let fresh = (projected_spl_db - solved_projection).abs() <= ACK_SPL_TOLERANCE_DB;
        if !fresh {
            return Err(SessionError::AckSplStale {
                acknowledged_spl_db: projected_spl_db,
                projected_spl_db: solved_projection,
            });
        }
        self.log.push(SessionEvent::Acknowledged {
            device_name: device_name.to_owned(),
            projected_spl_db,
        });
        self.phase = SessionPhase::Acknowledged;
        Ok(())
    }

    /// Play the sweep through the sink, block by block, polling the abort
    /// handle before every block (MS-14: the ramp starts on the first block
    /// after the trigger). Requires [`SessionPhase::Acknowledged`], re-checks
    /// MS-6 at the gate, and verifies the stimulus's level provenance matches
    /// the session's decided level.
    pub fn sweep(&mut self, stimulus: &AssembledStimulus) -> Result<SweepOutcome, SessionError> {
        self.require(SessionPhase::Acknowledged, "sweep")?;
        // MS-6, re-checked at the last gate before samples flow: exclusion
        // can vanish mid-session (a device change forces a tap rebuild).
        if !self.tap.self_excluded() {
            return Err(self.refuse(MeasurementDiagnostic::SelfExclusionUnavailable));
        }
        // Provenance, part 1 — kind. The sweep gate takes a sweep of the
        // Direct path: `Sweep`, or `BracketedSweep` (the same sweep with
        // timing markers spliced around it). It still refuses `Pilot` — the
        // reason this gate exists, since the pilot's fixed −40 dBFS could
        // coincide with a session's margined level and let a pilot play under
        // a `SweepStarted`/`SweepCompleted` log — and it also refuses
        // `VerificationSweep`, which plays from the HELPER child process and
        // must never reach an in-process sink.
        //
        // The provenance property survives the widening rather than being
        // traded away: `BracketedSweep` is strictly HARDER to forge than
        // `Sweep`, because `assemble_bracketed` consumes a verified
        // `AssembledStimulus` by value and adds three whole-buffer checks on
        // top of `verify_stimulus`. Part 2 below is untouched, and a bracketed
        // buffer's `level()` is still exact.
        if !matches!(
            stimulus.kind(),
            StimulusKind::Sweep | StimulusKind::BracketedSweep
        ) {
            return Err(SessionError::StimulusNotSweep {
                kind: stimulus.kind(),
            });
        }
        let level = self.emit_level.expect("Acknowledged phase implies a level");
        // Provenance, part 2 — level: the buffer must be scaled to the very
        // level this session decided — a stimulus assembled at any other level
        // is not this run's stimulus.
        if stimulus.level() != level {
            return Err(SessionError::StimulusLevelMismatch {
                emit_dbfs_rms: level.dbfs_rms(),
                stimulus_dbfs_rms: stimulus.level().dbfs_rms(),
            });
        }
        self.log.push(SessionEvent::SweepStarted {
            level_dbfs_rms: level.dbfs_rms(),
        });

        let format = self.sink.format();
        let block = format.frames_per_block.max(1);
        let samples = stimulus.samples();
        let mut counts = GuardCounts::default();
        let mut offset = 0;
        while offset < samples.len() {
            // Poll before every block: the ramp starts on the first block
            // after the trigger (MS-14).
            if let Some(reason) = self.abort.triggered() {
                let diagnostic = reason.diagnostic();
                self.ramp_down(&samples[offset..], block, format.sample_rate_hz);
                let warnings = counts.warnings();
                for warning in &warnings {
                    self.log.push(SessionEvent::Warning {
                        diagnostic: *warning,
                    });
                }
                self.teardown(Some(diagnostic));
                return Ok(SweepOutcome::Aborted {
                    diagnostic,
                    warnings,
                });
            }
            let end = (offset + block).min(samples.len());
            let mut chunk = samples[offset..end].to_vec();
            // The MS-4 emit guard, per block — the same last-stage guard
            // `AssembledStimulus::emit_to` runs; a verified stimulus never
            // trips it, and a nonzero count surfaces as a warning.
            let chunk_counts = emit_guard(&mut chunk);
            counts.clamped += chunk_counts.clamped;
            counts.sanitized += chunk_counts.sanitized;
            if let Err(e) = self.sink.emit(&chunk, level) {
                // A dead sink mid-sweep is an exit path: collect the fault,
                // run the full restore sequence, then report.
                self.log.push(SessionEvent::SinkFault {
                    error: e.to_string(),
                });
                self.teardown(None);
                return Err(SessionError::Seam(e));
            }
            offset = end;
        }

        let warnings = counts.warnings();
        for warning in &warnings {
            self.log.push(SessionEvent::Warning {
                diagnostic: *warning,
            });
        }
        // An abort that arms during the final block (or between the last emit
        // and the loop exit) is polled here too — otherwise the run would be
        // logged as a clean `Completed` with no diagnostic. No ramp: the
        // stimulus is already exhausted, so there is nothing left to fade.
        if let Some(reason) = self.abort.triggered() {
            let diagnostic = reason.diagnostic();
            self.teardown(Some(diagnostic));
            return Ok(SweepOutcome::Aborted {
                diagnostic,
                warnings,
            });
        }
        self.log.push(SessionEvent::SweepCompleted);
        self.phase = SessionPhase::Swept;
        Ok(SweepOutcome::Completed { warnings })
    }

    /// Terminate outside a sweep — a trigger that fires while nothing is
    /// playing (mic unplugged between rungs, engine failure while the ack
    /// dialog is up). No ramp is needed; the restore sequence still runs in
    /// order and the trigger's diagnostic terminates the log.
    pub fn abort_now(&mut self, reason: AbortReason) {
        self.teardown(Some(reason.diagnostic()));
    }

    /// Normal completion: run the restore sequence (idempotent — a session
    /// already terminated by an abort restores nothing twice) and hand back
    /// the MS-23 log by value.
    pub fn finish(mut self) -> SessionLog {
        self.teardown(None);
        std::mem::take(&mut self.log)
    }

    /// MS-5 case 1: set the system output volume for the measurement, and
    /// RECORD that this session did it.
    ///
    /// **The recording is the whole reason this exists** (R-B2). `teardown`
    /// restores only what the session actually wrote, and never above where
    /// the volume is when it runs — see [`volume_restore_target`]. Without a
    /// method that records the write, the only honest teardown is one that
    /// writes nothing, and the only alternative is the unconditional restore
    /// that could turn a user's system back up after they reached for the
    /// volume mid-sweep.
    ///
    /// Nothing in the product calls this yet: the volume-policy rows of MS-5
    /// (`FixedMaxVolume` / `VolumeUncontrollable`) are the caller's to
    /// arbitrate, and the wizard that would is Stage 7.
    pub fn set_measurement_volume(&mut self, scalar: f64) -> Result<(), MeasureError> {
        // Before the call, not after: a write that fails partway has still
        // moved the system, and the restore duty must not depend on the HAL
        // agreeing that it did.
        self.volume_written = true;
        self.volume.set_volume(scalar)
    }

    /// A clone of the abort trigger for the UI, metering and listeners.
    pub fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }

    /// The level the session decided at the solve boundary, once solved.
    pub fn emit_level(&self) -> Option<SweepLevel> {
        self.emit_level
    }

    /// The log so far (by value on [`Self::finish`]).
    pub fn log(&self) -> &SessionLog {
        &self.log
    }

    /// Where the machine is.
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    fn require(&self, required: SessionPhase, method: &'static str) -> Result<(), SessionError> {
        if self.phase == required {
            Ok(())
        } else {
            Err(SessionError::WrongPhase {
                actual: self.phase,
                method,
                required: phase_name(required),
            })
        }
    }

    /// Refuse: terminate through the full restore sequence with `diagnostic`,
    /// then hand the caller the refusal. By the time a `Refused` error is
    /// observable, the system is already back in its pre-measurement state.
    fn refuse(&mut self, diagnostic: MeasurementDiagnostic) -> SessionError {
        let refusal = Refusal::new(diagnostic);
        self.teardown(Some(diagnostic));
        SessionError::Refused(refusal)
    }

    /// § Abort Guards, steps 2–6, in order — the same shape as the engine's
    /// tap teardown, with volume restore in the position tap destruction
    /// occupies: 1) stop the sink (idempotent per its contract), 2) restore
    /// the pre-measurement volume, 3) state teardown and the terminating log
    /// event. Guarded by `torn_down` (the `TapSystem` shape); failures are
    /// collected into the log, never allowed to mask later steps. The
    /// stimulus is already at zero when this runs — `sweep` ramps before
    /// terminating, and every other caller has nothing playing.
    fn teardown(&mut self, diagnostic: Option<MeasurementDiagnostic>) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        if let Err(e) = self.sink.stop() {
            self.log.push(SessionEvent::SinkFault {
                error: e.to_string(),
            });
        }
        // R-B2. The read-back is taken HERE, at teardown, not reused from
        // `begin`: the user may have moved the volume since, and the one thing
        // this must never do is move it back up.
        let current = self.volume.volume().ok();
        match volume_restore_target(self.volume_written, self.pre_volume, current) {
            Some(target) => {
                if let Err(e) = self.volume.set_volume(target) {
                    self.log.push(SessionEvent::VolumeRestoreFailed {
                        error: e.to_string(),
                    });
                }
            }
            None => {
                if self.volume_written && current.is_none() {
                    self.log.push(SessionEvent::VolumeRestoreFailed {
                        error: VOLUME_UNREADABLE_AT_TEARDOWN.to_owned(),
                    });
                }
            }
        }
        self.phase = SessionPhase::Terminated;
        self.log.push(SessionEvent::Terminated { diagnostic });
    }

    /// The MS-14 ramp: multiply the samples the sweep *would* have played by
    /// a raised cosine falling from ~1 to exactly 0.0 over [`ABORT_RAMP_MS`]
    /// of block time, zero-fill to the block edge, and emit. Never a
    /// re-render, never a hard stop. The envelope's last sample is forced to
    /// exactly `0.0` rather than trusting `cos(π)` rounding.
    ///
    /// The envelope itself lives in [`crate::ramp`], in ONE place, so the
    /// in-process abort here and the helper child process's cross-process
    /// abort are one shape with one test — two envelopes would mean two fade
    /// shapes, and "a hard stop is itself a full-scale click". The second
    /// argument is `padded`, not `ramp_len`: this loop runs `0..padded` and
    /// `padded > ramp_len` for every block size that is not an exact divisor,
    /// so the padded tail is part of the function's contract rather than this
    /// caller's. The lift is sample-identical by construction —
    /// `abort_envelope` returns exactly `0.0` at and past `ramp_len - 1`,
    /// which is what the expression it replaced produced for those indices.
    fn ramp_down(&mut self, remaining: &[f64], block: usize, sample_rate_hz: f64) {
        let ramp_len = crate::ramp::abort_ramp_len(sample_rate_hz);
        let padded = ramp_len.div_ceil(block) * block;
        let env = crate::ramp::abort_envelope(ramp_len, padded);
        let mut out = Vec::with_capacity(padded);
        // `env.len() == padded` by construction, so this is the shipped
        // `0..padded` loop with the coefficient read from the shared envelope.
        for (i, coefficient) in env.iter().enumerate() {
            out.push(remaining.get(i).copied().unwrap_or(0.0) * coefficient);
        }
        let level = self
            .emit_level
            .expect("ramp_down only runs inside sweep, which requires a level");
        for chunk in out.chunks(block) {
            if let Err(e) = self.sink.emit(chunk, level) {
                // Collect and keep going: a failing sink must not block the
                // volume restore behind it.
                self.log.push(SessionEvent::SinkFault {
                    error: e.to_string(),
                });
                break;
            }
        }
    }
}

/// What `VolumeRestoreFailed` says when the teardown cannot read the current
/// volume. One string, because both teardowns log it and a user comparing two
/// logs should not have to decide whether two wordings mean the same thing.
pub const VOLUME_UNREADABLE_AT_TEARDOWN: &str =
    "the current output volume could not be read, so the pre-measurement value was not \
     re-asserted — re-asserting it blind can turn the system UP";

/// MS-5's restore, decided: what — if anything — teardown should WRITE to the
/// output volume. `None` means "leave it alone".
///
/// R-B2. The old rule was `set_volume(pre_measurement)`, unconditionally, on
/// every exit path. Two things are wrong with that, and the second is the
/// serious one:
///
/// 1. **Nothing in the shipped product writes the volume.** The pass only
///    READS it, so the "restore" re-asserted a value that had never been
///    changed. MS-14's step 4 presumes a step 0 that set it; MS-5's case 1
///    ("set to the solved value") is not built yet.
/// 2. **It could turn the system UP.** A sweep is loud; the reflex to "too
///    loud" is to reach for the volume. Writing the pinned scalar back at
///    teardown overrides the user at the one moment the product must not — and
///    the next thing they play is at the level they just rejected.
///
/// So: restore only what we actually set, and never above where the volume is
/// now. `pre_measurement` is what `begin` read; `current` is a FRESH read taken
/// at teardown, `None` when it could not be taken — in which case nothing is
/// written, because "never raise it" cannot be honoured blind.
///
/// This is deliberately a free function rather than a method: it is the whole
/// of the policy, both teardowns call it, and it is the piece worth testing
/// directly (the `written == true` arm has no production caller yet, and the
/// point of pinning it now is that the arm is correct when one arrives).
pub fn volume_restore_target(
    volume_written: bool,
    pre_measurement: Option<f64>,
    current: Option<f64>,
) -> Option<f64> {
    if !volume_written {
        return None;
    }
    let pre = pre_measurement?;
    let current = current?;
    // Never raise. `pre > current` means the user lowered it while we were
    // running — their number wins, and there is nothing to write.
    if pre >= current {
        return None;
    }
    Some(pre)
}

impl std::fmt::Debug for MeasurementSession {
    /// Manual: the boxed seams are not `Debug`. Shows the machine's state,
    /// which is what an `unwrap_err` in a test wants to print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeasurementSession")
            .field("emit_level", &self.emit_level)
            .field("phase", &self.phase)
            .field("pin", &self.pin)
            .field("pre_volume", &self.pre_volume)
            .field("torn_down", &self.torn_down)
            .finish_non_exhaustive()
    }
}

impl Drop for MeasurementSession {
    /// MS-5's RAII half: every exit path — command, drop, panic — runs the
    /// full restore sequence. Idempotent via the `torn_down` guard, so a
    /// session that already terminated (refusal, abort, `finish`) restores
    /// nothing twice.
    fn drop(&mut self) {
        self.teardown(None);
    }
}

fn phase_name(phase: SessionPhase) -> &'static str {
    match phase {
        SessionPhase::Preflight => "Preflight",
        SessionPhase::Solved => "Solved",
        SessionPhase::Acknowledged => "Acknowledged",
        SessionPhase::Swept => "Swept",
        SessionPhase::Terminated => "Terminated",
    }
}
