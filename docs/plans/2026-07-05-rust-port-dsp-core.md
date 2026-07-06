# DSP Core Port (Stage 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port every pure-DSP module from the Python oracle into `paraeq-dsp` (and the two block processors into `paraeq-engine`) via fixture-TDD, numerically matching the committed goldens in `fixtures/`.

**Architecture:** Per `docs/specs/2026-07-02-rust-port-design.md` (port map + tolerances). Every module follows the same cycle: write a test that loads the committed fixture and asserts the Rust output matches the oracle's within the spec tolerance → watch it fail → port the algorithm from the named Python reference → green → commit. The Python sources under `prototype/paraeq/` are the authoritative reference for every formula; the fixtures are the arbiter. **Scope note (deliberate deviation from the spec's stage list):** the spec placed `engine::convolver`/`engine::iir` in stage 3; they are pure fixture-backed DSP, so this plan includes them (Tasks 11–12) to leave stage 3 purely CoreAudio/realtime work.

**Tech Stack:** Rust stable, `rustfft`/`realfft` (already workspace deps), `approx` + `proptest` + `serde_json` (already dev-deps), `biquad` crate (new dev-dep, cross-check only).

## Global Constraints

- All DSP math in **f64** (numpy default). No `ndarray`, no `scirs2*`, no `fundsp` (spec ban).
- `paraeq-dsp` stays zero-platform-deps: no CoreAudio, no Tauri, no filesystem access in core algorithms (path-taking helpers thin-wrap string parsers).
- Tolerances (spec): closed-form coefficient math ≤ 1e-12; single-FFT paths ~1e-9 relative; minimum-phase chain ~1e-6 absolute on taps AND magnitude response within 0.01 dB over 20 Hz–20 kHz.
- numpy-style comparison helper everywhere: `max |a-b| ≤ atol + rtol·max|b|`.
- **rustfft/realfft do not normalize inverse transforms** — every inverse gets an explicit `1/n` scale. numpy `irfft`/`ifft` divide by n.
- Fixture format (committed, do not modify): `fixtures/<stage>/<case>.json` with `{"params","scalars","arrays"}`; arrays are `<case>.<key>.f64` raw little-endian f64, C-order; shapes like `[4096, 2]` are sample-major (row = frame, column = channel).
- Alphabetical ordering for imports/module lists/dep lists where order doesn't matter.
- Every commit message ends with the two trailers:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and the `Claude-Session:` link line the harness supplies.
- Gates before every commit: the task's tests green, `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --all` applied.
- Execution on a feature branch/worktree (superpowers:using-git-worktrees). All paths relative to the worktree root.

## File Structure (end state)

```
crates/paraeq-dsp/src/
├── lib.rs            # pub mod list + DspError
├── autofit.rs        ├── biquad.rs         ├── compensation.rs
├── deconvolution.rs  ├── fir.rs            ├── fr.rs
├── peq.rs            ├── spline.rs         ├── sweep.rs
└── targets.rs
crates/paraeq-dsp/tests/
├── common/mod.rs     # fixture loader + assert helpers (shared)
├── fixtures_smoke.rs # (exists)
├── test_autofit.rs  test_biquad.rs  test_compensation.rs  test_deconvolution.rs
├── test_fir.rs      test_fr.rs      test_peq.rs           test_props.rs
├── test_spline.rs   test_sweep.rs   test_targets.rs
crates/paraeq-engine/src/
├── lib.rs            # + pub mod convolver; pub mod iir;
├── convolver.rs      └── iir.rs
crates/paraeq-engine/tests/
├── common/mod.rs     # copy of the dsp loader (test-only duplication, acceptable)
├── test_convolver.rs └── test_iir.rs
crates/paraeq-dsp/DIVERGENCES.md   # deliberate-divergence log (spec requirement)
```

Python references (read them; they are the spec for each formula): `prototype/paraeq/measurement/{sweep,deconvolution,compensation,frequency_response}.py`, `prototype/paraeq/correction/{biquad,fir_filter,target_curves,parametric_eq,auto_fit}.py`, `prototype/paraeq/engine/{convolver,iir_processor}.py`.

---

### Task 1: Fixture test harness + sweep module

**Files:**
- Create: `crates/paraeq-dsp/tests/common/mod.rs`
- Create: `crates/paraeq-dsp/tests/test_sweep.rs`
- Create: `crates/paraeq-dsp/src/sweep.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs`

**Interfaces:**
- Consumes: committed `fixtures/` tree (format in Global Constraints).
- Produces (every later task consumes these exact items):
  - `common::Case::load(stage: &str, name: &str) -> Case`
  - `Case::array(&self, key: &str) -> Vec<f64>` (1-D), `Case::array2(&self, key: &str) -> Array2` where `Array2 { data: Vec<f64>, rows: usize, cols: usize }` with `col(&self, c: usize) -> Vec<f64>` (C-order de-interleave)
  - `Case::param_f64/param_u64/param_str`, `Case::scalar(&self, key) -> &serde_json::Value`
  - `common::assert_allclose(actual: &[f64], expected: &[f64], rtol: f64, atol: f64, ctx: &str)`
  - `paraeq_dsp::sweep::{generate_sweep, generate_inverse_sweep}`

- [ ] **Step 1: Write the harness**

`crates/paraeq-dsp/tests/common/mod.rs`:

```rust
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
        (0..self.rows).map(|r| self.data[r * self.cols + c]).collect()
    }
}

pub struct Case {
    dir: PathBuf,
    json: serde_json::Value,
}

impl Case {
    pub fn load(stage: &str, name: &str) -> Case {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(stage);
        let text = std::fs::read_to_string(dir.join(format!("{name}.json")))
            .unwrap_or_else(|e| panic!("fixture {stage}/{name}.json: {e}"));
        Case { dir, json: serde_json::from_str(&text).unwrap() }
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
        bytes.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect()
    }

    pub fn array2(&self, key: &str) -> Array2 {
        let (meta, _) = self.array_meta(key);
        let shape: Vec<usize> =
            meta["shape"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
        assert_eq!(shape.len(), 2, "'{key}' is not 2-D");
        Array2 { data: self.array(key), rows: shape[0], cols: shape[1] }
    }

    pub fn param_f64(&self, key: &str) -> f64 {
        self.json["params"][key].as_f64().unwrap_or_else(|| panic!("param {key}"))
    }
    pub fn param_u64(&self, key: &str) -> u64 {
        self.json["params"][key].as_u64().unwrap_or_else(|| panic!("param {key}"))
    }
    pub fn param_str(&self, key: &str) -> &str {
        self.json["params"][key].as_str().unwrap_or_else(|| panic!("param {key}"))
    }
    pub fn scalar(&self, key: &str) -> &serde_json::Value {
        &self.json["scalars"][key]
    }
}

/// numpy-style allclose with a max-abs report on failure.
pub fn assert_allclose(actual: &[f64], expected: &[f64], rtol: f64, atol: f64, ctx: &str) {
    assert_eq!(actual.len(), expected.len(), "{ctx}: length {} vs {}", actual.len(), expected.len());
    let mut worst = 0.0f64;
    let mut worst_i = 0usize;
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
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
        actual[worst_i], expected[worst_i]
    );
}
```

- [ ] **Step 2: Write the failing sweep test**

`crates/paraeq-dsp/tests/test_sweep.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::sweep::{generate_inverse_sweep, generate_sweep};

#[test]
fn sweep_matches_oracle() {
    let c = Case::load("sweep", "basic");
    let sweep = generate_sweep(
        c.param_f64("duration"),
        c.param_u64("sample_rate") as u32,
        c.param_f64("f_start"),
        c.param_f64("f_end"),
    );
    assert_allclose(&sweep, &c.array("sweep"), 1e-12, 1e-12, "sweep");
}

#[test]
fn inverse_sweep_matches_oracle() {
    let c = Case::load("sweep", "basic");
    let sweep = c.array("sweep");
    let inv = generate_inverse_sweep(
        &sweep,
        c.param_u64("sample_rate") as u32,
        c.param_f64("f_start"),
        c.param_f64("f_end"),
    );
    assert_allclose(&inv, &c.array("inverse"), 1e-12, 1e-12, "inverse sweep");
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p paraeq-dsp --test test_sweep`
Expected: FAIL — `could not find 'sweep' in 'paraeq_dsp'`.

- [ ] **Step 4: Implement `sweep.rs`**

Python reference: `prototype/paraeq/measurement/sweep.py` (formulas quoted below are exact).

```rust
//! Farina logarithmic sine sweep + inverse filter.
//! Oracle: prototype/paraeq/measurement/sweep.py

/// Log sine sweep. n_samples = trunc(duration * sample_rate);
/// phase = 2π·f_start·duration/ln(rate) · (rate^(t/duration) − 1), rate = f_end/f_start.
pub fn generate_sweep(duration: f64, sample_rate: u32, f_start: f64, f_end: f64) -> Vec<f64> {
    let n = (duration * sample_rate as f64) as usize;
    let rate = f_end / f_start;
    let k = 2.0 * std::f64::consts::PI * f_start * duration / rate.ln();
    (0..n)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            (k * (rate.powf(t / duration) - 1.0)).sin()
        })
        .collect()
}

/// Time-reversed sweep with 6 dB/oct decay envelope and scalar normalization:
/// envelope[i] = rate^(−t_rev[i]/duration), t_rev = reversed arange(n)/sr;
/// inverse /= Σ(sweep · reversed(sweep) · envelope) / n.
pub fn generate_inverse_sweep(sweep: &[f64], sample_rate: u32, f_start: f64, f_end: f64) -> Vec<f64> {
    let n = sweep.len();
    let duration = n as f64 / sample_rate as f64;
    let rate = f_end / f_start;
    let mut inverse: Vec<f64> = sweep.iter().rev().copied().collect();
    let envelope: Vec<f64> = (0..n)
        .map(|i| {
            let t = (n - 1 - i) as f64 / sample_rate as f64;
            rate.powf(-t / duration)
        })
        .collect();
    let norm: f64 = sweep
        .iter()
        .zip(sweep.iter().rev())
        .zip(&envelope)
        .map(|((s, srev), e)| s * srev * e)
        .sum::<f64>()
        / n as f64;
    for (v, e) in inverse.iter_mut().zip(&envelope) {
        *v *= e / norm;
    }
    inverse
}
```

**⚠ One subtlety to check against the oracle:** in the Python, `inverse = sweep[::-1] * envelope` happens via two statements (`inverse = sweep[::-1].copy()` … envelope applied where?). Read `prototype/paraeq/measurement/sweep.py:23-42` before finalizing: if the envelope multiplies BEFORE the normalization sum is computed the code above is right; if the Python normalizes the un-enveloped reversal, move the `*e` accordingly. The fixture decides — if `inverse_sweep_matches_oracle` fails with a constant scale factor or shape mismatch, this ordering is the cause.

Add to `crates/paraeq-dsp/src/lib.rs` (keep the existing VERSION const):

```rust
pub mod sweep;

/// Error type shared by parsing/validation entry points across modules.
#[derive(Debug, thiserror::Error)]
pub enum DspError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p paraeq-dsp --test test_sweep`
Expected: 2 passed.

- [ ] **Step 6: Gates + commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): fixture harness + sweep module, golden-matched to oracle"
```

---

### Task 2: biquad module (RBJ cookbook + cascade response)

**Files:**
- Create: `crates/paraeq-dsp/src/biquad.rs`, `crates/paraeq-dsp/tests/test_biquad.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (add `pub mod biquad;`), `crates/paraeq-dsp/Cargo.toml` (dev-dep `biquad = "0.6"` for the cross-check test only)

**Interfaces:**
- Produces:
  - `pub fn peaking(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6]`
  - `pub fn low_shelf(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6]`
  - `pub fn high_shelf(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6]`
  - `pub fn notch(fc: f64, q: f64, sample_rate: f64) -> [f64; 6]`
  - `pub fn sos_frequency_response_db(sos: &[[f64; 6]], freqs: &[f64], sample_rate: f64) -> Vec<f64>`
  - SOS row convention (fixture + scipy): `[b0, b1, b2, 1.0, a1, a2]`, all normalized by a0.
- Consumed by: Tasks 8 (peq), 9 (autofit), 12 (iir tests).

- [ ] **Step 1: Failing fixture test**

