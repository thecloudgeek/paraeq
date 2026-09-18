//! Property tests over generated bundles.
//!
//! Scope, from the plan's own ruling: **proptest for determinism and the
//! algebraic invariants; fixtures for idempotence.** Overriding a decision to
//! an ARBITRARY generated value is not idempotence, and that property would be
//! false by design — so the idempotence property here overrides one generated
//! FIELD to the value auto already chose, never to a generated value. The
//! exhaustive 22-field sweep lives in `test_invariants.rs`.
//!
//! The generator half is what justifies the dev-dependency: synthesizing
//! bundles across the four transducer classes, both coupling paths, 1 or 2
//! channels and the three shipping sample rates is exactly the input space a
//! handful of literals cannot cover, and "the analysis stage runs on all of it
//! without panicking" is the claim B7b and B7c build on.
//!
//! Case counts are low on purpose. Each case runs the full analysis stage —
//! per position, per channel: a gate, an FFT of up to 128 k points, a log-grid
//! resample, an FDW pass and a smooth — so a hundred cases would buy coverage
//! this input space does not have and cost minutes of CI.

mod common;

use common::{
    assert_every_value_in_domain, assert_refusal_is_consistent, synthetic_bundle, with_override,
    SyntheticSpec, EVERY_CLASS,
};
use paraeq_decide::{decide, TransducerClass};
use proptest::prelude::*;

/// The four classes, two per coupling path.
fn any_class() -> impl Strategy<Value = TransducerClass> {
    prop::sample::select(EVERY_CLASS.to_vec())
}

/// The three rates the product ships against: 44.1 kHz (CD / most DACs),
/// 48 kHz (the Mac default) and 96 kHz (the high-rate case, where every window
/// in samples doubles).
fn any_rate() -> impl Strategy<Value = u32> {
    prop::sample::select(vec![44_100u32, 48_000, 96_000])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// The plan's own property: `decide()` is TOTAL (it returns a complete
    /// `DecisionSet` for every bundle in this space) and every value is inside
    /// its domain.
    ///
    /// `positions` starts at 3 because that is the decision table's own hard
    /// minimum and therefore `positions_domain`'s lower bound: below it the
    /// echoed count is outside its domain BY DESIGN, and that case is
    /// `TooFewPositions` (B7c), tested separately below for totality alone.
    #[test]
    fn decide_is_total_and_in_domain_over_synthetic_bundles(
        cal in any::<bool>(),
        channels in 1usize..=2,
        class in any_class(),
        positions in 3usize..=9,
        sample_rate in any_rate(),
        seed in any::<u64>(),
    ) {
        let bundle = synthetic_bundle(SyntheticSpec { cal, channels, class, positions, sample_rate, seed });
        let set = decide(&bundle);

        prop_assert_eq!(set.decisions.iter().count(), 22);
        prop_assert_eq!(set.analysis.per_position_db.len(), positions);
        prop_assert_eq!(set.analysis.averaged_db.len(), channels);
        prop_assert_eq!(set.analysis.sigma_db.len(), set.analysis.freqs_hz.len());
        prop_assert_eq!(
            set.analysis.excess_group_delay_s.len(),
            set.analysis.freqs_hz.len()
        );
        for curve in set.analysis.averaged_db.iter().chain(&set.analysis.per_position_db) {
            prop_assert_eq!(curve.len(), set.analysis.freqs_hz.len());
            prop_assert!(curve.iter().all(|v| v.is_finite()));
        }
        assert_every_value_in_domain(&set.decisions, class);
        assert_refusal_is_consistent(&set);
    }

    /// **The four transducer classes and both coupling paths run through the
    /// analysis stage without panicking**, down to a single position — which
    /// the room path's `positions_floor` forbids but the TYPE permits, and
    /// which makes σ(f) identically zero because a population standard
    /// deviation of one sample is zero. That degenerate σ must produce a curve,
    /// not a panic.
    #[test]
    fn decide_never_panics_on_any_class_path_or_rate(
        channels in 1usize..=2,
        class in any_class(),
        positions in 1usize..=9,
        sample_rate in any_rate(),
        seed in any::<u64>(),
    ) {
        let set = decide(&synthetic_bundle(SyntheticSpec {
            cal: true,
            channels,
            class,
            positions,
            sample_rate,
            seed,
        }));
        prop_assert_eq!(set.decisions.iter().count(), 22);
        assert_refusal_is_consistent(&set);
    }

    /// Determinism over the generated space, not just the four fixed bundles:
    /// the same bundle decides the same way, twice.
    #[test]
    fn decide_is_deterministic_over_synthetic_bundles(
        channels in 1usize..=2,
        class in any_class(),
        positions in 3usize..=5,
        sample_rate in any_rate(),
        seed in any::<u64>(),
    ) {
        let bundle = synthetic_bundle(SyntheticSpec {
            cal: true,
            channels,
            class,
            positions,
            sample_rate,
            seed,
        });
        prop_assert_eq!(decide(&bundle), decide(&bundle));
    }

    /// Idempotence over the generated space. One field per case, chosen by the
    /// generator, overridden **to the value auto already chose** — never to a
    /// generated value, which would not be idempotence.
    #[test]
    fn overriding_one_generated_field_to_its_auto_value_changes_only_its_source(
        channels in 1usize..=2,
        class in any_class(),
        field in 0usize..22,
        positions in 3usize..=5,
        seed in any::<u64>(),
    ) {
        let bundle = synthetic_bundle(SyntheticSpec {
            cal: true,
            channels,
            class,
            positions,
            sample_rate: 48_000,
            seed,
        });
        let auto = decide(&bundle);
        let view = auto.decisions.iter().nth(field).expect("22 decisions, index < 22");
        let id = view.id;
        let value = view.value.clone();

        let pinned = decide(&with_override(&bundle, id, value));
        let mut expected = serde_json::to_value(&auto).expect("DecisionSet is plain derived data");
        expected["decisions"][id]["source"] = serde_json::json!("UserOverride");
        prop_assert_eq!(
            serde_json::to_value(&pinned).expect("DecisionSet is plain derived data"),
            expected
        );
    }
}
