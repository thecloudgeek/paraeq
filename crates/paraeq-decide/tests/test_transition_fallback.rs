//! The 200 Hz transition fallback, as the UI sees it.
//!
//! `docs/specs/2026-07-15-wizard-design.md:566`: if excess-group-delay masking
//! slips to v1.1, "the 200 Hz **fallback constant** is what ships — and it must
//! be labelled a fallback in the code, the UI, and the log, never a rule". The
//! three legs live in three places on purpose — the named constant is
//! `paraeq_dsp::room::TRANSITION_FALLBACK_HZ`, the log line is one `log::info!`
//! at the desktop boundary, and **the UI leg is a Rust string**, because
//! `Rationale::text` is "Rendered here, in Rust. The UI is a text field, not an
//! author". This file is the UI leg's test.
//!
//! **Why the string is not selected on `Decision::source`.** It would be the
//! obvious selector and it would break the crate's idempotence invariant
//! (`test_invariants.rs::overriding_a_decision_to_its_auto_value_changes_only_its_source`
//! and its proptest twin): overriding a decision to the value auto already chose
//! must move `source` and NOTHING else, so any field that reads `source` forks
//! the two runs. The selector is the ANALYSIS FACT instead — the σ-crossing scan
//! found no sustained crossing — which no override can touch. The last two tests
//! here are that claim's falsifiers, in both directions.
//!
//! Tier: **3 (analytic)** — policy and copy, like the rest of `decide()`'s
//! tests. The bundles are `common::shaped_bundle`'s: a unit impulse through a
//! biquad cascade this file names, so σ(f) is a quantity the test chose.

mod common;

use common::{shaped_bundle, with_override, SyntheticSpec};
use paraeq_decide::{decide, Evidence, EvidenceLabel, RationaleKey, Source, TransducerClass};
use paraeq_dsp::biquad;
use paraeq_dsp::room::TRANSITION_FALLBACK_HZ;

/// The rate every shaped bundle is built at.
const RATE: f64 = 48_000.0;

/// The derived case: five positions that disagree broadly around 140 Hz, which
/// puts σ(f) over 3 dB for about an octave — a crossing the scan can sustain,
/// so `transition_hz` is MEASURED.
fn measured_crossing_bundle() -> paraeq_decide::MeasurementBundle {
    shaped_bundle(
        SyntheticSpec {
            cal: false,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0003,
        },
        |index| vec![biquad::peaking(140.0, -6.0 + 3.0 * index as f64, 1.0, RATE)],
    )
}

/// The fallback case: identical positions, so σ(f) ≈ 0 everywhere and no bin
/// anywhere on the grid starts a sustained crossing. Nothing was measured, and
/// the copy has to say so.
fn no_crossing_bundle() -> paraeq_decide::MeasurementBundle {
    shaped_bundle(
        SyntheticSpec {
            cal: false,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0004,
        },
        |_| Vec::new(),
    )
}

/// The decision table's sentence for this row is written for a MEASUREMENT
/// ("Below {f_t:.0} Hz your room's problems are the same everywhere you sit"),
/// and rendering it over the fallback claims a measurement that was never made.
/// The two cases must render different strings, and the fallback one must say
/// what actually happened.
#[test]
fn the_transition_fallback_renders_as_a_fallback_not_a_measurement() {
    let derived = decide(&measured_crossing_bundle()).decisions.transition_hz;
    let fallback = decide(&no_crossing_bundle()).decisions.transition_hz;

    // Preconditions, so a change that stops either bundle producing the case it
    // is named for fails here rather than making the assertions vacuous.
    assert_eq!(
        derived.source,
        Source::Auto,
        "precondition: the scattered bundle has a sustained crossing"
    );
    assert_eq!(
        fallback.source,
        Source::Default,
        "precondition: the flat bundle has none"
    );

    assert_ne!(
        derived.rationale.text, fallback.rationale.text,
        "one sentence for both cases is the lie about a measurement wizard:566 forbids"
    );
    assert_eq!(
        derived.rationale.key, fallback.rationale.key,
        "same decision-table row, so same key: the fallback is a second STRING, \
         not a second row"
    );
    assert_eq!(fallback.rationale.key, RationaleKey::TransitionHz);

    // No measurement claim.
    let text = fallback.rationale.text.to_lowercase();
    assert!(
        !text.contains("your room's problems are the same"),
        "the derived sentence rendered over the fallback: {:?}",
        fallback.rationale.text
    );
    assert!(
        !text.contains("we measured"),
        "the fallback string claims a measurement: {:?}",
        fallback.rationale.text
    );

    // …and it says, in plain words, that nothing could be measured and the
    // number is an assumption.
    assert!(
        text.contains("fallback"),
        "the UI leg of wizard:566 is this word: {:?}",
        fallback.rationale.text
    );
    assert!(
        text.contains("couldn't measure") || text.contains("could not measure"),
        "the fallback string must say the measurement could not be made: {:?}",
        fallback.rationale.text
    );
    assert!(
        fallback
            .rationale
            .text
            .contains(&format!("{TRANSITION_FALLBACK_HZ:.0} Hz")),
        "the fallback string must name the number it assumed: {:?}",
        fallback.rationale.text
    );
    assert!(
        !fallback.rationale.text.contains('{'),
        "unsubstituted placeholder in {:?}",
        fallback.rationale.text
    );
}