`crates/paraeq-dsp/tests/test_biquad.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::biquad;

#[test]
fn coefficient_matrix_matches_oracle() {
    let c = Case::load("biquad", "matrix");
    let sr = c.param_f64("sample_rate");
    let cases = c.scalar("cases").as_array().expect("cases array");
    assert!(!cases.is_empty());
    for case in cases {
        let kind = case["kind"].as_str().unwrap();
        let fc = case["fc"].as_f64().unwrap();
        let gain = case["gain_db"].as_f64().unwrap();
        let q = case["q"].as_f64().unwrap();
        let expected: Vec<f64> =
            case["sos"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let sos = match kind {
            "peaking" => biquad::peaking(fc, gain, q, sr),
            "low_shelf" => biquad::low_shelf(fc, gain, q, sr),
            "high_shelf" => biquad::high_shelf(fc, gain, q, sr),
            "notch" => biquad::notch(fc, q, sr),
            other => panic!("unknown kind {other}"),
        };
        assert_allclose(&sos, &expected, 0.0, 1e-12, &format!("{kind} fc={fc} q={q}"));
    }
}

#[test]
fn cascade_response_matches_oracle() {
    let c = Case::load("biquad", "matrix");
    let sr = c.param_f64("sample_rate");
    let freqs = c.array("resp_freqs");
    let sos = biquad::peaking(1000.0, 6.0, 1.0, sr); // the case the generator used
    let resp = biquad::sos_frequency_response_db(&[sos], &freqs, sr);
    assert_allclose(&resp, &c.array("resp_db"), 1e-9, 1e-12, "sosfreqz response");
}

/// Independent cross-check against the `biquad` crate (spec: test-only).
/// NOTE: the dev-dep crate and our module share the name `biquad` — use the
/// leading-`::` extern-crate path for the dev-dep to disambiguate.
#[test]
fn peaking_agrees_with_biquad_crate() {
    let coeffs = ::biquad::Coefficients::<f64>::from_params(
        ::biquad::Type::PeakingEQ(6.0),
        ::biquad::Hertz::<f64>::from_hz(48000.0).unwrap(),
        ::biquad::Hertz::<f64>::from_hz(1000.0).unwrap(),
        1.0,
    )
    .unwrap();
    let ours = paraeq_dsp::biquad::peaking(1000.0, 6.0, 1.0, 48000.0);
    for (a, b) in [coeffs.b0, coeffs.b1, coeffs.b2, 1.0, coeffs.a1, coeffs.a2]
        .iter()
        .zip(&ours)
    {
        assert!((a - b).abs() < 1e-12, "crate {a} vs ours {b}");
    }
}
```

(If the `biquad` crate's API surface differs from the above — e.g. field or constructor names — adapt the cross-check test to the crate's real API; its job is only to compare six coefficient values. Do NOT adapt our production code to the crate.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paraeq-dsp --test test_biquad`
Expected: FAIL — module `biquad` not found.

- [ ] **Step 3: Implement `biquad.rs`**

Python reference: `prototype/paraeq/correction/biquad.py` (RBJ Audio EQ Cookbook; `a_lin = 10^(gain/40)`, `w0 = 2π·fc/sr`, `alpha = sin(w0)/(2q)`; shelves use `2·√a_lin·alpha`).

```rust
//! RBJ Audio EQ Cookbook biquad designers + cascade response.
//! Oracle: prototype/paraeq/correction/biquad.py. SOS rows: [b0,b1,b2,1,a1,a2]/a0.

use std::f64::consts::PI;

fn wa(fc: f64, q: f64, sample_rate: f64) -> (f64, f64) {
    let w0 = 2.0 * PI * fc / sample_rate;
    (w0, w0.sin() / (2.0 * q))
}

fn row(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> [f64; 6] {
    [b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]
}

pub fn peaking(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let a = 10f64.powf(gain_db / 40.0);
    let (w0, alpha) = wa(fc, q, sample_rate);
    row(
        1.0 + alpha * a, -2.0 * w0.cos(), 1.0 - alpha * a,
        1.0 + alpha / a, -2.0 * w0.cos(), 1.0 - alpha / a,
    )
}

pub fn low_shelf(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let a = 10f64.powf(gain_db / 40.0);
    let (w0, alpha) = wa(fc, q, sample_rate);
    let (c, s2a) = (w0.cos(), 2.0 * a.sqrt() * alpha);
    row(
        a * ((a + 1.0) - (a - 1.0) * c + s2a),
        2.0 * a * ((a - 1.0) - (a + 1.0) * c),
        a * ((a + 1.0) - (a - 1.0) * c - s2a),
        (a + 1.0) + (a - 1.0) * c + s2a,
        -2.0 * ((a - 1.0) + (a + 1.0) * c),
        (a + 1.0) + (a - 1.0) * c - s2a,
    )
}

pub fn high_shelf(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let a = 10f64.powf(gain_db / 40.0);
    let (w0, alpha) = wa(fc, q, sample_rate);
    let (c, s2a) = (w0.cos(), 2.0 * a.sqrt() * alpha);
    row(
        a * ((a + 1.0) + (a - 1.0) * c + s2a),
        -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
        a * ((a + 1.0) + (a - 1.0) * c - s2a),
        (a + 1.0) - (a - 1.0) * c + s2a,
        2.0 * ((a - 1.0) - (a + 1.0) * c),
        (a + 1.0) - (a - 1.0) * c - s2a,
    )
}

pub fn notch(fc: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let (w0, alpha) = wa(fc, q, sample_rate);
    row(
        1.0, -2.0 * w0.cos(), 1.0,
        1.0 + alpha, -2.0 * w0.cos(), 1.0 - alpha,
    )
}

/// scipy.signal.sosfreqz equivalent at worN = freqs·2π/sr, returned as
/// 20·log10(|H| + 1e-10)  — NOTE the +1e-10 is ADDED (biquad.py convention),
/// not a max() floor (frequency_response.py uses max(); do not mix them up).
pub fn sos_frequency_response_db(sos: &[[f64; 6]], freqs: &[f64], sample_rate: f64) -> Vec<f64> {
    freqs
        .iter()
        .map(|f| {
            let w = f * 2.0 * PI / sample_rate;
            let (c1, s1) = (w.cos(), w.sin());
            let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
            let mut mag = 1.0f64;
            for r in sos {
                let nr = r[0] + r[1] * c1 + r[2] * c2;
                let ni = -(r[1] * s1 + r[2] * s2);
                let dr = r[3] + r[4] * c1 + r[5] * c2;
                let di = -(r[4] * s1 + r[5] * s2);
                mag *= ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt();
            }
            20.0 * (mag + 1e-10).log10()
        })
        .collect()
}
```

`Cargo.toml` addition under `[dev-dependencies]`: `biquad = "0.6"`.
`lib.rs`: add `pub mod biquad;` (alphabetical: before `sweep`).

**Verify against the Python before trusting my shelf formulas**: open `prototype/paraeq/correction/biquad.py` and compare `biquad_low_shelf`/`biquad_high_shelf` term-for-term (signs on the `±2·((a−1) ± (a+1)·cos)` middle terms are the classic transcription trap). The matrix fixture covers 2 shelf cases each, so an error cannot survive to commit.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p paraeq-dsp --test test_biquad`
Expected: 3 passed.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): RBJ biquad designers + cascade response, golden-matched (+crate cross-check)"
```

---

### Task 3: fr module (FFT response, octave smoothing, averaging, normalization)

**Files:**
- Create: `crates/paraeq-dsp/src/fr.rs`, `crates/paraeq-dsp/tests/test_fr.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod fr;`)

**Interfaces:**
- Produces:
  - `pub fn compute_frequency_response(ir: &[f64], sample_rate: u32, n_fft: Option<usize>) -> (Vec<f64>, Vec<f64>)` — (freqs, mag_db); mono only, callers loop channels
  - `pub fn fractional_octave_smooth(magnitude_db: &[f64], freqs: &[f64], fraction: u32) -> Vec<f64>`
  - `pub fn average_measurements(measurements_db: &[Vec<f64>]) -> Vec<f64>`
  - `pub fn normalize_to_reference_band(freqs: &[f64], magnitude_db: &[f64], low_hz: f64, high_hz: f64) -> Result<Vec<f64>, crate::DspError>`

- [ ] **Step 1: Failing tests**

`crates/paraeq-dsp/tests/test_fr.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::fr;

#[test]
fn stereo_response_matches_oracle_per_channel() {
    let c = Case::load("fr", "stereo_decay");
    let ir = c.array2("ir");
    let mag = c.array2("mag_db");
    let n_fft = c.param_u64("n_fft") as usize;
    let sr = c.param_u64("sample_rate") as u32;
    let (freqs, mag0) = fr::compute_frequency_response(&ir.col(0), sr, Some(n_fft));
    let (_, mag1) = fr::compute_frequency_response(&ir.col(1), sr, Some(n_fft));
    assert_allclose(&freqs, &c.array("freqs"), 1e-12, 1e-9, "rfftfreq");
    assert_allclose(&mag0, &mag.col(0), 1e-9, 1e-9, "mag ch0");
    assert_allclose(&mag1, &mag.col(1), 1e-9, 1e-9, "mag ch1");
}

#[test]
fn smoothing_matches_oracle_all_fractions() {
    for (case, fraction) in [("smooth_1_3", 3u32), ("smooth_1_6", 6), ("smooth_1_12", 12)] {
        let c = Case::load("fr", case);
        let out = fr::fractional_octave_smooth(&c.array("mag_db"), &c.array("freqs"), fraction);
        assert_allclose(&out, &c.array("smoothed"), 1e-9, 1e-9, case);
    }
}

#[test]
fn averaging_is_in_db_domain() {
    let c = Case::load("fr", "average");
    let out = fr::average_measurements(&[c.array("a"), c.array("b")]);
    assert_allclose(&out, &c.array("avg"), 1e-12, 1e-12, "average");
}

#[test]
fn normalization_matches_oracle() {
    let c = Case::load("fr", "normalize");
    let out = fr::normalize_to_reference_band(
        &c.array("freqs"),
        &c.array("mag_db"),
        c.param_f64("low_hz"),
        c.param_f64("high_hz"),
    )
    .unwrap();
    assert_allclose(&out, &c.array("normalized"), 1e-12, 1e-12, "normalize");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paraeq-dsp --test test_fr` — FAIL, module not found.

- [ ] **Step 3: Implement `fr.rs`**

Python reference: `prototype/paraeq/measurement/frequency_response.py`. Load-bearing details: default `n_fft = ir.len()` (exact length, NOT power-of-2); magnitude floor is `max(|X|, 1e-10)` (a floor, not an add); smoothing operates on linear amplitude `10^(dB/20)` with per-bin inclusive window `[f/ratio, f·ratio]`, `ratio = 2^(1/(2·fraction))`, DC bins (`f <= 0`) keep their raw value; averaging is a plain arithmetic mean of dB arrays.

```rust
//! Frequency-response computation and shaping.
//! Oracle: prototype/paraeq/measurement/frequency_response.py

use realfft::RealFftPlanner;

pub fn compute_frequency_response(
    ir: &[f64],
    sample_rate: u32,
    n_fft: Option<usize>,
) -> (Vec<f64>, Vec<f64>) {
    let n = n_fft.unwrap_or(ir.len());
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = vec![0.0; n];
    let m = ir.len().min(n);
    input[..m].copy_from_slice(&ir[..m]);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    let freqs: Vec<f64> = (0..spectrum.len())
        .map(|i| i as f64 * sample_rate as f64 / n as f64)
        .collect();
    let mag_db: Vec<f64> =
        spectrum.iter().map(|c| 20.0 * c.norm().max(1e-10).log10()).collect();
    (freqs, mag_db)
}

pub fn fractional_octave_smooth(magnitude_db: &[f64], freqs: &[f64], fraction: u32) -> Vec<f64> {
    let ratio = 2f64.powf(1.0 / (2.0 * fraction as f64));
    let linear: Vec<f64> = magnitude_db.iter().map(|db| 10f64.powf(db / 20.0)).collect();
    let mut out = Vec::with_capacity(linear.len());
    for (i, &f) in freqs.iter().enumerate() {
        if f <= 0.0 {
            out.push(linear[i]);
            continue;
        }
        let (lo, hi) = (f / ratio, f * ratio);
        // freqs is monotonically increasing (rfftfreq): contiguous window.
        let mut sum = 0.0;
        let mut count = 0usize;
        for (j, &fj) in freqs.iter().enumerate() {
            if fj >= lo && fj <= hi {
                sum += linear[j];
                count += 1;
            }
        }
        out.push(sum / count as f64);
    }
    out.iter().map(|v| 20.0 * v.max(1e-10).log10()).collect()
}

pub fn average_measurements(measurements_db: &[Vec<f64>]) -> Vec<f64> {
    let n = measurements_db[0].len();
    let mut out = vec![0.0; n];
    for m in measurements_db {
        for (o, v) in out.iter_mut().zip(m) {
            *o += v;
        }
    }
    for o in &mut out {
        *o /= measurements_db.len() as f64;
    }
    out
}

pub fn normalize_to_reference_band(
    freqs: &[f64],
    magnitude_db: &[f64],
    low_hz: f64,
    high_hz: f64,
) -> Result<Vec<f64>, crate::DspError> {
    let band: Vec<f64> = freqs
        .iter()
        .zip(magnitude_db)
        .filter(|(f, _)| **f >= low_hz && **f <= high_hz)
        .map(|(_, m)| *m)
        .collect();
    if band.is_empty() {
        return Err(crate::DspError::InvalidInput(format!(
            "no bins in reference band {low_hz}-{high_hz} Hz"
        )));
    }
    let mean = band.iter().sum::<f64>() / band.len() as f64;
    Ok(magnitude_db.iter().map(|m| m - mean).collect())
}
```

Performance note for the reviewer: the O(n²) smoothing loop deliberately mirrors the oracle for parity; the analyzer's realtime path (stage 3+) may swap in the O(n) two-pointer version — that swap must go through the fixture test unchanged. Note the DC-bin rule: the raw *linear* value is pushed and then converted back through the dB floor at the end — matching the Python, which also round-trips DC through `20·log10(max(lin,1e-10))`. If `smooth_1_*` fixtures disagree only at bin 0, re-read `frequency_response.py:68-112` and match its exact DC handling.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p paraeq-dsp --test test_fr` — 4 passed.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): frequency-response module (FFT mag, octave smoothing, averaging, band normalize), golden-matched"
```

---

### Task 4: compensation module

**Files:**
- Create: `crates/paraeq-dsp/src/compensation.rs`, `crates/paraeq-dsp/tests/test_compensation.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod compensation;`)

**Interfaces:**
- Produces:
  - `pub fn parse_compensation(content: &str) -> Result<(Vec<f64>, Vec<f64>), crate::DspError>`
  - `pub fn load_compensation(path: &std::path::Path) -> Result<(Vec<f64>, Vec<f64>), crate::DspError>` (thin fs wrapper)
  - `pub fn apply_compensation(magnitude_db: &[f64], freqs_fft: &[f64], comp_freqs: &[f64], comp_gains_db: &[f64]) -> Vec<f64>`
  - `pub(crate) fn linear_interp_edge_hold(x: &[f64], xp: &[f64], fp: &[f64]) -> Vec<f64>` — shared linear interpolation with edge-hold fill (also used by nothing else yet; keep in this module)

- [ ] **Step 1: Failing tests**

`crates/paraeq-dsp/tests/test_compensation.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::compensation;
use std::path::PathBuf;

fn proto_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../prototype/tests/fixtures").join(name)
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
    assert_allclose(&out, &c.array("compensated"), 1e-12, 1e-12, "apply_compensation");
}

