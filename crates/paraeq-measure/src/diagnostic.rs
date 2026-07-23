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
            Self::LowSnr => 100,
            Self::FixedMaxVolume => 101,
            Self::TwoClock => 102,
            Self::EmitClamped { .. } => 103,
            Self::EmitNonFiniteSanitized { .. } => 104,
        }
    }

    /// Blocking [`Severity::Error`] for every refusal row and stimulus
    /// defect; [`Severity::Warning`] for the spec's warn rows.
    pub fn severity(&self) -> Severity {
        match self {
            Self::CapExceedsMicFullScale
            | Self::EngineFailed
            | Self::InputClipping
            | Self::MicDisconnected
            | Self::MicUnidentified
            | Self::OutputDeviceChanged
            | Self::ProjectedSplOverCap
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
            | Self::UserAborted
            | Self::VolumeUncontrollable => Severity::Error,
            Self::EmitClamped { .. }
            | Self::EmitNonFiniteSanitized { .. }
            | Self::FixedMaxVolume
            | Self::LowSnr
            | Self::TwoClock => Severity::Warning,
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
