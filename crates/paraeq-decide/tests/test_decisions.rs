//! `Decisions` is named fields rather than a map so that the Advanced drawer
//! is exhaustive by construction. That property is only real if `iter()` and
//! `Overrides` cannot fall behind the struct, so both tests below derive their
//! expectation from the struct definition itself (via `derive(Serialize)`'s
//! field list) rather than from a hand-maintained list a 22nd field could
//! silently miss.

mod common;

use paraeq_decide::{Invalidation, Overrides, Source};

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

#[test]
fn decisions_carries_the_spec_s_twenty_one_fields() {
    assert_eq!(common::decisions().iter().count(), 21);
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
