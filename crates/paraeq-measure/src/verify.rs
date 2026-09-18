//! The closed-loop verification pass: nine gates, an acknowledgement, a
//! helper child process, a metered capture, and a teardown ladder that runs on
//! every exit path.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**. Gate order, refusal
//! timing and teardown order are product policy; `tests/test_verify.rs` pins
//! them mock-driven — no engine, no device, no process.
//!
//! # What this module is, and why it is not a `MeasurementSession`
//!
//! A measurement session's precondition is that ParaEQ's own audio is
//! EXCLUDED from the tap, so the Direct sweep never traverses the correction
//! chain. Verification's precondition is the opposite: the stimulus must go
//! *through* the chain, which is why it plays from a **helper child process**
//! — a different PID, which the tap does see. So this is a separate,
//! session-shaped runtime with its own phase machine, writing into the **same**
//! MS-23 [`SessionLog`] rather than minting a second one.
//!
//! ```text
//! Armed ─ack─▶ Acknowledged ─▶ HelperReady ─▶ Playing ─▶ Captured ─▶ Analyzed
//!    └──────────── refusal / abort / helper exit / drop ─────────▶ Terminated
//! ```
//!
//! # Gate order is load-bearing, and the first two are NOT in the obvious
//! # order
//!
//! **The lease is gate 1 and the engine read is gate 2.** The fail-open
//! watchdog auto-disables the engine after a spell with no input, that state
//! is documented as **sticky** (there is no auto-retry — re-engaging would
//! re-mute the system), and the verification pre-roll is *precisely* a window
//! of deliberate silence, i.e. the condition that arms it. Reading the engine
//! first and leasing second leaves a live race: the engine can auto-disable
//! between the two, both gates pass, the helper spawns, and the sweep plays
//! into a torn-down chain. Acquiring the lease first suspends the watchdog
//! **before** anything is read, so what gate 2 observes is what the spawn will
//! get. The shipped lease doc independently endorses this order: "It knows
//! nothing about engine status … Callers gate on `EngineState::status` /
//! `EngineState::stream` themselves."
//!
//! This is written here because the order otherwise looks like an oversight
//! and the next reader "tidies" it back.
//!
//! # The engine must be ENGAGED, which is not `status == Running`
//!
//! The wizard spec opens its verification sequence with "Engine is `Running`".
//! Under the shipped watchdog that word names a state set **only** while
//! nonzero input is advancing, decaying to `InputSilent` and then `Idle` on a
//! quiet machine — and Window A below *requires* a quiet machine, because the
//! tap is global and excludes only ParaEQ, so any other application's audio
//! would land in the corrected capture. The spec asks for audio to be flowing
//! and the gate asks for silence, at the same instant. Gate 2 therefore reads
//! [`EngineFacts::engine_engaged`], and the reinterpretation is escalated to
//! the owner rather than closed here.
//!
//! # The level book, in four lines
//!
//! * `L_verify = L_measure + preamp_db`, `preamp_db ≤ 0`. A hard spec
//!   requirement, not a tunable.
//! * `preamp_db` is the **engine's own** number, recomputed by gate 2 at the
//!   LIVE rate over the plan's bands and folded `min` across channels — the
//!   identical call the controller makes. Never `plan.preamp_db`, which is
//!   `decide()`'s at the design rate over all bands.
//! * `peak_correction_gain_db ≜ max_f H(f)` with the preamp **excluded**: a
//!   safety interlock may not be conditioned on the hypothesis it is testing.
//!   If the level were solved assuming the preamp cancels the boost, then in
//!   exactly the failure case this pass hunts for, the stimulus would reach
//!   the transducer ABOVE the baseline the caps table validated.
//! * **The MS-11 cal-error margin is applied exactly ONCE, upstream.**
//!   `install_solve` runs `margined_emit_dbfs` and hands its result to
//!   `SweepLevel::new`, so `L_measure` is already post-margin and `L_verify`
//!   inherits it through the addition. Re-applying it here would subtract
//!   `CAL_ERROR_MARGIN_DB` twice — off by 6.0 dB against a gate as small as
//!   2.0 dB, on every run with a mismatched input gain, which is the *common*
//!   case the margin exists for. Quiet is still the safe direction and the
//!   derate is still in `L_verify`; it is inherited, not re-applied.
//!
//! # Abort across a process boundary, honestly budgeted
//!
//! The chain is: parent writes `abort` → the child's reader wakes → the
//! child's NEXT callback starts the 5 ms ramp → audio already in flight
//! traverses tap, chain and DAC. The child guarantees the middle two terms,
//! the parent guarantees the first, and **nobody** can guarantee the last. So
//! [`VerifyOutcome::abort_acoustic_budget_ms`] is computed per run — one
//! helper block, plus `ABORT_RAMP_MS`, plus the engine's own reported latency,
//! plus slack — and reported rather than asserted.
//!
//! Mitigating fact, stated so nobody "tightens" the budget by hard-stopping:
//! `L_verify ≤ L_measure` **always**, so exposure during those extra
//! milliseconds is bounded below the Direct path's, which the level ladder
//! already accepts. A hard stop is itself a full-scale click.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::align::{self, AlignRequest, Alignment};
use crate::cal::CalSummary;
use crate::capture::{record, CaptureEnd, CaptureMeter};
use crate::diagnostic::{MeasurementDiagnostic, Refusal};
use crate::engine_seam::{EngineControl, EngineFacts, GainPin, MeasurementLeaseToken, TapActivity};
use crate::level::{caps_for, SweepLevel};
use crate::seam::{CaptureSource, DeviceFacts, HelperExit, HelperLine, HelperProcess};
use crate::seam::{HelperRouting, StimulusHelper, TapStatus, VolumeControl};
use crate::session::{
    volume_restore_target, AbortHandle, SessionEvent, SessionLog, ABORT_RAMP_MS,
    ACK_SPL_TOLERANCE_DB, VOLUME_UNREADABLE_AT_TEARDOWN,
};
use crate::stimulus::{assemble_bracketed, assemble_sweep, AssembledStimulus, StimulusKind};
use crate::MeasureError;
use paraeq_dsp::gating::ImpulseResponse;
use paraeq_dsp::peq::{EQBand, ParametricEQ};
use paraeq_dsp::two_clock::{layout_marker, MarkerLayout};

/// How closely the engine's ARMED preamp must match the gate's own
/// recomputation of it, linear.
///
/// Satisfiable to this precision **only because** `bands_dropped() == 0` is
/// required first: the engine computes at the live rate over the SURVIVING
/// bands, so with a band dropped the two legitimately differ by far more than
/// this and a naive compare would refuse a correct engine.
pub const PREAMP_MATCH_TOLERANCE: f32 = 1e-6;

/// The projected signal-to-noise `L_verify` must clear before a sweep is
/// armed, dB.
///
/// **`[NEEDS DATA]`.** Cut at MS-8's own warn row ("≥30 dB warn"): a pass that
/// cannot reach even the degraded row produces a residual made of noise, and
/// the honest thing is to refuse before spawning rather than play an unusably
/// quiet sweep and report a shape failure. The one remedy that would fix it is
/// the one MS-8 forbids — "remedies never touch output level".
pub const VERIFY_MIN_SNR_DB: f64 = 30.0;

/// Floor for the capture-side peak and RMS, dBFS.
///
/// [`CaptureMeter::peak_dbfs`] answers `f64::NEG_INFINITY` on digital silence
/// rather than flooring, which is right for a meter and wrong for a wire: a
/// non-finite `f64` serializes as `null` and then refuses to read back. The
/// floor is two orders of magnitude below a 24-bit converter's own LSB
/// (≈ −144 dBFS), so a floored reading means digital silence and nothing else
/// — and a digitally silent capture is refused one step later anyway, when no
/// credible marker train can be found in it.
pub const CAPTURE_FLOOR_DBFS: f64 = -180.0;

/// Where in the one-way phase machine a pass is. Declaration order is the
/// machine's order; [`VerifyPhase::Terminated`] is absorbing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyPhase {
    /// Gates 1–6 and the pre-spawn routing fence passed; the level is decided
    /// and logged. No process exists.
    Armed,
    /// The MS-18 acknowledgement is on record. This is the only door to a
    /// spawn, and therefore to a sample.
    Acknowledged,
    /// The helper opened its device and emitted `ready`. Nothing has played.
    HelperReady,
    /// `play` was sent.
    Playing,
    /// The mic capture completed.
    Captured,
    /// `t = 0` was recovered and the impulse response cut.
    Analyzed,
    /// Torn down: helper stopped and reaped, capture stopped, volume and trim
    /// restored, lease released. Absorbing.
    Terminated,
}

/// Wall-clock budgets. Defaults are the product's; tests shorten them so the
/// witness windows are real windows rather than a mocked-out clock.
///
/// Nothing here is on a realtime path — the pacing lives in the sink and the
/// capture source. These are control-plane deadlines.
#[derive(Clone, Copy, Debug)]
pub struct VerifyTiming {
    /// Scheduler slack added to every abort rung and to the acoustic budget.
    pub abort_slack_ms: f64,
    /// Extra capture past the end of the assembled file, seconds, so
    /// transport latency cannot truncate the closing marker.
    pub capture_tail_s: f64,
    /// How long to wait for the child's terminating status line.
    pub helper_done_ms: u64,
    /// How long to wait for the child's `ready` line.
    pub helper_ready_ms: u64,
    /// How long SIGTERM gets before SIGKILL.
    pub sigterm_deadline_ms: u64,
    /// Poll period for the realtime activity witness.
    pub tap_poll_ms: u64,
    /// Window A — the pre-roll during which realtime activity must be
    /// STATIONARY.
    pub window_a_ms: u64,
    /// Window B — from the first marker, during which realtime activity must
    /// ADVANCE. One second plus one controller tick.
    pub window_b_ms: u64,
}

