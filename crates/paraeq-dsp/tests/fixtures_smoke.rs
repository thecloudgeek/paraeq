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

/// Every Tier-2 scipy-direct case is present and internally consistent. Their
/// DSP consumers land in stages 3-4; until then this is the only Rust-side
/// guard that the generator's output is well-formed, and it is the same one
/// Tier 1 gets above. The case list is spelled out so a silently dropped
/// gen_*() case fails here rather than surfacing as a missing-file panic in a
/// later stage.
#[test]
fn tier2_scipy_direct_fixtures_are_wellformed() {
    let mut cases: Vec<(String, String)> = [
        ("fr", "gaussian_sigma2"),
        ("fr", "gaussian_sigma32"),
        ("fr", "gaussian_sigma8"),
        ("fr", "rms_average"),
        ("logf", "resample_db"),
        ("room", "schroeder_decay"),
    ]
    .iter()
    .map(|(s, n)| (s.to_string(), n.to_string()))
    .collect();
    for kind in ["blackmanharris", "hann", "rect", "tukey"] {
        for n in [8, 9, 64, 4096] {
            cases.push(("window".to_string(), format!("{kind}_{n}")));
        }
    }

    for (stage, name) in &cases {
        let dir = fixtures_dir().join(stage);
        let text = std::fs::read_to_string(dir.join(format!("{name}.json")))
            .unwrap_or_else(|e| panic!("fixture {stage}/{name}.json: {e}"));
        let case: serde_json::Value = serde_json::from_str(&text).unwrap();
        let arrays = case["arrays"].as_object().unwrap();
        assert!(!arrays.is_empty(), "{stage}/{name}: no arrays");
        for (key, meta) in arrays {
            let bin = std::fs::read(dir.join(meta["file"].as_str().unwrap()))
                .unwrap_or_else(|e| panic!("{stage}/{name}.{key}: {e}"));
            let len = meta["len"].as_u64().unwrap() as usize;
            assert_eq!(bin.len(), len * 8, "{stage}/{name}.{key}: f64 byte count");
            let shape: Vec<usize> = meta["shape"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as usize)
                .collect();
            assert_eq!(
                shape.iter().product::<usize>(),
                len,
                "{stage}/{name}.{key}: shape {shape:?} vs len {len}"
            );
        }
        // The windows' declared length is the scipy call's own argument, so a
        // mismatch means the generator's loop and its params disagree.
        if stage == "window" {
            assert_eq!(
                case["params"]["n"].as_u64().unwrap() as usize,
                arrays["window"]["len"].as_u64().unwrap() as usize,
                "{stage}/{name}: param n vs window len"
            );
        }
    }
}
