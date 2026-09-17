//! The auto-decision engine: one pure function over one serializable value.
//!
//! Every parameter a competitor asks the user or hardcodes becomes a typed
//! [`Decision<T>`] carrying its value, its legal domain, its provenance, the
//! measured evidence that produced it, the rationale guided mode renders, and
//! the invalidation tier an override triggers. Both front-ends ("just fix my
//! sound", "walk me through it") and the Advanced drawer are pure renderers
//! over one [`DecisionSet`]: there is no second code path and no second set of
//! defaults, and [`decide`] cannot know which of them is rendering it.
//!
//! No filesystem I/O, no clock, no RNG: targets and cal contents arrive
//! pre-parsed in the bundle, so a stored bundle plus its expected
//! `DecisionSet` is a fixture and "auto picked something stupid" is a bug
//! report you can attach.
//!
//! Test tier: **Tier 3 (analytic)** — `decide()` is policy, not DSP, and there
//! is no prototype decision engine to port; writing one for the purpose would
//! not be an independent oracle, it would launder a design bug into a golden
//! fixture. The contract is pinned by construction: serde round-trips on the
//! wire types, the [`PathProfile`] data table, and a structural exhaustiveness
//! test over [`Decisions::iter`]. `decide()`'s own rules land in Stage 6
//! against `fixtures/decide/<case>/{bundle,expected}.json` characterization
//! bundles — owner-reviewed once, then frozen, so that a diff in
//! `expected.json` is a policy change that must be argued for in the PR.

pub mod bundle;
pub mod decision;
pub mod decisions;
pub mod outcome;
pub mod profile;

pub use bundle::{
    CalFile, CalVariant, CapturePlan, CaptureRouting, CaptureStats, ImpulseResponse,
    MeasurementBundle, NoiseFloor, Position, SweepPlan, TwoClockFit, Verification,
};
pub use decision::{
    Decision, Domain, Evidence, EvidenceLabel, Invalidation, Rationale, RationaleKey, Source, Unit,
};
pub use decisions::{
    AuthorityCurve, AuthorityPreset, CorrectionForm, DecisionView, Decisions, Overrides,
    QCapPolicy, TargetChoice, WindowType,
};
pub use outcome::{
    Analysis, CorrectionPlan, DecisionSet, Diagnostic, DiagnosticCode, Severity, Verdict,
    VerificationReport,
};
/// Re-exported: the enum lives in `paraeq-dsp` because that is the only crate
/// every consumer may depend on, but its semantics are owned here — it is
/// `decide()`'s one unavoidable question.
pub use paraeq_dsp::targets::TransducerClass;
pub use profile::{
    profile_for, AuthorityKind, AveragingMode, CouplingPath, GatingMode, PathProfile, SmoothingMode,
};

/// Decide everything, from one measurement.
///
/// One argument and one return, by design. The profile is derivable from
/// `bundle.class`, so a second argument would be a second place to disagree;
/// overrides live in the bundle so the whole input stays one value; and there
/// is no `Result`, because a refusal must still carry the decisions and
/// evidence that produced it (see [`Verdict::Refuse`]).
///
/// # Panics
///
/// Always: the decision rules are Stage 6's work and this crate currently
/// scaffolds only their contract. The alternative — returning a `Refuse`
/// carrying 21 plausible-looking default values and empty rationales — would
/// materialize the decision table's policy columns without the rules that
/// justify them, and a `DecisionSet` that looks decided but was not is exactly
/// the "second, lying source of truth about what the app did" this design
/// exists to prevent. A panic cannot be mistaken for a decision.
pub fn decide(bundle: &MeasurementBundle) -> DecisionSet {
    let _ = bundle;
    unimplemented!("decision rules land in Stage 6; this crate scaffolds the contract")
}
