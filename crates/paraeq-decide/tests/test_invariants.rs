//! The analytic invariants, which the spec ranks **above** the golden bundles
//! in authority: "`decide()` is deterministic (same bundle → same set, N
//! times); every `Decision.value` is inside its `Domain`; `verdict == Refuse ⟺
//! correction.is_none()`; `verdict == Refuse ⟺ any diagnostic has
//! `Severity::Refuse`" (`docs/specs/2026-07-15-decision-engine-design.md`,
//! § Testing Strategy, item 2), plus the idempotence property from § Override
//! Semantics.
//!
//! These are B7a's, and they are written against the SKELETON deliberately:
//! they are the guard rails the rules (B7b) and the refusal table (B7c) land
//! inside. The two `refuse_iff_*` tests are vacuously true while the refusal
//! table is an empty pass — that is the reason to write them now rather than
//! after the thing they constrain.
//!
//! The Jury / authority-envelope / boost-Q invariants from the same spec list
//! are NOT here: they are properties of emitted bands, and the skeleton emits
//! none. They land with B7b, which emits the first one.

mod common;

use common::{
    assert_every_value_in_domain, assert_refusal_is_consistent, minimal_bundle, synthetic_bundle,
    well_formed_bundle, with_override, SyntheticSpec, EVERY_CLASS,
};
use paraeq_decide::{decide, Source, TransducerClass};

/// The cheapest, fastest-failing guard on "no clock, no RNG, no I/O". A
/// `decide()` that read any of the three would disagree with itself here.
#[test]
fn decide_is_deterministic() {
    for class in EVERY_CLASS {
        let bundle = well_formed_bundle(class);
        let first = decide(&bundle);
        for repetition in 0..8 {
            assert_eq!(
                decide(&bundle),
                first,
                "{class:?}: repetition {repetition} disagreed with the first run"
            );
        }
    }
}

/// `Position::captured_at_ms` is documented "Provenance only. NEVER a decision
/// input — `decide()` is deterministic". Perturbing every timestamp must leave
/// the whole `DecisionSet` identical.
#[test]
fn captured_at_ms_is_never_a_decision_input() {
    let bundle = well_formed_bundle(TransducerClass::Bookshelf);
    let mut perturbed = bundle.clone();
    for (offset, position) in perturbed.positions.iter_mut().enumerate() {
        // Distinct, large, and running BACKWARDS, so an implementation that
        // sorted or differenced them could not accidentally agree.
        position.captured_at_ms = 9_000_000_000_000 - offset as u64 * 86_400_000;
    }
    assert_eq!(decide(&perturbed), decide(&bundle));
}

/// Every `Decision.value` is inside its `Domain`, on all four paths.
///
/// The bundles are WELL FORMED (each path's own `positions_default`), and that
/// restriction is deliberate: `positions_n` echoes the accepted count verbatim
/// per the decision table, so a two-position bundle carries a value outside
/// `Range 3..=15` by design — that is the `TooFewPositions` refusal (B7c), not
/// a domain violation to paper over by clamping the echo.
#[test]
fn every_decision_value_is_inside_its_domain() {
    for class in EVERY_CLASS {
        let set = decide(&well_formed_bundle(class));
        assert_every_value_in_domain(&set.decisions, class);
    }
}

/// "`None` iff `verdict == Refuse`" — an installable correction and a refusal
/// are the two halves of one bit, in both directions.
#[test]
fn refuse_iff_correction_is_none() {
    for class in EVERY_CLASS {
        assert_refusal_is_consistent(&decide(&well_formed_bundle(class)));
    }
}

/// "`verdict == Refuse ⟺ any diagnostic has `Severity::Refuse`". The
/// complementary half stops a `Refuse` verdict that cannot explain itself, and
/// a refusing diagnostic that was raised and then ignored.
#[test]
fn refuse_iff_a_diagnostic_is_refuse_severity() {
    for class in EVERY_CLASS {
        assert_refusal_is_consistent(&decide(&well_formed_bundle(class)));
    }
}

