//! `MeasurementDiagnostic` (MS-20): the stable numbered outcome vocabulary,
//! and the MS-9 refusal framework that makes "fall back to an uncapped sweep"
//! unrepresentable.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**, per the four-tier
//! convention and the `spline.rs:1-5` precedent. Diagnostic codes, severities
//! and user strings are product policy; the prototype has nothing to say about
//! them. What is pinned instead: the wire numbers of every variant, the
//! refusal-vs-warn split row-for-row against the safety spec, both user
//! strings non-empty for every variant, and an exhaustive match with no
//! wildcard so that adding a variant breaks the test deliberately.
//!
//! # Numbering contract (wire/log stability)
//!
//! The number is [`MeasurementDiagnostic::code`]'s value, mirrored by the
//! declaration's explicit discriminants (kept adjacent in this file — they
//! must move together). Blocking errors number from 1, warnings from 100, as
//! a reading aid only — [`MeasurementDiagnostic::severity`] is the authority.
//! Each block is **append-only**: a new variant takes the next free number in
//! its block, and a number, once assigned, is **never reused and never
//! reordered**. Session logs and any future wire consumer key on these
//! numbers; renumbering is a breaking change to recorded sessions and is
//! forbidden. Additions are cheap — that is the point of the contract.
//!
//! # Sources
//!
//! Every variant is a named condition in
//! `docs/specs/2026-07-15-measurement-safety-design.md`: the "Hard Caps and
//! Refusals" table, the level-ladder escalation aborts (§ The Level Ladder,
//! step 5), the MS-3 post-fade assertion set and MS-4 emit guard (§ Fade and
//! DC; enforced in [`crate::stimulus`]), the SNR criterion's warn row, the
//! volume logic's `FixedMaxVolume`, and § The Two-Clock Complication's
//! `Warn(TwoClock)` fallback. The spec's single `NoMicSensitivity` row is
//! split into its two causes ([`MeasurementDiagnostic::SensitivityMissing`] /
//! [`MeasurementDiagnostic::SensitivityUnparseable`]) because the user's
//! remedy differs; both refuse (MS-9).
//!
//! The one variant sourced elsewhere is
//! [`MeasurementDiagnostic::EngineNotRunning`], which comes from
//! `docs/specs/2026-07-15-wizard-design.md` (§ The fail-open hazard and § The
//! fail-open watchdog, plus "engine lease" in the `Probe` box of the spine):
//! the wizard requires a running engine, and the safety spec never names the
//! condition because it assumes one. **That variant's own doc was written
//! against a literal `status == Running` reading and is re-pointed there at
//! the predicate the verification gate actually evaluates** — see its doc
//! comment, and [`crate::verify`]'s header for the argument.
//!
//! # The verification block (25–40, 105–107)
//!
//! Stage 6 appends nineteen codes for the closed-loop verification pass:
//! sixteen blocking errors and three warnings. Two rules about them, stated
//! once here so the next reader neither mints more nor deletes one:
//!
//! - **Nothing is minted for a failure this crate already names.** A
//!   disengaged engine reuses [`MeasurementDiagnostic::EngineNotRunning`]
//!   (24); a railed microphone reuses
//!   [`MeasurementDiagnostic::InputClipping`] (12); a level the sole
//!   constructor refuses reuses
//!   [`MeasurementDiagnostic::SolvedLevelIllegal`] (23); a lost baseline
//!   witness reuses [`MeasurementDiagnostic::SelfExclusionUnavailable`] (1).
//! - **Exactly ONE of the nineteen has a `paraeq-decide` twin**:
//!   [`MeasurementDiagnostic::VerificationPreampMismatch`] (40), because
//!   `decide()` re-checks a number the pass CARRIES rather than trusting it.
//!   Every other verification code refuses **before** the capture, so no
//!   bundle carrying that failure can exist for `decide()` to see, and none
//!   of them needs a twin.

/// The blocking/non-blocking split the spec draws between "Refuse — do not
/// warn, do not degrade, do not escalate" and "proceed, with a warning".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    /// Blocking. The run (or the escalation, or the sweep) does not proceed.
    Error,
    /// Non-blocking. The run proceeds; the condition is surfaced and logged.
    Warning,
}

