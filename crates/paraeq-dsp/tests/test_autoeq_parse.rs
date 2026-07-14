//! AutoEQ text parser parity against the oracle fixture.
//! Oracle: prototype/paraeq/correction/autoeq_db.py::parse_parametric_eq.
//!
//! The fixture `fixtures/autoeq_parser.json` is a flat top-level list of cases
//! (pure text, no `.f64` arrays), a deliberate exception to the per-stage-dir
//! `save_case()` layout — see the comment beside gen_autoeq_parser() in the
//! generator. It is therefore loaded directly here, not via the `Case` helper.

use paraeq_dsp::peq::{parse_autoeq, EQBand, FilterType, ParametricEQ, ParsedPreset};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    input: String,
    expected: ParsedPreset,
}

fn load_cases() -> Vec<Case> {
    // Same relative-path convention as the neighboring fixture tests
    // (tests/common/mod.rs): CARGO_MANIFEST_DIR + ../../fixtures.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join("autoeq_parser.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn parse_matches_oracle_fixtures() {
    for case in load_cases() {
        let got = parse_autoeq(&case.input);
        // Floats are parsed from identical decimal strings on both sides, so
        // exact equality (via ParsedPreset/EQBand PartialEq) holds.
        assert_eq!(
            got, case.expected,
            "case '{}': parsed {:?} != expected {:?}",
            case.name, got, case.expected
        );
    }
}

#[test]
fn export_then_parse_roundtrip() {
    // Rust's own export -> parse. Rounding in export_autoeq_format:
    // Fc {:.0} (±0.5), Gain {:.1} (±0.05), Q {:.3} (±0.0005).
    let bands = vec![
        EQBand {
            filter_type: FilterType::Peaking,
            fc: 105.3,
            gain_db: 5.04,
            q: 1.201,
        },
        EQBand {
            filter_type: FilterType::HighShelf,
            fc: 9000.0,
            gain_db: -3.5,
            q: 0.707,
        },
    ];
    let peq = ParametricEQ {
        bands: bands.clone(),
        sample_rate: 48000.0,
    };
    let text = peq.export_autoeq_format(-3.0);
    let parsed = parse_autoeq(&text);

    assert!((parsed.preamp_db - (-3.0)).abs() < 1e-12);
    assert_eq!(parsed.bands.len(), bands.len());
    for (got, want) in parsed.bands.iter().zip(&bands) {
        assert_eq!(got.filter_type, want.filter_type);
        assert!(
            (got.fc - want.fc).abs() <= 0.5,
            "fc {} vs {}",
            got.fc,
            want.fc
        );
        assert!(
            (got.gain_db - want.gain_db).abs() <= 0.05,
            "gain {} vs {}",
            got.gain_db,
            want.gain_db
        );
        assert!(
            (got.q - want.q).abs() <= 0.0005,
            "q {} vs {}",
            got.q,
            want.q
        );
    }
}