#[test]
fn parses_paraeq_csv_format() {
    let (f, g) = compensation::load_compensation(&proto_fixture("test_compensation.csv")).unwrap();
    assert_eq!(f.len(), g.len());
    assert!(!f.is_empty());
    assert!(f.windows(2).all(|w| w[0] < w[1]), "freqs sorted");
}

#[test]
fn parses_minidsp_format() {
    let (f, g) =
        compensation::load_compensation(&proto_fixture("test_compensation_minidsp.txt")).unwrap();
    assert_eq!(f.len(), g.len());
    assert!(!f.is_empty());
}
```

Before finalizing the two parse tests, run the oracle to pin exact expectations and strengthen them from "non-empty" to exact values:

```bash
.venv/bin/python - <<'EOF'
from pathlib import Path
from paraeq.measurement.compensation import load_compensation
for name in ["test_compensation.csv", "test_compensation_minidsp.txt"]:
    f, g = load_compensation(Path("prototype/tests/fixtures") / name)
    print(name, len(f), f[0], f[-1], g[0], g[-1])
EOF
```

Then assert those printed lengths/endpoint values exactly (atol 1e-12) in the two tests — the plan cannot know them ahead of time, the oracle can.

- [ ] **Step 2: Run to verify failure** — `cargo test -p paraeq-dsp --test test_compensation` FAILs.

- [ ] **Step 3: Implement `compensation.rs`**

Python reference: `prototype/paraeq/measurement/compensation.py`. Two formats: (1) ParaEQ CSV — `freq,gain` rows, `#`-comments and blank lines skipped; (2) miniDSP/REW — detected by a first line starting with `"`, parsed skipping 2 header rows, `*`-comment lines skipped, first two whitespace-separated columns used (phase column dropped). Interpolation for apply: linear with edge-hold fill `(fp[0], fp[last])` outside range.

```rust
//! Mic/jig calibration loading and application.
//! Oracle: prototype/paraeq/measurement/compensation.py

use crate::DspError;

pub fn parse_compensation(content: &str) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let first = content.lines().next().unwrap_or("");
    if first.trim_start().starts_with('"') {
        parse_minidsp(content)
    } else {
        parse_paraeq_csv(content)
    }
}

fn parse_paraeq_csv(content: &str) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut parts = t.split(',');
        let (f, g) = (parts.next(), parts.next());
        match (f, g) {
            (Some(f), Some(g)) => {
                freqs.push(f.trim().parse::<f64>().map_err(|e| DspError::Parse(format!("{t}: {e}")))?);
                gains.push(g.trim().parse::<f64>().map_err(|e| DspError::Parse(format!("{t}: {e}")))?);
            }
            _ => return Err(DspError::Parse(format!("bad CSV row: {t}"))),
        }
    }
    Ok((freqs, gains))
}

fn parse_minidsp(content: &str) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    for line in content.lines().skip(2) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('*') {
            continue;
        }
        let mut cols = t.split_whitespace();
        let (f, g) = (cols.next(), cols.next());
        match (f, g) {
            (Some(f), Some(g)) => {
                freqs.push(f.parse::<f64>().map_err(|e| DspError::Parse(format!("{t}: {e}")))?);
                gains.push(g.parse::<f64>().map_err(|e| DspError::Parse(format!("{t}: {e}")))?);
            }
            _ => return Err(DspError::Parse(format!("bad miniDSP row: {t}"))),
        }
    }
    Ok((freqs, gains))
}

pub fn load_compensation(path: &std::path::Path) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    parse_compensation(&std::fs::read_to_string(path)?)
}

/// np.interp semantics: linear inside, edge-hold outside; xp must be increasing.
pub(crate) fn linear_interp_edge_hold(x: &[f64], xp: &[f64], fp: &[f64]) -> Vec<f64> {
    x.iter()
        .map(|&q| {
            if q <= xp[0] {
                fp[0]
            } else if q >= xp[xp.len() - 1] {
                fp[fp.len() - 1]
            } else {
                let j = xp.partition_point(|&v| v <= q) - 1;
                let t = (q - xp[j]) / (xp[j + 1] - xp[j]);
                fp[j] + t * (fp[j + 1] - fp[j])
            }
        })
        .collect()
}

pub fn apply_compensation(
    magnitude_db: &[f64],
    freqs_fft: &[f64],
    comp_freqs: &[f64],
    comp_gains_db: &[f64],
) -> Vec<f64> {
    let interp = linear_interp_edge_hold(freqs_fft, comp_freqs, comp_gains_db);
    magnitude_db.iter().zip(&interp).map(|(m, c)| m - c).collect()
}
```

**Check the exact miniDSP parse against the oracle** (`compensation.py` uses `np.loadtxt(skiprows=2, comments="*", usecols=(0,1))`): the `skip(2)` above skips the first two LINES unconditionally, while loadtxt's `skiprows=2` does the same — but confirm the sample file's layout (`prototype/tests/fixtures/test_compensation_minidsp.txt`) parses to the same values the oracle prints in Step 1's pinning script. Comma-decimal or extra-header variants are NOT handled by the oracle either — do not add handling the oracle lacks.

- [ ] **Step 4: Run to verify pass** — 3 passed (with exact pinned values filled in).

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): compensation parsing (ParaEQ CSV + miniDSP) and application, oracle-pinned"
```

---

### Task 5: spline module (not-a-knot cubic spline)

**Files:**
- Create: `crates/paraeq-dsp/src/spline.rs`, `crates/paraeq-dsp/tests/test_spline.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod spline;`)

**Interfaces:**
- Produces: `pub struct NakSpline` with
  - `pub fn new(x: &[f64], y: &[f64]) -> Result<NakSpline, crate::DspError>` (x strictly increasing, len ≥ 2)
  - `pub fn eval(&self, q: f64) -> f64` — in-range piecewise cubic; out-of-range uses the first/last segment polynomial extended (scipy `extrapolate=True`); callers that clamp do so themselves (targets does)
- Consumed by: Task 6 (targets). The REAL parity gate for this module is Task 6's fixtures; this task's own tests are structural.

- [ ] **Step 1: Failing tests**

`crates/paraeq-dsp/tests/test_spline.rs`:

```rust
mod common;
use paraeq_dsp::spline::NakSpline;

#[test]
fn passes_through_knots() {
    let x = [1.0, 2.0, 3.5, 5.0, 8.0, 13.0];
    let y = [0.0, 3.0, -1.0, 2.5, 2.5, -4.0];
    let s = NakSpline::new(&x, &y).unwrap();
    for (xi, yi) in x.iter().zip(&y) {
        assert!((s.eval(*xi) - yi).abs() < 1e-12, "knot {xi}");
    }
}

/// scipy oracle values, pinned by running the script in this task's Step 1 notes.
#[test]
fn matches_scipy_at_pinned_points() {
    let x = [0.0, 1.0, 2.0, 3.0, 4.0];
    let y = [0.0, 1.0, 0.0, 1.0, 0.0];
    let s = NakSpline::new(&x, &y).unwrap();
    let queries = [0.5, 1.5, 2.5, 3.5, -0.5, 4.5]; // incl. extrapolation
    let expected: [f64; 6] = [PIN_ME; 6]; // <- replace with oracle printout
    for (q, e) in queries.iter().zip(&expected) {
        assert!((s.eval(*q) - e).abs() < 1e-10, "q={q}: {} vs {e}", s.eval(*q));
    }
}

#[test]
fn n2_is_linear_and_n3_is_parabola() {
    let s2 = NakSpline::new(&[0.0, 2.0], &[1.0, 5.0]).unwrap();
    assert!((s2.eval(1.0) - 3.0).abs() < 1e-12);
    // parabola y = x^2 through (0,0),(1,1),(2,4) must be reproduced exactly
    let s3 = NakSpline::new(&[0.0, 1.0, 2.0], &[0.0, 1.0, 4.0]).unwrap();
    assert!((s3.eval(0.5) - 0.25).abs() < 1e-12);
    assert!((s3.eval(1.7) - 2.89).abs() < 1e-12);
}
```

Pin the oracle values (replaces `PIN_ME` — the test MUST NOT be committed with the placeholder):

```bash
.venv/bin/python - <<'EOF'
from scipy.interpolate import CubicSpline
s = CubicSpline([0,1,2,3,4],[0,1,0,1,0])  # bc_type='not-a-knot', extrapolate=True
print([float(s(q)) for q in [0.5,1.5,2.5,3.5,-0.5,4.5]])
EOF
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement `spline.rs`**

This is a direct port of scipy `CubicSpline.__init__` (bc_type='not-a-knot'; scipy 1.17.1 `scipy/interpolate/_cubic.py:790-958`) reduced to the 1-D real case. The algorithm solves a tridiagonal system for the first derivatives `s[i]` at each knot, then evaluates cubic Hermite segments.

System (n ≥ 4), with `dx[i] = x[i+1]-x[i]`, `slope[i] = (y[i+1]-y[i])/dx[i]`:
- interior row i (1..n-2): `dx[i]·s[i-1] + 2(dx[i-1]+dx[i])·s[i] + dx[i-1]·s[i+1] = 3(dx[i]·slope[i-1] + dx[i-1]·slope[i])`
- start row (not-a-knot), `d = x[2]-x[0]`: diag `dx[1]`, super `d`; rhs `((dx[0]+2d)·dx[1]·slope[0] + dx[0]²·slope[1]) / d`
- end row (not-a-knot), `d = x[n-1]-x[n-3]`: sub `d`, diag `dx[n-3]`… **careful**: scipy sets `A[1,-1] = dx[-2]` (diag = dx[n-2]? no — numpy dx[-2] is the second-to-last of the n-1 dx values = dx[n-3]) and `A[-1,-2] = d`; rhs `(dx[-1]²·slope[-2] + (2d+dx[-1])·dx[-2]·slope[-1]) / d` where numpy `dx[-1]`=dx[n-2], `dx[-2]`=dx[n-3], `slope[-1]`=slope[n-2], `slope[-2]`=slope[n-3].
- Special cases (port both): n == 2 → both bcs become first-derivative = slope[0] (result: the straight line); n == 3 → parabola through the 3 points via the 3×3 system scipy builds (`A=[[1,1,0],[dx[1],2(dx[0]+dx[1]),dx[0]],[0,1,1]]`, `b=[2·slope[0], 3(dx[0]·slope[1]+dx[1]·slope[0]), 2·slope[1]]`).

