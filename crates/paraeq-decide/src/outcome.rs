//! `decide()`'s output: one serializable value the two front-ends and the
//! Advanced drawer all render, plus the typed refusals that earn auto mode the
//! right to hide everything.

use crate::decision::Evidence;
use crate::decisions::Decisions;
use paraeq_dsp::authority::{AuthorityCurve, Clamp};
use paraeq_dsp::peq::EQBand;
use paraeq_dsp::PerChannel;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DecisionSet {
    /// The derived curves both front-ends plot. Not decisions — products.
    pub analysis: Analysis,
    /// `None` iff `verdict == Refuse`. Rate-independent by construction.
    pub correction: Option<CorrectionPlan>,
    pub decisions: Decisions,
    pub diagnostics: Vec<Diagnostic>,
    pub verdict: Verdict,
    /// Present iff the bundle carried a `verification`. The residual curves and
    /// scalars, as [`Evidence`], plus the two numbers the gate compared.
    ///
    /// NEVER a decision: nothing here has a `Domain` the drawer may override, a
    /// `Source` or an `Invalidation`, and forcing it into [`Decisions`] would
    /// make `iter()` emit a drawer control for a measurement result. `Option`
    /// keeps every golden case that carries no verification byte-stable.
    pub verification: Option<VerificationReport>,
}

/// What the closed-loop verification pass measured, and what it was graded
/// against.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VerificationReport {
    /// The residual curves and scalars. `ResidualVsPrediction` appears once per
    /// capture channel, so the drawer can say WHICH ear failed.
    pub evidence: Vec<Evidence>,
    /// The threshold [`Self::residual_rms_db`] was compared against: the
    /// verification multiple of the decided `flatness_target_db`.
    pub gate_db: f64,
    /// RMS of `residual_vs_prediction` over the authority band, dB, as the
    /// **max over capture channels** — the worst channel, never the mean. One
    /// bad ear must not be rescued by a good one, which is the same
    /// "worst channel wins" posture the engine already takes for the preamp.
    pub residual_rms_db: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Verdict {
    Proceed,
    /// At least one Warn diagnostic. `correction` is present and installable.
    ProceedWithWarnings,
    /// At least one Refuse diagnostic. `correction` is None, but `decisions`,
    /// `analysis` and `evidence` are still populated as far as they got — a
    /// refusal must be able to explain itself and the guided path must be able
    /// to pick up where auto stopped. This is why `decide()` returns
    /// `DecisionSet`, not `Result<DecisionSet, E>`.
    Refuse,
}

/// The curves every decision was read off, retained so a refusal can plot its
/// own reasoning and the drawer can show the margin.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Analysis {
    /// The RESOLVED authority curve the `authority` preset selected.
    ///
    /// A product, not a decision: `decide()` publishes one curve per run and
    /// there is no control to bound it with, which is exactly why § D-D puts
    /// the NAME on `Decisions::authority` and the curve here. Sealed — only
    /// `paraeq_dsp::authority::build_authority` constructs one and
    /// deserialization is guarded, so nothing can fabricate a ceiling.
    pub authority: AuthorityCurve,
    /// Per channel, dB, on `freqs_hz`: the aligned, averaged, smoothed curve.
    pub averaged_db: Vec<Vec<f64>>,
    /// Measured phase minus the phase of its minimum-phase reconstruction,
    /// seconds, on `freqs_hz`. Flat over a region ⇒ boost is meaningful there.
    pub excess_group_delay_s: Vec<f64>,
    /// The log-f analysis grid every curve here is sampled on.
    pub freqs_hz: Vec<f64>,
    /// Aligned, smoothed, per-position curves, dB — index-parallel to
    /// `MeasurementBundle::positions`. σ(f) is one pass over these.
    pub per_position_db: Vec<Vec<f64>>,
    /// Per-bin standard deviation in dB across positions, on `freqs_hz`.
    pub sigma_db: Vec<f64>,
}

