//! The contract: one serializable input, one serializable output, and the
//! Domain/Invalidation semantics the spec pins on them. `decide()`'s rules are
//! Stage 6's; only the seam is tested here.

mod common;

use paraeq_decide::{
    decide, CaptureStats, DecisionSet, Domain, Invalidation, MeasurementBundle, TransducerClass,
    Verdict, Verification,
};
use proptest::prelude::*;

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
    assert_eq!(set.decisions.iter().count(), 22);
    assert_eq!(round_trip(&set), serde_json::to_string(&set).unwrap());
}

/// A band-valued decision bounds BOTH its endpoints. Rust's tuple ordering is
/// lexicographic, so a naive `min <= value && value <= max` reads only the
/// lower edge whenever it settles the comparison — `align_spl_band`'s
/// documented `Range` within 100..=8000 would then admit a band running to
/// 99 kHz. The two band decisions (align_spl_band, correction_range) are the
/// spec's only tuple-valued ones, and the drawer and the override path both
/// rest on "every value is inside its domain".
#[test]
fn a_band_domain_bounds_both_endpoints() {
    // align_spl_band's spec row: value (500.0, 2000.0), Range within 100..=8000.
    let domain = Domain::Range {
        max: (8000.0, 8000.0),
        min: (100.0, 100.0),
        step: None,
    };
    assert!(domain.contains(&(500.0, 2000.0)), "the spec's own value");
    assert!(domain.contains(&(100.0, 8000.0)), "endpoints are inclusive");

    // The lexicographic bug: a low edge inside the range decides the whole
    // comparison and the upper edge rides free.
    assert!(
        !domain.contains(&(500.0, 99_999.0)),
        "an out-of-range upper edge must not be admitted by an in-range lower edge"
    );
    // ...and the mirror, which lexicographic ordering happens to catch.
    assert!(!domain.contains(&(99.0, 2000.0)));
    // Both edges out, on the same side.
    assert!(!domain.contains(&(9000.0, 9000.0)));
}

/// `CorrectionPlan.bands` became `PerChannel<Vec<EQBand>>`, a TYPE change with
/// no WIRE change. `#[serde(transparent)]` on `PerChannel` is what makes that
/// true, and it is the whole reason the change costs no fixture diff: if the
/// container ever serialized as `{"0": […]}` or as a newtype wrapper, every
/// `fixtures/decide/<case>/expected.json` would move at once.
#[test]
fn a_per_channel_bands_field_serializes_as_a_bare_array() {
    let plan = common::correction_plan();
    let json = serde_json::to_value(&plan).expect("a plan serializes");

    let bands = json.get("bands").expect("CorrectionPlan carries bands");
    assert!(
        bands.is_array(),
        "PerChannel must serialize as the bare array a Vec<Vec<EQBand>> \
         already produced, not as a wrapper: got {bands}"
    );
    assert_eq!(bands.as_array().unwrap().len(), plan.bands.channels());
    assert!(
        bands[0].is_array(),
        "each channel is still a bare array of bands"
    );

    // And the whole plan still round-trips, so the transparent form reads back.
    assert_eq!(round_trip(&plan), serde_json::to_string(&plan).unwrap());
}

/// The verification pass's own capture metering (MS-21) is a NON-`Option` field
/// on `Verification`, and `MeasurementBundle` derives plain `Deserialize` with
/// no `deny_unknown_fields` and no field defaults — so a bundle that omits it
/// does not load. That is the wanted behaviour, not an accident: a
/// `#[serde(default)]` would silently report "nothing clipped", and a residual
/// computed over a railed microphone is meaningless.
#[test]
fn a_verification_without_capture_stats_fails_to_deserialize() {
    let mut json = serde_json::to_value(common::verification()).expect("serializes");
    assert!(
        json.as_object_mut()
            .expect("Verification is a JSON object")
            .remove("capture")
            .is_some(),
        "the field must be there to remove"
    );

    let reloaded: Result<Verification, _> = serde_json::from_value(json);
    assert!(
        reloaded.is_err(),
        "a Verification with no capture stats must refuse to load"
    );
}