```rust
//! Not-a-knot cubic spline — direct port of scipy.interpolate.CubicSpline
//! (bc_type='not-a-knot', extrapolate=True), 1-D real case.
//! Oracle: scipy 1.17.1 _cubic.py:790-958. Parity gate: Task 6 target fixtures.

use crate::DspError;

pub struct NakSpline {
    x: Vec<f64>,
    y: Vec<f64>,
    s: Vec<f64>, // first derivative at each knot
}

impl NakSpline {
    pub fn new(x: &[f64], y: &[f64]) -> Result<Self, DspError> {
        if x.len() != y.len() || x.len() < 2 {
            return Err(DspError::InvalidInput("spline needs >=2 equal-length points".into()));
        }
        if x.windows(2).any(|w| w[1] <= w[0]) {
            return Err(DspError::InvalidInput("spline x must be strictly increasing".into()));
        }
        let n = x.len();
        let dx: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
        let slope: Vec<f64> = dx
            .iter()
            .enumerate()
            .map(|(i, d)| (y[i + 1] - y[i]) / d)
            .collect();

        let s = if n == 2 {
            vec![slope[0], slope[0]]
        } else if n == 3 {
            // parabola through the three points (scipy special case)
            let a = [
                [1.0, 1.0, 0.0],
                [dx[1], 2.0 * (dx[0] + dx[1]), dx[0]],
                [0.0, 1.0, 1.0],
            ];
            let b = [
                2.0 * slope[0],
                3.0 * (dx[0] * slope[1] + dx[1] * slope[0]),
                2.0 * slope[1],
            ];
            solve3(a, b)
        } else {
            // tridiagonal system in banded form: sub[i]·s[i-1] + diag[i]·s[i] + sup[i]·s[i+1] = b[i]
            let mut sub = vec![0.0; n];
            let mut diag = vec![0.0; n];
            let mut sup = vec![0.0; n];
            let mut b = vec![0.0; n];
            for i in 1..n - 1 {
                sub[i] = dx[i];
                diag[i] = 2.0 * (dx[i - 1] + dx[i]);
                sup[i] = dx[i - 1];
                b[i] = 3.0 * (dx[i] * slope[i - 1] + dx[i - 1] * slope[i]);
            }
            let d0 = x[2] - x[0];
            diag[0] = dx[1];
            sup[0] = d0;
            b[0] = ((dx[0] + 2.0 * d0) * dx[1] * slope[0] + dx[0] * dx[0] * slope[1]) / d0;
            let dn = x[n - 1] - x[n - 3];
            diag[n - 1] = dx[n - 3];
            sub[n - 1] = dn;
            b[n - 1] = (dx[n - 2] * dx[n - 2] * slope[n - 3]
                + (2.0 * dn + dx[n - 2]) * dx[n - 3] * slope[n - 2])
                / dn;
            thomas(&sub, &diag, &sup, &b)
        };

        Ok(NakSpline { x: x.to_vec(), y: y.to_vec(), s })
    }

    pub fn eval(&self, q: f64) -> f64 {
        let n = self.x.len();
        // segment index: clamp to [0, n-2]; out-of-range extends end polynomials
        let i = if q <= self.x[0] {
            0
        } else if q >= self.x[n - 1] {
            n - 2
        } else {
            self.x.partition_point(|&v| v <= q) - 1
        };
        let h = self.x[i + 1] - self.x[i];
        let t = q - self.x[i];
        let slope = (self.y[i + 1] - self.y[i]) / h;
        let c2 = (3.0 * slope - 2.0 * self.s[i] - self.s[i + 1]) / h;
        let c3 = (self.s[i] + self.s[i + 1] - 2.0 * slope) / (h * h);
        self.y[i] + self.s[i] * t + c2 * t * t + c3 * t * t * t
    }
}

/// Thomas algorithm for a tridiagonal system (no pivoting; diagonally dominant here).
fn thomas(sub: &[f64], diag: &[f64], sup: &[f64], b: &[f64]) -> Vec<f64> {
    let n = b.len();
    let mut c = vec![0.0; n];
    let mut d = vec![0.0; n];
    c[0] = sup[0] / diag[0];
    d[0] = b[0] / diag[0];
    for i in 1..n {
        let m = diag[i] - sub[i] * c[i - 1];
        c[i] = if i < n - 1 { sup[i] / m } else { 0.0 };
        d[i] = (b[i] - sub[i] * d[i - 1]) / m;
    }
    let mut xout = vec![0.0; n];
    xout[n - 1] = d[n - 1];
    for i in (0..n - 1).rev() {
        xout[i] = d[i] - c[i] * xout[i + 1];
    }
    xout
}

fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> Vec<f64> {
    // Gaussian elimination on a 3x3 (partial pivoting unnecessary for these systems,
    // but do it anyway for robustness)
    let mut m = [
        [a[0][0], a[0][1], a[0][2], b[0]],
        [a[1][0], a[1][1], a[1][2], b[1]],
        [a[2][0], a[2][1], a[2][2], b[2]],
    ];
    for col in 0..3 {
        let piv = (col..3).max_by(|&r1, &r2| m[r1][col].abs().total_cmp(&m[r2][col].abs())).unwrap();
        m.swap(col, piv);
        for r in col + 1..3 {
            let f = m[r][col] / m[col][col];
            for k in col..4 {
                m[r][k] -= f * m[col][k];
            }
        }
    }
    let mut s = [0.0; 3];
    for r in (0..3).rev() {
        let mut v = m[r][3];
        for k in r + 1..3 {
            v -= m[r][k] * s[k];
        }
        s[r] = v / m[r][r];
    }
    s.to_vec()
}
```

**The not-a-knot end-row indices are the classic transcription trap.** After implementing, if `matches_scipy_at_pinned_points` fails, print scipy's derivative vector for the same data and compare against `self.s`:
`.venv/bin/python -c "from scipy.interpolate import CubicSpline; s=CubicSpline([0,1,2,3,4],[0,1,0,1,0]); print(s.derivative()( [0,1,2,3,4] ))"` — mismatch localizes to whichever end row is wrong.

- [ ] **Step 4: Run to verify pass** — `cargo test -p paraeq-dsp --test test_spline` — 3 passed.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): not-a-knot cubic spline (scipy CubicSpline port incl. n=2/n=3 special cases)"
```

---

### Task 6: targets module (curves, interpolation, anchors, matching)

**Files:**
- Create: `crates/paraeq-dsp/src/targets.rs`, `crates/paraeq-dsp/tests/test_targets.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod targets;`)

**Interfaces:**
- Consumes: `spline::NakSpline` (Task 5).
- Produces:
  - `pub const ANCHOR_FREQS: [f64; 16] = [20.0, 32.0, 50.0, 80.0, 125.0, 200.0, 315.0, 500.0, 800.0, 1250.0, 2000.0, 3150.0, 5000.0, 8000.0, 12500.0, 20000.0];`
  - `pub struct TargetCurve { pub name: String, pub frequencies: Vec<f64>, pub gains_db: Vec<f64>, pub category: Option<String>, pub description: Option<String>, pub source: Option<String> }`
  - `impl TargetCurve { pub fn interpolate(&self, query: &[f64]) -> Vec<f64> }`
  - `pub fn parse_target_csv(content: &str, fallback_name: &str) -> Result<TargetCurve, crate::DspError>`
  - `pub fn load_target_csv(path: &std::path::Path) -> Result<TargetCurve, crate::DspError>`
  - `pub fn list_targets(dir: &std::path::Path) -> Result<Vec<TargetCurve>, crate::DspError>` (files sorted by name, `.csv` only)
  - `pub fn compute_correction(measured_db: &[f64], target_db: &[f64]) -> Vec<f64>`
  - `pub fn build_anchor_target(base: &TargetCurve, anchor_freqs: &[f64], offsets_db: &[f64], name: &str) -> TargetCurve`
  - `pub fn match_closest_target<'a>(measured_freqs: &[f64], measured_db: &[f64], targets: &'a [TargetCurve]) -> &'a TargetCurve`

- [ ] **Step 1: Failing tests**

`crates/paraeq-dsp/tests/test_targets.rs`:

```rust
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
    assert_allclose(&curve.interpolate(&c.array("dense")), &c.array("interp_dense"), 1e-9, 1e-9, "dense");
    assert_allclose(&curve.interpolate(&c.array("edges")), &c.array("interp_edges"), 1e-9, 1e-9, "edges");
}

#[test]
fn anchor_deviation_matches_oracle() {
    let c = Case::load("targets", "anchor_deviation");
    let base = targets::load_target_csv(&targets_dir().join("harman_ie_2019.csv")).unwrap();
    let out = targets::build_anchor_target(&base, &c.array("anchor_freqs"), &c.array("offsets_db"), "Custom");
    assert_allclose(&out.frequencies, &c.array("result_freqs"), 1e-12, 1e-12, "grid");
    assert_allclose(&out.gains_db, &c.array("result_gains"), 1e-9, 1e-9, "gains");
}

#[test]
fn match_closest_matches_oracle() {
    let c = Case::load("targets", "match_closest");
    let all = targets::list_targets(&targets_dir()).unwrap();
    assert_eq!(all.len(), 6, "builtin count");
    let best = targets::match_closest_target(&c.array("measured_freqs"), &c.array("measured_db"), &all);
    assert_eq!(best.name, c.scalar("expected_name").as_str().unwrap());
}

#[test]
fn csv_metadata_parsing_matches_prototype_fixtures() {
    let proto = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../prototype/tests/fixtures");
    let with_meta = targets::load_target_csv(&proto.join("target_curve_with_metadata.csv")).unwrap();
    assert!(with_meta.category.is_some(), "metadata parsed");
    let no_meta = targets::load_target_csv(&proto.join("target_curve_no_metadata.csv")).unwrap();
    assert_eq!(no_meta.name, "target_curve_no_metadata", "name falls back to file stem");
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
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement `targets.rs`**

Python reference: `prototype/paraeq/correction/target_curves.py`. Load-bearing rules:
- `interpolate`: spline over `log10(max(f, 0.1))` of BOTH curve freqs and query, then hard clamp — query < first freq → `gains_db[0]`, query > last freq → `gains_db[last]`.
- CSV: rows `freq,gain`; `#`-comment lines skipped EXCEPT `# key: value` lines whose lowercased key ∈ {category, description, name, source} populate metadata (empty value = absent); `name` falls back to the file stem.
- `build_anchor_target`: grid = sorted dedup of base.frequencies ∪ anchor_freqs; deviation spline over `log10(anchor_freqs)` (no 0.1 floor — anchors start at 20 Hz), evaluated at `log10(grid)`, then edge-held to `offsets_db[0]`/`offsets_db[last]` outside the anchor range; result gains = `base.interpolate(grid) + deviation`; metadata copied from base, name from arg.
- `match_closest_target`: per target `residual = t.interpolate(measured_freqs) − measured_db`; subtract its mean; score = RMS; strict `<` — first-lowest wins ties in input order.

```rust
//! Target curves: load, interpolate (log-f not-a-knot spline + clamp),
//! anchor deviation layer, closest-match. Oracle: prototype/paraeq/correction/target_curves.py

use crate::spline::NakSpline;
use crate::DspError;
use std::path::Path;

pub const ANCHOR_FREQS: [f64; 16] = [
    20.0, 32.0, 50.0, 80.0, 125.0, 200.0, 315.0, 500.0, 800.0, 1250.0, 2000.0, 3150.0, 5000.0,
    8000.0, 12500.0, 20000.0,
];

#[derive(Clone, Debug)]
pub struct TargetCurve {
    pub name: String,
    pub frequencies: Vec<f64>,
    pub gains_db: Vec<f64>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub source: Option<String>,
}

fn log10_floored(freqs: &[f64]) -> Vec<f64> {
    freqs.iter().map(|f| f.max(0.1).log10()).collect()
}

impl TargetCurve {
    pub fn interpolate(&self, query: &[f64]) -> Vec<f64> {
        let s = NakSpline::new(&log10_floored(&self.frequencies), &self.gains_db)
            .expect("target curve freqs must be strictly increasing");
        let (first, last) = (self.frequencies[0], *self.frequencies.last().unwrap());
        let (g0, gn) = (self.gains_db[0], *self.gains_db.last().unwrap());
        query
            .iter()
            .map(|&q| {
                if q < first {
                    g0
                } else if q > last {
                    gn
                } else {
                    s.eval(q.max(0.1).log10())
                }
            })
            .collect()
    }
}

pub fn parse_target_csv(content: &str, fallback_name: &str) -> Result<TargetCurve, DspError> {
    let mut curve = TargetCurve {
        name: fallback_name.to_string(),
        frequencies: Vec::new(),
        gains_db: Vec::new(),
        category: None,
        description: None,
        source: None,
    };
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(comment) = t.strip_prefix('#') {
            if let Some((key, value)) = comment.split_once(':') {
                let value = value.trim();
                if !value.is_empty() {
                    match key.trim().to_lowercase().as_str() {
                        "category" => curve.category = Some(value.to_string()),
                        "description" => curve.description = Some(value.to_string()),
                        "name" => curve.name = value.to_string(),
                        "source" => curve.source = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
            continue;
        }
        let (f, g) = t
            .split_once(',')
            .ok_or_else(|| DspError::Parse(format!("bad target row: {t}")))?;
        curve
            .frequencies
            .push(f.trim().parse().map_err(|e| DspError::Parse(format!("{t}: {e}")))?);
        curve
            .gains_db
            .push(g.trim().parse().map_err(|e| DspError::Parse(format!("{t}: {e}")))?);
    }
    if curve.frequencies.is_empty() {
        return Err(DspError::Parse("target CSV has no data rows".into()));
    }
    Ok(curve)
}

pub fn load_target_csv(path: &Path) -> Result<TargetCurve, DspError> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("target");
    parse_target_csv(&std::fs::read_to_string(path)?, stem)
}

pub fn list_targets(dir: &Path) -> Result<Vec<TargetCurve>, DspError> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("csv"))
        .collect();
    paths.sort();
    paths.iter().map(|p| load_target_csv(p)).collect()
}

pub fn compute_correction(measured_db: &[f64], target_db: &[f64]) -> Vec<f64> {
    target_db.iter().zip(measured_db).map(|(t, m)| t - m).collect()
}

pub fn build_anchor_target(
    base: &TargetCurve,
    anchor_freqs: &[f64],
    offsets_db: &[f64],
    name: &str,
) -> TargetCurve {
    let mut grid: Vec<f64> = base.frequencies.iter().chain(anchor_freqs).copied().collect();
    grid.sort_by(|a, b| a.total_cmp(b));
    grid.dedup();
    let log_anchor: Vec<f64> = anchor_freqs.iter().map(|f| f.log10()).collect();
    let dev_spline = NakSpline::new(&log_anchor, offsets_db).expect("anchors strictly increasing");
    let (a_first, a_last) = (anchor_freqs[0], *anchor_freqs.last().unwrap());
    let (o_first, o_last) = (offsets_db[0], *offsets_db.last().unwrap());
    let deviation: Vec<f64> = grid
        .iter()
        .map(|&f| {
            if f < a_first {
                o_first
            } else if f > a_last {
                o_last
            } else {
                dev_spline.eval(f.log10())
            }
        })
        .collect();
    let base_gains = base.interpolate(&grid);
    TargetCurve {
        name: name.to_string(),
        gains_db: base_gains.iter().zip(&deviation).map(|(b, d)| b + d).collect(),
        frequencies: grid,
        category: base.category.clone(),
        description: base.description.clone(),
        source: base.source.clone(),
    }
}

pub fn match_closest_target<'a>(
    measured_freqs: &[f64],
    measured_db: &[f64],
    targets: &'a [TargetCurve],
) -> &'a TargetCurve {
    let mut best: Option<(&TargetCurve, f64)> = None;
    for t in targets {
        let interp = t.interpolate(measured_freqs);
        let residual: Vec<f64> = interp.iter().zip(measured_db).map(|(i, m)| i - m).collect();
        let mean = residual.iter().sum::<f64>() / residual.len() as f64;
        let score = (residual.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
            / residual.len() as f64)
            .sqrt();
        if best.is_none() || score < best.unwrap().1 {
            best = Some((t, score));
        }
    }
    best.expect("targets list must be non-empty").0
}
```

