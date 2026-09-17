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
//! test over [`Decisions::iter`]. Above those sit the analytic invariants
//! (`tests/test_invariants.rs`, `tests/test_props.rs`), which the spec ranks
//! above the golden bundles: determinism, every value inside its domain, the
//! two refusal equivalences, and the idempotence property. The golden
//! `fixtures/decide/<case>/{bundle,expected}.json` characterization bundles are
//! owner-reviewed once and then frozen, so that a diff in `expected.json` is a
//! policy change that must be argued for in the PR.

mod analysis;
pub mod bundle;
pub mod decision;
pub mod decisions;
pub mod outcome;
pub mod profile;
mod rationale;
mod refusal;
mod rules;

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
/// # The stages, in order
///
/// 1. **`class`**, first and alone, because the [`PathProfile`] is derived from
///    it and every other rule reads the profile.
/// 2. **The eleven decisions the analysis stage consumes** — the window, the
///    two gates, the FDW cycles, smoothing, the alignment band, averaging, the
///    two echoes and the clock toggle.
/// 3. **The analysis**: gate → spectrum → derotate → log grid → FDW →
///    compensation → smoothing → Align SPL → averaging → σ(f) → the EGD trace.
///    Pure orchestration over `paraeq-dsp`; see `analysis.rs`.
/// 4. **The ten decisions read off those curves**, including the `authority`
///    preset, whose resolved curve is published in [`Analysis`] as a product.
/// 5. **The correction plan**, and then `preamp_db`, which is a property of the
///    bands the fit emitted rather than of the curve they were fitted to.
/// 6. **The refusal table**, and the verdict it implies.
///
/// # Skeleton status
///
/// This is B7a: the stages, the types and the invariants are here; the decision
/// RULES (B7b) and the refusal table (B7c) are not. Every decision therefore
/// carries [`Source::Default`] — the enum's own documented meaning, "we could
/// not measure this, so we used the default" — and the correction carries no
/// bands. Nothing here is a plausible-looking value pretending to be a decided
/// one, which is the condition the previous `unimplemented!()` existed to avoid.
pub fn decide(bundle: &MeasurementBundle) -> DecisionSet {
    let grid = paraeq_dsp::logf::LogGrid::standard();
    let geometry = analysis::geometry(bundle);
    let class = rules::class(bundle);
    let profile = profile_for(class.value);

    let analysis_decisions = rules::analysis_decisions(bundle, class, &grid, profile, &geometry);
    let products = analysis::analyze(
        bundle,
        &grid,
        &analysis::AnalysisSettings {
            align_spl_band: analysis_decisions.align_spl_band.value,
            averaging: analysis_decisions.averaging.value,
            // The profile decides whether the pass runs at all — "Coupler: off"
            // is `GatingMode::None` — while the two decisions carry the cycle
            // counts it runs with.
            fdw: match profile.gating {
                GatingMode::Fdw { .. } => Some(paraeq_dsp::fdw::FdwSpec {
                    post_cycles: analysis_decisions.fdw_post_cycles.value,
                    pre_cycles: analysis_decisions.fdw_pre_cycles.value,
                }),
                GatingMode::None => None,
            },
            left_window_ms: analysis_decisions.left_window_ms.value,
            right_window_ms: analysis_decisions.right_window_ms.value,
            smoothing: analysis_decisions.smoothing.value,
            window: paraeq_dsp::window::WindowSpec {
                left: analysis::window_kind(analysis_decisions.window_type.value),
                right: analysis::window_kind(analysis_decisions.window_type.value),
            },
        },
    );

    let (design, authority_curve) = rules::design_decisions(
        bundle,
        analysis_decisions.class.value,
        &grid,
        profile,
        &products,
    );

    // B7b's `autofit::auto_fit_room` call lands here, between the design
    // decisions and the preamp: it is the one step that turns the decided
    // authority, Q cap, correction range, filter budget, flatness target and
    // shelf policy into bands. Until it does, the plan carries no bands, and
    // `preamp_db` is correspondingly 0.0 — the same answer a pure-cut cascade
    // gets, and not a guess about one.
    //
    // `.max(1)`: `PerChannel` is non-empty by construction, and a bundle the
    // analysis could not read reports zero channels. One channel of zero bands
    // is the honest shape for "there is a plan and it contains nothing".
    let bands: Vec<Vec<paraeq_dsp::peq::EQBand>> = vec![Vec::new(); products.channels.max(1)];
    let design_rate = geometry.sample_rate;
    let preamp_db = rules::preamp_decision(bundle, &bands, design_rate);
    let decisions = rules::assemble(analysis_decisions, design, preamp_db);

    let diagnostics = refusal::diagnostics(bundle, &decisions, &products);
    let verdict = verdict_for(&diagnostics);
    DecisionSet {
        analysis: Analysis {
            authority: authority_curve,
            averaged_db: products.averaged_db,
            excess_group_delay_s: products.excess_group_delay_s,
            freqs_hz: grid.freqs().to_vec(),
            per_position_db: products.per_position_db,
            sigma_db: products.sigma_db,
        },
        correction: match verdict {
            // "`None` iff `verdict == Refuse`", and the refusal is what earns
            // auto mode the right to hide everything: a `Refuse` produces no
            // installable correction, so the system stays as it was.
            Verdict::Refuse => None,
            _ => Some(CorrectionPlan {
                bands: paraeq_dsp::PerChannel::new(bands)
                    .expect("at least one channel by construction"),
                clamps: Vec::new(),
                design_rate,
                dropped: Vec::new(),
                preamp_db: decisions.preamp_db.value,
            }),
        },
        decisions,
        diagnostics,
        verdict,
        // B8 fills this from the bundle's `verification` block. `Option` keeps
        // every case that carries no verification byte-stable.
        verification: None,
    }
}

/// The verdict the diagnostics imply. Refusal outranks a warning, and a
/// `Refuse` is not "severity theatre": it produces no installable correction.
fn verdict_for(diagnostics: &[Diagnostic]) -> Verdict {
    if diagnostics.iter().any(|d| d.severity == Severity::Refuse) {
        Verdict::Refuse
    } else if diagnostics.is_empty() {
        Verdict::Proceed
    } else {
        Verdict::ProceedWithWarnings
    }
}