impl Default for VerifyTiming {
    fn default() -> Self {
        Self {
            abort_slack_ms: 5.0,
            capture_tail_s: 0.5,
            helper_done_ms: 30_000,
            helper_ready_ms: 5_000,
            sigterm_deadline_ms: 250,
            tap_poll_ms: 10,
            window_a_ms: 1_000,
            window_b_ms: 1_250,
        }
    }
}

/// The sweep to re-level and play. The SAME shape the baseline used at this
/// position — only the level differs, which is the whole of MS-19.
#[derive(Clone, Copy, Debug)]
pub struct SweepShape {
    pub duration_s: f64,
    pub f_end_hz: f64,
    pub f_start_hz: f64,
    pub sample_rate_hz: u32,
}

/// The correction the engine is supposed to be running, as `decide()` planned
/// it.
///
/// `preamp_db` is carried for provenance and is **not** what the level book
/// uses: gate 2 recomputes the preamp at the LIVE rate over these bands,
/// because the engine does the same and the two numbers legitimately differ
/// whenever the design rate and the stream rate differ.
#[derive(Clone, Debug)]
pub struct VerifyPlan {
    /// Per channel, indexed by the ENGINE's channel index.
    pub bands: Vec<Vec<EQBand>>,
    pub design_rate_hz: f64,
    pub preamp_db: f64,
}

/// Everything the pass needs that is not a seam.
pub struct VerifyRequest {
    /// The routing the BASELINE used at `position_index`. Gate 9 forces the
    /// verification routing equal to it: differencing a per-ear baseline
    /// against an L+R sum is not a residual.
    pub baseline_routing: HelperRouting,
    pub cal: CalSummary,
    /// The physical output device the helper renders to.
    pub device_uid: String,
    pub input_gain_read_back: f64,
    /// `L_measure`: the level the Direct sweep actually played, **post-margin**
    /// by construction.
    pub l_measure: SweepLevel,
    /// The SPL `L_measure` projected at the mic, from the Direct session's
    /// solve. `L_verify`'s projection is this plus `preamp_db`.
    pub l_measure_projected_spl_db: f64,
    /// The bracket. The SAME layout the baseline was assembled with, or the
    /// two captures are aligned by different means and cannot be subtracted.
    pub layout: MarkerLayout,
    /// Broadband noise floor measured before the first sweep, dBFS.
    pub noise_floor_dbfs: f64,
    pub plan: VerifyPlan,
    pub position_index: usize,
    pub routing: HelperRouting,
    pub sweep: SweepShape,
    pub timing: VerifyTiming,
    /// Where the parent writes the WAV the child plays. Deleted by teardown.
    pub wav_path: PathBuf,
}

/// The six seams a verification pass drives, bundled so the constructor
/// cannot be handed a partial set.
pub struct VerifySeam {
    pub capture: Box<dyn CaptureSource>,
    pub control: Box<dyn EngineControl>,
    /// HAL device facts, by UID. The rate fence and the teardown's
    /// device-gone check ask it; nothing else does.
    pub devices: Box<dyn DeviceFacts>,
    pub engine: Box<dyn EngineFacts>,
    pub helper: Box<dyn StimulusHelper>,
    pub tap: Box<dyn TapStatus>,
    pub volume: Box<dyn VolumeControl>,
}

/// Capture-side peak + clip metering for this pass (MS-21).
///
/// **Documented twin of `paraeq_decide::bundle::CaptureStats`**, field for
/// field, and a twin rather than a re-export because `paraeq-measure` may not
/// depend on `paraeq-decide` — the same crate-DAG reason `CaptureStats`' own
/// doc gives on the other side. A caller with both types in scope maps them
/// one to one.
///
/// This is the MIC's ADC, on the FAR side of the transducer. The engine's own
/// ±1.0 output clamp is the near side and is reported separately, as
/// [`MeasurementDiagnostic::VerificationChainClipped`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VerifyCaptureStats {
    pub clipped_samples: u64,
    /// Floored at [`CAPTURE_FLOOR_DBFS`]; never `-inf`.
    pub peak_dbfs: f64,
    /// Floored at [`CAPTURE_FLOOR_DBFS`]; never `-inf`.
    pub rms_dbfs: f64,
}

/// The marker fit, as evidence.
///
/// **Documented twin of `paraeq_decide::bundle::TwoClockFit`**, minus the
/// per-marker residual vector, which is fit-internal detail a frozen bundle
/// does not need. Evidence only — the credibility ladder gates the fit's
/// ADMISSIBILITY, never this figure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VerifyTwoClockFit {
    pub intercept_samples: f64,
    pub residual_peak_samples: f64,
    pub residual_rms_samples: f64,
    pub skew_ppm: f64,
}

/// What a completed pass produces.
///
/// Maps one to one onto `paraeq_decide::bundle::Verification`: `capture`,
/// `gain_db`, `installed_preamp_db`, `ir`, `level_dbfs`, `position_index`,
/// `routing` and `running_rate_hz` are that struct's fields under the same
/// names, and `two_clock` is its `Option<TwoClockFit>`. The caller supplies
/// `installed` (the plan it already holds) and lifts the mono [`ImpulseResponse`]
/// into the bundle's per-channel shape.
#[derive(Clone, Debug)]
pub struct VerifyOutcome {
    /// One helper block + [`ABORT_RAMP_MS`] + the engine's reported latency +
    /// slack. Reported, never asserted — see the module header.
    pub abort_acoustic_budget_ms: f64,
    pub capture: VerifyCaptureStats,
    /// The user's trim during the capture. Pinned to 0.0 and read back.
    pub gain_db: f32,
    /// The engine's OWN armed preamp, dB — the number gate 2 agreed with.
    pub installed_preamp_db: f64,
    pub ir: ImpulseResponse,
    /// `L_verify`, SWEEP-SPAN RMS.
    pub level_dbfs: f64,
    pub position_index: usize,
    pub routing: HelperRouting,
    /// The LIVE stream rate during the capture.
    pub running_rate_hz: f64,
    pub two_clock: Option<VerifyTwoClockFit>,
    /// Non-blocking diagnostics raised during the pass.
    pub warnings: Vec<MeasurementDiagnostic>,
}