/// Every named measurement outcome representable at this stage, numbered for
/// the session log and any future wire consumer. See the module header for
/// the append-only numbering contract.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u16)]
pub enum MeasurementDiagnostic {
    // ── Blocking errors (refusals and defects): 1… ──────────────────────────
    /// Tap self-exclusion not established (`translate_pid` fell back to an
    /// empty exclusion list): the stimulus topology is a different,
    /// unvalidated one — the sweep would be muted at the device and processed
    /// by the chain. Refuse, do not adapt (spec § Invariant, consequence 3;
    /// MS-6).
    SelfExclusionUnavailable = 1,
    /// No cal file loaded, so no Sens Factor. Half of the spec's
    /// `NoMicSensitivity` row: a missing sensitivity must never fall back to
    /// an uncapped sweep (MS-9). No SPL, no closed loop, no safety.
    SensitivityMissing = 2,
    /// A cal file is present but its sensitivity could not be parsed (or
    /// parsed non-finite). The other half of `NoMicSensitivity`; same
    /// consequence, different user remedy.
    SensitivityUnparseable = 3,
    /// Mic not identified / not in the validated list and not manually
    /// confirmed. Sensitivity is meaningless without knowing what produced it.
    MicUnidentified = 4,
    /// The class's SPL cap exceeds the mic's full-scale SPL, so the cap could
    /// never fire — REW's rule that an SPL limit above the input's clipping
    /// point offers no protection, enforced at cal load (MS-10).
    CapExceedsMicFullScale = 5,
    /// Solved chain sensitivity outside the class envelope: an empty jig, a
    /// misrouted output, or the wrong class. Refuse rather than escalate —
    /// escalating into an empty jig is precisely how the 122 dB scenario
    /// happens (MS-17).
    SensitivityOutOfEnvelope = 6,
    /// Volume is uncontrollable **and** no SPL solve is available: no
    /// protection exists at all (spec § Hard Caps, volume logic, case 3).
    VolumeUncontrollable = 7,
    /// Projected SPL exceeds the class cap before a single sample is emitted.
    /// The cheapest possible refusal.
    ProjectedSplOverCap = 8,
    /// Escalation abort: a rung's measured SPL exceeded the class cap during
    /// the ≤6 dB climb (ladder step 5). Named separately from [`Self::SplOverCap`]
    /// because the ladder and the sweep abort at different stages of the run.
    RungOverCap = 9,
    /// Escalation abort: measured SPL deviates >6 dB from projection at a
    /// rung — the signature of a wrong device, a stale cal file, or a broken
    /// chain.
    SplProjectionMismatch = 10,
    /// Measured SPL exceeded the cap mid-sweep; the abort path ran.
    SplOverCap = 11,
    /// More than 30% of samples in an input block clipped (REW's rule); the
    /// measurement is invalid regardless of level.
    ///
    /// This is the **mic's ADC**, on the FAR side of the transducer, and the
    /// verification pass reuses it rather than minting a code for a failure
    /// this crate already names (MS-21, via [`crate::capture::CaptureMeter`]
    /// and its per-attempt `reset_clips`). Distinct from
    /// [`Self::VerificationChainClipped`], which reads the engine's own ±1.0
    /// **output** clamp on the NEAR side: both can fire, neither implies the
    /// other, and a residual computed over a railed capture is meaningless
    /// whichever one is silent.
    InputClipping = 12,
    /// The SNR gate was still missed after ≤2 automatic remedies — and
    /// remedies are input gain and length, never output level.
    SnrUnachievable = 13,
    /// Stimulus defect (MS-3): an assembled buffer does not start and end at
    /// exactly 0.0. Never emitted by a correct assembly; a defect, not a user
    /// error.
    StimulusEndpointNonzero = 14,
    /// Stimulus defect (MS-3): the applied fade-out envelope is not
    /// monotonically non-increasing.
    StimulusFadeNotMonotone = 15,
    /// Stimulus defect (MS-3): |mean| ≥ 1e-4 after the DC-block.
    StimulusDcOffset { mean: f64 } = 16,
    /// Stimulus defect (MS-3): a sample exceeds full scale after scaling.
    /// A clamp here is counted as a **defect**, never silently applied — it
    /// means the level solve is wrong, and clamping-and-continuing would play
    /// a distorted stimulus at an unintended level.
    StimulusOverFullScale { peak: f64 } = 17,
    /// Stimulus defect (MS-3): non-finite samples in an assembled buffer.
    StimulusNonFinite { count: u64 } = 18,
    /// The run was stopped by request — Esc, Space, or window close (§ Abort
    /// Guards, first trigger row). The abort ramp and the full restore
    /// sequence ran; nothing about the chain is suspect.
    UserAborted = 19,
    /// The measurement microphone disappeared mid-run (§ Abort Guards).
    /// Without the mic there is no SPL witness, so the closed loop the caps
    /// depend on is gone and the run aborts.
    MicDisconnected = 20,
    /// The output device changed or died mid-run (§ Abort Guards). A solved
    /// level is valid only for the chain it was solved on; a new device is an
    /// uncharacterized chain, so the run aborts rather than playing into it.
    OutputDeviceChanged = 21,
    /// The engine entered `Failed` mid-run (§ Abort Guards,
    /// `EngineStatus::Failed`): the audio topology is no longer known-good,
    /// so the run aborts.
    EngineFailed = 22,
    /// The solved level is a legal projected SPL but not a legal output level:
    /// after the MS-11 margin it lands above the class's dBFS cap or above the
    /// −3 dBFS RMS ceiling, so `SweepLevel::new` refuses it. Distinct from
    /// [`Self::ProjectedSplOverCap`] (which is the SPL projection, not the
    /// dBFS level) — a very insensitive chain can project an in-cap SPL yet
    /// demand a level ParaEQ will not emit.
    SolvedLevelIllegal = 23,
    /// ParaEQ's engine is not running — switched off by the user, or
    /// auto-disabled by the fail-open watchdog (wizard § The fail-open
    /// watchdog). Distinct from [`Self::EngineFailed`]: nothing is broken.
    /// Distinct from [`Self::SelfExclusionUnavailable`] too, which is what a
    /// stopped engine would otherwise surface as — with no session there is
    /// no tap and therefore no exclusion witness — and whose remedy is
    /// "Restart ParaEQ", the wrong instruction for a switch the user can
    /// flip. Measurement needs the engine: the MS-6 witness only exists while
    /// a tap does, and the verification pass plays through the correction
    /// chain (MS-19).
    ///
    /// **What "not running" means at the verification gate, stated here
    /// because the code and its own diagnostic must not disagree.** The gate
    /// reads [`EngineFacts::engine_engaged()`](crate::engine_seam::EngineFacts::engine_engaged)
    /// — `enabled && stream.is_some() && status ∉ {Stopped, Failed,
    /// AutoDisabledNoInput}` — and **not** `status == EngineStatus::Running`.
    /// The shipped watchdog sets `Running` only while `nonzero_blocks` is
    /// advancing, and it decays to `InputSilent` and then `Idle` on a quiet
    /// machine; the verification pre-roll *requires* a quiet machine, because
    /// the tap is global and excludes only ParaEQ, so any other application's
    /// audio would land in the corrected capture. A literal-`Running` gate
    /// would therefore refuse a perfectly healthy engine on every run. This
    /// variant landed under that literal reading and is re-pointed here.
    ///
    /// The verification gate also refuses with this code for `bypass == true`
    /// and for "no correction installed": all three are the same sentence to
    /// the user — the chain is not running the correction we came to verify.
    EngineNotRunning = 24,

