mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::targets::{self, TargetCurve, ANCHOR_FREQS};
use std::path::PathBuf;

fn targets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../targets")
}

fn curve_from_case(c: &Case, fkey: &str, gkey: &str, name: &str) -> TargetCurve {
    TargetCurve {
        name: name.into(),
        frequencies: c.array(fkey),
        gains_db: c.array(gkey),
        category: None,
        description: None,
        source: None,
    }
}

#[test]
fn interpolation_matches_oracle_incl_clamp_edges() {
    let c = Case::load("targets", "harman_ie_interp");
    let curve = curve_from_case(&c, "curve_freqs", "curve_gains", "h");
    assert_allclose(
        &curve.interpolate(&c.array("dense")),
        &c.array("interp_dense"),
        1e-9,
        1e-9,
        "dense",
    );
    assert_allclose(
        &curve.interpolate(&c.array("edges")),
        &c.array("interp_edges"),
        1e-9,
        1e-9,
        "edges",
    );
}

#[test]
fn anchor_deviation_matches_oracle() {
    let c = Case::load("targets", "anchor_deviation");
    let base = targets::load_target_csv(&targets_dir().join("harman_ie_2019.csv")).unwrap();
    let out = targets::build_anchor_target(
        &base,
        &c.array("anchor_freqs"),
        &c.array("offsets_db"),
        "Custom",
    );
    assert_allclose(
        &out.frequencies,
        &c.array("result_freqs"),
        1e-12,
        1e-12,
        "grid",
    );
    assert_allclose(&out.gains_db, &c.array("result_gains"), 1e-9, 1e-9, "gains");
}

#[test]
fn match_closest_matches_oracle() {
    let c = Case::load("targets", "match_closest");
    let all = targets::list_targets(&targets_dir()).unwrap();
    assert_eq!(all.len(), 6, "builtin count");
    let best =
        targets::match_closest_target(&c.array("measured_freqs"), &c.array("measured_db"), &all);
    assert_eq!(best.name, c.scalar("expected_name").as_str().unwrap());
}

#[test]
fn csv_metadata_parsing_matches_prototype_fixtures() {
    let proto = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../prototype/tests/fixtures");
    let with_meta =
        targets::load_target_csv(&proto.join("target_curve_with_metadata.csv")).unwrap();
    assert!(with_meta.category.is_some(), "metadata parsed");
    let no_meta = targets::load_target_csv(&proto.join("target_curve_no_metadata.csv")).unwrap();
    assert_eq!(
        no_meta.name, "target_curve_no_metadata",
        "name falls back to file stem"
    );
    assert!(no_meta.category.is_none());
    // mixed/unknown metadata keys must not error:
    targets::load_target_csv(&proto.join("target_curve_unknown_and_mixed_case.csv")).unwrap();
}

#[test]
fn anchor_freqs_constant_matches_oracle() {
    assert_eq!(ANCHOR_FREQS.len(), 16);
    assert_eq!(ANCHOR_FREQS[0], 20.0);
    assert_eq!(ANCHOR_FREQS[15], 20000.0);
}