/// Everything that can go wrong. Refusals have already terminated the pass —
/// helper down, capture stopped, volume and trim restored, lease released —
/// by the time the caller sees one.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("acknowledgement names no device")]
    AckNamesNoDevice,
    #[error(
        "acknowledged SPL {acknowledged_spl_db} dB is not this pass's projection \
         {projected_spl_db} dB — a stale acknowledgement authorizes nothing"
    )]
    AckSplStale {
        acknowledged_spl_db: f64,
        projected_spl_db: f64,
    },
    #[error("the verification helper spoke a line this parent cannot read: {line}")]
    HelperProtocol { line: String },
    #[error("refused: {0:?}")]
    Refused(Refusal),
    #[error("seam failure: {0}")]
    Seam(#[from] MeasureError),
    #[error("stimulus assembly failed: {0}")]
    Stimulus(#[from] crate::stimulus::StimulusError),
    #[error("could not write the verification WAV to {path}: {reason}")]
    Wav { path: PathBuf, reason: String },
    #[error("{method}() requires phase {required}; the pass is {actual:?}")]
    WrongPhase {
        actual: VerifyPhase,
        method: &'static str,
        required: &'static str,
    },
}

/// A pass that never armed, with the log it produced getting that far.
///
/// The log is handed back rather than dropped because a refusal at gate 2 is
/// exactly the case a bug report is about, and MS-23's terminating diagnostic
/// is already in it.
#[derive(Debug)]
pub struct ArmingFailure {
    pub error: VerifyError,
    pub log: SessionLog,
}

impl std::fmt::Display for ArmingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for ArmingFailure {}

/// The private render aggregate a helper of this pid is expected to have
/// created, and to have destroyed on its way out.
///
/// ONE place, so the parent's teardown assertion and the child's own
/// composition cannot disagree about the name.
pub fn expected_render_device_uid(pid: u32) -> String {
    format!("com.paraeq.render.{pid}")
}

/// The verification pass runtime. See the module header for the machine and
/// the gate order; see `tests/test_verify.rs` for the pinned guarantees.
///
/// **Field order below is teardown order, not alphabetical, and that is
/// deliberate.** Rust drops fields in declaration order, so the RAII group at
/// the end releases the trim before the lease. `teardown` does it explicitly
/// too — belt and braces on the one sequence that must never be reordered by
/// accident.
pub struct VerificationPass {
    abort: AbortHandle,
    capture_source: Box<dyn CaptureSource>,
    /// Blocking findings collected during teardown, never masked.
    collected: Vec<MeasurementDiagnostic>,
    control: Box<dyn EngineControl>,
    devices: Box<dyn DeviceFacts>,
    engine: Box<dyn EngineFacts>,
    helper_spawner: Box<dyn StimulusHelper>,
    log: SessionLog,
    phase: VerifyPhase,
    pre_volume: Option<f64>,
    request: VerifyRequest,
    tap: Box<dyn TapStatus>,
    torn_down: bool,
    volume: Box<dyn VolumeControl>,
    /// Did THIS pass ever write the output volume? R-B2: teardown restores
    /// only what it actually changed. Nothing in the shipped product sets the
    /// volume, so this is `false` for the whole of a run today — see
    /// [`volume_restore_target`].
    volume_written: bool,

    // Decided at arming.
    clipped_at_arm: u64,
    l_verify: Option<SweepLevel>,
    peak_correction_gain_db: f64,
    projected_spl_db: f64,
    recomputed_preamp_db: f64,
    stream_rate_hz: f64,

    // Runtime.
    child_exited: bool,
    ramp_deadline_ms: u64,
    warnings: Vec<MeasurementDiagnostic>,

    // RAII, in teardown order. The lease is LAST.
    child: Option<Box<dyn HelperProcess>>,
    gain_pin: Option<GainPin>,
    lease: Option<Box<dyn MeasurementLeaseToken>>,
}

impl VerificationPass {
    /// Run gates 1–6 and the pre-spawn half of gate 9, decide `L_verify`, and
    /// leave the pass [`VerifyPhase::Armed`]. **No process is spawned and no
    /// sample is emitted by anything in here.**
    ///
    /// # Errors
    ///
    /// The refusal, with the MS-23 log the pass produced getting that far. The
    /// pass has already torn down: volume restored, trim restored, lease
    /// released.
    pub fn arm(request: VerifyRequest, seam: VerifySeam) -> Result<Self, ArmingFailure> {
        let VerifySeam {
            capture,
            control,
            devices,
            engine,
            helper,
            tap,
            volume,
        } = seam;
        let mut pass = VerificationPass {
            abort: AbortHandle::new(),
            capture_source: capture,
            collected: Vec::new(),
            control,
            devices,
            engine,
            helper_spawner: helper,
            log: SessionLog::default(),
            phase: VerifyPhase::Armed,
            pre_volume: None,
            request,
            tap,
            torn_down: false,
            volume,
            volume_written: false,
            clipped_at_arm: 0,
            l_verify: None,
            peak_correction_gain_db: 0.0,
            projected_spl_db: 0.0,
            recomputed_preamp_db: 0.0,
            stream_rate_hz: 0.0,
            child_exited: false,
            ramp_deadline_ms: 0,
            warnings: Vec::new(),
            child: None,
            gain_pin: None,
            lease: None,
        };
        match pass.run_arming_gates() {
            Ok(()) => Ok(pass),
            Err(error) => {
                let log = std::mem::take(&mut pass.log);
                Err(ArmingFailure { error, log })
            }
        }
    }

    /// MS-18: the pre-sweep acknowledgement, naming the output device and the
    /// projected SPL the user actually saw. The only door to
    /// [`VerifyPhase::Acknowledged`], and therefore to a spawn.
    ///
    /// The verification pass is a real sweep into a transducer at up to
    /// `L_measure`, so MS-18 binds it exactly as it binds the Direct path, and
    /// it reuses the shipped mechanism — the same staleness tolerance, the
    /// same refusals, the same `SessionEvent` — rather than inventing a second
    /// one.
    pub fn acknowledge(
        &mut self,
        device_name: &str,
        projected_spl_db: f64,
    ) -> Result<(), VerifyError> {
        self.require(VerifyPhase::Armed, "acknowledge", "Armed")?;
        if device_name.trim().is_empty() {
            return Err(VerifyError::AckNamesNoDevice);
        }
        // Positive requirement (NaN is stale): the acknowledged number must be
        // THIS pass's projection, or the ack authorizes nothing.
        let fresh = (projected_spl_db - self.projected_spl_db).abs() <= ACK_SPL_TOLERANCE_DB;
        if !fresh {
            return Err(VerifyError::AckSplStale {
                acknowledged_spl_db: projected_spl_db,
                projected_spl_db: self.projected_spl_db,
            });
        }
        self.log.push(SessionEvent::Acknowledged {
            device_name: device_name.to_owned(),
            projected_spl_db,
        });
        self.phase = VerifyPhase::Acknowledged;
        Ok(())
    }

    /// Spawn, fence, witness, play, capture, align — then tear down in the
    /// MS-14 order whatever happened.
    pub fn run(&mut self) -> Result<VerifyOutcome, VerifyError> {
        self.require(VerifyPhase::Acknowledged, "run", "Acknowledged")?;
        match self.run_inner() {
            Ok(outcome) => Ok(outcome),
            Err(error) => Err(self.fail(error)),
        }
    }

    /// The MS-23 log so far.
    pub fn log(&self) -> &SessionLog {
        &self.log
    }

    /// Where the machine is.
    pub fn phase(&self) -> VerifyPhase {
        self.phase
    }

    /// The level this pass decided, once armed.
    pub fn level(&self) -> Option<SweepLevel> {
        self.l_verify
    }

    /// The SPL `L_verify` projects at the mic — the number the user
    /// acknowledges.
    pub fn projected_spl_db(&self) -> f64 {
        self.projected_spl_db
    }

    /// A clone of the abort trigger, for the UI and the device listeners.
    pub fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }

    /// Terminate and hand back the MS-23 log by value. Idempotent: a pass
    /// already terminated by a refusal restores nothing twice.
    pub fn finish(mut self) -> SessionLog {
        self.teardown(None);
        std::mem::take(&mut self.log)
    }

    // ───────────────────────────── gates 1–6, 9a ────────────────────────────

    fn run_arming_gates(&mut self) -> Result<(), VerifyError> {
        // GATE 1 — the lease, FIRST. See the module header: the auto-disable
        // state is sticky and the pre-roll is the silence that arms it, so a
        // lease taken after the first engine read covers a window that has
        // already closed.
        let lease = self
            .control
            .acquire_measurement_lease()
            .map_err(|e| self.fail_seam(e))?;
        self.lease = Some(lease);

        // MS-23's cal columns, by the SAME derivation `MeasurementSession::begin`
        // uses — one call, one cal identity, no second source of truth.
        let pin = self.request.cal.pin_gain(self.request.input_gain_read_back);
        self.log.push(SessionEvent::CalLoaded {
            class: self.request.cal.class(),
            file_identity: self.request.cal.file_identity().to_owned(),
            gain_matches: pin.matches(),
            input_gain_read_back: self.request.input_gain_read_back,
            reference_input_gain: self.request.cal.reference_input_gain(),
            sens_factor_dbfs: self.request.cal.sensitivity().sens_factor_dbfs(),
            spl_cap_db: caps_for(self.request.cal.class()).spl_refuse_db,
        });

        // GATE 2 — the engine, through the seam. Never by linking the engine.
        self.gate_engine()?;

        // GATE 3 — the BASELINE's MS-6 witness. Read the inversion note in
        // `gate_self_exclusion`: this is NOT a re-check for the verification
        // capture.
        if !self.tap.self_excluded() {
            return Err(self.refuse(MeasurementDiagnostic::SelfExclusionUnavailable));
        }

        // The volume pin (gate 7's duty) is taken HERE, before the ack, for
        // two reasons: MS-23's log reads cal → volume → level, matching
        // `begin`'s own order; and owning the restore duty from the first gate
        // onward is strictly safer than owning it from the seventh.
        //
        // It is a READ and stays one. The verification pass never sets the
        // system volume — the WAV is the level — so R-B2 makes `teardown`
        // write nothing: re-asserting this scalar would override a user who
        // turned the volume down mid-sweep, at the one moment it must not.
        self.pre_volume = self.volume.volume().ok();
        self.log.push(SessionEvent::VolumePinned {
            pre_measurement_scalar: self.pre_volume,
        });

        // GATE 4 — pin the user's trim to 0.0 and READ IT BACK.
        self.gate_gain_pin()?;

        // GATE 5 — the level.
        self.gate_level()?;

        // GATE 6 — the SNR budget, before a process exists.
        self.gate_snr_budget()?;

        // GATE 9 (pre-spawn half) — routing.
        self.gate_routing_pre_spawn()?;

        Ok(())
    }

    /// GATE 2. Engaged, not bypassed, a correction installed, no rate
    /// mismatch, no dropped bands, an armed preamp that agrees with our own
    /// live-rate recomputation of it.
    fn gate_engine(&mut self) -> Result<(), VerifyError> {
        if !self.engine.engine_engaged() {
            // The remedy names what the engine reported rather than saying
            // "not running", which is why `status_kind()` exists at all.
            let kind = self.engine.status_kind();
            self.log.push(SessionEvent::SinkFault {
                error: format!("the engine reported {kind:?}"),
            });
            return Err(self.refuse(MeasurementDiagnostic::EngineNotRunning));
        }
        if self.engine.bypass() || !self.engine.correction_installed() {
            return Err(self.refuse(MeasurementDiagnostic::EngineNotRunning));
        }
        let Some(stream_rate_hz) = self.engine.stream_rate_hz() else {
            return Err(self.refuse(MeasurementDiagnostic::EngineNotRunning));
        };
        // Positive requirement: a NaN or non-positive rate refuses rather
        // than being used as a design rate.
        let rate_is_real = stream_rate_hz > 0.0;
        if !rate_is_real {
            return Err(self.refuse(MeasurementDiagnostic::EngineNotRunning));
        }
        self.stream_rate_hz = stream_rate_hz;

        if let Some(mismatch_hz) = self.engine.correction_rate_mismatch_hz() {
            // The chain is running coefficients designed for another rate —
            // "stale coefficients after a rate change", the wizard's own named
            // failure. Same code as the device-side fence, whose doc carries
            // both clauses.
            return Err(
                self.refuse(MeasurementDiagnostic::OutputRateChangedDuringVerify {
                    expected_hz: stream_rate_hz,
                    observed_hz: mismatch_hz,
                }),
            );
        }

        let dropped = self.engine.bands_dropped();
        if dropped > 0 {
            return Err(
                self.refuse(MeasurementDiagnostic::VerificationBandsDropped {
                    dropped: dropped as u64,
                    rate_hz: stream_rate_hz,
                }),
            );
        }

        // The recomputation: the IDENTICAL call the controller makes — the
        // plan's bands, at the LIVE rate, folded `min` across channels,
        // because the worst channel wins.
        let (recomputed_db, peak_db) = recompute_preamp(&self.request.plan.bands, stream_rate_hz);
        self.recomputed_preamp_db = recomputed_db;
        self.peak_correction_gain_db = peak_db;

        let Some(armed_lin) = self.engine.installed_preamp_lin() else {
            // A correction IS installed (checked above) and yet no preamp is
            // armed. `None` is not 1.0 and must never be defaulted to it: a
            // silently-unity preamp is precisely the failure this pass exists
            // to catch.
            return Err(
                self.refuse(MeasurementDiagnostic::VerificationPreampMismatch {
                    armed_db: f64::NAN,
                    recomputed_db,
                }),
            );
        };
        let expected_lin = preamp_lin(recomputed_db);
        // Positive requirement: a NaN difference — which passes no comparison
        // — is a mismatch, never a match.
        let agrees = (armed_lin - expected_lin).abs() <= PREAMP_MATCH_TOLERANCE;
        if !agrees {
            let armed_db = if armed_lin > 0.0 {
                20.0 * f64::from(armed_lin).log10()
            } else {
                f64::NEG_INFINITY
            };
            return Err(
                self.refuse(MeasurementDiagnostic::VerificationPreampMismatch {
                    armed_db,
                    recomputed_db,
                }),
            );
        }

        self.clipped_at_arm = self.engine.clipped_samples();
        Ok(())
    }

    /// GATE 4. `G_user` is the user's manual trim, and it is pinned to 0.0 for
    /// the pass and RAII-restored on every exit path including panic.
    ///
    /// Zeroing it is safe ONLY because the computed preamp lives inside the
    /// installed `Correction` and is applied on the corrected path — under the
    /// rejected carrier this pin would have erased the very thing under test.
    /// And it is necessary: with `G_user = +10` against a `+6` dB peak boost
    /// the verification sweep would reach the transducer LOUDER than the
    /// baseline the level ladder validated. That is a safety failure, not an
    /// accounting one, which is why the write is read back rather than
    /// assumed.
    fn gate_gain_pin(&mut self) -> Result<(), VerifyError> {
        let pin = self
            .control
            .pin_gain_db(0.0)
            .map_err(|e| self.fail_seam(e))?;
        self.gain_pin = Some(pin);
        let read_back = self.engine.gain_db();
        if read_back != 0.0 {
            return Err(
                self.refuse(MeasurementDiagnostic::VerificationGainNotPinned {
                    read_back_db: f64::from(read_back),
                }),
            );
        }
        Ok(())
    }

    /// GATE 5. `L_verify = L_measure + preamp_db`, through `SweepLevel::new`
    /// so the class cap and the absolute ceiling both bind.
    ///
    /// It does **not** go through `margined_emit_dbfs`: `L_measure` is already
    /// post-margin and re-applying would subtract 6 dB twice. See the module
    /// header.
    fn gate_level(&mut self) -> Result<(), VerifyError> {
        let class = self.request.cal.class();
        let l_measure = self.request.l_measure.dbfs_rms();
        let l_verify_dbfs = l_measure + self.recomputed_preamp_db;
        let Ok(level) = SweepLevel::new(l_verify_dbfs, class) else {
            // `preamp_db ≤ 0` means a level the baseline already cleared
            // cannot fail the ceiling — but it is CONSTRUCTED rather than
            // asserted, because the constructor is the interlock.
            return Err(self.refuse(MeasurementDiagnostic::SolvedLevelIllegal));
        };
        self.l_verify = Some(level);
        self.projected_spl_db = self.request.l_measure_projected_spl_db + self.recomputed_preamp_db;

        let cap_db = caps_for(class).spl_refuse_db;
        // Positive requirement so a NaN projection refuses rather than passing
        // under an unwitnessed cap.
        let under_cap = self.projected_spl_db.is_finite() && self.projected_spl_db <= cap_db;
        if !under_cap {
            return Err(self.refuse(MeasurementDiagnostic::ProjectedSplOverCap));
        }

        self.log.push(SessionEvent::VerifyLevelled {
            class,
            gain_db_pinned: 0.0,
            l_measure_dbfs_rms: l_measure,
            l_verify_dbfs_rms: level.dbfs_rms(),
            peak_correction_gain_db: self.peak_correction_gain_db,
            spl_cap_db: cap_db,
        });
        Ok(())
    }

    /// GATE 6. Refuse a level that cannot produce a meaningful residual,
    /// before spawning. Never remedied by raising the level.
    fn gate_snr_budget(&mut self) -> Result<(), VerifyError> {
        let level = self.l_verify.expect("gate 5 decided a level").dbfs_rms();
        let projected_snr_db = level - self.request.noise_floor_dbfs;
        let ok = projected_snr_db.is_finite() && projected_snr_db >= VERIFY_MIN_SNR_DB;
        if !ok {
            return Err(
                self.refuse(MeasurementDiagnostic::VerificationLevelBelowSnrBudget {
                    projected_snr_db,
                }),
            );
        }
        Ok(())
    }

    /// GATE 9, pre-spawn half. Both refusals are the parent's own data, so
    /// there is no reason to burn a sweep discovering them.
    fn gate_routing_pre_spawn(&mut self) -> Result<(), VerifyError> {
        if self.request.routing != self.request.baseline_routing {
            return Err(self.refuse(MeasurementDiagnostic::HelperRoutingMismatch));
        }
        if self.request.routing == HelperRouting::Both {
            // A `Both` capture heard the SUM of every output channel. A sum of
            // differently-corrected channels is not the response of any one
            // band set, and no amount of algebra makes it one.
            let bands = &self.request.plan.bands;
            let divergent = bands.windows(2).any(|w| w[0] != w[1]);
            if divergent {
                return Err(self.refuse(MeasurementDiagnostic::HelperRoutingMismatch));
            }
        } else if let HelperRouting::Only(channel) = self.request.routing {
            // A routing naming a channel the plan never corrected cannot be
            // predicted at all.
            if channel as usize >= self.request.plan.bands.len() {
                return Err(self.refuse(MeasurementDiagnostic::HelperRoutingMismatch));
            }
        }
        Ok(())
    }

    // ──────────────────────────────── the run ───────────────────────────────

    fn run_inner(&mut self) -> Result<VerifyOutcome, VerifyError> {
        let rate = self.request.sweep.sample_rate_hz;
        let level = self.l_verify.expect("armed implies a level");

        // The file. `assemble_bracketed` consumes a verified sweep by value,
        // so the MS-3 assertion set has already run on the span and the three
        // whole-buffer checks run on the assembled file.
        let sweep = assemble_sweep(
            self.request.sweep.duration_s,
            rate,
            self.request.sweep.f_start_hz,
            self.request.sweep.f_end_hz,
            level,
        )?;
        let reference: Vec<f64> = sweep.samples().to_vec();
        let sweep_len = sweep.len();
        let (bracketed, _span) =
            assemble_bracketed(sweep, &self.request.layout, StimulusKind::VerificationSweep)?;
        write_wav(&self.request.wav_path, &bracketed)?;

        // Spawn. The child opens its device and emits `ready` BEFORE any
        // audio, so a spawn that returns Ok has not yet played a sample.
        let child = self
            .helper_spawner
            .spawn(
                &self.request.wav_path,
                &self.request.device_uid,
                self.request.routing,
            )
            .map_err(|e| {
                self.log.push(SessionEvent::SinkFault {
                    error: e.to_string(),
                });
                VerifyError::Refused(Refusal::new(MeasurementDiagnostic::HelperUnavailable))
            })?;
        self.child = Some(child);

        let ready = self.read_ready()?;
        self.phase = VerifyPhase::HelperReady;

        // GATE 8, first read — on the device the CHILD actually opened, not on
        // the default output. Without this the child's own rate assert and the
        // parent's fence read different devices and the child exits 3 on every
        // run while the failure reads as a WAV bug.
        self.gate_rate_fence(&ready, rate)?;
        // GATE 9, echoed half.
        if ready.routing != self.request.routing.as_channel_arg() {
            return Err(VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::HelperRoutingMismatch,
            )));
        }

        let block_ms = if ready.frames_per_block > 0 && ready.sample_rate_hz > 0.0 {
            ready.frames_per_block as f64 / ready.sample_rate_hz * 1000.0
        } else {
            0.0
        };
        let slack = self.request.timing.abort_slack_ms;
        self.ramp_deadline_ms = (ABORT_RAMP_MS + block_ms + slack).ceil() as u64;
        let abort_acoustic_budget_ms =
            block_ms + ABORT_RAMP_MS + self.engine.latency_ms().unwrap_or(0.0) + slack;

        // WINDOW A — the pre-roll must be QUIET. The tap is global and
        // excludes only ParaEQ, so any other app's audio lands in
        // `measured_corrected`.
        self.window_a()?;

        let mut meter = CaptureMeter::new();
        // Scoped to THIS pass, not to the whole session — which is what
        // `reset_clips`' own doc gives as its purpose.
        meter.reset_clips();

        let activity_at_play = self.activity()?;
        self.send_play()?;
        self.phase = VerifyPhase::Playing;

        // The capture, in three spans, so Window B genuinely opens at the
        // FIRST MARKER rather than at `play`: the file starts with the
        // lead-in's silence, and a window opened at `play` and closed a second
        // later would close before any audio arrived.
        let rate_f = f64::from(rate);
        let total = self.request.layout.total_len(sweep_len, rate_f);
        let tail = (self.request.timing.capture_tail_s * rate_f)
            .round()
            .max(0.0) as usize;
        let lead_in = self.request.layout.lead_in_frames(rate_f).min(total);
        let witness = ((self.request.timing.window_b_ms as f64 / 1000.0) * rate_f).round() as usize;
        let witness = witness.min(total + tail - lead_in);
        let rest = (total + tail) - lead_in - witness;

        let mut samples: Vec<f64> = Vec::with_capacity(total + tail);
        self.capture_span(lead_in, &mut meter, &mut samples)?;
        let activity_at_first_marker = self.activity()?;
        self.capture_span(witness, &mut meter, &mut samples)?;
        let activity_after_window = self.activity()?;

        // WINDOW B — the witness. `nonzero_blocks` must ADVANCE while the
        // helper plays: the helper is a different process and is therefore NOT
        // excluded from the tap, so the tap MUST see it. `callbacks` is the
        // wrong counter — a mic-capable default output can cycle the IOProc
        // with zeros.
        if activity_after_window.nonzero_blocks <= activity_at_first_marker.nonzero_blocks {
            return Err(VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::TapSilentDuringVerification,
            )));
        }
        let _ = activity_at_play;

        self.capture_span(rest, &mut meter, &mut samples)?;
        self.phase = VerifyPhase::Captured;

        // GATE 8, second read. Creating a second aggregate on a live device is
        // exactly when the HAL may renegotiate, and stale coefficients are the
        // wizard's own named failure.
        let running_rate_hz = self.engine.stream_rate_hz().unwrap_or(f64::NAN);
        // `!=` rather than a negated `==` because NaN compares unequal to
        // everything, which is the refusing arm we want.
        if running_rate_hz != self.stream_rate_hz {
            return Err(VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::OutputRateChangedDuringVerify {
                    expected_hz: self.stream_rate_hz,
                    observed_hz: running_rate_hz,
                },
            )));
        }
        // ... and on the RENDER DEVICE itself, which the engine cannot speak
        // for: the child's aggregate is another process's object, and the HAL
        // may renegotiate it independently of the engine's stream. The first
        // half of this gate read the child's ECHO of that rate before a sample
        // played; this reads the device directly, after.
        //
        // `None` is permissive HERE and nowhere else in this pass. The child is
        // normally finished by now and destroys its aggregate on the way out,
        // so "no such device" is the expected answer rather than a missing
        // witness — and the engine's own rate, which cannot vanish, has already
        // been checked one line above.
        if let Some(render_rate_hz) = self.devices.nominal_sample_rate(&ready.render_device_uid) {
            let expected_hz = f64::from(rate);
            // `!=` rather than a negated `==`: NaN compares unequal to
            // everything, which is the refusing arm we want.
            if render_rate_hz != expected_hz {
                return Err(VerifyError::Refused(Refusal::new(
                    MeasurementDiagnostic::OutputRateChangedDuringVerify {
                        expected_hz,
                        observed_hz: render_rate_hz,
                    },
                )));
            }
        }

        // The engine's own ±1.0 output clamp, as a DELTA across the window.
        // The NEAR side of the transducer; the meter above is the far side.
        let clipped = self
            .engine
            .clipped_samples()
            .saturating_sub(self.clipped_at_arm);
        if clipped > 0 {
            return Err(VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::VerificationChainClipped { count: clipped },
            )));
        }

        // The child's terminating line and its exit status.
        self.drain_to_exit()?;

        // `t = 0`. The verification template is the marker filtered through
        // the installed correction and scaled by the armed preamp, because the
        // marker traverses the cascade on its way to the mic; the baseline
        // passes the raw marker. That asymmetry is the ONLY difference between
        // the two paths and it is visible here, at the call site.
        let template = self.verification_template(rate_f);
        let aligned: Alignment = align::align(AlignRequest {
            capture: &samples,
            layout: &self.request.layout,
            sample_rate_hz: rate,
            sweep: &reference,
            sweep_len,
            template: &template,
        })
        .map_err(|d| VerifyError::Refused(Refusal::new(d)))?;
        self.phase = VerifyPhase::Analyzed;

        for warning in &aligned.warnings {
            self.log.push(SessionEvent::Warning {
                diagnostic: *warning,
            });
        }
        self.warnings.extend(aligned.warnings.iter().copied());

        let capture = capture_stats(&meter, &samples);
        let installed_preamp_db = self.recomputed_preamp_db;
        let outcome = VerifyOutcome {
            abort_acoustic_budget_ms,
            capture,
            gain_db: self.engine.gain_db(),
            installed_preamp_db,
            ir: aligned.ir,
            level_dbfs: level.dbfs_rms(),
            position_index: self.request.position_index,
            routing: self.request.routing,
            running_rate_hz,
            two_clock: Some(VerifyTwoClockFit {
                intercept_samples: aligned.fit.intercept_samples,
                residual_peak_samples: aligned.fit.residual_peak_samples,
                residual_rms_samples: aligned.fit.residual_rms_samples,
                skew_ppm: aligned.fit.skew_ppm,
            }),
            warnings: self.warnings.clone(),
        };

        self.teardown(None);
        // A teardown finding is not masked by a successful run: a leaked
        // render device means the user's output may still be wrapped by a
        // private aggregate, which is the one thing teardown exists to prevent.
        if let Some(blocking) = self.collected.iter().copied().find(|d| d.is_blocking()) {
            return Err(VerifyError::Refused(Refusal::new(blocking)));
        }
        let mut outcome = outcome;
        outcome.warnings = self.warnings.clone();
        Ok(outcome)
    }

    /// The correction-filtered matched-filter template.
    ///
    /// `peq.apply_offline(marker, running_rate) × preamp_lin`. Filtering the
    /// TEMPLATE rather than correcting the RESULT is unbiased by construction
    /// — the autocorrelation of `h*m` peaks at zero lag — and it handles
    /// magnitude shaping as well as phase. Correlating a raw template against
    /// a chain-shaped marker would bias the peak by the cascade's group delay
    /// in the marker's band, common-mode across both ends of the bracket, so
    /// it would land entirely in the intercept, which is exactly the number
    /// used as `t = 0`, and be invisible to a residual-scatter gate.
    fn verification_template(&self, sample_rate_hz: f64) -> Vec<f64> {
        let marker = layout_marker(&self.request.layout, sample_rate_hz);
        let Some(bands) = self.request.plan.bands.first() else {
            return marker;
        };
        let eq = ParametricEQ {
            bands: bands.clone(),
            sample_rate: sample_rate_hz,
        };
        let gain = f64::from(preamp_lin(self.recomputed_preamp_db));
        let mut filtered = eq.apply_offline(&marker, sample_rate_hz);
        for v in filtered.iter_mut() {
            *v *= gain;
        }
        filtered
    }

    /// Window A: realtime activity must be STATIONARY through the pre-roll.
    ///
    /// This is exactly why gate 2 cannot read `status == Running`: a
    /// stationary `nonzero_blocks` is the REQUIREMENT here and the
    /// DISQUALIFIER there. One of the two had to give, and it is the status
    /// read.
    fn window_a(&mut self) -> Result<(), VerifyError> {
        let start = self.activity()?;
        let deadline = Instant::now() + Duration::from_millis(self.request.timing.window_a_ms);
        loop {
            let now = self.activity()?;
            if now.nonzero_blocks != start.nonzero_blocks {
                return Err(VerifyError::Refused(Refusal::new(
                    MeasurementDiagnostic::SystemAudioNotQuiet,
                )));
            }
            if Instant::now() >= deadline {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(
                self.request.timing.tap_poll_ms.max(1),
            ));
        }
    }

    /// `None` means no realtime block to read — before the first start, or
    /// after a teardown. "Cannot witness" is not "witnessed nothing", so both
    /// windows refuse rather than assuming either way.
    fn activity(&mut self) -> Result<TapActivity, VerifyError> {
        self.engine.tap_activity().ok_or_else(|| {
            VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::TapSilentDuringVerification,
            ))
        })
    }

    fn capture_span(
        &mut self,
        frames: usize,
        meter: &mut CaptureMeter,
        out: &mut Vec<f64>,
    ) -> Result<(), VerifyError> {
        if frames == 0 {
            return Ok(());
        }
        let run = record(&mut *self.capture_source, frames, meter, &self.abort)?;
        out.extend_from_slice(&run.samples);
        match run.ended_early {
            None => Ok(()),
            // REW's 30 %-of-a-block rule. The SHIPPED code, reused: no new
            // number is minted for a failure this crate already names.
            Some(CaptureEnd::Clipping) => Err(VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::InputClipping,
            ))),
            Some(CaptureEnd::Aborted(reason)) => {
                Err(VerifyError::Refused(Refusal::new(reason.diagnostic())))
            }
            // The mic stream ended under us. Without a capture there is no SPL
            // witness and no residual.
            Some(CaptureEnd::SourceExhausted) => Err(VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::MicDisconnected,
            ))),
        }
    }

    fn send_play(&mut self) -> Result<(), VerifyError> {
        let timing = self.request.timing;
        let child = self.child.as_mut().expect("a spawned child");
        child.send_play().map_err(|e| {
            let _ = timing;
            VerifyError::Seam(e)
        })
    }

    /// Read lines until `ready`. EOF or a read failure before it means the
    /// child died during device open, which the two-phase start-up exists to
    /// turn into a clean refusal instead of five seconds of silence.
    fn read_ready(&mut self) -> Result<HelperReady, VerifyError> {
        let deadline = Duration::from_millis(self.request.timing.helper_ready_ms);
        let end = Instant::now() + deadline;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            let child = self.child.as_mut().expect("a spawned child");
            let line = match child.read_line(remaining) {
                Ok(HelperLine::Line(line)) => line,
                // The deadline, not a death: fall through to the timeout arm
                // below so a child that opens its device slowly is reported as
                // stalled rather than as having failed to start.
                Ok(HelperLine::DeadlineExpired) => {
                    return Err(VerifyError::Refused(Refusal::new(
                        MeasurementDiagnostic::HelperStalled,
                    )));
                }
                Ok(HelperLine::Eof) | Err(_) => {
                    self.child_exited = true;
                    return Err(VerifyError::Refused(Refusal::new(
                        MeasurementDiagnostic::HelperFailed { exit_code: -1 },
                    )));
                }
            };
            let fields = parse_line(&line)
                .ok_or_else(|| VerifyError::HelperProtocol { line: line.clone() })?;
            match str_field(&fields, "event") {
                Some("ready") => return HelperReady::from_fields(&fields, &line),
                Some("error") => {
                    let code = num_field(&fields, "code").unwrap_or(-1.0) as i32;
                    self.child_exited = true;
                    return Err(VerifyError::Refused(Refusal::new(
                        MeasurementDiagnostic::HelperFailed { exit_code: code },
                    )));
                }
                _ => {
                    if Instant::now() >= end {
                        return Err(VerifyError::Refused(Refusal::new(
                            MeasurementDiagnostic::HelperStalled,
                        )));
                    }
                }
            }
        }
    }

    /// GATE 8, first read. The child echoes BOTH rates because nothing
    /// establishes that a private aggregate's nominal rate equals its single
    /// sub-device's, and a rate refusal has to be able to say WHICH one
    /// disagreed.
    fn gate_rate_fence(&mut self, ready: &HelperReady, expected: u32) -> Result<(), VerifyError> {
        let expected_hz = f64::from(expected);
        for observed_hz in [ready.sample_rate_hz, ready.sub_device_sample_rate_hz] {
            // `!=` rather than a negated `==`: NaN compares unequal to
            // everything, which is the refusing arm we want.
            if observed_hz != expected_hz {
                return Err(VerifyError::Refused(Refusal::new(
                    MeasurementDiagnostic::OutputRateChangedDuringVerify {
                        expected_hz,
                        observed_hz,
                    },
                )));
            }
        }
        if ready.render_device_uid.is_empty() {
            return Err(VerifyError::Refused(Refusal::new(
                MeasurementDiagnostic::HelperFailed { exit_code: -1 },
            )));
        }
        Ok(())
    }

    /// Read status lines until the child reports `done` or `aborted`, then map
    /// its exit status.
    fn drain_to_exit(&mut self) -> Result<(), VerifyError> {
        let end = Instant::now() + Duration::from_millis(self.request.timing.helper_done_ms);
        let mut terminated = false;
        while !terminated {
            let remaining = end.saturating_duration_since(Instant::now());
            let child = self.child.as_mut().expect("a spawned child");
            match child.read_line(remaining) {
                Ok(HelperLine::Line(line)) => {
                    let fields = parse_line(&line)
                        .ok_or_else(|| VerifyError::HelperProtocol { line: line.clone() })?;
                    match str_field(&fields, "event") {
                        Some("done") => {
                            self.note_guard_counts(&fields);
                            terminated = true;
                        }
                        Some("aborted") => terminated = true,
                        Some("error") => {
                            let code = num_field(&fields, "code").unwrap_or(-1.0) as i32;
                            self.child_exited = true;
                            return Err(VerifyError::Refused(Refusal::new(
                                MeasurementDiagnostic::HelperFailed { exit_code: code },
                            )));
                        }
                        _ => {}
                    }
                }
                // EOF: the child exited without a terminating line. Its exit
                // status still speaks, so fall through to the reap below.
                Ok(HelperLine::Eof) => terminated = true,
                // The deadline expired with the child still holding the pipe
                // open: a wedged child, which the check below reports as
                // stalled. Distinguishing it from EOF is the point of the
                // three-state read — under the old two-state one a wedged
                // child read as a clean exit and its status was believed.
                Ok(HelperLine::DeadlineExpired) => {
                    return Err(VerifyError::Refused(Refusal::new(
                        MeasurementDiagnostic::HelperStalled,
                    )));
                }
                Err(_) => {
                    self.child_exited = true;
                    return Err(VerifyError::Refused(Refusal::new(
                        MeasurementDiagnostic::HelperStalled,
                    )));
                }
            }
            if !terminated && Instant::now() >= end {
                return Err(VerifyError::Refused(Refusal::new(
                    MeasurementDiagnostic::HelperStalled,
                )));
            }
        }
        self.child_exited = true;

        let child = self.child.as_mut().expect("a spawned child");
        let exit = child.reap().map_err(VerifyError::Seam)?;
        match exit_diagnostic(classify_exit(exit)) {
            None => Ok(()),
            Some(diagnostic) => Err(VerifyError::Refused(Refusal::new(diagnostic))),
        }
    }

    /// The child's MS-4 guard counts, mapped onto the shipped warnings. A
    /// verified stimulus produces zeros, so a nonzero count is an upstream
    /// defect the child contained rather than one it caused.
    fn note_guard_counts(&mut self, fields: &[(String, Value)]) {
        let make_clamped =
            |count| MeasurementDiagnostic::EmitClamped { count } as MeasurementDiagnostic;
        let make_sanitized = |count| MeasurementDiagnostic::EmitNonFiniteSanitized { count };
        let pairs: [(&str, &dyn Fn(u64) -> MeasurementDiagnostic); 2] =
            [("clamped", &make_clamped), ("sanitized", &make_sanitized)];
        for (key, make) in pairs {
            let count = num_field(fields, key).unwrap_or(0.0);
            if count > 0.0 {
                let warning = make(count as u64);
                self.log.push(SessionEvent::Warning {
                    diagnostic: warning,
                });
                self.warnings.push(warning);
            }
        }
    }

    // ────────────────────────────── teardown ────────────────────────────────

    /// MS-14's restore sequence, with the helper occupying the
    /// stimulus-stream position, on **every** exit path — command, drop,
    /// panic.
    ///
    /// ```text
    /// 1  ramp the helper (write `abort`)
    /// 1a wait ramp + one block + slack
    /// 1b SIGTERM — a SECOND ramp request, not a stop; the child's handler
    ///    arms the same abort flag `abort` does
    /// 1c SIGKILL — LAST, and only after both deadlines
    /// 1d reap, then check the render device is gone
    /// 2  stop the capture stream
    /// 4  restore system volume
    /// 4b restore the user's trim (drop the gain pin)
    /// 5b release the measurement lease
    /// 6  publish the terminating diagnostic and close the MS-23 log
    /// ```
    ///
    /// Guarded by `torn_down` so the whole sequence is idempotent; failures
    /// are COLLECTED into the log, never allowed to mask a later step. Every
    /// helper rung is safe on an already-dead child — a rung that panicked
    /// there would abort the ladder before the render device is destroyed,
    /// which is the exact failure the ladder exists to prevent.
    fn teardown(&mut self, diagnostic: Option<MeasurementDiagnostic>) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;

        self.stop_helper();

        if let Err(e) = self.capture_source.stop() {
            self.log.push(SessionEvent::SinkFault {
                error: e.to_string(),
            });
        }
        // R-B2. The read-back is taken HERE, at teardown, not reused from the
        // arming gate: the user may have moved the volume since, and the one
        // thing this must never do is move it back up.
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
        if let Some(mut pin) = self.gain_pin.take() {
            if let Err(e) = pin.restore() {
                self.log.push(SessionEvent::SinkFault {
                    error: e.to_string(),
                });
            }
        }
        // LAST: releasing the lease re-arms the fail-open watchdog, so nothing
        // that still needs the engine may run after this point.
        self.lease = None;

        let _ = std::fs::remove_file(&self.request.wav_path);

        let terminating =
            diagnostic.or_else(|| self.collected.iter().copied().find(|d| d.is_blocking()));
        for collected in self.collected.clone() {
            if collected.is_blocking() {
                if Some(collected) != terminating {
                    self.log.push(SessionEvent::SinkFault {
                        error: format!("{collected:?} (code {})", collected.code()),
                    });
                }
            } else {
                self.log.push(SessionEvent::Warning {
                    diagnostic: collected,
                });
                self.warnings.push(collected);
            }
        }

        self.phase = VerifyPhase::Terminated;
        self.log.push(SessionEvent::Terminated {
            diagnostic: terminating,
        });
    }

    fn stop_helper(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let pid = child.pid();
        if !self.child_exited {
            // Rung 1 — the POLITE one. The child runs the shared 5 ms raised
            // cosine rather than stopping, because a hard stop is itself a
            // full-scale click.
            if let Err(e) = child.request_abort() {
                self.log.push(SessionEvent::SinkFault {
                    error: e.to_string(),
                });
            }
            if !await_exit(&mut *child, self.ramp_deadline_ms) {
                // Rung 1b — SIGTERM. A SECOND ramp request, not a stop.
                if let Err(e) = child.request_terminate() {
                    self.log.push(SessionEvent::SinkFault {
                        error: e.to_string(),
                    });
                }
                if !await_exit(&mut *child, self.request.timing.sigterm_deadline_ms) {
                    // Rung 1c — SIGKILL, LAST.
                    if let Err(e) = child.kill() {
                        self.log.push(SessionEvent::SinkFault {
                            error: e.to_string(),
                        });
                    }
                    self.collected
                        .push(MeasurementDiagnostic::HelperKilledAfterRampDeadline);
                }
            }
        }
        // Rung 1d — reap, then the device-gone check. Asserting only the reap
        // is not enough: a reaped child that leaked an aggregate still
        // satisfies "no zombie" while leaving a private device wrapping the
        // user's output.
        if let Err(e) = child.reap() {
            self.log.push(SessionEvent::SinkFault {
                error: e.to_string(),
            });
        }
        // The real query, not an inference from the exit status. A SIGKILL
        // bypassing `Drop` is the LIKELIEST way the aggregate survives, but it
        // is neither necessary nor sufficient: a cleanly-exited child can still
        // have failed to destroy it, and the HAL can reclaim one whose creator
        // was killed. `device_exists` answers `true` when it cannot establish
        // the answer, so an unverifiable teardown reports a possible leak
        // rather than assuming a clean one.
        if self.devices.device_exists(&expected_render_device_uid(pid)) {
            self.collected
                .push(MeasurementDiagnostic::RenderDeviceLeaked { pid });
        }
    }

    // ────────────────────────────── plumbing ────────────────────────────────

    fn require(
        &self,
        required: VerifyPhase,
        method: &'static str,
        name: &'static str,
    ) -> Result<(), VerifyError> {
        if self.phase == required {
            Ok(())
        } else {
            Err(VerifyError::WrongPhase {
                actual: self.phase,
                method,
                required: name,
            })
        }
    }

    /// Terminate through the full restore sequence, then hand the caller the
    /// error. By the time one is observable the system is already back in its
    /// pre-measurement state.
    fn fail(&mut self, error: VerifyError) -> VerifyError {
        let diagnostic = match &error {
            VerifyError::Refused(refusal) => Some(refusal.diagnostic()),
            _ => None,
        };
        self.teardown(diagnostic);
        error
    }

    fn fail_seam(&mut self, error: MeasureError) -> VerifyError {
        self.log.push(SessionEvent::SinkFault {
            error: error.to_string(),
        });
        self.fail(VerifyError::Seam(error))
    }

    fn refuse(&mut self, diagnostic: MeasurementDiagnostic) -> VerifyError {
        self.fail(VerifyError::Refused(Refusal::new(diagnostic)))
    }
}