    // ── Verification loop (Stage 6): 25… ────────────────────────────────────
    /// The verification helper could not be started, or the device it was
    /// pointed at does not exist. Nothing played.
    HelperUnavailable = 25,
    /// The helper started but ended badly: bad arguments, a WAV it refused, or
    /// its own level backstop. `exit_code` is the child's own number, which is
    /// a closed table on its side, so a value outside it is itself a finding.
    HelperFailed { exit_code: i32 } = 26,
    /// The helper stopped producing status lines, or its device stopped
    /// cycling. The teardown ladder still ran: ramp request, SIGTERM, then
    /// SIGKILL only after both deadlines.
    HelperStalled = 27,
    /// A timing-marker fit FORMED but scattered: the per-marker residual peak
    /// is past the credibility ladder's refuse bound, so `t = 0` is not
    /// trustworthy and the impulse response would be cut at the wrong instant.
    ///
    /// Distinct from [`Self::VerificationMarkersNotCredible`], which is "no
    /// credible marker train at all" — a different remedy (raise the level or
    /// quiet the room, versus a clock problem).
    VerificationMarkersNotFound { residual_peak_samples: f64 } = 28,
    /// The user's trim did not read back as 0.0 after the verification pass
    /// pinned it. Refuse rather than proceed: an unpinned trim of +10 dB
    /// against a +6 dB peak boost makes the verification sweep LOUDER than the
    /// baseline the level ladder validated — a safety failure, not an
    /// accounting one.
    VerificationGainNotPinned { read_back_db: f64 } = 29,
    /// `L_verify` sits too close to the measured noise floor to produce a
    /// trustworthy residual. Refused **before spawning**, rather than emitting
    /// an unusably quiet sweep and reporting a shape failure afterwards.
    ///
    /// Never remedied by raising the level — MS-8: "≤2 automatic remedies, and
    /// **remedies never touch output level**".
    VerificationLevelBelowSnrBudget { projected_snr_db: f64 } = 30,
    /// The tap saw no audio while the helper was playing — the genuine TCC
    /// silent failure the wizard's fail-open section calls "still a `Refuse`".
    ///
    /// Also raised when the realtime activity counters cannot be read at all
    /// (`tap_activity()` is `None`: no realtime block, so no session). "Cannot
    /// witness" is not "witnessed nothing", and the pass refuses rather than
    /// assuming either way.
    TapSilentDuringVerification = 31,
    /// Something else was playing during the verification pre-roll. The tap is
    /// global and excludes only ParaEQ, so another application's audio lands
    /// in the corrected capture and the residual measures that instead.
    SystemAudioNotQuiet = 32,
    /// **Two triggers, one code.** (i) The helper echoed a `--channel` that is
    /// not the baseline's routing for this position, so the verification
    /// capture and the baseline are not the same measurement. (ii) The routing
    /// is `Both` while the plan's per-channel band sets differ, so the capture
    /// heard the SUM of two differently-EQ'd channels and no single predicted
    /// response exists to difference it against. Both refuse before `play`.
    HelperRoutingMismatch = 33,
    /// **Two triggers, one code.** (i) Before spawning: the engine reports
    /// `correction_rate_mismatch`, i.e. the chain is running coefficients
    /// designed for a different rate — the wizard's own named failure, "stale
    /// coefficients after a rate change". (ii) During the pass: the rate of
    /// the device the child actually opened disagrees with the rate the WAV
    /// was generated at, or moves while the measurement aggregate is created.
    /// Creating a second aggregate on a live device is exactly when the HAL
    /// may renegotiate.
    OutputRateChangedDuringVerify { expected_hz: f64, observed_hz: f64 } = 34,
    /// The engine's own ±1.0 output clamp fired during the verification
    /// window: the corrected chain clipped, which is on the list of things
    /// verification exists to catch, and "the chain clipped" is a far better
    /// sentence than "the residual was too high".
    ///
    /// This is the **NEAR** side of the transducer. The mic's ADC is
    /// [`Self::InputClipping`]; each names the other so a later reader does
    /// not collapse two facts into one.
    VerificationChainClipped { count: u64 } = 35,
    /// No credible timing-marker train was found at all: the matched filter's
    /// picks did not clear their own off-peak floor, so no fit was formed.
    ///
    /// This is the FIRST thing that fails when the verification sweep is
    /// quiet, which is why it gets its own code and its own remedy rather than
    /// the generic not-found ([`Self::VerificationMarkersNotFound`], which is
    /// a fit that formed and scattered).
    ///
    /// Also raised when the aligned slice carries no direct arrival at all —
    /// the deconvolution finds no peak because the capture is digitally
    /// silent or non-finite. Same user-visible cause, same remedy: nothing
    /// usable reached the microphone.
    VerificationMarkersNotCredible = 36,
    /// A clock-skew fit formed but asks for an adjustment past the reject
    /// bound. Four markers on a straight line fit a straight line perfectly no
    /// matter how steep it is, so a residual-only ladder would bless an absurd
    /// ppm and then stretch the capture on the strength of it.
    ClockAdjustTooLarge { skew_ppm: f64 } = 37,
    /// The helper's private render device may have survived its own process.
    ///
    /// SIGKILL bypasses `Drop`, so the child's render-aggregate RAII never
    /// ran; a reaped child that leaked an aggregate still satisfies "no
    /// zombie" while leaving a private device wrapping the user's output. The
    /// expected UID is `com.paraeq.render.<pid>`. Collected, never masked, and
    /// it never aborts the rest of the teardown ladder.
    RenderDeviceLeaked { pid: u32 } = 38,
    /// The engine could not design every band of the plan at the live rate and
    /// dropped some. A partially-installed cascade is not the plan's cascade,
    /// and verifying it would grade the wrong object — the residual would be
    /// nonzero and would blame the chain for our own prediction fault.
    VerificationBandsDropped { dropped: u64, rate_hz: f64 } = 39,
    /// The engine's ARMED preamp disagrees with the gate's own recomputation
    /// of it at the live rate over the plan's bands.
    ///
    /// Also raised when a correction is installed but the armed preamp reads
    /// `None`: a missing preamp and a unity preamp predict different
    /// responses, and defaulting the first to the second is precisely the
    /// failure verification exists to catch.
    ///
    /// **One condition, two crates, two codes with the same name — read both
    /// before deleting either.** THIS code is what `paraeq-measure` refuses
    /// with **before the capture**, at the verify gate; no bundle exists yet.
    /// `DiagnosticCode::VerificationPreampMismatch` (`paraeq-decide`) is what
    /// `decide()` refuses with **after the fact**, when a bundle arrives
    /// carrying an `installed_preamp_db` its own bands do not reproduce.
    /// `decide()` re-checks rather than trusts, which is the only reason one
    /// condition is reachable from two sides; every other verification code
    /// here refuses before the capture, so no bundle carrying that failure can
    /// exist for `decide()` to see, and none of them needs a twin.
    VerificationPreampMismatch { armed_db: f64, recomputed_db: f64 } = 40,

    // ── Non-blocking warnings: 100… ─────────────────────────────────────────
    /// SNR accepted on the degraded row (median ≥ 30 dB but below the
    /// 40 dB / min-20 dB accept row).
    LowSnr = 100,
    /// Volume is readable but not settable and reads at maximum; the SPL
    /// solve still characterizes the actual acoustic result, so the caps
    /// still bind. Proceed, warned (spec § Hard Caps, volume logic, case 2).
    FixedMaxVolume = 101,
    /// No two-clock skew estimate could be formed, so no resample correction
    /// was applied — the spec's `Warn(TwoClock)` fallback (§ The Two-Clock
    /// Complication; decision 2026-07-21 §Q6).
    TwoClock = 102,
    /// The MS-4 emit guard clamped `count` samples to ±1.0. A verified
    /// stimulus never needs the clamp, so a nonzero count is an upstream
    /// level defect worth reporting even though the guard contained it.
    EmitClamped { count: u64 } = 103,
    /// The MS-4 emit guard zeroed `count` non-finite samples. Same posture as
    /// [`Self::EmitClamped`]: contained, surfaced, counted.
    EmitNonFiniteSanitized { count: u64 } = 104,
    /// The verification helper had to be SIGKILLed after both abort deadlines
    /// passed. The kill IS the full-scale click the ramp exists to prevent, so
    /// it is reported rather than being silent — and it is the reason
    /// [`Self::RenderDeviceLeaked`] is checked immediately afterwards.
    HelperKilledAfterRampDeadline = 105,
    /// The clock-skew fit formed and was used, but its per-marker residuals
    /// scatter more than the silent rung of the credibility ladder allows.
    /// `t = 0` is usable; it is less precise than a clean bracket.
    TwoClockResidualHigh { residual_peak_samples: f64 } = 106,
    /// The timing markers were located, but only just: their level above the
    /// capture's own pre-roll floor is thin, so the next quieter run may not
    /// find them at all.
    ///
    /// Distinct from [`Self::VerificationMarkersNotCredible`], which is "no
    /// credible train at all". This is the early warning for it.
    VerificationMarkerSnrLow { margin_db: f64 } = 107,
}