/// A bundle carrying a full verification pass survives the trip. Everything B8
/// needs to compute `K` and the prediction — the level, the gain pin, the
/// engine's own armed preamp, the live rate, the routing and the marker fit —
/// is on this struct, so a field that does not round-trip is a field a golden
/// bundle cannot pin.
#[test]
fn a_bundle_with_a_verification_round_trips() {
    let bundle = common::bundle_with_verification();
    assert_eq!(round_trip(&bundle), serde_json::to_string(&bundle).unwrap());
}

/// `DecisionSet.verification` is `Option` precisely so the golden cases that
/// carry no verification stay byte-stable.
#[test]
fn a_decision_set_without_a_verification_report_round_trips() {
    let mut set = common::decision_set();
    set.verification = None;
    assert_eq!(round_trip(&set), serde_json::to_string(&set).unwrap());
}

/// `CaptureMeter::peak_dbfs()` answers `f64::NEG_INFINITY` on digital silence
/// rather than flooring — deliberately, so nobody mistakes −100 for a level.
/// serde_json writes a non-finite f64 as `null` and then refuses to read it
/// back into an f64, so a silent pass cannot be carried in a bundle at all.
///
/// This test exists to make that a stated constraint on the capture layer
/// rather than a broken fixture discovered during the freeze: whatever writes a
/// `CaptureStats` must floor or refuse first.
#[test]
fn a_non_finite_peak_dbfs_does_not_round_trip() {
    let silent = CaptureStats {
        clipped_samples: 0,
        peak_dbfs: f64::NEG_INFINITY,
        rms_dbfs: f64::NEG_INFINITY,
    };
    let json = serde_json::to_string(&silent).expect("serializing does not fail");
    assert!(
        json.contains("null"),
        "serde_json writes a non-finite f64 as null: {json}"
    );
    assert!(
        serde_json::from_str::<CaptureStats>(&json).is_err(),
        "…and refuses to read null back into an f64, so a digitally silent \
         capture must be floored or refused before it reaches a bundle"
    );
}

proptest! {
    /// `CaptureRouting::Only`'s payload is `u32` rather than `usize` because
    /// this is a frozen wire shape. Every value it can hold must survive the
    /// trip, including the two ends nobody writes by hand.
    #[test]
    fn every_capture_routing_round_trips(channel in any::<u32>()) {
        let routing = paraeq_decide::CaptureRouting::Only(channel);
        let json = serde_json::to_string(&routing).unwrap();
        prop_assert_eq!(
            serde_json::from_str::<paraeq_decide::CaptureRouting>(&json).unwrap(),
            routing
        );
    }

    /// The transparent wire form holds at every channel width, not just the
    /// one and two the hand-written fixtures use.
    #[test]
    fn a_per_channel_of_any_width_serializes_as_a_bare_array(channels in 1usize..8) {
        let per_channel =
            paraeq_dsp::PerChannel::splat(vec![1.0_f64, 2.0], channels).unwrap();
        let json = serde_json::to_value(&per_channel).unwrap();
        prop_assert!(json.is_array());
        prop_assert_eq!(json.as_array().unwrap().len(), channels);
    }
}

/// The cost of `#[serde(transparent)]`, recorded rather than discovered.
///
/// `PerChannel::new` refuses an empty channel list — "a zero-channel value has
/// no meaning anywhere downstream, so emptiness is unrepresentable" — but a
/// transparent deserialization does not route through the constructor, so `[]`
/// on the wire produces exactly the value the constructor refuses. Guarding it
/// would need `serde(try_from = …)`, which cannot be combined with
/// `transparent`, and the ruling pins the wire form (no fixture may move).
///
/// This test states the gap so it is a known constraint on whoever authors
/// `fixtures/decide/` rather than a surprise found after the freeze: producers
/// must not emit an empty channel list.
#[test]
fn an_empty_per_channel_deserializes_although_the_constructor_refuses_it() {
    assert!(
        paraeq_dsp::PerChannel::<Vec<f64>>::new(Vec::new()).is_err(),
        "the constructor is the guard"
    );

    let through_the_wire: paraeq_dsp::PerChannel<Vec<f64>> =
        serde_json::from_str("[]").expect("transparent deserialization bypasses the constructor");
    assert_eq!(
        through_the_wire.channels(),
        0,
        "…and yields the zero-channel value new() refuses — do not emit one"
    );
}