/// Bands and a design rate, never baked coefficients.
///
/// The engine rebuilds its chain on every format change, so a plan carrying
/// SOS would reinstall coefficients designed for the old rate: at 44.1→48 kHz
/// a 47 Hz mode filter lands at 51 Hz with the wrong Q. Coefficients are
/// derived at the LIVE rate, at install time, from these bands.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CorrectionPlan {
    /// Per channel. `PerChannel` rather than a bare `Vec<Vec<_>>` because the
    /// index is the ENGINE's channel index and the container says so; it is
    /// `#[serde(transparent)]`, so the wire form is the same bare array and no
    /// fixture moves with the type.
    pub bands: PerChannel<Vec<EQBand>>,
    /// Per channel, index-parallel to [`Self::bands`]: every change
    /// `clamp_band` and the autofit made, and why.
    ///
    /// `auto_fit_room` returns a `RoomFitReport { bands, clamps, dropped }`
    /// specifically so that the mandated reporting is reachable from its only
    /// caller, and until this field existed the clamps and drops had **nowhere
    /// to go** — which contradicts the Advanced drawer's own premise: "'your
    /// +6 dB became +1 dB' is not an explanation without saying whether the
    /// excursion envelope or the seat-to-seat disagreement did it."
    pub clamps: Vec<Vec<Clamp>>,
    /// The rate the bands were fitted at. Provenance only — never the rate the
    /// engine designs at.
    pub design_rate: f64,
    /// Indices of candidate bands the fit found and did not emit at all.
    /// Index-parallel to nothing: these are positions in the fit's own
    /// candidate order, carried so the drawer can say how many were dropped.
    pub dropped: Vec<usize>,
    /// `-max(0, peak of the REALIZED cascade)`. No headroom constant.
    ///
    /// Reaches the engine as `Correction.preamp_lin` — inside the correction,
    /// applied on the **corrected path only** — not as a separate
    /// `EngineCommand::SetGainDb`, which is applied on both chain paths and
    /// would leave the bypassed side of an A/B quieter by the whole preamp. It
    /// is not just the export text either. See
    /// `docs/specs/2026-07-15-engine-hardening-design.md` R1-1 and
    /// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` section D-1.
    pub preamp_db: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    /// Which position, if position-scoped.
    pub position: Option<usize>,
    /// Plain-language remedy. Rendered in Rust, same reason as Rationale.
    pub remedy: String,
    pub severity: Severity,
    /// The number that tripped it — so the drawer can show the margin.
    pub value: Option<f64>,
}

/// `Refuse` means *we do not know how to do this correctly and will not
/// guess*. `Warn` means *we did it, and here is what you should know*. The
/// distinction is not severity theatre: a `Refuse` produces no installable
/// correction, so the system stays as it was.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Severity {
    /// Cannot proceed. `verdict = Refuse`, `correction = None`.
    Refuse,
    /// Proceed with a caveat; may de-weight a position.
    Warn,
}

