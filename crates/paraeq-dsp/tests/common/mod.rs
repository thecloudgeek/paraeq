//! Shared fixture loader for golden-parity tests. Fixture format: see
//! prototype/tools/generate_fixtures.py (the oracle generator).
#![allow(dead_code)]

use std::path::PathBuf;

pub struct Array2 {
    pub data: Vec<f64>,
    pub rows: usize,
    pub cols: usize,
}

impl Array2 {
    /// De-interleave one column from C-order (row-major) data.
    pub fn col(&self, c: usize) -> Vec<f64> {
        assert!(c < self.cols, "col {c} out of range ({})", self.cols);
        (0..self.rows)
            .map(|r| self.data[r * self.cols + c])
            .collect()
    }
}

pub struct Case {
    dir: PathBuf,
    json: serde_json::Value,
}

impl Case {
    pub fn load(stage: &str, name: &str) -> Case {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(stage);
        let text = std::fs::read_to_string(dir.join(format!("{name}.json")))
            .unwrap_or_else(|e| panic!("fixture {stage}/{name}.json: {e}"));
        Case {
            dir,
            json: serde_json::from_str(&text).unwrap(),
        }
    }

    fn array_meta(&self, key: &str) -> (&serde_json::Value, Vec<u8>) {
        let meta = &self.json["arrays"][key];
        assert!(!meta.is_null(), "no array '{key}' in fixture");
        let bytes = std::fs::read(self.dir.join(meta["file"].as_str().unwrap())).unwrap();
        (meta, bytes)
    }

    pub fn array(&self, key: &str) -> Vec<f64> {
        let (meta, bytes) = self.array_meta(key);
        let len = meta["len"].as_u64().unwrap() as usize;
        assert_eq!(bytes.len(), len * 8, "byte length mismatch for '{key}'");
        bytes
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    pub fn array2(&self, key: &str) -> Array2 {
        let (meta, _) = self.array_meta(key);
        let shape: Vec<usize> = meta["shape"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as usize)
            .collect();
        assert_eq!(shape.len(), 2, "'{key}' is not 2-D");
        Array2 {
            data: self.array(key),
            rows: shape[0],
            cols: shape[1],
        }
    }

    /// Path to a non-array file shipped alongside a case — e.g. a cal file
    /// the parser under test must read verbatim, byte for byte.
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn param_f64(&self, key: &str) -> f64 {
        self.json["params"][key]
            .as_f64()
            .unwrap_or_else(|| panic!("param {key}"))
    }
    pub fn param_u64(&self, key: &str) -> u64 {
        self.json["params"][key]
            .as_u64()
            .unwrap_or_else(|| panic!("param {key}"))
    }
    pub fn param_str(&self, key: &str) -> &str {
        self.json["params"][key]
            .as_str()
            .unwrap_or_else(|| panic!("param {key}"))
    }
    pub fn scalar(&self, key: &str) -> &serde_json::Value {
        &self.json["scalars"][key]
    }
}

/// numpy-style allclose with a max-abs report on failure.
pub fn assert_allclose(actual: &[f64], expected: &[f64], rtol: f64, atol: f64, ctx: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{ctx}: length {} vs {}",
        actual.len(),
        expected.len()
    );
    let mut worst = 0.0f64;
    let mut worst_i = 0usize;
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a.is_finite(),
            "{ctx}: non-finite actual at [{i}]: {a} (expected {e})"
        );
        let tol = atol + rtol * e.abs();
        let d = (a - e).abs();
        if d > tol && d - tol > worst {
            worst = d - tol;
            worst_i = i;
        }
    }
    assert!(
        worst == 0.0,
        "{ctx}: worst mismatch at [{worst_i}]: actual={} expected={} (over tolerance by {worst:e})",
        actual[worst_i],
        expected[worst_i]
    );
}