impl std::fmt::Debug for VerificationPass {
    /// Manual: the boxed seams are not `Debug`. Shows the machine's state,
    /// which is what an `unwrap_err` in a test wants to print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerificationPass")
            .field("l_verify", &self.l_verify)
            .field("phase", &self.phase)
            .field("pre_volume", &self.pre_volume)
            .field("torn_down", &self.torn_down)
            .finish_non_exhaustive()
    }
}

impl Drop for VerificationPass {
    /// Every exit path — command, drop, panic — runs the full sequence.
    fn drop(&mut self) {
        self.teardown(None);
    }
}

/// Has the child exited within `budget_ms`?
///
/// The probe is [`HelperProcess::try_reap`] — the process's own status — and
/// not stdout. Stdout is a proxy that is wrong in both directions: a child that
/// closed its pipe while still rendering reads as dead (so the ladder stops
/// before SIGTERM and leaves it playing), and a child that keeps emitting
/// progress lines past the deadline reads as alive forever. The reads are still
/// made, because they drain the pipe — a child blocked writing into a full
/// stdout buffer can never reach its own teardown — but the ANSWER comes from
/// the status.
fn await_exit(child: &mut dyn HelperProcess, budget_ms: u64) -> bool {
    let end = Instant::now() + Duration::from_millis(budget_ms);
    loop {
        match child.try_reap() {
            Ok(Some(_)) => return true,
            // The status cannot be read at all. The child is unreachable, so
            // escalating the ladder is the safe direction: a SIGTERM to a
            // corpse is harmless, and stopping here on a live child is not.
            Err(_) => return false,
            Ok(None) => {}
        }
        if Instant::now() >= end {
            return false;
        }
        let remaining = end.saturating_duration_since(Instant::now());
        // Drain whatever is queued, bounded by what is left of the budget.
        match child.read_line(remaining) {
            Ok(HelperLine::Line(_)) => {}
            // Nothing more will arrive on this pipe. One last status read, so
            // a child that closed stdout microseconds before exiting is not
            // reported as wedged.
            Ok(HelperLine::DeadlineExpired) | Ok(HelperLine::Eof) | Err(_) => {
                return matches!(child.try_reap(), Ok(Some(_)));
            }
        }
    }
}

