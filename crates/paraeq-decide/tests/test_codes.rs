//! `DiagnosticCode`'s numbering contract (D-R / MS-20), and the doc-comment
//! mapping to `paraeq_measure::MeasurementDiagnostic` that is the only link
//! between the two vocabularies the crate DAG permits.
//!
//! These are grep tests over this crate's own source as well as value tests,
//! because half of D-R's ruling IS documentation — "plus a doc-comment mapping
//! between the two vocabularies" — and a doc comment nothing checks is a doc
//! comment the next reader deletes.

use paraeq_decide::DiagnosticCode;
use std::collections::BTreeSet;

/// The declaration's explicit discriminants and `code()` are two hand-written
/// lists of the same numbers. They must move together, so pin that they agree.
#[test]
fn every_diagnostic_code_matches_its_own_discriminant() {
    for code in DiagnosticCode::ALL {
        assert_eq!(
            code.code(),
            code as u16,
            "{code:?}: code() and the declared discriminant disagree"
        );
    }
}

/// Append-only means: numbers are unique, none is zero, and the space is
/// contiguous from 1 — so "the next free number" is unambiguous for whoever
/// adds the next variant.
///
/// Contiguity is NOT the same as declaration order, and deliberately so: the
/// variants are declared alphabetically (a reading aid) while the numbers are
/// assigned in order of addition (the contract). `OverrideOutOfDomain` and the
/// two `Verification*` codes B6 added sit alphabetically among their peers and
/// numerically at the end.
#[test]
fn diagnostic_code_numbers_are_unique_and_contiguous_from_one() {
    let numbers: Vec<u16> = DiagnosticCode::ALL.iter().map(|c| c.code()).collect();
    let unique: BTreeSet<u16> = numbers.iter().copied().collect();

    assert_eq!(
        unique.len(),
        numbers.len(),
        "a reused number is a breaking change to every recorded session"
    );
    assert_eq!(
        unique.into_iter().collect::<Vec<_>>(),
        (1..=numbers.len() as u16).collect::<Vec<_>>(),
        "the space runs 1..=N with no holes, so the next free number is obvious"
    );
}

/// `ALL` is a hand-maintained list, and a hand-maintained list can fall behind
/// the enum. Its LENGTH is what makes that impossible to do silently: adding a
/// variant without extending the array does not compile, and this pins the
/// count so the array cannot be padded either.
#[test]
fn the_diagnostic_code_vocabulary_is_twenty_seven_codes() {
    assert_eq!(DiagnosticCode::ALL.len(), 27);
}

/// The numbers `fixtures/decide/<case>/expected.json` and any session log will
/// key on. Spelled out rather than derived, because the point of the contract
/// is that these exact values never move.
#[test]
fn the_shipped_diagnostic_code_numbers_are_pinned() {
    assert_eq!(DiagnosticCode::AbsurdCurve.code(), 1);
    assert_eq!(DiagnosticCode::SelfExclusionUnavailable.code(), 19);
    assert_eq!(DiagnosticCode::VerificationResidual.code(), 21);
    assert_eq!(DiagnosticCode::TwoClock.code(), 22);
    assert_eq!(DiagnosticCode::WrongTransducer.code(), 24);
    // The three B6 added, at the end of the append-only space.
    assert_eq!(DiagnosticCode::OverrideOutOfDomain.code(), 25);
    assert_eq!(DiagnosticCode::VerificationPreampMismatch.code(), 26);
    assert_eq!(DiagnosticCode::VerificationRoutingMismatch.code(), 27);
}

/// A code round-trips by NAME, not by number — the enum is externally tagged,
/// so the number is the log/contract identity and the name is the wire
/// identity. Both are frozen; this pins that they are two different things and
/// that renaming a variant is also a wire break.
#[test]
fn a_diagnostic_code_serializes_by_name() {
    let json = serde_json::to_string(&DiagnosticCode::VerificationPreampMismatch).unwrap();
    assert_eq!(json, "\"VerificationPreampMismatch\"");
    assert_eq!(
        serde_json::from_str::<DiagnosticCode>(&json).unwrap(),
        DiagnosticCode::VerificationPreampMismatch
    );
}

fn outcome_source() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/outcome.rs"))
        .expect("the crate can read its own source")
}

/// One condition, seen from two crates, carrying the same name in both — and
/// the crate DAG forbids a code-level link between them, so the ONLY thing
/// stopping a future reader deleting one as a duplicate is that each doc
/// comment names the other and says which side of the capture it fires on.
///
/// Cheap, and it is the whole guard.
#[test]
fn the_two_preamp_mismatch_codes_are_documented_as_two_sides_of_one_condition() {
    let source = outcome_source();
    let doc_start = source
        .find("VerificationPreampMismatch = 26")
        .expect("the variant is declared with its number");
    let doc = &source[..doc_start];

    for phrase in [
        "MeasurementDiagnostic::VerificationPreampMismatch",
        "before the capture",
        "after the fact",
    ] {
        assert!(
            doc.contains(phrase),
            "VerificationPreampMismatch's doc must say {phrase:?}: it is the \
             only link between the two vocabularies that the crate DAG allows"
        );
    }
}

/// D-R asks for "a doc-comment mapping between the two vocabularies". The seven
/// overlaps are named in the enum's header and on the variants themselves; this
/// pins that each counterpart is actually spelled, so the mapping cannot rot
/// into a sentence that says a mapping exists without being one.
#[test]
fn the_diagnostic_code_header_maps_all_seven_overlapping_vocabularies() {
    let source = outcome_source();
    for counterpart in [
        "SelfExclusionUnavailable = 1",
        "SensitivityMissing = 2",
        "SensitivityUnparseable = 3",
        "SensitivityOutOfEnvelope = 6",
        "InputClipping = 12",
        "MicDisconnected = 20",
        "LowSnr = 100",
        "TwoClock = 102",
    ] {
        assert!(
            source.contains(counterpart),
            "the vocabulary mapping must name `MeasurementDiagnostic::{counterpart}`"
        );
    }
}

/// `VerificationRoutingMismatch` is one code with four documented triggers
/// rather than four serde-visible variants. That is only defensible while all
/// four are written down: an undocumented trigger reads, to the next
/// implementer, as a condition nothing refuses.
#[test]
fn the_routing_mismatch_code_documents_all_four_of_its_triggers() {
    let source = outcome_source();
    let doc_start = source
        .find("VerificationRoutingMismatch = 27")
        .expect("the variant is declared with its number");
    let doc = &source[..doc_start];
    let quadrant = &doc[doc
        .rfind("/// The verification capture cannot be differenced")
        .expect("the variant carries a doc comment")..];

    for trigger in [
        "positions[position_index].routing",
        "different widths",
        "installed.bands.channels()",
        "divergent per-channel band sets",
    ] {
        assert!(
            quadrant.contains(trigger),
            "trigger {trigger:?} must be named on the code that refuses it"
        );
    }
}