impl MeasurementDiagnostic {
    /// The wire/log number. **This is the contract** — the declaration's
    /// explicit discriminants mirror these values and the two must move
    /// together (kept adjacent in this file; `test_diagnostic.rs` pins every
    /// number).
    pub fn code(&self) -> u16 {
        match self {
            Self::SelfExclusionUnavailable => 1,
            Self::SensitivityMissing => 2,
            Self::SensitivityUnparseable => 3,
            Self::MicUnidentified => 4,
            Self::CapExceedsMicFullScale => 5,
            Self::SensitivityOutOfEnvelope => 6,
            Self::VolumeUncontrollable => 7,
            Self::ProjectedSplOverCap => 8,
            Self::RungOverCap => 9,
            Self::SplProjectionMismatch => 10,
            Self::SplOverCap => 11,
            Self::InputClipping => 12,
            Self::SnrUnachievable => 13,
            Self::StimulusEndpointNonzero => 14,
            Self::StimulusFadeNotMonotone => 15,
            Self::StimulusDcOffset { .. } => 16,
            Self::StimulusOverFullScale { .. } => 17,
            Self::StimulusNonFinite { .. } => 18,
            Self::UserAborted => 19,
            Self::MicDisconnected => 20,
            Self::OutputDeviceChanged => 21,
            Self::EngineFailed => 22,
            Self::SolvedLevelIllegal => 23,
            Self::EngineNotRunning => 24,
            Self::HelperUnavailable => 25,
            Self::HelperFailed { .. } => 26,
            Self::HelperStalled => 27,
            Self::VerificationMarkersNotFound { .. } => 28,
            Self::VerificationGainNotPinned { .. } => 29,
            Self::VerificationLevelBelowSnrBudget { .. } => 30,
            Self::TapSilentDuringVerification => 31,
            Self::SystemAudioNotQuiet => 32,
            Self::HelperRoutingMismatch => 33,
            Self::OutputRateChangedDuringVerify { .. } => 34,
            Self::VerificationChainClipped { .. } => 35,
            Self::VerificationMarkersNotCredible => 36,
            Self::ClockAdjustTooLarge { .. } => 37,
            Self::RenderDeviceLeaked { .. } => 38,
            Self::VerificationBandsDropped { .. } => 39,
            Self::VerificationPreampMismatch { .. } => 40,
            Self::LowSnr => 100,
            Self::FixedMaxVolume => 101,
            Self::TwoClock => 102,
            Self::EmitClamped { .. } => 103,
            Self::EmitNonFiniteSanitized { .. } => 104,
            Self::HelperKilledAfterRampDeadline => 105,
            Self::TwoClockResidualHigh { .. } => 106,
            Self::VerificationMarkerSnrLow { .. } => 107,
        }
    }

    /// Blocking [`Severity::Error`] for every refusal row and stimulus
    /// defect; [`Severity::Warning`] for the spec's warn rows.
    pub fn severity(&self) -> Severity {
        match self {
            Self::CapExceedsMicFullScale
            | Self::ClockAdjustTooLarge { .. }
            | Self::EngineFailed
            | Self::EngineNotRunning
            | Self::HelperFailed { .. }
            | Self::HelperRoutingMismatch
            | Self::HelperStalled
            | Self::HelperUnavailable
            | Self::InputClipping
            | Self::MicDisconnected
            | Self::MicUnidentified
            | Self::OutputDeviceChanged
            | Self::OutputRateChangedDuringVerify { .. }
            | Self::ProjectedSplOverCap
            | Self::RenderDeviceLeaked { .. }
            | Self::RungOverCap
            | Self::SelfExclusionUnavailable
            | Self::SensitivityMissing
            | Self::SensitivityOutOfEnvelope
            | Self::SensitivityUnparseable
            | Self::SnrUnachievable
            | Self::SolvedLevelIllegal
            | Self::SplOverCap
            | Self::SplProjectionMismatch
            | Self::StimulusDcOffset { .. }
            | Self::StimulusEndpointNonzero
            | Self::StimulusFadeNotMonotone
            | Self::StimulusNonFinite { .. }
            | Self::StimulusOverFullScale { .. }
            | Self::SystemAudioNotQuiet
            | Self::TapSilentDuringVerification
            | Self::UserAborted
            | Self::VerificationBandsDropped { .. }
            | Self::VerificationChainClipped { .. }
            | Self::VerificationGainNotPinned { .. }
            | Self::VerificationLevelBelowSnrBudget { .. }
            | Self::VerificationMarkersNotCredible
            | Self::VerificationMarkersNotFound { .. }
            | Self::VerificationPreampMismatch { .. }
            | Self::VolumeUncontrollable => Severity::Error,
            Self::EmitClamped { .. }
            | Self::EmitNonFiniteSanitized { .. }
            | Self::FixedMaxVolume
            | Self::HelperKilledAfterRampDeadline
            | Self::LowSnr
            | Self::TwoClock
            | Self::TwoClockResidualHigh { .. }
            | Self::VerificationMarkerSnrLow { .. } => Severity::Warning,
        }
    }

    /// `severity() == Severity::Error`.
    pub fn is_blocking(&self) -> bool {
        self.severity() == Severity::Error
    }

