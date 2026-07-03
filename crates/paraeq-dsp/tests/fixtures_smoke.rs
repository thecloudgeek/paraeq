//! Proves the committed golden fixtures are reachable and well-formed from
//! Rust tests (path convention: CARGO_MANIFEST_DIR/../../fixtures).

use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[test]
fn manifest_is_readable_and_f64le() {
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["dtype"], "<f8");
    assert!(manifest["numpy"].is_string());
}

#[test]
fn sweep_fixture_array_lengths_match_json() {
    let case: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("sweep/basic.json")).unwrap(),
    )
    .unwrap();
    let arr = &case["arrays"]["sweep"];
    let bin = std::fs::read(
        fixtures_dir()
            .join("sweep")
            .join(arr["file"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!(bin.len(), arr["len"].as_u64().unwrap() as usize * 8);
    // 0.25 s at 48 kHz
    assert_eq!(arr["len"].as_u64().unwrap(), 12000);
}
