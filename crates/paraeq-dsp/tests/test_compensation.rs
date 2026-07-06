mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::compensation;
use std::path::PathBuf;

fn proto_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../prototype/tests/fixtures")
        .join(name)
}

#[test]
fn apply_matches_oracle_with_edge_hold() {
    let c = Case::load("compensation", "edge_hold");
    let out = compensation::apply_compensation(
        &c.array("mag_db"),
        &c.array("grid"),
        &c.array("comp_freqs"),
        &c.array("comp_gains"),
    );
    assert_allclose(
        &out,
        &c.array("compensated"),
        1e-12,
        1e-12,
        "apply_compensation",
    );
}

// Oracle pin (.venv/bin/python, prototype.paraeq.measurement.compensation.load_compensation):
// test_compensation.csv          7 20.0 20000.0 0.5 -2.0
// test_compensation_minidsp.txt  7 20.0 20000.0 0.5 -2.0
#[test]
fn parses_paraeq_csv_format() {
    let (f, g) = compensation::load_compensation(&proto_fixture("test_compensation.csv")).unwrap();
    assert_eq!(f.len(), 7);
    assert_eq!(g.len(), 7);
    assert_eq!(f[0], 20.0);
    assert_eq!(f[f.len() - 1], 20000.0);
    assert_eq!(g[0], 0.5);
    assert_eq!(g[g.len() - 1], -2.0);
    assert!(f.windows(2).all(|w| w[0] < w[1]), "freqs sorted");
}

#[test]
fn parses_minidsp_format() {
    let (f, g) =
        compensation::load_compensation(&proto_fixture("test_compensation_minidsp.txt")).unwrap();
    assert_eq!(f.len(), 7);
    assert_eq!(g.len(), 7);
    assert_eq!(f[0], 20.0);
    assert_eq!(f[f.len() - 1], 20000.0);
    assert_eq!(g[0], 0.5);
    assert_eq!(g[g.len() - 1], -2.0);
}