    /// Easy mode: plain-language what-to-do. Non-empty for every variant.
    pub fn fix_easy(&self) -> String {
        match self {
            Self::SelfExclusionUnavailable => {
                "Restart ParaEQ and try the measurement again. If this keeps \
                 happening, restart your Mac."
            }
            Self::SensitivityMissing => {
                "Load your microphone's calibration file before measuring. It \
                 usually comes from the mic maker's website, matched to your \
                 mic's serial number."
            }
            Self::SensitivityUnparseable => {
                "Your microphone's calibration file could not be read. \
                 Re-download it for your mic's serial number and load it again."
            }
            Self::MicUnidentified => {
                "Pick your microphone from the list, or confirm it manually if \
                 it is not listed."
            }
            Self::CapExceedsMicFullScale => {
                "Lower your microphone's input gain and reload the calibration \
                 file."
            }
            Self::SensitivityOutOfEnvelope => {
                "Check that the headphone or speaker is connected and in place, \
                 that the right output device is selected, and that the \
                 transducer type matches what you are measuring — then try \
                 again."
            }
            Self::VolumeUncontrollable => {
                "Turn your output device's volume down to a moderate level by \
                 hand, load a mic calibration file, and try again."
            }
            Self::ProjectedSplOverCap => {
                "Turn the output volume down and run the level check again. \
                 Nothing was played."
            }
            Self::RungOverCap => {
                "Turn the output volume down and start the level check again \
                 from the beginning."
            }
            Self::SplProjectionMismatch => {
                "Check that the right output device is selected and that the \
                 calibration file matches this microphone, then run the level \
                 check again."
            }
            Self::SplOverCap => {
                "The sweep was stopped for safety. Turn the volume down before \
                 measuring again."
            }
            Self::InputClipping => "Turn down the microphone's input gain and measure again.",
            Self::SnrUnachievable => {
                "The room is too noisy for a trustworthy measurement. Reduce \
                 background noise — close windows, pause appliances — or move \
                 the microphone closer, then try again."
            }
            Self::StimulusEndpointNonzero
            | Self::StimulusFadeNotMonotone
            | Self::StimulusDcOffset { .. }
            | Self::StimulusNonFinite { .. } => {
                "This is a defect in ParaEQ, not something you did. Nothing was \
                 played. Please report it."
            }
            Self::StimulusOverFullScale { .. } => {
                "This is a defect in ParaEQ's level solve, not something you \
                 did. Nothing was played. Please report it."
            }
            Self::UserAborted => {
                "The measurement was stopped. Nothing more was played, and \
                 your volume was put back. Start again whenever you like."
            }
            Self::MicDisconnected => {
                "The measurement microphone disconnected. Plug it back in and \
                 start the measurement again."
            }
            Self::OutputDeviceChanged => {
                "The output device changed during the measurement. Pick the \
                 device you are measuring and start again."
            }
            Self::EngineFailed => {
                "ParaEQ's audio engine hit a problem, so the measurement was \
                 stopped. Restart ParaEQ and try again."
            }
            Self::SolvedLevelIllegal => {
                "This device needs an unsafe output level to reach a usable \
                 measurement volume. Turn up the device's own volume (or use a \
                 more sensitive one) and try again."
            }
            Self::EngineNotRunning => {
                "ParaEQ's EQ is switched off. Turn it on with Enable, then \
                 start the measurement again."
            }
            Self::HelperUnavailable => {
                "ParaEQ could not start the small player it uses to check its \
                 own work. Reinstall ParaEQ and try again. Nothing was played."
            }
            Self::HelperFailed { .. } => {
                "The check playback stopped before it started. Nothing useful \
                 was played, and your volume was put back. Try again — if it \
                 keeps happening, please report it."
            }
            Self::HelperStalled => {
                "The check playback stopped responding and was shut down. Try \
                 again; if it keeps happening, restart ParaEQ."
            }
            Self::VerificationMarkersNotFound { .. } => {
                "ParaEQ could not line the check recording up with what it \
                 played. Run the check again with as little else going on as \
                 possible."
            }
            Self::VerificationGainNotPinned { .. } => {
                "ParaEQ could not set its volume slider to 0 dB for the check. \
                 Set it to 0 dB yourself and run the check again."
            }
            Self::VerificationLevelBelowSnrBudget { .. } => {
                "The room is too noisy for this check at a safe volume. Reduce \
                 background noise — close windows, pause appliances — and try \
                 again. ParaEQ will not play louder to get around it."
            }
            Self::TapSilentDuringVerification => {
                "ParaEQ heard nothing from your system audio while the check \
                 was playing. Check that ParaEQ still has permission to \
                 process system audio, then try again."
            }
            Self::SystemAudioNotQuiet => {
                "Something else was playing audio. Pause it and run the check \
                 again."
            }
            Self::HelperRoutingMismatch => {
                "The check played to a different speaker or ear than the \
                 measurement did, so the two cannot be compared. Run the \
                 measurement again from the start."
            }
            Self::OutputRateChangedDuringVerify { .. } => {
                "Your output device changed its sample rate. Set it back in \
                 Audio MIDI Setup, or just run the measurement again."
            }
            Self::VerificationChainClipped { .. } => {
                "The correction is asking for more level than your output can \
                 give and it clipped. Turn the output volume down, or reduce \
                 the boost, and check again."
            }
            Self::VerificationMarkersNotCredible => {
                "The check playback was too quiet to find in the recording. \
                 Turn the output device's own volume up a little and run the \
                 check again."
            }
            Self::ClockAdjustTooLarge { .. } => {
                "Your microphone and your speakers disagree about time by more \
                 than ParaEQ will correct for. Run both at the same sample \
                 rate in Audio MIDI Setup and try again."
            }
            Self::RenderDeviceLeaked { .. } => {
                "ParaEQ may have left a hidden audio device behind. Restart \
                 ParaEQ, and restart your Mac if a strange device is still \
                 listed."
            }
            Self::VerificationBandsDropped { .. } => {
                "Some filters cannot run at your device's current sample rate, \
                 so there is nothing complete to check. Run the measurement \
                 again at this sample rate."
            }
            Self::VerificationPreampMismatch { .. } => {
                "ParaEQ's safety volume reduction does not match the filters \
                 it is running. This is a defect, not something you did. \
                 Nothing was measured; please report it."
            }
            Self::LowSnr => {
                "The measurement is usable, but a quieter room would make it \
                 better."
            }
            Self::FixedMaxVolume => {
                "No action needed. Your output device runs at a fixed volume, \
                 and the level check measured the actual loudness."
            }
            Self::TwoClock => {
                "No action needed, but if the result looks odd, run the \
                 measurement again."
            }
            Self::EmitClamped { .. } | Self::EmitNonFiniteSanitized { .. } => {
                "The safety limiter engaged. The measurement may be unreliable \
                 — please report this."
            }
            Self::HelperKilledAfterRampDeadline => {
                "No action needed. The check playback had to be stopped \
                 abruptly; you may have heard a short click."
            }
            Self::TwoClockResidualHigh { .. } => {
                "No action needed, but if the result looks odd, run the check \
                 again with less going on in the room."
            }
            Self::VerificationMarkerSnrLow { .. } => {
                "No action needed this time. If checks start failing, turn \
                 your output device's own volume up a little."
            }
        }
        .to_owned()
    }