Note on `interpolate`'s clamp comparisons: the Python clamps with `query < frequencies[0]` / `query > frequencies[-1]` via `np.where` (strict inequalities; boundary points go through the spline) — keep the strict `<`/`>` above. `grid.dedup()` relies on exact float equality — same as `np.unique`; fine because anchor/base values come from the same literals the oracle used.

- [ ] **Step 4: Run to verify pass** — `cargo test -p paraeq-dsp --test test_targets` — 5 passed. The `harman_ie_interp` case is the true gate for Task 5's spline: a not-a-knot transcription error fails HERE (dense grid over 695 knots) even if Task 5's small cases pass.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): target curves — CSV+metadata, log-f spline interpolation, anchor deviation, closest match; golden-matched"
```

---

### Task 7: fir module (frequency-sampling design + homomorphic minimum phase)

**Files:**
- Create: `crates/paraeq-dsp/src/fir.rs`, `crates/paraeq-dsp/tests/test_fir.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod fir;`)

**Interfaces:**
- Produces:
  - `pub enum FirPhase { Linear, Minimum }`
  - `pub fn design_fir_correction(correction_db: &[f64], n_taps: usize, phase: FirPhase) -> Vec<f64>`
  - **Divergence (log it in Task 13):** the Python signature has a `freqs` parameter that is documented but never used (resampling is positional); the Rust drops it.
- Consumes: nothing from other tasks (self-contained FFT work).

- [ ] **Step 1: Failing tests**

`crates/paraeq-dsp/tests/test_fir.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::fir::{design_fir_correction, FirPhase};

fn run_case(name: &str, phase: FirPhase, atol: f64) {
    let c = Case::load("fir", name);
    let taps = design_fir_correction(&c.array("correction_db"), c.param_u64("n_taps") as usize, phase);
    assert_allclose(&taps, &c.array("taps"), 0.0, atol, name);
}

#[test]
fn linear_phase_matches_oracle() {
    run_case("linear_512", FirPhase::Linear, 1e-9);
    run_case("linear_4096", FirPhase::Linear, 1e-9);
}

#[test]
fn minimum_phase_taps_match_oracle() {
    // log/exp cepstrum amplifies FFT rounding: 1e-6 abs per spec
    run_case("minimum_512", FirPhase::Minimum, 1e-6);
    run_case("minimum_4096", FirPhase::Minimum, 1e-6);
}

/// The perceptually true criterion (spec): the min-phase filter's magnitude
/// response must match the oracle taps' response within 0.01 dB, 20 Hz-20 kHz.
#[test]
fn minimum_phase_response_within_001_db() {
    for name in ["minimum_512", "minimum_4096"] {
        let c = Case::load("fir", name);
        let ours = design_fir_correction(&c.array("correction_db"), c.param_u64("n_taps") as usize, FirPhase::Minimum);
        let theirs = c.array("taps");
        let sr = 48000.0;
        for k in 0..200 {
            // 200-point log grid, 20 Hz .. 20 kHz
            let f = 20.0 * (20000.0f64 / 20.0).powf(k as f64 / 199.0);
            let a = fir_mag_db(&ours, f, sr);
            let b = fir_mag_db(&theirs, f, sr);
            assert!((a - b).abs() < 0.01, "{name} @ {f:.1} Hz: {a:.5} vs {b:.5} dB");
        }
    }
}

fn fir_mag_db(taps: &[f64], f: f64, sr: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f / sr;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, t) in taps.iter().enumerate() {
        re += t * (w * n as f64).cos();
        im -= t * (w * n as f64).sin();
    }
    20.0 * (re * re + im * im).sqrt().max(1e-30).log10()
}
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement `fir.rs`**

Python reference: `prototype/paraeq/correction/fir_filter.py` + scipy 1.17.1 `scipy/signal/_fir_filter_design.py:1387-1411` (`minimum_phase`, homomorphic branch, `half=True`). The exact recipe:

**Linear path** (`_design_linear_phase`): `magnitude = 10^(dB/20)`; resample POSITIONALLY onto `n_taps/2 + 1` bins via linear interp of index position (`np.interp(linspace(0,1,bins), linspace(0,1,len), magnitude)`); real-spectrum irfft to `n_taps`; `roll(n_taps/2)`; multiply by symmetric Hann `np.hanning(n_taps)` = `0.5 − 0.5·cos(2πi/(n−1))`; **peak-normalize** (`ir /= max|ir|`); symmetrize `(ir + reversed(ir))/2`.

**Minimum path** (`_design_minimum_phase`): prototype length `n_proto = 2·n_taps − 1` (odd); build the linear-phase prototype as above but with magnitude **squared** and length `n_proto` (bins `n_proto/2+1`), rolled by `n_proto/2`, Hann `n_proto`, **NO peak normalization, NO symmetrize on this path** — ⚠ read `fir_filter.py` carefully: the min-phase path builds its own prototype inline (check whether it reuses `_design_linear_phase` including its normalize step, or builds raw irfft+roll+window; port EXACTLY what the Python does — the fixture will expose any difference). Then scipy `minimum_phase(proto, method="homomorphic", n_fft=2^ceil(log2(4·n_proto)))`:

1. `h_temp = |FFT(proto, n_fft)|` (full complex FFT, zero-padded)
2. `h_temp += 1e-7 · min(h_temp[h_temp > 0])`
3. `h_temp = ln(h_temp)`; then `h_temp *= 0.5` (half=True)
4. `h_temp = Re(IFFT(h_temp))` (rustfft inverse + explicit ÷n_fft)
5. window: `win[0]=1; win[1..n_fft/2]=2; win[n_fft/2]=1 if n_fft odd else 0; rest 0` — n_fft is a power of two here, so `win[n_fft/2]` stays 0
6. `h_temp *= win`
7. `h_temp = IFFT(exp(FFT(h_temp)))` (complex exp; explicit ÷n_fft on the inverse), take real part
8. output length `(n_proto + 1)/2 = n_taps`; the prototype then pads/truncates to `n_taps` (already exact)

```rust
//! FIR correction design: frequency sampling (linear phase) + homomorphic
//! minimum phase. Oracles: prototype/paraeq/correction/fir_filter.py and
//! scipy.signal.minimum_phase (homomorphic, half=True), scipy 1.17.1.

use realfft::RealFftPlanner;
use rustfft::{num_complex::Complex, FftPlanner};

pub enum FirPhase {
    Linear,
    Minimum,
}

pub fn design_fir_correction(correction_db: &[f64], n_taps: usize, phase: FirPhase) -> Vec<f64> {
    let magnitude: Vec<f64> = correction_db.iter().map(|db| 10f64.powf(db / 20.0)).collect();
    match phase {
        FirPhase::Linear => design_linear(&magnitude, n_taps, true),
        FirPhase::Minimum => {
            let n_proto = 2 * n_taps - 1;
            // squared magnitude: homomorphic half=True takes the square root
            let squared: Vec<f64> = magnitude.iter().map(|m| m * m).collect();
            let proto = design_linear(&squared, n_proto, false);
            let n_fft = 1usize << ((4 * n_proto) as f64).log2().ceil() as u32;
            let min_ph = minimum_phase_homomorphic(&proto, n_fft);
            min_ph.into_iter().take(n_taps).collect()
        }
    }
}

/// Frequency-sampling linear-phase design. `normalize` = the peak-normalize +
/// symmetrize steps (the public linear path does them; the min-phase prototype
/// does NOT — verify against fir_filter.py and flip if the fixtures disagree).
fn design_linear(magnitude: &[f64], n_taps: usize, normalize: bool) -> Vec<f64> {
    let bins = n_taps / 2 + 1;
    // positional resample: np.interp(linspace(0,1,bins), linspace(0,1,len), magnitude)
    let resampled: Vec<f64> = (0..bins)
        .map(|i| {
            let pos = i as f64 / (bins - 1) as f64 * (magnitude.len() - 1) as f64;
            let j = pos.floor() as usize;
            if j + 1 >= magnitude.len() {
                magnitude[magnitude.len() - 1]
            } else {
                let t = pos - j as f64;
                magnitude[j] * (1.0 - t) + magnitude[j + 1] * t
            }
        })
        .collect();
    // irfft(real spectrum) with explicit 1/n
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(n_taps);
    let mut spectrum: Vec<Complex<f64>> =
        resampled.iter().map(|m| Complex::new(*m, 0.0)).collect();
    let mut ir = ifft.make_output_vec();
    ifft.process(&mut spectrum, &mut ir).unwrap();
    for v in &mut ir {
        *v /= n_taps as f64;
    }
    // roll by n_taps/2
    ir.rotate_right(n_taps / 2);
    // symmetric Hann (np.hanning): 0.5 - 0.5*cos(2*pi*i/(n-1))
    for (i, v) in ir.iter_mut().enumerate() {
        *v *= 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n_taps - 1) as f64).cos();
    }
    if normalize {
        let peak = ir.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        if peak > 0.0 {
            for v in &mut ir {
                *v /= peak;
            }
        }
        let rev: Vec<f64> = ir.iter().rev().copied().collect();
        for (v, r) in ir.iter_mut().zip(&rev) {
            *v = (*v + r) / 2.0;
        }
    }
    ir
}

/// scipy.signal.minimum_phase(h, method="homomorphic", n_fft, half=True), exact port.
fn minimum_phase_homomorphic(h: &[f64], n_fft: usize) -> Vec<f64> {
    let mut planner = FftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let inv = planner.plan_fft_inverse(n_fft);

    let mut buf: Vec<Complex<f64>> = h
        .iter()
        .map(|v| Complex::new(*v, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
        .take(n_fft)
        .collect();
    fwd.process(&mut buf);
    let mut mag: Vec<f64> = buf.iter().map(|c| c.norm()).collect();
    let min_pos = mag.iter().copied().filter(|v| *v > 0.0).fold(f64::INFINITY, f64::min);
    for v in &mut mag {
        *v = (*v + 1e-7 * min_pos).ln() * 0.5; // += eps, log, half=True
    }
    // real cepstrum: Re(IFFT(log-mag))
    let mut cep: Vec<Complex<f64>> = mag.iter().map(|v| Complex::new(*v, 0.0)).collect();
    inv.process(&mut cep);
    for c in &mut cep {
        *c /= n_fft as f64;
    }
    // homomorphic window: [1, 2,2,...,2 (up to n_fft/2-1), 0 (n_fft even), 0...]
    let stop = n_fft / 2;
    let mut windowed: Vec<Complex<f64>> = vec![Complex::new(0.0, 0.0); n_fft];
    windowed[0] = Complex::new(cep[0].re, 0.0);
    for i in 1..stop {
        windowed[i] = Complex::new(2.0 * cep[i].re, 0.0);
    }
    if n_fft % 2 == 1 {
        windowed[stop] = Complex::new(cep[stop].re, 0.0);
    }
    // exp(FFT(windowed)), then IFFT, take real
    fwd.process(&mut windowed);
    for c in &mut windowed {
        *c = c.exp();
    }
    inv.process(&mut windowed);
    let scale = 1.0 / n_fft as f64;
    let n_out = (h.len() + 1) / 2;
    windowed.iter().take(n_out).map(|c| c.re * scale).collect()
}
```