/// `-max(0, peak of the realized cascade)` per channel, folded `min` across
/// channels, plus the peak itself.
///
/// The fold is `min` and the reason is the engine's own: **the worst channel
/// wins** — a stereo config must not clip on the loud side because the quiet
/// side needed less headroom. The peak is returned separately because it is
/// `peak_correction_gain_db`, with the preamp EXCLUDED: a safety interlock may
/// not be conditioned on the hypothesis it is testing.
fn recompute_preamp(bands: &[Vec<EQBand>], rate_hz: f64) -> (f64, f64) {
    let mut preamp_db = 0.0f64;
    let mut peak_db = f64::NEG_INFINITY;
    for channel in bands {
        let eq = ParametricEQ {
            bands: channel.clone(),
            sample_rate: rate_hz,
        };
        preamp_db = preamp_db.min(eq.preamp_db());
        let grid = eq.preamp_grid();
        let channel_peak = eq
            .realized_response(&grid, rate_hz)
            .into_iter()
            .fold(f64::NEG_INFINITY, f64::max);
        peak_db = peak_db.max(channel_peak);
    }
    if !peak_db.is_finite() {
        peak_db = 0.0;
    }
    (preamp_db, peak_db)
}

/// dB to linear, with the engine's own two guards transcribed rather than
/// imported: a non-finite or non-negative preamp is unity.
///
/// Transcribed because MS-1 forbids `paraeq-measure` from naming
/// `paraeq-engine`. Both halves matter — without the `>= 0.0` guard a
/// pure-cut correction's exactly-`0.0` preamp would round-trip through
/// `powf` rather than being exactly 1.0, and the gate-2 compare is against a
/// number the engine produced with these guards applied.
fn preamp_lin(preamp_db: f64) -> f32 {
    if !preamp_db.is_finite() || preamp_db >= 0.0 {
        return 1.0;
    }
    10f64.powf(preamp_db / 20.0) as f32
}