    /// Guided mode: what happened and why. Non-empty for every variant.
    pub fn explain_guided(&self) -> String {
        match self {
            Self::SelfExclusionUnavailable => {
                "ParaEQ could not exclude its own audio from the system tap, so \
                 a sweep would be muted at the device and routed through the \
                 correction engine — an unvalidated signal path. Measurement \
                 refuses rather than adapting: an unknown audio topology is not \
                 a thing to guess at with a full-level stimulus armed."
                    .to_owned()
            }
            Self::SensitivityMissing => {
                "No calibration file is loaded, so the microphone's sensitivity \
                 is unknown. Without it ParaEQ cannot convert what the mic \
                 hears into a sound pressure level, and the loudness caps \
                 cannot bind. A missing sensitivity never falls back to an \
                 uncapped sweep: no SPL, no closed loop, no safety."
                    .to_owned()
            }
            Self::SensitivityUnparseable => {
                "A calibration file is present, but no usable sensitivity could \
                 be read from it. An unreadable sensitivity has the same \
                 consequence as a missing one — ParaEQ cannot convert the mic's \
                 reading into a sound pressure level, so the loudness caps \
                 cannot bind and the measurement refuses."
                    .to_owned()
            }
            Self::MicUnidentified => {
                "The selected input is not a recognized measurement microphone \
                 and was not manually confirmed. A sensitivity figure is \
                 meaningless without knowing what produced it, so the safety \
                 solve cannot be trusted on an unidentified mic."
                    .to_owned()
            }
            Self::CapExceedsMicFullScale => {
                "The loudness cap for this transducer class is above the \
                 loudest sound this microphone can register at its current \
                 gain. A cap the mic cannot witness could never fire, so it \
                 offers no protection — the run refuses at calibration load \
                 rather than measuring behind a blind limiter."
                    .to_owned()
            }
            Self::SensitivityOutOfEnvelope => {
                "The measured chain sensitivity is far outside the expected \
                 range for this transducer class. That signature usually means \
                 an empty measurement jig, output routed to the wrong device \
                 (like the laptop speakers), or the wrong transducer type \
                 selected. ParaEQ refuses rather than escalates: driving more \
                 level into an unexplained chain is exactly how an in-ear ends \
                 up dangerously loud when it is finally worn."
                    .to_owned()
            }
            Self::VolumeUncontrollable => {
                "The output device's volume can neither be read nor set, and no \
                 microphone-based level solve is available, so nothing bounds \
                 how loud the sweep would actually be. With no protection in \
                 the chain at all, the measurement refuses."
                    .to_owned()
            }
            Self::ProjectedSplOverCap => {
                "The solved sweep level projects a sound pressure above this \
                 class's hard cap, so the run refused before a single sample \
                 was emitted — the cheapest possible refusal."
                    .to_owned()
            }
            Self::RungOverCap => "While stepping up toward the target level in ≤6 dB rungs, a \
                 rung's measured sound pressure exceeded the class cap. The \
                 escalation aborted: the caps bind at every rung, not only at \
                 the end."
                .to_owned(),
            Self::SplProjectionMismatch => {
                "A rung's measured sound pressure deviated more than 6 dB from \
                 what the pilot solve projected. That is the signature of a \
                 wrong output device, a stale calibration file, or a broken \
                 chain — so the escalation aborted rather than climbing further \
                 into a chain that does not behave as characterized."
                    .to_owned()
            }
            Self::SplOverCap => "The measured sound pressure exceeded the class's hard cap \
                 mid-sweep. The stimulus was ramped to zero and the session \
                 aborted, then the pre-measurement volume and engine state were \
                 restored."
                .to_owned(),
            Self::InputClipping => "More than 30% of the samples in an input block clipped. \
                 Whatever the level, a clipped capture is not a valid \
                 measurement, so the run refuses (REW's rule)."
                .to_owned(),
            Self::SnrUnachievable => "Even after the automatic remedies — input gain, then sweep \
                 length — the signal-to-noise gate was missed. Remedies never \
                 raise the output level: making the test signal louder is the \
                 one fix ParaEQ will not apply, so the run refuses instead."
                .to_owned(),
            Self::StimulusEndpointNonzero => {
                "An assembled stimulus failed its pre-flight check: it does not \
                 start and end at exactly zero, which would play as a broadband \
                 click — a hearing hazard, a tweeter hazard, and a measurement \
                 defect at once. The buffer never reached the output."
                    .to_owned()
            }
            Self::StimulusFadeNotMonotone => {
                "An assembled stimulus failed its pre-flight check: the fade-out \
                 envelope is not monotonically decreasing, so the ending would \
                 not be click-free. The buffer never reached the output."
                    .to_owned()
            }
            Self::StimulusDcOffset { mean } => {
                format!(
                    "An assembled stimulus failed its pre-flight check: it \
                     carries a DC offset (mean {mean:.3e}, gate 1e-4) that \
                     would put silent cone displacement into the driver and \
                     bias the measurement. The buffer never reached the output."
                )
            }
            Self::StimulusOverFullScale { peak } => {
                format!(
                    "An assembled stimulus failed its pre-flight check: after \
                     scaling to the solved level its peak is {peak:.3} — over \
                     full scale. That means the level solve is wrong, so \
                     clamping and continuing would play a distorted stimulus \
                     at an unintended level; the buffer never reached the \
                     output."
                )
            }
            Self::StimulusNonFinite { count } => {
                format!(
                    "An assembled stimulus failed its pre-flight check: \
                     {count} sample(s) are NaN or infinite. Non-finite audio \
                     is permanently broken audio, so the buffer never reached \
                     the output."
                )
            }
            Self::UserAborted => "The run was stopped by request — Esc, Space, or closing the \
                 window. The stimulus was ramped to zero within a few \
                 milliseconds (a hard stop would itself be a full-scale \
                 click), the output stream stopped, and the pre-measurement \
                 volume and engine state were restored."
                .to_owned(),
            Self::MicDisconnected => "The measurement microphone vanished mid-run. The mic is the \
                 only witness the SPL caps have — without it the closed loop \
                 that bounds the sweep's loudness is gone, so the stimulus \
                 was ramped to zero and the session aborted."
                .to_owned(),
            Self::OutputDeviceChanged => {
                "The output device changed or disappeared mid-run. The solved \
                 sweep level is only valid for the exact chain it was solved \
                 on; a different device is an uncharacterized chain, so the \
                 stimulus was ramped to zero and the session aborted."
                    .to_owned()
            }
            Self::EngineFailed => "ParaEQ's audio engine entered its failed state mid-run, so \
                 the audio topology the measurement depends on is no longer \
                 known-good. The stimulus was ramped to zero and the session \
                 aborted, then the pre-measurement state was restored."
                .to_owned(),
            Self::SolvedLevelIllegal => "The level ladder solved a projected SPL within the class \
                 cap, but the output level it requires — after the calibration \
                 safety margin — lands above the class's dBFS ceiling or above \
                 −3 dBFS RMS, which the sole level constructor refuses. The chain \
                 is too insensitive to measure safely at this device volume; no \
                 sample was emitted and the pre-measurement state was restored."
                .to_owned(),
            Self::EngineNotRunning => {
                "ParaEQ's engine is not running — either it was switched off, \
                 or it disabled itself after a spell with no audio playing \
                 (15 seconds by default). Nothing is broken; measuring simply \
                 needs the engine on. While it is off there is no system tap, \
                 so ParaEQ cannot prove it is keeping its own sound out of \
                 the measurement, and there is no correction chain for the \
                 verification pass to play through. If a measurement was \
                 already running it was stopped and your volume put back."
                    .to_owned()
            }
            Self::HelperUnavailable => {
                "The verification pass plays its sweep from a separate helper \
                 process, because ParaEQ's own audio is deliberately excluded \
                 from the system tap and therefore never traverses the \
                 correction chain. That helper could not be started, or the \
                 output device it was pointed at no longer exists, so no \
                 sample was ever emitted."
                    .to_owned()
            }
            Self::HelperFailed { exit_code } => {
                format!(
                    "The verification helper exited with status {exit_code} \
                     instead of playing. Its exit codes are a closed table — \
                     bad arguments, a WAV it would not accept, or its own \
                     independent level backstop refusing the file before it \
                     opened a device. Whichever fired, it fired BEFORE audio, \
                     which is what the two-phase start-up exists for."
                )
            }
            Self::HelperStalled => "The verification helper stopped producing status lines, or \
                 its render device stopped cycling. The teardown ladder ran in \
                 full: a ramp request first, then SIGTERM (which arms the same \
                 5 ms fade), and only after both deadlines a kill — because a \
                 hard stop is itself a full-scale click."
                .to_owned(),
            Self::VerificationMarkersNotFound {
                residual_peak_samples,
            } => {
                format!(
                    "Timing markers were found and a clock fit was formed, but \
                     the markers scatter {residual_peak_samples:.1} samples \
                     around that fit — past the bound at which `t = 0` can be \
                     trusted. Every gate downstream cuts the impulse response \
                     at that instant, and a wrong t = 0 produces a plausible \
                     wrong answer rather than an obvious failure, so the pass \
                     refuses instead."
                )
            }
            Self::VerificationGainNotPinned { read_back_db } => {
                format!(
                    "The verification pass pins your manual trim to 0 dB for \
                     the duration and reads it back; it still reports \
                     {read_back_db} dB. The read-back is not a formality: with \
                     a +10 dB trim against a +6 dB peak boost the verification \
                     sweep would reach the transducer LOUDER than the baseline \
                     the level ladder validated. Nothing was played."
                )
            }
            Self::VerificationLevelBelowSnrBudget { projected_snr_db } => {
                format!(
                    "The verification sweep must play at the baseline level \
                     minus the correction's peak boost, which puts it \
                     {projected_snr_db:.1} dB above the measured noise floor — \
                     below the bar at which a residual means anything. It is \
                     refused before spawning rather than played and then \
                     reported as a shape failure. The one remedy ParaEQ will \
                     not apply is raising the output level."
                )
            }
            Self::TapSilentDuringVerification => {
                "While the helper was playing, the system tap recorded no \
                 blocks carrying audio — or its realtime counters could not be \
                 read at all. The helper is a separate process and is \
                 therefore NOT excluded from the tap, so the tap must see it. \
                 Silence here is the genuine permission-denied failure, and \
                 'cannot witness' is not 'witnessed nothing': the pass refuses \
                 rather than assuming either way."
                    .to_owned()
            }
            Self::SystemAudioNotQuiet => "The system tap's activity counters advanced during the \
                 pre-roll, which means some other application was playing. The \
                 tap is global and excludes only ParaEQ itself, so that audio \
                 would be captured alongside the verification sweep and \
                 measured as part of the corrected response."
                .to_owned(),
            Self::HelperRoutingMismatch => {
                "The verification capture cannot be differenced against the \
                 baseline it claims to verify. Either the helper played to a \
                 different channel than the baseline did, or it played to \
                 every channel while the two channels carry different filters \
                 — in which case the microphone heard their sum, and a sum of \
                 differently-corrected channels is not the response of either \
                 one. Refused before playing."
                    .to_owned()
            }
            Self::OutputRateChangedDuringVerify {
                expected_hz,
                observed_hz,
            } => {
                format!(
                    "The verification pass expected {expected_hz} Hz and saw \
                     {observed_hz} Hz. Either the correction chain is running \
                     coefficients designed for a different rate, or the render \
                     device renegotiated while the measurement aggregate was \
                     being created. Both make the measured response an \
                     artefact of the rate change rather than of the filter."
                )
            }
            Self::VerificationChainClipped { count } => {
                format!(
                    "The correction chain's own ±1.0 output clamp engaged on \
                     {count} sample(s) during the verification window, so what \
                     reached the transducer is not what the filter designed. \
                     That is the NEAR side of the transducer; the microphone's \
                     own converter clipping is reported separately. 'The chain \
                     clipped' is a far better sentence than 'the residual was \
                     too high', which is why this has its own code."
                )
            }
            Self::VerificationMarkersNotCredible => {
                "The verification file is bracketed with short timing chirps, \
                 and none of them cleared the matched filter's own credibility \
                 floor — six times the off-peak correlation level. No fit was \
                 formed, so no t = 0 exists and no impulse response can be \
                 cut. This is the first thing that fails when the verification \
                 sweep is quiet, which is why it is reported separately from a \
                 fit that formed and scattered."
                    .to_owned()
            }
            Self::ClockAdjustTooLarge { skew_ppm } => {
                format!(
                    "The microphone and the output device run on independent \
                     crystals, and the fit says they differ by {skew_ppm:.0} \
                     parts per million — past the reject bound. Four markers \
                     on a straight line fit a straight line perfectly however \
                     steep it is, so the scatter figure alone would bless this \
                     and then stretch the capture on the strength of it."
                )
            }
            Self::RenderDeviceLeaked { pid } => {
                format!(
                    "The verification helper had to be killed outright, which \
                     bypasses its cleanup, so the private render device it \
                     created (UID com.paraeq.render.{pid}) may still be \
                     wrapping your output. A reaped child that leaked a device \
                     still satisfies 'no zombie', which is why this is checked \
                     separately. It is reported, never masked, and it does not \
                     stop the rest of the teardown."
                )
            }
            Self::VerificationBandsDropped { dropped, rate_hz } => {
                format!(
                    "The engine could not design {dropped} of the plan's \
                     filters at the live rate of {rate_hz} Hz and dropped \
                     them. Verification compares the measured result against \
                     the response the PLAN predicts, so grading a partially \
                     installed cascade would blame the chain for a prediction \
                     fault of our own."
                )
            }
            Self::VerificationPreampMismatch {
                armed_db,
                recomputed_db,
            } => {
                format!(
                    "The engine reports an armed safety attenuation of \
                     {armed_db} dB while recomputing it from the plan's own \
                     filters at the live rate gives {recomputed_db} dB. Those \
                     are two independent computations of one number, and they \
                     must agree before a residual means anything. The same \
                     condition is checked a second time after the fact, in the \
                     decision engine, against the number this pass carries."
                )
            }
            Self::LowSnr => "The measurement cleared the reduced signal-to-noise bar \
                 (median ≥ 30 dB) but not the preferred one (median ≥ 40 dB \
                 with every band ≥ 20 dB). It is usable; the quiet parts of \
                 the response carry more uncertainty than usual."
                .to_owned(),
            Self::FixedMaxVolume => {
                "The output device reports maximum volume and offers no way to \
                 change it in software — common for USB DACs. The pilot solve \
                 still measured the actual acoustic result, so the chain is \
                 characterized and the loudness caps still bind; the run \
                 proceeds with this note."
                    .to_owned()
            }
            Self::TwoClock => "The microphone and the output device run on independent \
                 clocks, and no skew estimate could be formed for this capture, \
                 so no resample correction was applied. Small clock drift can \
                 slightly blur the measured impulse response."
                .to_owned(),
            Self::EmitClamped { count } => {
                format!(
                    "The output guard clamped {count} sample(s) to full scale \
                     just before the device. A verified stimulus never needs \
                     the clamp, so a nonzero count means something upstream \
                     produced a hotter buffer than the solved level — \
                     contained here, but worth reporting."
                )
            }
            Self::EmitNonFiniteSanitized { count } => {
                format!(
                    "The output guard zeroed {count} non-finite (NaN or \
                     infinite) sample(s) just before the device. A verified \
                     stimulus contains none, so a nonzero count means an \
                     upstream defect — contained here, but worth reporting."
                )
            }
            Self::HelperKilledAfterRampDeadline => {
                "The verification helper did not fade out within its abort \
                 deadline, and did not respond to SIGTERM either, so it was \
                 killed. A kill IS the full-scale click the 5 ms ramp exists \
                 to prevent, so it is recorded rather than passed over — and \
                 it is why the render device is checked for survival \
                 immediately afterwards."
                    .to_owned()
            }
            Self::TwoClockResidualHigh {
                residual_peak_samples,
            } => {
                format!(
                    "The timing markers fit a clock-skew line, but they \
                     scatter {residual_peak_samples:.1} samples around it — \
                     more than a clean bracket, less than the bound at which \
                     the fit stops being usable. The impulse response's time \
                     origin carries that much extra uncertainty."
                )
            }
            Self::VerificationMarkerSnrLow { margin_db } => {
                format!(
                    "The timing markers sit only {margin_db:.1} dB above the \
                     capture's own pre-roll floor. They were found, so this \
                     pass is fine; a quieter verification sweep — which a \
                     larger peak boost produces — may not clear the matched \
                     filter's credibility floor at all."
                )
            }
        }
    }
}