**⚠ Two verify-against-oracle points before trusting this task:**
1. The min-phase prototype's construction — whether `fir_filter.py`'s `_design_minimum_phase` reuses the peak-normalizing linear designer or builds a raw (un-normalized) prototype. The `normalize: bool` flag above encodes my best reading of the facts ("No peak normalization on this path"); the fixtures are the arbiter. If `minimum_*` fixtures fail with a uniform gain offset, flip which steps `normalize` guards.
2. scipy's cepstrum steps use the FULL complex values (`h_temp = real(ifft(...))` keeps only the real part before windowing — the port above takes `.re` at the windowing step, equivalent since the win multiplies element-wise; keep it).

- [ ] **Step 4: Run to verify pass** — `cargo test -p paraeq-dsp --test test_fir` — 3 passed (4 fixture cases + 2 response sweeps inside them).

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): FIR design — frequency-sampling linear phase + scipy-exact homomorphic minimum phase, golden-matched"
```

---

### Task 8: peq module (EQBand, ParametricEQ, AutoEQ export)

**Files:**
- Create: `crates/paraeq-dsp/src/peq.rs`, `crates/paraeq-dsp/tests/test_peq.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod peq;`)

**Interfaces:**
- Consumes: `biquad::{peaking, low_shelf, high_shelf, notch, sos_frequency_response_db}` (Task 2).
- Produces:
  - `pub enum FilterType { HighShelf, LowShelf, Notch, Peaking }` with `pub fn as_str(&self) -> &'static str` returning the Python keys (`"high_shelf"` etc.) and `pub fn from_str(s: &str) -> Result<FilterType, crate::DspError>`
  - `pub struct EQBand { pub filter_type: FilterType, pub fc: f64, pub gain_db: f64, pub q: f64 }` with `pub fn to_sos(&self, sample_rate: f64) -> [f64; 6]`
  - `pub struct ParametricEQ { pub bands: Vec<EQBand>, pub sample_rate: f64 }` with `combined_sos() -> Vec<[f64; 6]>`, `frequency_response(&self, freqs: &[f64]) -> Vec<f64>`, `export_autoeq_format(&self) -> String`
- Consumed by: Task 9 (autofit uses EQBand + response).

- [ ] **Step 1: Failing tests**

`crates/paraeq-dsp/tests/test_peq.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};

fn bands_from_scalars(c: &Case) -> Vec<EQBand> {
    c.scalar("bands")
        .as_array()
        .unwrap()
        .iter()
        .map(|b| EQBand {
            filter_type: FilterType::from_str(b["filter_type"].as_str().unwrap()).unwrap(),
            fc: b["fc"].as_f64().unwrap(),
            gain_db: b["gain_db"].as_f64().unwrap(),
            q: b["q"].as_f64().unwrap(),
        })
        .collect()
}

#[test]
fn composite_response_matches_oracle() {
    let c = Case::load("peq", "two_band");
    let peq = ParametricEQ { bands: bands_from_scalars(&c), sample_rate: c.param_f64("sample_rate") };
    assert_eq!(peq.combined_sos().len(), 2);
    let resp = peq.frequency_response(&c.array("freqs"));
    assert_allclose(&resp, &c.array("response_db"), 1e-9, 1e-9, "composite response");
}

#[test]
fn autoeq_export_matches_oracle_exactly() {
    let c = Case::load("peq", "two_band");
    let peq = ParametricEQ { bands: bands_from_scalars(&c), sample_rate: c.param_f64("sample_rate") };
    assert_eq!(peq.export_autoeq_format(), c.scalar("autoeq_export").as_str().unwrap());
}
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement `peq.rs`**

Python reference: `prototype/paraeq/correction/parametric_eq.py` — READ IT FIRST; two details must be transcribed from the source, not guessed:
1. **`frequency_response` composition**: implement exactly what the Python does. If it calls `biquad_frequency_response(self.combined_sos(), ...)` → cascade product with a single `+1e-10` dB floor: implement as `crate::biquad::sos_frequency_response_db(&self.combined_sos(), freqs, self.sample_rate)`. If it instead sums per-band dB responses → each band gets its own `+1e-10` floor before summation: implement that. The fixture's 1e-9 tolerance will reject the wrong choice (floors differ at ~1e-10·N level only when |H|≈0, but band responses near unity make the two nearly identical — trust the code you read over the fixture's discrimination here).
2. **`export_autoeq_format` exact string**: transcribe the Python's format calls verbatim (preamp line, filter lines, float formatting/precision, line separators, trailing newline or not). The fixture's `autoeq_export` scalar is the byte-exact expected output; the test compares whole strings.

Skeleton (fill the two transcription points from the Python):

```rust
//! Parametric EQ container + AutoEQ text export.
//! Oracle: prototype/paraeq/correction/parametric_eq.py

use crate::biquad;
use crate::DspError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    HighShelf,
    LowShelf,
    Notch,
    Peaking,
}

impl FilterType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterType::HighShelf => "high_shelf",
            FilterType::LowShelf => "low_shelf",
            FilterType::Notch => "notch",
            FilterType::Peaking => "peaking",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<FilterType, DspError> {
        match s {
            "high_shelf" => Ok(FilterType::HighShelf),
            "low_shelf" => Ok(FilterType::LowShelf),
            "notch" => Ok(FilterType::Notch),
            "peaking" => Ok(FilterType::Peaking),
            other => Err(DspError::InvalidInput(format!("unknown filter type {other}"))),
        }
    }
    /// AutoEQ type tag (parametric_eq.py _AUTOEQ_FILTER_NAMES).
    pub fn autoeq_tag(&self) -> &'static str {
        match self {
            FilterType::HighShelf => "HSC",
            FilterType::LowShelf => "LSC",
            FilterType::Notch => "NO",
            FilterType::Peaking => "PK",
        }
    }
}

#[derive(Clone, Debug)]
pub struct EQBand {
    pub filter_type: FilterType,
    pub fc: f64,
    pub gain_db: f64,
    pub q: f64,
}

impl EQBand {
    pub fn to_sos(&self, sample_rate: f64) -> [f64; 6] {
        match self.filter_type {
            FilterType::HighShelf => biquad::high_shelf(self.fc, self.gain_db, self.q, sample_rate),
            FilterType::LowShelf => biquad::low_shelf(self.fc, self.gain_db, self.q, sample_rate),
            FilterType::Notch => biquad::notch(self.fc, self.q, sample_rate),
            FilterType::Peaking => biquad::peaking(self.fc, self.gain_db, self.q, sample_rate),
        }
    }
}

pub struct ParametricEQ {
    pub bands: Vec<EQBand>,
    pub sample_rate: f64,
}

impl ParametricEQ {
    pub fn combined_sos(&self) -> Vec<[f64; 6]> {
        self.bands.iter().map(|b| b.to_sos(self.sample_rate)).collect()
    }

    pub fn frequency_response(&self, freqs: &[f64]) -> Vec<f64> {
        // TRANSCRIPTION POINT 1 — match parametric_eq.py exactly (see task notes)
        biquad::sos_frequency_response_db(&self.combined_sos(), freqs, self.sample_rate)
    }

    pub fn export_autoeq_format(&self) -> String {
        // TRANSCRIPTION POINT 2 — copy the exact format from parametric_eq.py:92-110
        // (preamp computation + per-filter lines + separators). The fixture scalar
        // "autoeq_export" in fixtures/peq/two_band.json is the byte-exact expected
        // output — open it and mirror the Python that produced it.
        todo!("transcribe from parametric_eq.py before first test run")
    }
}
```

(The `todo!` MUST be replaced in this same task — Step 4 cannot pass with it. It is a transcription marker, not deferred work: the reference implementation is 20 lines of in-repo Python and the expected output is a committed fixture string.)

- [ ] **Step 4: Run to verify pass** — `cargo test -p paraeq-dsp --test test_peq` — 2 passed.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): parametric EQ container + byte-exact AutoEQ export, golden-matched"
```

---

### Task 9: autofit module (greedy PEQ fitting)

**Files:**
- Create: `crates/paraeq-dsp/src/autofit.rs`, `crates/paraeq-dsp/tests/test_autofit.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod autofit;`)

**Interfaces:**
- Consumes: `peq::{EQBand, FilterType, ParametricEQ}` (Task 8).
- Produces: `pub fn auto_fit_parametric_eq(correction_db: &[f64], freqs: &[f64], sample_rate: f64, max_bands: usize, min_gain_db: f64) -> Vec<EQBand>`

- [ ] **Step 1: Failing test**

`crates/paraeq-dsp/tests/test_autofit.rs`:

```rust
mod common;
use common::Case;
use paraeq_dsp::autofit::auto_fit_parametric_eq;

#[test]
fn greedy_fit_matches_oracle_bands() {
    let c = Case::load("autofit", "two_peaks");
    let fitted = auto_fit_parametric_eq(
        &c.array("correction_db"),
        &c.array("freqs"),
        c.param_f64("sample_rate"),
        c.param_u64("max_bands") as usize,
        0.5,
    );
    let expected = c.scalar("fitted").as_array().unwrap();
    assert_eq!(fitted.len(), expected.len(), "band count");
    for (b, e) in fitted.iter().zip(expected) {
        assert_eq!(b.filter_type.as_str(), e["filter_type"].as_str().unwrap());
        let (fc, gain, q) =
            (e["fc"].as_f64().unwrap(), e["gain_db"].as_f64().unwrap(), e["q"].as_f64().unwrap());
        assert!((b.fc - fc).abs() <= 1e-9 * fc.abs().max(1.0), "fc {} vs {fc}", b.fc);
        assert!((b.gain_db - gain).abs() <= 1e-9 * gain.abs().max(1.0), "gain {} vs {gain}", b.gain_db);
        assert!((b.q - q).abs() <= 1e-9 * q.abs().max(1.0), "q {} vs {q}", b.q);
    }
}
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement `autofit.rs`**

Python reference: `prototype/paraeq/correction/auto_fit.py`. The algorithm is deterministic; port it EXACTLY:
- audible mask: `20.0 <= f <= 20000.0` (inclusive both ends); indices outside never selected.
- loop up to `max_bands`: find `argmax |residual|` within mask (numpy argmax = FIRST index on ties); `peak_gain = residual[idx]` (signed); stop if `|peak_gain| < min_gain_db` or `freqs[idx] <= 0`.
- `_estimate_q`: walk LEFT from `peak_idx-1` down to index 1 inclusive (`range(peak_idx-1, 0, -1)` — never index 0!), first bin with `|residual| < 0.5·|peak|` → f_low; walk RIGHT from `peak_idx+1` to end, same test → f_high; if either edge missing, or `f_high <= f_low`, or `f_center <= 0` → Q = 2.0; else `Q = clamp(f_center/(f_high−f_low), 0.5, 20.0)`.
- append `EQBand { Peaking, fc: freqs[idx], gain_db: peak_gain, q }`; subtract that ONE band's `ParametricEQ::frequency_response(freqs)` from residual, with NaN→0.0 sanitation (`np.nan_to_num`).

```rust
//! Greedy parametric-EQ auto-fit. Oracle: prototype/paraeq/correction/auto_fit.py

use crate::peq::{EQBand, FilterType, ParametricEQ};

pub fn auto_fit_parametric_eq(
    correction_db: &[f64],
    freqs: &[f64],
    sample_rate: f64,
    max_bands: usize,
    min_gain_db: f64,
) -> Vec<EQBand> {
    let mut residual = correction_db.to_vec();
    let mask: Vec<bool> = freqs.iter().map(|f| (20.0..=20000.0).contains(f)).collect();
    let mut bands = Vec::new();
    for _ in 0..max_bands {
        let mut peak_idx = None;
        let mut peak_abs = f64::NEG_INFINITY;
        for i in 0..residual.len() {
            if mask[i] && residual[i].abs() > peak_abs {
                peak_abs = residual[i].abs();
                peak_idx = Some(i);
            }
        }
        let Some(idx) = peak_idx else { break };
        let peak_gain = residual[idx];
        if peak_gain.abs() < min_gain_db || freqs[idx] <= 0.0 {
            break;
        }
        let q = estimate_q(&residual, freqs, idx);
        let band = EQBand { filter_type: FilterType::Peaking, fc: freqs[idx], gain_db: peak_gain, q };
        let response = ParametricEQ { bands: vec![band.clone()], sample_rate }.frequency_response(freqs);
        for (r, resp) in residual.iter_mut().zip(&response) {
            let v = if resp.is_nan() { 0.0 } else { *resp };
            *r -= v;
        }
        bands.push(band);
    }
    bands
}