/// The meter's three numbers, floored so a silent pass cannot put `-inf` on a
/// wire. See [`CAPTURE_FLOOR_DBFS`].
fn capture_stats(meter: &CaptureMeter, samples: &[f64]) -> VerifyCaptureStats {
    let rms = if samples.is_empty() {
        0.0
    } else {
        (samples.iter().map(|v| v * v).sum::<f64>() / samples.len() as f64).sqrt()
    };
    let rms_dbfs = if rms > 0.0 {
        20.0 * rms.log10()
    } else {
        f64::NEG_INFINITY
    };
    VerifyCaptureStats {
        clipped_samples: meter.clipped_samples(),
        peak_dbfs: floor_dbfs(meter.peak_dbfs()),
        rms_dbfs: floor_dbfs(rms_dbfs),
    }
}

fn floor_dbfs(value: f64) -> f64 {
    if value.is_finite() && value > CAPTURE_FLOOR_DBFS {
        value
    } else {
        CAPTURE_FLOOR_DBFS
    }
}

/// Write the file the child plays.
///
/// 32-bit float, mono — identical to `store.rs`'s spec, which is the repo's
/// only shipped WAV writer, so there is ONE convention and not two. A
/// quantizing format would move `L_verify`, which the level book makes exact
/// to 1e-12.
fn write_wav(path: &Path, stimulus: &AssembledStimulus) -> Result<(), VerifyError> {
    let spec = hound::WavSpec {
        bits_per_sample: 32,
        channels: 1,
        sample_format: hound::SampleFormat::Float,
        sample_rate: stimulus.sample_rate_hz(),
    };
    let wav = |source: hound::Error| VerifyError::Wav {
        path: path.to_path_buf(),
        reason: source.to_string(),
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| VerifyError::Wav {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
        }
    }
    let mut writer = hound::WavWriter::create(path, spec).map_err(wav)?;
    for &v in stimulus.samples() {
        // f32 is the storage format the spec chose. The cast is the lossy step
        // and it is the intended one — and it is the ONLY transformation
        // between the assembled buffer and the file, which is what makes "the
        // WAV is the assembled stimulus bit for bit" a meaningful claim.
        writer.write_sample(v as f32).map_err(wav)?;
    }
    writer.finalize().map_err(wav)?;
    Ok(())
}