/// A refusal: a *blocking* diagnostic, as a type (MS-9).
///
/// The point of the wrapper is what it makes unrepresentable: a refusal
/// cannot be downgraded into a warning, carries no fallback value, and is not
/// convertible into a [`crate::level::SweepLevel`] or a [`MicSensitivity`] —
/// so a code path that "handles" a refusal by sweeping anyway does not
/// typecheck. Mirrors the `SweepLevel` philosophy: the interlock is the type,
/// not reviewer vigilance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Refusal(MeasurementDiagnostic);

impl Refusal {
    /// Sole constructor. Only blocking diagnostics form refusals; passing a
    /// warning is a programming error and panics — a diagnostic that is
    /// not a refusal must not travel as one, because a refusal that could be
    /// waved through is exactly the degrade-and-continue path MS-9 forbids.
    pub fn new(diagnostic: MeasurementDiagnostic) -> Self {
        assert!(
            diagnostic.is_blocking(),
            "{diagnostic:?} is a warning, not a refusal — a non-blocking \
             diagnostic must not travel as one (MS-9)"
        );
        Self(diagnostic)
    }

    /// The diagnostic this refusal carries, for the session log and the UI.
    pub fn diagnostic(self) -> MeasurementDiagnostic {
        self.0
    }
}

/// What a cal-file load reports about the mic's Sens Factor. Producing this is
/// the loader's job (a later stage); consuming it safely is
/// [`MicSensitivity::from_cal`]'s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CalSensitivity {
    /// No cal file was loaded at all.
    Missing,
    /// A Sens Factor was parsed (dBFS at 94 dB SPL, the UMIK convention).
    Parsed(f64),
    /// A cal file is present but no sensitivity could be parsed from it.
    Unparseable,
}