/// One per row of the spec's refusal table. Severity is carried on the
/// [`Diagnostic`], not implied by the code, because two of these fire at two
/// thresholds with two severities.
///
/// # Numbering contract (wire/log stability)
///
/// The number is [`DiagnosticCode::code`]'s value, mirrored by the
/// declaration's explicit discriminants (kept adjacent in this file — they must
/// move together). The space is **append-only**: a new variant takes the next
/// free number, and a number, once assigned, is **never reused and never
/// renumbered**. Variants are declared alphabetically, which is NOT the
/// numbering order and is not meant to be — the alphabetical position is a
/// reading aid, the number is the contract.
///
/// `fixtures/decide/<case>/expected.json` and any session log key on these
/// numbers; renumbering is a breaking change to recorded sessions and to every
/// frozen golden case. Adopted from `paraeq_measure::MeasurementDiagnostic`'s
/// contract per
/// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-R, verbatim:
/// "**Adopt the same contract** — explicit discriminants, a `code()` method,
/// the same append-only header comment, plus a doc-comment mapping between the
/// two vocabularies … it costs one commit now and is a breaking change to
/// recorded sessions later."
///
/// Unlike `MeasurementDiagnostic` there is **no blocking/warning block split**,
/// because severity is not a property of the code here: `LowSnrSoft`/
/// `LowSnrHard` and the two `PositionOutlierCoupler*` codes fire at two
/// thresholds with two severities, which is why [`Diagnostic::severity`] exists.
/// One flat space, numbered from 1.
///
/// # The two vocabularies, and why they must not be merged
///
/// `paraeq_measure::MeasurementDiagnostic` is the **pre- and during-capture**
/// vocabulary; this one is **post-capture**, over a bundle that already exists.
/// They overlap in seven places and are deliberately separate types:
/// `paraeq-decide` may not depend on `paraeq-measure` (the dependency arrow
/// would point the wrong way and would drag the capture layer into a pure
/// crate), so the relationship is carried by **convention and this comment**,
/// not by code. `paraeq-measure` carries `paraeq-decide` as a **dev-dependency
/// only**, which is why no code-level mapping ships in either direction.
///
/// The mapping, for anyone reading a session log that contains both:
///
/// | this enum | `MeasurementDiagnostic` |
/// |---|---|
/// | [`Self::SelfExclusionUnavailable`] | `SelfExclusionUnavailable = 1` |
/// | [`Self::CalMissing`] / [`Self::CalMalformed`] | `SensitivityMissing = 2` / `SensitivityUnparseable = 3` |
/// | [`Self::WrongTransducer`] | `SensitivityOutOfEnvelope = 6` |
/// | [`Self::ClippingPosition`] / [`Self::ClippingSession`] | `InputClipping = 12` |
/// | [`Self::MicNotConnected`] | `MicDisconnected = 20` |
/// | [`Self::LowSnrSoft`] | `LowSnr = 100` |
/// | [`Self::TwoClock`] | `TwoClock = 102` |
///
/// The risk this table guards against is not the duplication; it is a future
/// "cleanup" that merges the two enums, breaks the crate boundary and takes
/// daemon-readiness with it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(u16)]
pub enum DiagnosticCode {
    /// Span > 40 dB or midband tilt > 20 dB/decade: not a loudspeaker or a
    /// headphone.
    AbsurdCurve = 1,
    /// EARS HEQ/HPN/IDF: applying another target would apply it twice.
    CalHasTargetBakedIn = 2,
    /// Capture-layer twin: `MeasurementDiagnostic::SensitivityUnparseable = 3`.
    CalMalformed = 3,
    /// Capture-layer twin: `MeasurementDiagnostic::SensitivityMissing = 2`.
    CalMissing = 4,
    /// `|g[i] − (g[i−1]+g[i+1])/2| > 1.5 dB`. Not hypothetical: a shipping
    /// vendor file has a bogus 0.0000 at 19.611 Hz between −3.13 and −3.11.
    CalNeighbourOutlier = 5,
    /// Capture-layer twin: `MeasurementDiagnostic::InputClipping = 12`, which
    /// refuses DURING the capture; this one is the post-hoc position-scoped
    /// read of the same physical event.
    ClippingPosition = 6,
    /// Capture-layer twin: `MeasurementDiagnostic::InputClipping = 12`,
    /// session-scoped.
    ClippingSession = 7,
    /// Multi-position data reached a vector/coherent routine. It collapses
    /// toward the incoherent floor `−10log₁₀(N)` once spread approaches a
    /// wavelength.
    CoherentAveragingRejected = 8,
    /// Median σ(f) > 6 dB below the transition, where positions should agree.
    ExcessiveVariance = 9,
    /// Below `positions_default` but at or above 3.
    FewPositions = 10,
    LowSnrHard = 11,
    /// Capture-layer twin: `MeasurementDiagnostic::LowSnr = 100`.
    LowSnrSoft = 12,
    /// Capture-layer twin: `MeasurementDiagnostic::MicDisconnected = 20`.
    MicNotConnected = 13,
    NoSignal = 14,
    NoiseFloorTooHigh = 15,
    /// An override arrived outside its decision's `Domain`.
    ///
    /// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-N,
    /// verbatim: "**Clamp into the domain, set `source: UserOverride` with the
    /// clamped value, and emit `DiagnosticCode::OverrideOutOfDomain` at
    /// `Severity::Warn`**, naming the decision and both numbers." `decide()` has
    /// no `Result`, so the only two candidates were clamp-and-warn and
    /// ignore-and-keep-Auto, and silently ignoring the user's intent recreates
    /// exactly the "second, lying source of truth about what the app did" this
    /// design exists to prevent.
    ///
    /// Declared here alphabetically; numbered 25 because the space is
    /// append-only.
    OverrideOutOfDomain = 25,
    PositionOutlierCouplerHf = 16,
    /// A seal problem, not the headphone.
    PositionOutlierCouplerLf = 17,
    PositionOutlierRoom = 18,
    /// `capture.self_excluded == false`: the tap's fail-open path left our own
    /// audio tapped, so the stimulus topology is unvalidated. Capture-layer
    /// twin: `MeasurementDiagnostic::SelfExclusionUnavailable = 1`.
    SelfExclusionUnavailable = 19,
    /// Internal error — this is a bug, not a user condition.
    SweepRateMismatch = 20,
    /// A verification bundle carried an `installed_preamp_db` its own
    /// `installed.bands` do not reproduce at `running_rate_hz`.
    ///
    /// **One condition, two crates, two codes with the same name — read both
    /// before deleting either.** `MeasurementDiagnostic::VerificationPreampMismatch`
    /// is what `paraeq-measure` refuses with **before the capture**, at the
    /// verify gate, when the engine's armed preamp disagrees with the gate's own
    /// live-rate recomputation; no bundle exists yet at that point. THIS code is
    /// what `decide()` refuses with **after the fact**, when a bundle arrives
    /// carrying a number `decide()`'s own recomputation does not reproduce.
    /// `decide()` re-checks rather than trusts, which is the only reason the
    /// condition is reachable from two sides at all — every other new capture
    /// diagnostic refuses before the capture, so no bundle carrying that failure
    /// can exist for `decide()` to see, and none of them needs a twin here.
    ///
    /// Declared here alphabetically, between `SweepRateMismatch` and
    /// `VerificationResidual`, so the `Verification*` codes stay together;
    /// numbered 26 because the space is append-only.
    VerificationPreampMismatch = 26,
    /// We checked our work and it didn't land.
    VerificationResidual = 21,
    /// The verification capture cannot be differenced against the baseline it
    /// claims to verify. **Four triggers, one code:**
    ///
    /// 1. `verification.routing != positions[position_index].routing` — the
    ///    original fence: differencing a per-ear baseline against an L+R sum.
    /// 2. The capture-channel-count precondition: the baseline and the
    ///    verification have different widths. Differencing captures of different
    ///    width is not a residual.
    /// 3. `Only(n)` with `n >= installed.bands.channels()` — a routing naming a
    ///    channel the plan never corrected cannot be predicted at all.
    /// 4. `Both` over divergent per-channel band sets — the capture heard the
    ///    SUM of differently-EQ'd channels, which is not the response of any one
    ///    band set and no algebra makes it one. Refused rather than modelled.
    ///
    /// One code with four documented reasons rather than four serde-visible
    /// variants: each would add a row to the pre-freeze window and to every
    /// golden case for no user-visible gain, and all four are the same user
    /// story. The capture layer refuses trigger 4 before spawning, under its own
    /// differently-named `HelperRoutingMismatch` — different names, so this is
    /// not a second shared-name pair like
    /// [`Self::VerificationPreampMismatch`].
    ///
    /// Declared here alphabetically; numbered 27 because the space is
    /// append-only.
    VerificationRoutingMismatch = 27,
    /// Mic and output on different clocks. Gating needs a trustworthy t=0 and
    /// the "Farina tolerates skew" result does not transfer to it. Capture-layer
    /// twin: `MeasurementDiagnostic::TwoClock = 102`.
    ///
    /// Per § D-Q this fires only when `capture.clock_skew_ppm` is `None`: a
    /// `Some(ppm)` means the estimate was formed and applied, and the ppm rides
    /// on `Diagnostic::value` instead.
    TwoClock = 22,
    TooFewPositions = 23,
    /// Solved chain sensitivity outside the declared class's envelope. A
    /// safety event, not a quality event. Capture-layer twin:
    /// `MeasurementDiagnostic::SensitivityOutOfEnvelope = 6`.
    WrongTransducer = 24,
}