fn estimate_q(residual: &[f64], freqs: &[f64], peak_idx: usize) -> f64 {
    let half = 0.5 * residual[peak_idx].abs();
    let f_center = freqs[peak_idx];
    let mut f_low = None;
    let mut i = peak_idx;
    while i > 1 {
        i -= 1;
        if residual[i].abs() < half {
            f_low = Some(freqs[i]);
            break;
        }
    }
    let mut f_high = None;
    for j in peak_idx + 1..residual.len() {
        if residual[j].abs() < half {
            f_high = Some(freqs[j]);
            break;
        }
    }
    match (f_low, f_high) {
        (Some(lo), Some(hi)) if hi > lo && f_center > 0.0 => (f_center / (hi - lo)).clamp(0.5, 20.0),
        _ => 2.0,
    }
}
```

**⚠ Left-walk bound check**: `range(peak_idx-1, 0, -1)` in Python visits `peak_idx-1 … 1` (never 0). The `while i > 1 { i -= 1; … }` above visits the same indices. Confirm against `auto_fit.py` and keep exact — an off-by-one here changes Q on edge-hugging peaks and the fixture will catch it.

- [ ] **Step 4: Run to verify pass** — 1 passed.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): greedy PEQ auto-fit, golden-matched to oracle bands"
```

---

### Task 10: deconvolution module (Wiener spectral division)

**Files:**
- Create: `crates/paraeq-dsp/src/deconvolution.rs`, `crates/paraeq-dsp/tests/test_deconvolution.rs`
- Modify: `crates/paraeq-dsp/src/lib.rs` (`pub mod deconvolution;`)