/// A mic sensitivity that provably came from a parsed cal file (MS-9).
///
/// The sole constructor is [`Self::from_cal`], the field is private, and
/// there is no `Default` and no `From<f64>` — so every SPL conversion
/// downstream (the pilot solve, the caps, the mid-sweep abort) can only be
/// built on a sensitivity that actually exists. The uncapped-sweep fallback
/// the spec forbids is not a forbidden branch; it is an unrepresentable one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MicSensitivity(f64);

impl MicSensitivity {
    /// The MS-9 gate. Missing → refuse [`MeasurementDiagnostic::SensitivityMissing`];
    /// unparseable, or parsed non-finite → refuse
    /// [`MeasurementDiagnostic::SensitivityUnparseable`]. Never a default,
    /// never a guess.
    pub fn from_cal(cal: CalSensitivity) -> Result<Self, Refusal> {
        match cal {
            CalSensitivity::Missing => Err(Refusal::new(MeasurementDiagnostic::SensitivityMissing)),
            // A value that "parsed" to NaN or ±∞ did not meaningfully parse —
            // and NaN would slip through every downstream SPL comparison
            // (`NaN > cap` is false), the same NaN-blindness `SweepLevel::new`
            // refuses first.
            CalSensitivity::Parsed(sens_factor_dbfs) if sens_factor_dbfs.is_finite() => {
                Ok(Self(sens_factor_dbfs))
            }
            CalSensitivity::Parsed(_) | CalSensitivity::Unparseable => {
                Err(Refusal::new(MeasurementDiagnostic::SensitivityUnparseable))
            }
        }
    }

    /// The parsed Sens Factor, dBFS at 94 dB SPL.
    pub fn sens_factor_dbfs(self) -> f64 {
        self.0
    }

    /// The spec's conversion (§ The Level Ladder, step 2):
    /// `SPL_measured = 94 + (dBFS_measured − Sens_Factor)`.
    pub fn spl_from_dbfs(self, dbfs_measured: f64) -> f64 {
        94.0 + (dbfs_measured - self.0)
    }
}