impl DiagnosticCode {
    /// The wire/log number. **This is the contract** — the declaration's
    /// explicit discriminants mirror these values and the two must move
    /// together (kept adjacent in this file; `tests/test_codes.rs` pins every
    /// number and that the two lists agree).
    pub fn code(self) -> u16 {
        match self {
            Self::AbsurdCurve => 1,
            Self::CalHasTargetBakedIn => 2,
            Self::CalMalformed => 3,
            Self::CalMissing => 4,
            Self::CalNeighbourOutlier => 5,
            Self::ClippingPosition => 6,
            Self::ClippingSession => 7,
            Self::CoherentAveragingRejected => 8,
            Self::ExcessiveVariance => 9,
            Self::FewPositions => 10,
            Self::LowSnrHard => 11,
            Self::LowSnrSoft => 12,
            Self::MicNotConnected => 13,
            Self::NoSignal => 14,
            Self::NoiseFloorTooHigh => 15,
            Self::PositionOutlierCouplerHf => 16,
            Self::PositionOutlierCouplerLf => 17,
            Self::PositionOutlierRoom => 18,
            Self::SelfExclusionUnavailable => 19,
            Self::SweepRateMismatch => 20,
            Self::VerificationResidual => 21,
            Self::TwoClock => 22,
            Self::TooFewPositions => 23,
            Self::WrongTransducer => 24,
            Self::OverrideOutOfDomain => 25,
            Self::VerificationPreampMismatch => 26,
            Self::VerificationRoutingMismatch => 27,
        }
    }

    /// Every variant, in NUMBER order, so a test can walk the whole space
    /// without a hand-maintained list falling behind the enum.
    ///
    /// The array length is part of the contract: adding a variant without
    /// extending this array does not compile.
    pub const ALL: [Self; 27] = [
        Self::AbsurdCurve,
        Self::CalHasTargetBakedIn,
        Self::CalMalformed,
        Self::CalMissing,
        Self::CalNeighbourOutlier,
        Self::ClippingPosition,
        Self::ClippingSession,
        Self::CoherentAveragingRejected,
        Self::ExcessiveVariance,
        Self::FewPositions,
        Self::LowSnrHard,
        Self::LowSnrSoft,
        Self::MicNotConnected,
        Self::NoSignal,
        Self::NoiseFloorTooHigh,
        Self::PositionOutlierCouplerHf,
        Self::PositionOutlierCouplerLf,
        Self::PositionOutlierRoom,
        Self::SelfExclusionUnavailable,
        Self::SweepRateMismatch,
        Self::VerificationResidual,
        Self::TwoClock,
        Self::TooFewPositions,
        Self::WrongTransducer,
        Self::OverrideOutOfDomain,
        Self::VerificationPreampMismatch,
        Self::VerificationRoutingMismatch,
    ];
}