/// "We could not measure this" must be SHOWABLE, not merely stated: the σ curve
/// stays attached on the fallback path, so the drawer has something to put
/// behind the sentence, and `Source::Default` carries the same fact in a form
/// the desktop boundary's `log::info!` (B14) can branch on.
#[test]
fn the_transition_fallback_decision_carries_source_default_and_attaches_sigma() {
    let fallback = decide(&no_crossing_bundle()).decisions.transition_hz;

    assert_eq!(fallback.value, TRANSITION_FALLBACK_HZ);
    assert_eq!(fallback.source, Source::Default);
    assert!(
        fallback.evidence.iter().any(|e| matches!(
            e,
            Evidence::Curve {
                label: EvidenceLabel::Sigma,
                ..
            }
        )),
        "the σ curve is the evidence for WHY no transition could be measured"
    );
}

/// The idempotence invariant, restated on the one decision whose copy now has
/// two forms — because a selector that read `source` would break it here first.
///
/// A user who overrides `transition_hz` to 200.0 on an unmeasurable room gets
/// the SAME sentence: it is still true that nothing was measured, and the only
/// field that may move is `source`.
#[test]
fn a_user_override_to_the_fallback_value_changes_only_the_source() {
    let bundle = no_crossing_bundle();
    let auto = decide(&bundle);
    let pinned = decide(&with_override(
        &bundle,
        "transition_hz",
        serde_json::json!(TRANSITION_FALLBACK_HZ),
    ));

    assert_eq!(pinned.decisions.transition_hz.source, Source::UserOverride);
    assert_eq!(
        pinned.decisions.transition_hz.rationale.text, auto.decisions.transition_hz.rationale.text,
        "the user's own override to the fallback value renders the same text"
    );

    let mut expected = serde_json::to_value(&auto).expect("DecisionSet is plain derived data");
    expected["decisions"]["transition_hz"]["source"] = serde_json::json!("UserOverride");
    assert_eq!(
        serde_json::to_value(&pinned).expect("DecisionSet is plain derived data"),
        expected,
        "overriding the fallback to its own value moved something other than its source"
    );
}

/// The other direction, and the falsifier for selecting on the VALUE: a room
/// whose transition really was measured, overridden to exactly 200.0, is still
/// a measurement. The scan found a crossing, so the derived sentence is true
/// and must not flip to the fallback one just because the number now matches
/// the constant.
#[test]
fn a_measured_transition_overridden_to_200_still_reads_as_measured() {
    let bundle = measured_crossing_bundle();
    let auto = decide(&bundle);
    assert_eq!(
        auto.decisions.transition_hz.source,
        Source::Auto,
        "precondition: this bundle has a measured crossing"
    );

    let pinned = decide(&with_override(
        &bundle,
        "transition_hz",
        serde_json::json!(TRANSITION_FALLBACK_HZ),
    ));
    assert_eq!(pinned.decisions.transition_hz.value, TRANSITION_FALLBACK_HZ);
    assert_eq!(
        pinned.decisions.transition_hz.rationale.text, auto.decisions.transition_hz.rationale.text,
        "the copy follows the scan, not the number"
    );
    assert!(
        pinned
            .decisions
            .transition_hz
            .rationale
            .text
            .to_lowercase()
            .contains("your room's problems are the same"),
        "a measured transition still reads as measured: {:?}",
        pinned.decisions.transition_hz.rationale.text
    );
}
