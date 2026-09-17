//! `Decisions` is named fields rather than a map so that the Advanced drawer
//! is exhaustive by construction. That property is only real if `iter()` and
//! `Overrides` cannot fall behind the struct, so both tests below derive their
//! expectation from the struct definition itself (via `derive(Serialize)`'s
//! field list) rather than from a hand-maintained list a 23rd field could
//! silently miss.

mod common;

use paraeq_decide::{Domain, Invalidation, Overrides, QCapPolicy, Source};
use paraeq_dsp::authority::{COUPLER_Q_CEILING, ROOM_Q_CEILING};

fn field_names(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("a derived struct serializes to a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

#[test]
fn iter_is_exhaustive_over_every_decisions_field() {
    let decisions = common::decisions();
    let mut ids: Vec<String> = decisions.iter().map(|v| v.id.to_string()).collect();
    ids.sort();

    assert_eq!(
        ids,
        field_names(&serde_json::to_value(&decisions).unwrap()),
        "Decisions::iter() must yield exactly one DecisionView per field: a \
         field it skips is a decision the Advanced drawer cannot render, \
         which is a hidden constant with extra steps"
    );
}

/// Twenty-one in the spec's decision table, plus `clock_adjust` — the 2026-07-21
/// record's § Q6 drawer toggle, which had a reject bound and an estimate but no
/// control until it landed here.
#[test]
fn decisions_carries_the_spec_s_twenty_two_fields() {
    assert_eq!(common::decisions().iter().count(), 22);
}

#[test]
fn overrides_mirrors_decisions_field_for_field() {
    assert_eq!(
        field_names(&serde_json::to_value(Overrides::default()).unwrap()),
        field_names(&serde_json::to_value(common::decisions()).unwrap()),
        "every decision must be overridable, and an Overrides field with no \
         Decisions field is a write nothing reads"
    );
}

#[test]
fn every_view_id_is_unique() {
    let decisions = common::decisions();
    let ids: Vec<&str> = decisions.iter().map(|v| v.id).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn a_view_erases_value_and_domain_without_losing_them() {
    let decisions = common::decisions();
    let view = decisions
        .iter()
        .find(|v| v.id == "fdw_post_cycles")
        .expect("fdw_post_cycles is a Decisions field");

    assert_eq!(view.value, serde_json::json!(15.0));
    assert_eq!(
        view.domain,
        serde_json::json!({ "Range": { "max": 61.0, "min": 3.0, "step": null } })
    );
    assert_eq!(view.invalidates, Invalidation::Reanalyze);
    assert_eq!(view.source, Source::Default);
    assert_eq!(view.rationale.key, decisions.fdw_post_cycles.rationale.key);
}

/// A `Derived` decision erases to a `Derived` domain — read-only in the
/// drawer, never absent from it.
#[test]
fn derived_decisions_are_still_views() {
    let decisions = common::decisions();
    let view = decisions
        .iter()
        .find(|v| v.id == "preamp_db")
        .expect("preamp_db is a Decisions field");
    assert_eq!(view.domain, serde_json::json!("Derived"));
}

/// Assert one decision's value lies inside its own domain, naming the field in
/// the failure so the report says which one drifted.
macro_rules! assert_value_in_domain {
    ($decisions:expr, $field:ident) => {
        assert!(
            $decisions.$field.domain.contains(&$decisions.$field.value),
            concat!(
                stringify!($field),
                ": the value is outside its own Domain, so the drawer would \
                 offer a control that refuses the number it is showing"
            )
        );
    };
}

/// The spec's standing invariant: "every `Decision.value` is inside its
/// `Domain`". Written BEFORE any decision rule exists, so that the two domain
/// defects the pre-freeze window exists to fix fail loudly rather than being
/// papered over once `decide()` has a body.
///
/// It is spelled out field by field rather than looped, because `Decisions::iter`
/// erases `value` and `domain` to `serde_json::Value` and a JSON-level
/// comparison would not be the `contains` the invariant is about.
#[test]
fn every_decision_value_is_inside_its_own_domain() {
    let d = common::decisions();
    assert_value_in_domain!(d, align_spl_band);
    assert_value_in_domain!(d, authority);
    assert_value_in_domain!(d, averaging);
    assert_value_in_domain!(d, class);
    assert_value_in_domain!(d, clock_adjust);
    assert_value_in_domain!(d, correction_kind);
    assert_value_in_domain!(d, correction_range);
    assert_value_in_domain!(d, fdw_post_cycles);
    assert_value_in_domain!(d, fdw_pre_cycles);
    assert_value_in_domain!(d, flatness_target_db);
    assert_value_in_domain!(d, left_window_ms);
    assert_value_in_domain!(d, low_corner_hz);
    assert_value_in_domain!(d, max_filters);
    assert_value_in_domain!(d, positions_n);
    assert_value_in_domain!(d, preamp_db);
    assert_value_in_domain!(d, q_cap);
    assert_value_in_domain!(d, right_window_ms);
    assert_value_in_domain!(d, shelves);
    assert_value_in_domain!(d, smoothing);
    assert_value_in_domain!(d, target);
    assert_value_in_domain!(d, transition_hz);
    assert_value_in_domain!(d, window_type);
}

/// D-C, the falsifier. `q_cap`'s domain used to be a `Range` over `QCapPolicy`,
/// which has no ordering — so `InRange`'s `false` default answered `contains`
/// for the room path's own value and the invariant above could never fail. The
/// `Choice` form is the two path policies, which is what the value actually is.
#[test]
fn the_q_cap_domain_is_a_choice_over_the_two_path_policies() {
    let domain = common::decisions().q_cap.domain;
    assert_eq!(
        domain,
        Domain::Choice(vec![COUPLER_Q_CEILING, ROOM_Q_CEILING]),
        "the drawer's control is a two-item select, read from paraeq-dsp"
    );

    // The defect the Choice form closes: a Range over QCapPolicy answers
    // `contains == false` for every value, including its own endpoints.
    let as_a_range = Domain::Range {
        max: QCapPolicy::Ceiling(20.0),
        min: QCapPolicy::Ceiling(1.0),
        step: None,
    };
    assert!(
        !as_a_range.contains(&QCapPolicy::Ceiling(5.0)),
        "a Range over an unordered type cannot contain anything — this is the \
         unfalsifiable invariant D-C removes, pinned so nobody restores it"
    );
}

/// `clock_adjust` is the 22nd field and its override costs a re-measure: the
/// resample is applied to the capture, so turning it off means capturing again.
#[test]
fn the_clock_adjust_decision_invalidates_at_the_recapture_tier() {
    let decisions = common::decisions();
    let view = decisions
        .iter()
        .find(|v| v.id == "clock_adjust")
        .expect("clock_adjust is a Decisions field");
    assert_eq!(view.invalidates, Invalidation::Recapture);
    assert_eq!(view.value, serde_json::json!(true), "defaulted on (Q6)");
}