// ─────────────────────────── the child's protocol ───────────────────────────

/// The child's `ready` line, which is the only thing the parent's device
/// fences may read: the parent never resolves a device itself.
#[derive(Clone, Debug, PartialEq)]
pub struct HelperReady {
    pub channels: u32,
    /// The PHYSICAL output device's UID — the route the parent taps.
    pub device_uid: String,
    pub frames_per_block: usize,
    /// The render wrapper's UID. The rate fence reads THIS device, not the
    /// default output, or the two sides are talking about different devices.
    pub render_device_uid: String,
    pub routing: String,
    /// The render wrapper's nominal rate.
    pub sample_rate_hz: f64,
    /// The physical output's own nominal rate. Both are carried because
    /// nothing establishes that a private aggregate's nominal rate equals its
    /// single sub-device's, and a refusal has to name which one disagreed.
    pub sub_device_sample_rate_hz: f64,
}

impl HelperReady {
    fn from_fields(fields: &[(String, Value)], line: &str) -> Result<HelperReady, VerifyError> {
        let missing = || VerifyError::HelperProtocol {
            line: line.to_owned(),
        };
        Ok(HelperReady {
            channels: num_field(fields, "channels").ok_or_else(missing)? as u32,
            device_uid: str_field(fields, "device_uid")
                .ok_or_else(missing)?
                .to_owned(),
            frames_per_block: num_field(fields, "frames_per_block").ok_or_else(missing)? as usize,
            render_device_uid: str_field(fields, "render_device_uid")
                .ok_or_else(missing)?
                .to_owned(),
            routing: str_field(fields, "routing").ok_or_else(missing)?.to_owned(),
            sample_rate_hz: num_field(fields, "sample_rate_hz").ok_or_else(missing)?,
            sub_device_sample_rate_hz: num_field(fields, "sub_device_sample_rate_hz")
                .ok_or_else(missing)?,
        })
    }
}

