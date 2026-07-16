//! The contract: one serializable input, one serializable output, and the
//! Domain/Invalidation semantics the spec pins on them. `decide()`'s rules are
//! Stage 6's; only the seam is tested here.

mod common;

use paraeq_decide::{
    decide, DecisionSet, Domain, Invalidation, MeasurementBundle, TransducerClass, Verdict,
};

fn round_trip<T>(value: &T) -> String
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("wire types must serialize");
    let back: T = serde_json::from_str(&json).expect("wire types must deserialize");
    serde_json::to_string(&back).expect("wire types must re-serialize")
}

/// "One serializable input: it is the fixture, the bug report attachment, and
/// the profile's analysis record." A field that does not survive the trip is a
/// field a golden bundle cannot pin.
#[test]
fn measurement_bundle_round_trips() {
    let bundle = common::bundle();
    assert_eq!(round_trip(&bundle), serde_json::to_string(&bundle).unwrap());
}

#[test]
fn decision_set_round_trips() {
    let set = common::decision_set();
    assert_eq!(round_trip(&set), serde_json::to_string(&set).unwrap());
}

#[test]
fn evidence_round_trips_in_all_three_shapes() {
    let evidence = common::evidence();
    assert_eq!(
        round_trip(&evidence),
        serde_json::to_string(&evidence).unwrap()
    );
}

#[test]
fn overrides_round_trip_carries_a_user_intent() {
    let overrides = paraeq_decide::Overrides {
        transition_hz: Some(140.0),
        ..Default::default()
    };
    assert_eq!(
        round_trip(&overrides),
        serde_json::to_string(&overrides).unwrap()
    );
}

/// The signature IS the design decision (no `Result`, no mode argument, one
/// argument in, one value out). If this stops compiling, the seam moved.
#[test]
fn decide_has_the_spec_s_signature() {
    let _: fn(&MeasurementBundle) -> DecisionSet = decide;
}

#[test]
fn range_domain_contains_its_endpoints_and_nothing_outside() {
    let domain = Domain::Range {
        max: 61.0,
        min: 3.0,
        step: None,
    };
    assert!(domain.contains(&3.0));
    assert!(domain.contains(&15.0));
    assert!(domain.contains(&61.0));
    assert!(!domain.contains(&2.9));
    assert!(!domain.contains(&61.1));
}

#[test]
fn choice_domain_contains_exactly_its_alternatives() {
    let domain = Domain::Choice(vec![TransducerClass::InEar, TransducerClass::OverEar]);
    assert!(domain.contains(&TransducerClass::InEar));
    assert!(domain.contains(&TransducerClass::OverEar));
    assert!(!domain.contains(&TransducerClass::Bookshelf));
}

/// "Derived and not user-settable directly — override its inputs instead.
/// Rendered read-only in the drawer, never hidden." So a Derived domain
/// constrains nothing (the rule that produced the value is the constraint) but
/// still answers the drawer's one question.
#[test]
fn derived_domain_constrains_nothing_and_is_not_user_settable() {
    let derived: Domain<f64> = Domain::Derived;
    assert!(derived.contains(&-4.2));
    assert!(derived.contains(&1e9));
    assert!(!derived.is_user_settable());

    assert!(Domain::Choice(vec![1.0_f64]).is_user_settable());
    assert!(Domain::Range {
        max: 1.0_f64,
        min: 0.0,
        step: None
    }
    .is_user_settable());
}

/// The tiers nest: `Reanalyze` re-runs the analysis "then Redesign";
/// `Recapture` re-runs everything. A cascade of overrides therefore takes the
/// max, which is only meaningful if the order is cost order rather than the
/// alphabetical declaration order.
#[test]
fn invalidation_tiers_order_by_cost_not_by_name() {
    assert!(Invalidation::Redesign < Invalidation::Reanalyze);
    assert!(Invalidation::Reanalyze < Invalidation::Recapture);
    assert_eq!(
        [
            Invalidation::Redesign,
            Invalidation::Recapture,
            Invalidation::Reanalyze,
        ]
        .into_iter()
        .max(),
        Some(Invalidation::Recapture)
    );
}

/// A refusal must still carry its decisions and evidence — that is why
/// `decide()` returns `DecisionSet` and not `Result<DecisionSet, E>`.
#[test]
fn a_refusal_is_still_a_full_decision_set() {
    let mut set = common::decision_set();
    set.correction = None;
    set.verdict = Verdict::Refuse;
    assert_eq!(set.decisions.iter().count(), 21);
    assert_eq!(round_trip(&set), serde_json::to_string(&set).unwrap());
}
