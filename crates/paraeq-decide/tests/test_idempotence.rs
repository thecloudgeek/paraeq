//! The anti-fork property, over the eight owner-reviewed bundles.
//!
//! The spec states it twice, and both statements are asserted here. As a
//! property: "Override each decision in turn to its own auto value; assert the
//! `DecisionSet` is unchanged but for `source`. This is the anti-fork test."
//! As an invariant: "Overriding a decision to the value auto already chose must
//! produce a `DecisionSet` identical in every field except that decision's
//! `source`. This is … the test that catches 'the override path recomputes
//! differently from the auto path' — the classic form of the fork this whole
//! design exists to prevent."
//!
//! **Why here as well as in `test_props.rs`.** The property test overrides ONE
//! generated field per generated bundle; this file sweeps ALL of them over the
//! eight committed inputs, which is the set the owner reviewed and the set a
//! regression will actually be reported against. The two are complements, not
//! duplicates.
//!
//! **Exhaustive by construction.** The sweep is driven by
//! `Decisions::iter()`'s `DecisionView { id, value }` and writes the override
//! through `Overrides`' matching field name, so a twenty-third decision is
//! covered the day it is added with no edit here. It also exercises
//! `AuthorityCurve`'s sealed `try_from` reload for free: a decided value that
//! cannot round-trip through JSON back into a valid value fails this test,
//! which is precisely the bug worth catching. Do not "optimise" it into typed
//! setters and lose that.

mod common;

use common::golden::{GoldenCase, CASES};
use common::with_override;
use paraeq_decide::{decide, Invalidation, Source};
use serde_json::json;

/// THE ANTI-FORK TEST: eight cases × twenty-two decisions, 176 `decide()` calls.
///
/// It proves three things at once, which is why the neighbouring tests are
/// narrow rather than repeats of this one:
///
/// - **Idempotence**: nothing moves but the one `source`.
/// - **Overrides are not inert**: if the override path ignored the value, the
///   `source` would not flip and this assertion would fail. "Make overrides do
///   nothing" is the obvious way to make an idempotence test pass, and it fails
///   here.
/// - **Every decided value survives a JSON round trip**, because the override
///   is written through `serde_json`. That is what exercises `AuthorityCurve`'s
///   sealed `try_from` reload for free.
#[test]
fn overriding_any_decision_to_its_auto_value_changes_only_its_source() {
    for case in CASES {
        let bundle = GoldenCase::load(case).bundle();
        let auto = decide(&bundle);
        let auto_json = serde_json::to_value(&auto).expect("DecisionSet is plain derived data");

        for view in auto.decisions.iter() {
            let id = view.id;
            let pinned = decide(&with_override(&bundle, id, view.value.clone()));

            let mut expected = auto_json.clone();
            expected["decisions"][id]["source"] = json!("UserOverride");
            assert_eq!(
                serde_json::to_value(&pinned).expect("DecisionSet is plain derived data"),
                expected,
                "{case}: overriding `{id}` to the value auto already chose changed \
                 something other than its own source"
            );
        }
    }
}

/// The same property over a `Default`-sourced decision, which is deliberately
/// NOT the same thing as an `Auto`-sourced one.
///
/// `Source::Default` means "we could not measure this, so we used the default",
/// and the drawer has to be able to say so. Overriding such a decision to the
/// very value the default supplied must still flip `Default → UserOverride` and
/// change nothing else — if the two sources were ever folded together, this is
/// the assertion that would notice.
///
/// Scoped to one case rather than all eight: the sweep above already covers
/// every decision of every case, and this test exists to NAME the `Default` leg
/// so it cannot be deleted by someone who reads the sweep as being only about
/// `Auto`. `in_ear_clean_coupler` is one of the smallest bundles in the set.
#[test]
fn overriding_a_default_sourced_decision_still_flips_only_its_source() {
    let case = "in_ear_clean_coupler";
    let bundle = GoldenCase::load(case).bundle();
    let auto = decide(&bundle);
    let auto_json = serde_json::to_value(&auto).expect("DecisionSet is plain derived data");

    let mut checked = 0usize;
    for view in auto
        .decisions
        .iter()
        .filter(|v| v.source == Source::Default)
    {
        let id = view.id;
        let pinned = decide(&with_override(&bundle, id, view.value.clone()));
        let mut expected = auto_json.clone();
        expected["decisions"][id]["source"] = json!("UserOverride");
        assert_eq!(
            serde_json::to_value(&pinned).expect("DecisionSet is plain derived data"),
            expected,
            "{case}: overriding the Default-sourced `{id}` to its own value moved \
             more than its source"
        );
        let rendered = pinned
            .decisions
            .iter()
            .find(|v| v.id == id)
            .expect("the decision is still present");
        assert_eq!(
            rendered.source,
            Source::UserOverride,
            "{case}: `{id}` was overridden but is still sourced as a default"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no decision came out Source::Default on {case}, so this property was asserted \
         vacuously — check that the two sources are still distinct"
    );
}

/// `decide()` is deterministic over every committed bundle.
///
/// The spec ranks this above the golden fixtures, and it is what makes them
/// meaningful: a characterization fixture over a non-deterministic function
/// would be a flake dressed as a freeze.
#[test]
fn decide_is_deterministic_across_every_golden_bundle() {
    for case in CASES {
        let bundle = GoldenCase::load(case).bundle();
        assert_eq!(
            decide(&bundle),
            decide(&bundle),
            "{case}: two calls on one bundle disagreed"
        );
    }
}

/// The three invalidation tiers PARTITION the decisions: 8 Redesign,
/// 11 Reanalyze, 3 Recapture.
///
/// The spec's own table is 8/11/2 over twenty-one decisions; `clock_adjust` is
/// the twenty-second and its tier is `Recapture` (the two-clock resample is a
/// property of the capture, so changing it re-measures), which makes the
/// partition 8/11/3 over twenty-two. Pinned by count so that a row silently
/// changing tier fails here — visibly, against a number in a test — rather than
/// inside a wall-clock budget nobody measures.
#[test]
fn the_invalidation_tiers_partition_the_twenty_two_decisions() {
    let bundle = GoldenCase::load(CASES[0]).bundle();
    let decisions = decide(&bundle).decisions;

    let count = |tier: Invalidation| decisions.iter().filter(|v| v.invalidates == tier).count();
    let reanalyze = count(Invalidation::Reanalyze);
    let recapture = count(Invalidation::Recapture);
    let redesign = count(Invalidation::Redesign);

    assert_eq!(redesign, 8, "Redesign-tier decisions");
    assert_eq!(reanalyze, 11, "Reanalyze-tier decisions");
    assert_eq!(recapture, 3, "Recapture-tier decisions");
    assert_eq!(
        redesign + reanalyze + recapture,
        decisions.iter().count(),
        "the three tiers must cover every decision exactly once"
    );
}