/// **The anti-fork test.** § Override Semantics, verbatim: "Overriding a
/// decision to the value auto already chose must produce a `DecisionSet`
/// identical in every field except that decision's `source`."
///
/// It catches "the override path recomputes differently from the auto path",
/// which is the classic form of the fork the whole design exists to prevent —
/// and it is fixture-driven rather than generated because overriding to an
/// ARBITRARY value is not idempotence, and that property would be false by
/// design.
///
/// Mechanics: the sweep goes through `Decisions::iter()`, whose `id` IS the
/// `Overrides` field name, so a 23rd decision joins the sweep automatically
/// instead of being silently skipped by a hand-written list.
#[test]
fn overriding_a_decision_to_its_auto_value_changes_only_its_source() {
    for class in EVERY_CLASS {
        let bundle = minimal_bundle(class);
        let auto = decide(&bundle);
        let auto_json = serde_json::to_value(&auto).expect("DecisionSet is plain derived data");
        for view in auto.decisions.iter() {
            let pinned = decide(&with_override(&bundle, view.id, view.value.clone()));
            let mut expected = auto_json.clone();
            expected["decisions"][view.id]["source"] =
                serde_json::to_value(Source::UserOverride).expect("Source is a unit variant");
            assert_eq!(
                serde_json::to_value(&pinned).expect("DecisionSet is plain derived data"),
                expected,
                "{class:?}: overriding {} to its own auto value moved something other than its \
                 source",
                view.id
            );
        }
    }
}

/// The complement of idempotence, and the reason it is not vacuous: an
/// override must actually ARRIVE. Without this, "idempotence" is satisfiable by
/// making overrides inert, which would be the same fork wearing the other mask.
#[test]
fn an_override_is_recorded_as_a_user_override() {
    let bundle = minimal_bundle(TransducerClass::Bookshelf);
    let auto = decide(&bundle);
    assert_eq!(auto.decisions.transition_hz.source, Source::Default);

    let pinned = decide(&with_override(
        &bundle,
        "transition_hz",
        serde_json::json!(137.0),
    ));
    assert_eq!(pinned.decisions.transition_hz.source, Source::UserOverride);
    assert_eq!(pinned.decisions.transition_hz.value, 137.0);
}

/// A bundle with no positions at all is not reachable in the product — the
/// wizard cannot get to Result without a capture — but it IS representable in
/// the type, and a hand-edited profile or a truncated bug report can carry one.
/// `decide()` has no `Result`, so the only correct behaviour is a complete
/// `DecisionSet`, not a panic.
#[test]
fn a_bundle_with_no_positions_still_produces_a_complete_decision_set() {
    let set = decide(&synthetic_bundle(SyntheticSpec {
        positions: 0,
        ..SyntheticSpec::default()
    }));
    assert_eq!(set.decisions.iter().count(), 22);
    assert!(set.analysis.per_position_db.is_empty());
    assert_eq!(set.analysis.sigma_db.len(), set.analysis.freqs_hz.len());
    assert_refusal_is_consistent(&set);
}

/// A non-finite sample is a refusal condition, not an analysis input: the
/// analysis stage must not feed it to `fr::smooth` (which refuses it) or
/// average it into a curve. It publishes the empty products instead and leaves
/// the diagnosis to B7c.
#[test]
fn a_non_finite_impulse_response_does_not_panic_or_poison_the_analysis() {
    let mut bundle = well_formed_bundle(TransducerClass::Bookshelf);
    bundle.positions[0].ir.samples[0][10] = f64::NAN;
    let set = decide(&bundle);
    assert!(set
        .analysis
        .averaged_db
        .iter()
        .flatten()
        .all(|v| v.is_finite()));
    assert!(set.analysis.sigma_db.iter().all(|v| v.is_finite()));
    assert_refusal_is_consistent(&set);
}

/// The analysis stage BAILS OUT silently by design — an unanalysable bundle
/// gets the empty products rather than a fabricated curve — so "the invariants
/// hold" is satisfiable by never analysing anything. This is the test that says
/// the four paths really do produce curves, and it is here because an early
/// draft of the skeleton passed every invariant above while quietly analysing
/// nothing at all (it asked `apply_gate` for a right window longer than the
/// whole recording, which that function refuses outright).
#[test]
fn the_analysis_stage_produces_curves_on_every_path() {
    for class in EVERY_CLASS {
        let bundle = well_formed_bundle(class);
        let set = decide(&bundle);
        assert_eq!(
            set.analysis.per_position_db.len(),
            bundle.positions.len(),
            "{class:?}: per_position_db must stay index-parallel to positions"
        );
        assert_eq!(
            set.analysis.averaged_db.len(),
            bundle.positions[0].ir.samples.len(),
            "{class:?}: averaged_db is per channel"
        );
        for curve in set
            .analysis
            .averaged_db
            .iter()
            .chain(&set.analysis.per_position_db)
        {
            assert_eq!(curve.len(), set.analysis.freqs_hz.len());
            assert!(curve.iter().all(|v| v.is_finite()));
        }
        // σ(f) over nine positions of a room capture is a real spread, not the
        // all-`SIGMA_NONE_DB` fallback the unanalysable path publishes.
        assert!(
            set.analysis.sigma_db.iter().any(|s| *s > 0.0 && *s < 6.0),
            "{class:?}: sigma looks like the no-evidence fallback, not a measurement"
        );
    }
}