/// How the child ended, as the parent reads it.
///
/// A mirror of the child's own closed exit-code table, declared here because
/// `paraeq-measure` may not depend on the helper crate: it is a `[[bin]]`
/// package and depending on one is not a thing Cargo does. The numbers are the
/// contract; [`classify_exit`] is the only place they appear.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelperExitKind {
    /// Exit 0 — played to the end, or aborted cleanly. An abort is NOT a
    /// failure: it ramped, it tore down, and it destroyed its render device.
    Ok,
    /// Exit 2 — bad arguments, including a channel index at or above the
    /// device's reported count.
    BadArgs,
    /// Exit 3 — the WAV is missing, is not mono 32-bit float, or its rate
    /// disagrees with the device.
    Wav,
    /// Exit 4 — no device by that UID, or it could not be opened.
    DeviceNotFound,
    /// Exit 5 — the device stopped cycling.
    Stalled,
    /// Exit 6 — the child's own level backstop refused the file, BEFORE any
    /// device was opened.
    BackstopRefused,
    /// Exit 7 — the device it opened is not the current default output. The
    /// tap watches the default output, so rendering anywhere else measures a
    /// route the engine is not on.
    NotDefaultOutput,
    /// Killed by a signal. 101 and above are panics and signals, which is why
    /// nothing in the table above uses them.
    Signalled,
    /// A number outside the closed table. Itself a finding.
    Unknown(i32),
}

/// The child's status, classified. `signalled` wins over any code, because a
/// killed child's code says nothing about what it managed to clean up.
pub fn classify_exit(exit: HelperExit) -> HelperExitKind {
    if exit.signalled {
        return HelperExitKind::Signalled;
    }
    match exit.code {
        Some(0) => HelperExitKind::Ok,
        Some(2) => HelperExitKind::BadArgs,
        Some(3) => HelperExitKind::Wav,
        Some(4) => HelperExitKind::DeviceNotFound,
        Some(5) => HelperExitKind::Stalled,
        Some(6) => HelperExitKind::BackstopRefused,
        Some(7) => HelperExitKind::NotDefaultOutput,
        Some(other) => HelperExitKind::Unknown(other),
        None => HelperExitKind::Signalled,
    }
}

/// What each exit means to the parent. **Exhaustive on purpose**: a new child
/// exit code cannot appear without a decision here, and a number that quietly
/// changed meaning would move a refusal onto the wrong cause.
pub fn exit_diagnostic(kind: HelperExitKind) -> Option<MeasurementDiagnostic> {
    match kind {
        HelperExitKind::Ok => None,
        // A killed child is reported by the teardown ladder, which knows
        // whether it killed it and whether the render device survived. The
        // exit status alone adds nothing.
        HelperExitKind::Signalled => None,
        HelperExitKind::BadArgs => Some(MeasurementDiagnostic::HelperFailed { exit_code: 2 }),
        HelperExitKind::Wav => Some(MeasurementDiagnostic::HelperFailed { exit_code: 3 }),
        HelperExitKind::DeviceNotFound => Some(MeasurementDiagnostic::HelperUnavailable),
        HelperExitKind::Stalled => Some(MeasurementDiagnostic::HelperStalled),
        HelperExitKind::BackstopRefused => {
            Some(MeasurementDiagnostic::HelperFailed { exit_code: 6 })
        }
        // The route moved under us: the child opened a device that is no
        // longer the default output, so the tap is not on it.
        HelperExitKind::NotDefaultOutput => Some(MeasurementDiagnostic::OutputDeviceChanged),
        HelperExitKind::Unknown(code) => {
            Some(MeasurementDiagnostic::HelperFailed { exit_code: code })
        }
    }
}

/// One value of a child status line.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Num(f64),
    Str(String),
}

/// Parse one FLAT JSON object — the only shape the child emits.
///
/// Hand-written because `paraeq-measure`'s dependencies are `paraeq-dsp`,
/// `hound` and `thiserror` and nothing else: a JSON library is not on that
/// list, and the alternative to twelve lines of scanning would be a
/// dependency-rule change for a protocol of six field types.
///
/// **Nested objects and arrays return `None` rather than being skipped.** The
/// child emits none, so one appearing means the protocol moved, and a parser
/// that silently ignored the part it did not understand would hand a gate a
/// field from the wrong nesting level. `null` is refused for the same reason.
pub fn parse_line(line: &str) -> Option<Vec<(String, Value)>> {
    let bytes: Vec<char> = line.trim().chars().collect();
    let mut i = 0usize;
    skip_ws(&bytes, &mut i);
    if bytes.get(i) != Some(&'{') {
        return None;
    }
    i += 1;
    let mut out: Vec<(String, Value)> = Vec::new();
    loop {
        skip_ws(&bytes, &mut i);
        if bytes.get(i) == Some(&'}') {
            i += 1;
            skip_ws(&bytes, &mut i);
            return if i == bytes.len() { Some(out) } else { None };
        }
        if !out.is_empty() {
            if bytes.get(i) != Some(&',') {
                return None;
            }
            i += 1;
            skip_ws(&bytes, &mut i);
        }
        let key = read_string(&bytes, &mut i)?;
        skip_ws(&bytes, &mut i);
        if bytes.get(i) != Some(&':') {
            return None;
        }
        i += 1;
        skip_ws(&bytes, &mut i);
        let value = read_value(&bytes, &mut i)?;
        out.push((key, value));
    }
}

fn skip_ws(bytes: &[char], i: &mut usize) {
    while matches!(bytes.get(*i), Some(c) if c.is_whitespace()) {
        *i += 1;
    }
}

fn read_string(bytes: &[char], i: &mut usize) -> Option<String> {
    if bytes.get(*i) != Some(&'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    loop {
        let c = *bytes.get(*i)?;
        *i += 1;
        match c {
            '"' => return Some(out),
            '\\' => {
                let esc = *bytes.get(*i)?;
                *i += 1;
                out.push(match esc {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            code = code * 16 + (*bytes.get(*i)?).to_digit(16)?;
                            *i += 1;
                        }
                        char::from_u32(code)?
                    }
                    _ => return None,
                });
            }
            other => out.push(other),
        }
    }
}

fn read_value(bytes: &[char], i: &mut usize) -> Option<Value> {
    match bytes.get(*i)? {
        '"' => read_string(bytes, i).map(Value::Str),
        '{' | '[' => None,
        't' | 'f' | 'n' => {
            let start = *i;
            while matches!(bytes.get(*i), Some(c) if c.is_ascii_alphabetic()) {
                *i += 1;
            }
            let word: String = bytes[start..*i].iter().collect();
            match word.as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                _ => None,
            }
        }
        _ => {
            let start = *i;
            while matches!(bytes.get(*i), Some(c) if !c.is_whitespace() && *c != ',' && *c != '}') {
                *i += 1;
            }
            let text: String = bytes[start..*i].iter().collect();
            text.parse::<f64>().ok().map(Value::Num)
        }
    }
}

/// One string field of a parsed line, or `None` if absent or not a string.
pub fn str_field<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|(k, v)| match (k.as_str() == key, v) {
            (true, Value::Str(s)) => Some(s.as_str()),
            _ => None,
        })
}

/// One numeric field of a parsed line, or `None` if absent or not a number.
pub fn num_field(fields: &[(String, Value)], key: &str) -> Option<f64> {
    fields
        .iter()
        .find_map(|(k, v)| match (k.as_str() == key, v) {
            (true, Value::Num(n)) => Some(*n),
            _ => None,
        })
}