**Interfaces:**
- Produces: `pub fn deconvolve(recorded: &[f64], sweep: &[f64], _sample_rate: u32) -> Vec<f64>` (mono; multi-channel callers loop; sample_rate accepted for API symmetry with the oracle but unused — same as the oracle's unused f_start/f_end).

- [ ] **Step 1: Failing test**

`crates/paraeq-dsp/tests/test_deconvolution.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::deconvolution::deconvolve;

#[test]
fn wiener_deconvolution_matches_oracle() {
    let c = Case::load("deconvolution", "delta_plus_tail");
    let ir = deconvolve(&c.array("recorded"), &c.array("sweep"), c.param_u64("sample_rate") as u32);
    assert_allclose(&ir, &c.array("ir_out"), 1e-9, 1e-9, "deconvolved IR");
}

#[test]
fn recovers_the_true_delay() {
    let c = Case::load("deconvolution", "delta_plus_tail");
    let ir = deconvolve(&c.array("recorded"), &c.array("sweep"), 48000);
    let argmax = ir.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
    assert!((argmax as i64 - 32).unsigned_abs() <= 10, "peak at {argmax}, true delay 32");
}
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement**

Python reference: `prototype/paraeq/measurement/deconvolution.py`. Exact recipe: `n_fft = next_pow2(len(recorded) + len(sweep))`; `S = rfft(sweep, n_fft)`; `P = |S|²`; `eps = 1e-10 · max(P)`; `inv = conj(S)/(P + eps)`; `ir = irfft(rfft(recorded, n_fft) · inv, n_fft)[..len(recorded)]` with explicit `1/n_fft` on the inverse.

```rust
//! Regularized (Wiener) spectral-division deconvolution.
//! Oracle: prototype/paraeq/measurement/deconvolution.py

use realfft::RealFftPlanner;

pub fn deconvolve(recorded: &[f64], sweep: &[f64], _sample_rate: u32) -> Vec<f64> {
    let n = recorded.len();
    let n_fft = (n + sweep.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let inv = planner.plan_fft_inverse(n_fft);

    let mut sweep_padded = vec![0.0; n_fft];
    sweep_padded[..sweep.len()].copy_from_slice(sweep);
    let mut s = fwd.make_output_vec();
    fwd.process(&mut sweep_padded, &mut s).unwrap();

    let power: Vec<f64> = s.iter().map(|c| c.norm_sqr()).collect();
    let eps = 1e-10 * power.iter().fold(0.0f64, |m, v| m.max(*v));

    let mut rec_padded = vec![0.0; n_fft];
    rec_padded[..n].copy_from_slice(recorded);
    let mut r = fwd.make_output_vec();
    fwd.process(&mut rec_padded, &mut r).unwrap();

    for ((ri, si), p) in r.iter_mut().zip(&s).zip(&power) {
        *ri *= si.conj() / (p + eps);
    }
    let mut ir = inv.make_output_vec();
    inv.process(&mut r, &mut ir).unwrap();
    ir.truncate(n);
    for v in &mut ir {
        *v /= n_fft as f64;
    }
    ir
}
```

Note: `next_power_of_two()` on an exact power of two returns the value itself; numpy's `2**ceil(log2(x))` does the same — but for the fixture's sizes (`12000+12000=24000 → 32768`) they agree regardless.

- [ ] **Step 4: Run to verify pass** — 2 passed.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): Wiener spectral-division deconvolution, golden-matched"
```

---

### Task 11: engine — overlap-add convolver

**Files:**
- Create: `crates/paraeq-engine/src/convolver.rs`, `crates/paraeq-engine/tests/common/mod.rs` (copy of `crates/paraeq-dsp/tests/common/mod.rs` verbatim — test-only duplication across crates, acceptable), `crates/paraeq-engine/tests/test_convolver.rs`
- Modify: `crates/paraeq-engine/src/lib.rs` (`pub mod convolver;`), `crates/paraeq-engine/Cargo.toml` (add `realfft = { workspace = true }`, `rustfft = { workspace = true }`; dev-dep `serde_json = { workspace = true }`)

**Interfaces:**
- Produces: `pub struct OverlapAddConvolver` with
  - `pub fn new(firs: Vec<Vec<f64>>, block_size: usize) -> OverlapAddConvolver` (one FIR per channel; a single FIR broadcast to all channels mirrors the oracle's `min(ch, n_firs-1)` rule)
  - `pub fn process(&mut self, input: &[&[f64]], output: &mut [Vec<f64>])` — each channel slice exactly `block_size` long; `output[ch]` is overwritten (len block_size). **No allocation after construction** (spec realtime rule — scratch buffers preallocated in `new`).
  - `pub fn reset(&mut self)`

- [ ] **Step 1: Failing test**

`crates/paraeq-engine/tests/test_convolver.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_engine::convolver::OverlapAddConvolver;

#[test]
fn stereo_blocks_match_oracle_block_by_block() {
    let c = Case::load("convolver", "stereo_8_blocks");
    let block = c.param_u64("block_size") as usize;
    let input = c.array2("input");
    let expected = c.array2("output");
    let mut conv =
        OverlapAddConvolver::new(vec![c.array("fir_l"), c.array("fir_r")], block);
    let mut out = vec![vec![0.0; block], vec![0.0; block]];
    let n_blocks = input.rows / block;
    assert_eq!(n_blocks, 8);
    let (in0, in1) = (input.col(0), input.col(1));
    let (exp0, exp1) = (expected.col(0), expected.col(1));
    for b in 0..n_blocks {
        let r = b * block..(b + 1) * block;
        conv.process(&[&in0[r.clone()], &in1[r.clone()]], &mut out);
        assert_allclose(&out[0], &exp0[r.clone()], 1e-9, 1e-9, &format!("block {b} ch0"));
        assert_allclose(&out[1], &exp1[r.clone()], 1e-9, 1e-9, &format!("block {b} ch1"));
    }
}

#[test]
fn mono_fir_broadcasts_to_stereo_without_panicking() {
    // regression for the prototype's mono-FIR IndexError (CONTEXT.md)
    let mut conv = OverlapAddConvolver::new(vec![vec![1.0, 0.5, 0.25]], 64);
    let silence = vec![0.0f64; 64];
    let mut out = vec![vec![0.0; 64], vec![0.0; 64]];
    conv.process(&[&silence, &silence], &mut out);
    assert!(out.iter().flatten().all(|v| v.abs() < 1e-10));
}
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement `convolver.rs`**

Python reference: `prototype/paraeq/engine/convolver.py`. Exact scheme: `n_fft = next_pow2(block_size + max_fir_len − 1)`; per-channel FIR spectra precomputed (`rfft(fir_padded_to_n_fft)`); per block per channel: zero-pad block to n_fft → rfft → multiply → irfft (÷n_fft) → add stored overlap (elementwise over the first `n_fft − block` samples? NO — read carefully) → emit first `block` samples → **new overlap = the conv result's tail `[block..]` AFTER adding the previous overlap into the full result** (accumulating-remainder: `full[i] += prev_overlap[i]` for i < prev_overlap.len(), then overlap = full[block..].to_vec()). Channel→FIR map: `fir_idx = min(ch, n_firs−1)`. Per-channel overlap state, one per INPUT channel (n_channels fixed at first process call? The Python allocates per constructor `stereo` flag — in Rust: size state lazily on first `process` by `input.len()`, or preallocate for `firs.len().max(2)` channels in `new`; choose: allocate overlap buffers for up to `input.len()` channels ON FIRST process (allowed: first call may allocate; steady-state must not — document this contract in the struct docs; the realtime engine calls process once during warm-up before going live).

```rust
//! Block overlap-add FFT convolver, block-for-block parity with
//! prototype/paraeq/engine/convolver.py (accumulating-remainder scheme).

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex;
use std::sync::Arc;

pub struct OverlapAddConvolver {
    block_size: usize,
    n_fft: usize,
    fir_spectra: Vec<Vec<Complex<f64>>>,
    /// per-channel accumulated tail; length n_fft - block_size
    overlaps: Vec<Vec<f64>>,
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
    // preallocated scratch
    time_scratch: Vec<f64>,
    spec_scratch: Vec<Complex<f64>>,
    full_scratch: Vec<f64>,
}

impl OverlapAddConvolver {
    pub fn new(firs: Vec<Vec<f64>>, block_size: usize) -> Self {
        assert!(!firs.is_empty());
        let fir_len = firs.iter().map(Vec::len).max().unwrap();
        let n_fft = (block_size + fir_len - 1).next_power_of_two();
        let mut planner = RealFftPlanner::<f64>::new();
        let fwd = planner.plan_fft_forward(n_fft);
        let inv = planner.plan_fft_inverse(n_fft);
        let fir_spectra = firs
            .iter()
            .map(|f| {
                let mut padded = vec![0.0; n_fft];
                padded[..f.len()].copy_from_slice(f);
                let mut spec = fwd.make_output_vec();
                fwd.process(&mut padded, &mut spec).unwrap();
                spec
            })
            .collect();
        let spec_scratch = fwd.make_output_vec();
        Self {
            block_size,
            n_fft,
            fir_spectra,
            overlaps: Vec::new(), // sized on first process()
            fwd,
            inv,
            time_scratch: vec![0.0; n_fft],
            spec_scratch,
            full_scratch: vec![0.0; n_fft],
        }
    }

    /// First call sizes per-channel state (may allocate); steady-state is
    /// allocation-free. Realtime users must warm up with one call off-thread.
    pub fn process(&mut self, input: &[&[f64]], output: &mut [Vec<f64>]) {
        assert_eq!(input.len(), output.len());
        if self.overlaps.len() < input.len() {
            self.overlaps
                .resize_with(input.len(), || vec![0.0; self.n_fft - self.block_size]);
        }
        for (ch, block) in input.iter().enumerate() {
            assert_eq!(block.len(), self.block_size, "block size mismatch");
            let fir_idx = ch.min(self.fir_spectra.len() - 1);
            self.time_scratch[..self.block_size].copy_from_slice(block);
            self.time_scratch[self.block_size..].fill(0.0);
            self.fwd.process(&mut self.time_scratch, &mut self.spec_scratch).unwrap();
            for (s, f) in self.spec_scratch.iter_mut().zip(&self.fir_spectra[fir_idx]) {
                *s *= f;
            }
            self.inv.process(&mut self.spec_scratch, &mut self.full_scratch).unwrap();
            let scale = 1.0 / self.n_fft as f64;
            for v in self.full_scratch.iter_mut() {
                *v *= scale;
            }
            // add previous overlap into the full result
            for (v, o) in self.full_scratch.iter_mut().zip(&self.overlaps[ch]) {
                *v += o;
            }
            // emit first block, store new overlap = tail after accumulation
            output[ch][..self.block_size]
                .copy_from_slice(&self.full_scratch[..self.block_size]);
            self.overlaps[ch].copy_from_slice(&self.full_scratch[self.block_size..]);
        }
    }

    pub fn reset(&mut self) {
        for o in &mut self.overlaps {
            o.fill(0.0);
        }
    }
}
```

(realfft's `ComplexToReal::process` requires a spectrum it may scratch over — the spec_scratch reuse above is exactly that. If realfft 3.5's inverse `process` signature demands `&mut input`, the code already provides it.)

- [ ] **Step 4: Run to verify pass** — 2 passed. Block-by-block parity across 8 blocks proves overlap state carry.

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "feat(engine): overlap-add convolver, block-for-block golden parity incl. mono-broadcast regression"
```

---

### Task 12: engine — IIR cascade processor

**Files:**
- Create: `crates/paraeq-engine/src/iir.rs`, `crates/paraeq-engine/tests/test_iir.rs`
- Modify: `crates/paraeq-engine/src/lib.rs` (`pub mod iir;`)

**Interfaces:**
- Produces: `pub struct IIRProcessor` with
  - `pub fn new() -> IIRProcessor`
  - `pub fn set_sos(&mut self, channel: usize, sos: Vec<[f64; 6]>)` — installs cascade + zero state `zi = [[0.0; 2]; n_sections]`
  - `pub fn process(&mut self, input: &[&[f64]], output: &mut [Vec<f64>])` — channels without a cascade pass through unchanged; state persists across calls
  - `pub fn reset(&mut self)` — zero all state, keep cascades

- [ ] **Step 1: Failing test**

`crates/paraeq-engine/tests/test_iir.rs`:

```rust
mod common;
use common::{assert_allclose, Case};
use paraeq_engine::iir::IIRProcessor;

#[test]
fn cascade_state_carries_across_blocks_matching_oracle() {
    let c = Case::load("iir", "cascade_4_blocks");
    let sos_flat = c.array2("sos"); // [3, 6]
    let sos: Vec<[f64; 6]> = (0..sos_flat.rows)
        .map(|r| {
            let row = &sos_flat.data[r * 6..(r + 1) * 6];
            [row[0], row[1], row[2], row[3], row[4], row[5]]
        })
        .collect();
    let input = c.array2("input");
    let expected = c.array2("output");
    let block = 512usize;
    let mut proc = IIRProcessor::new();
    proc.set_sos(0, sos.clone());
    proc.set_sos(1, sos);
    let (in0, in1) = (input.col(0), input.col(1));
    let (exp0, exp1) = (expected.col(0), expected.col(1));
    let mut out = vec![vec![0.0; block], vec![0.0; block]];
    for b in 0..input.rows / block {
        let r = b * block..(b + 1) * block;
        proc.process(&[&in0[r.clone()], &in1[r.clone()]], &mut out);
        assert_allclose(&out[0], &exp0[r.clone()], 1e-9, 1e-12, &format!("block {b} ch0"));
        assert_allclose(&out[1], &exp1[r.clone()], 1e-9, 1e-12, &format!("block {b} ch1"));
    }
}

#[test]
fn channel_without_sos_passes_through() {
    let mut proc = IIRProcessor::new();
    proc.set_sos(0, vec![[0.5, 0.0, 0.0, 1.0, 0.0, 0.0]]); // pure -6dB gain section
    let x: Vec<f64> = (0..64).map(|i| (i as f64 * 0.1).sin()).collect();
    let mut out = vec![vec![0.0; 64], vec![0.0; 64]];
    proc.process(&[&x, &x], &mut out);
    assert_allclose(&out[0], &x.iter().map(|v| v * 0.5).collect::<Vec<_>>(), 1e-12, 1e-12, "filtered");
    assert_allclose(&out[1], &x, 0.0, 0.0, "passthrough");
}
```

- [ ] **Step 2: Run to verify failure** — module not found.

- [ ] **Step 3: Implement `iir.rs`**

Python reference: `prototype/paraeq/engine/iir_processor.py` (scipy `sosfilt` + persistent `zi`). scipy sosfilt per-section DF2T recurrence with state `z = [z0, z1]`:
`y = b0·x + z0; z0 = b1·x − a1·y + z1; z1 = b2·x − a2·y` — sections chained in row order, state per (channel, section), zero-initialized (NOT sosfilt_zi steady state).

```rust
//! Cascaded-biquad block processor with persistent per-channel state
//! (scipy.signal.sosfilt semantics). Oracle: prototype/paraeq/engine/iir_processor.py

struct ChannelState {
    sos: Vec<[f64; 6]>,
    zi: Vec<[f64; 2]>,
}

#[derive(Default)]
pub struct IIRProcessor {
    channels: Vec<Option<ChannelState>>,
}

impl IIRProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_sos(&mut self, channel: usize, sos: Vec<[f64; 6]>) {
        if self.channels.len() <= channel {
            self.channels.resize_with(channel + 1, || None);
        }
        let zi = vec![[0.0; 2]; sos.len()];
        self.channels[channel] = Some(ChannelState { sos, zi });
    }

    pub fn process(&mut self, input: &[&[f64]], output: &mut [Vec<f64>]) {
        for (ch, block) in input.iter().enumerate() {
            let out = &mut output[ch];
            out.clear();
            out.extend_from_slice(block);
            if let Some(Some(state)) = self.channels.get_mut(ch) {
                for (sec, z) in state.sos.iter().zip(state.zi.iter_mut()) {
                    let [b0, b1, b2, _, a1, a2] = *sec;
                    for v in out.iter_mut() {
                        let x = *v;
                        let y = b0 * x + z[0];
                        z[0] = b1 * x - a1 * y + z[1];
                        z[1] = b2 * x - a2 * y;
                        *v = y;
                    }
                }
            }
        }
    }

    pub fn reset(&mut self) {
        for ch in self.channels.iter_mut().flatten() {
            for z in &mut ch.zi {
                *z = [0.0; 2];
            }
        }
    }
}
```

- [ ] **Step 4: Run to verify pass** — 2 passed (4-block state carry is the load-bearing assertion).

- [ ] **Step 5: Gates + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "feat(engine): sosfilt-semantics IIR cascade processor with persistent state, golden-matched"
```

---

### Task 13: property tests, divergence log, docs, final green

**Files:**
- Create: `crates/paraeq-dsp/tests/test_props.rs`, `crates/paraeq-dsp/DIVERGENCES.md`
- Modify: `docs/CONTEXT.md` (stage-2 status), `CLAUDE.md` (add the missing oracle bootstrap command — carried finding from the foundation review)

- [ ] **Step 1: Property tests**

`crates/paraeq-dsp/tests/test_props.rs`:

```rust
use paraeq_dsp::{biquad, fr, spline::NakSpline};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Designed peaking filters are stable: poles inside the unit circle.
    #[test]
    fn peaking_is_stable(fc in 20.0f64..20000.0, gain in -24.0f64..24.0, q in 0.1f64..20.0) {
        let sos = biquad::peaking(fc, gain, q, 48000.0);
        let (a1, a2) = (sos[4], sos[5]);
        // |poles| < 1  <=>  |a2| < 1 && |a1| < 1 + a2   (real-coefficient biquad)
        prop_assert!(a2.abs() < 1.0 + 1e-12);
        prop_assert!(a1.abs() < 1.0 + a2 + 1e-9);
    }

    /// Smoothing preserves a flat spectrum exactly (any fraction).
    #[test]
    fn smoothing_preserves_flat(level in -60.0f64..20.0, fraction in 1u32..24) {
        let freqs: Vec<f64> = (0..256).map(|i| i as f64 * 48000.0 / 512.0).collect();
        let flat = vec![level; 256];
        let out = fr::fractional_octave_smooth(&flat, &freqs, fraction);
        for v in &out {
            prop_assert!((v - level).abs() < 1e-9);
        }
    }

    /// Spline interpolates its knots.
    #[test]
    fn spline_passes_through_knots(n in 4usize..24, seed in 0u64..1000) {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut next = || { state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (state >> 33) as f64 / (1u64 << 31) as f64 - 1.0 };
        let x: Vec<f64> = (0..n).map(|i| i as f64 + 0.25 * next().abs()).collect();
        let y: Vec<f64> = (0..n).map(|_| 10.0 * next()).collect();
        let s = NakSpline::new(&x, &y).unwrap();
        for (xi, yi) in x.iter().zip(&y) {
            prop_assert!((s.eval(*xi) - yi).abs() < 1e-9);
        }
    }
}

/// OLA convolver output equals direct convolution (cross-crate check lives in
/// paraeq-engine's own tests; here we verify the FFT round-trip primitive the
/// fixtures already pin, so no duplicate — intentionally no OLA prop here).
#[test]
fn fir_design_impulse_is_finite() {
    let taps = paraeq_dsp::fir::design_fir_correction(&[0.0; 128], 256, paraeq_dsp::fir::FirPhase::Minimum);
    assert!(taps.iter().all(|t| t.is_finite()));
}
```

And the OLA-vs-direct property in `crates/paraeq-engine/tests/test_convolver.rs` (append):

```rust
#[test]
fn ola_equals_direct_convolution() {
    // deterministic pseudo-random input, 3 blocks of 128, FIR len 37
    let mut state = 0x9E3779B97F4A7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let fir: Vec<f64> = (0..37).map(|_| next()).collect();
    let input: Vec<f64> = (0..384).map(|_| next()).collect();
    let mut conv = OverlapAddConvolver::new(vec![fir.clone()], 128);
    let mut got = Vec::new();
    let mut out = vec![vec![0.0; 128]];
    for b in 0..3 {
        conv.process(&[&input[b * 128..(b + 1) * 128]], &mut out);
        got.extend_from_slice(&out[0]);
    }
    // direct convolution, first 384 samples
    for (i, g) in got.iter().enumerate() {
        let mut acc = 0.0;
        for (k, f) in fir.iter().enumerate() {
            if i >= k {
                acc += f * input[i - k];
            }
        }
        assert!((g - acc).abs() < 1e-9, "sample {i}: {g} vs {acc}");
    }
}
```

- [ ] **Step 2: Run all new tests** — `cargo test --workspace` — everything green.

- [ ] **Step 3: Write `crates/paraeq-dsp/DIVERGENCES.md`**

```markdown
# Deliberate divergences from the Python oracle

Spec requirement (2026-07-02 design): every place the Rust intentionally
differs from `prototype/paraeq/`, so a red parity test is always actionable.

1. **`design_fir_correction` drops the `freqs` parameter.** The Python accepts
   `freqs` but never uses it (resampling is positional). Rust omits it.
2. **Inverse-FFT normalization is explicit.** numpy divides by n inside
   `ifft/irfft`; rustfft/realfft do not — every inverse in Rust carries `1/n`.
   Numerically identical; structurally different.
3. **`compute_frequency_response` / `deconvolve` are mono.** The Python accepts
   `(samples, channels)` 2-D arrays; Rust callers loop channels explicitly.
4. **`fractional_octave_smooth` window scan is O(n²) by design** to mirror the
   oracle sample-for-sample. Any future O(n) optimization must keep the fixture
   test byte-identical.
5. **Errors are `Result<_, DspError>`** where the Python raises `ValueError`/
   `FileNotFoundError`. Same conditions, different mechanism.
(add entries here as they are discovered during implementation)
```

- [ ] **Step 4: Docs updates**

`docs/CONTEXT.md` — in the Phase 2 "What's Left" entry, append: `Stage 2 (DSP core) complete: all pure-DSP modules golden-matched in crates/paraeq-dsp + engine processors in crates/paraeq-engine (see crates/paraeq-dsp/DIVERGENCES.md). Next: stage 3 — production tap engine in paraeq-coreaudio/paraeq-engine (obligations from docs/spikes/2026-07-tap-spike.md: silence watchdog, ~5s tap-engage tolerance, buffer-size latency tuning, no spike unsafe patterns).`

`CLAUDE.md` — in the Commands section, after the `source .venv/bin/activate` line, add the bootstrap line (carried finding from the foundation review):

```bash
# First-time setup: python3 -m venv .venv && .venv/bin/pip install -e "./prototype[dev,gui]"
```

- [ ] **Step 5: Final whole-workspace gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # all dsp + engine + desktop suites
.venv/bin/pytest prototype/tests -q   # oracle still green (nothing should have touched it)
```

Expected: all green; pytest `128 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/paraeq-dsp crates/paraeq-engine docs/CONTEXT.md CLAUDE.md
git commit -m "test(dsp): property invariants + divergence log; docs: stage-2 status + oracle bootstrap"
```

---

## Completion Checklist (whole plan)

- [ ] Every fixture stage consumed by at least one Rust test: sweep, deconvolution, fr (×6 cases), compensation, targets (×3), fir (×4), biquad, peq, autofit, convolver, iir.
- [ ] All tolerances at spec levels (1e-12 coefficients / 1e-9 FFT / 1e-6 + 0.01 dB min-phase).
- [ ] `cargo test --workspace` + clippy + fmt green; oracle suite untouched and green.
- [ ] DIVERGENCES.md exists and lists every intentional difference discovered.
- [ ] No new dependencies beyond the `biquad` dev-dep; no ndarray/scirs2/fundsp anywhere.
- [ ] Merge per superpowers:finishing-a-development-branch; next plan = stage 3 (production tap engine — carry the spike-findings obligations into its brief).
