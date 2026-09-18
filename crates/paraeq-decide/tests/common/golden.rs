//! The `fixtures/decide/` harness: the case list, the sidecar hydrator, the
//! freeze digest, and the bless writer.
//!
//! **Why this file exists at all.** `MeasurementBundle` carries its impulse
//! responses INLINE (`ImpulseResponse::samples: Vec<Vec<f64>>`). Against the
//! shipped store window (`crates/paraeq-measure/src/store.rs`: 100 ms before
//! the peak, 1500 ms after) one nine-position room bundle is ~25-30 MB of f64
//! JSON text — not committable, not reviewable, and it would bury the
//! `expected.json` the owner is meant to read. So each IR array goes to a
//! little-endian raw-f64 sidecar in the repo's OWN existing convention
//! (`prototype/tools/generate_fixtures.py`'s `save_case` writes
//! `<name>.<key>.f64` and records `{file, len, shape}` in the JSON envelope;
//! the reader is `crates/paraeq-dsp/tests/common/mod.rs`), and `bundle.json`
//! names them. [`hydrate`] puts them back before deserialization, so what
//! `decide()` sees is a plain `MeasurementBundle` and nothing in `src/` knows
//! this file exists.
//!
//! **Not f32 WAVs**, which would make "the fixture IS a saved profile"
//! literally true (`IrStore` writes `pos<NN>_ch<N>.wav`): that costs a `hound`
//! dev-dependency on a crate whose manifest deliberately lists three, and f32
//! storage would silently round every sample on the way in — so the
//! byte-for-byte freeze would be a freeze over rounded inputs. Flagged in
//! `fixtures/decide/README.md` as the better long-term shape if the owner wants
//! it.

#![allow(dead_code)]

use paraeq_decide::MeasurementBundle;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The eight owner-reviewed cases, **hard-coded on purpose**.
///
/// A dropped directory must fail loudly rather than shrink the reviewed set in
/// silence; the precedent is `crates/paraeq-dsp/tests/fixtures_smoke.rs`, which
/// spells its case list out for exactly this reason. Alphabetical, per the
/// repo's ordering convention.
pub const CASES: [&str; 8] = [
    "bookshelf_clean_room",
    "floorstander_null_one_of_five",
    "in_ear_clean_coupler",
    "over_ear_ears_heq_cal",
    "over_ear_lost_seal_reseat",
    "room_cal_neighbour_outlier",
    "room_clipped_position",
    "room_noisy_snr_boundary",
];

/// The file every case carries besides its IR sidecars. `expected.json` is NOT
/// here: it is written by the bless (B10), which is owner-gated, and the
/// workspace must stay green before it runs.
pub const REQUIRED_FILES: [&str; 2] = ["bundle.json", "notes.md"];

/// The canonical bless gate. Named for the crate so a second fixture set can
/// never be re-blessed by the same switch.
pub const BLESS_VAR: &str = "PARAEQ_BLESS_DECIDE";

/// Accepted alias, because the plan text and the build order each name one of
/// the two. Both mean the same thing and both are off in CI.
pub const BLESS_ALIAS: &str = "PARAEQ_BLESS";

/// Redirects the bless to a directory OUTSIDE the repo, for a dry run that
/// proves the mechanism without committing a freeze. Setting it implies
/// [`BlessMode::To`].
pub const BLESS_OUT_VAR: &str = "PARAEQ_BLESS_DECIDE_OUT";

/// `fixtures/decide/`, resolved the same way every other fixture reader in this
/// workspace resolves `fixtures/`.
pub fn decide_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/decide")
}

pub fn case_dir(case: &str) -> PathBuf {
    decide_dir().join(case)
}

pub fn manifest_path() -> PathBuf {
    decide_dir().join("manifest.json")
}

// ---------------------------------------------------------------------------
// JSON style
// ---------------------------------------------------------------------------

/// The one JSON style every Rust-authored file under `fixtures/decide/` uses:
/// `serde_json::to_string_pretty` (two-space indent, struct field order) plus a
/// trailing newline.
///
/// It deliberately differs from the Python fixtures' `indent=1, sort_keys=True`
/// — that is `generate_fixtures.py`'s style and these files are not its output.
/// Pinned by `expected_json_is_canonical`, so the choice cannot drift into a
/// silent re-format that reads as a policy diff.
pub fn canonical_json<T: Serialize>(value: &T) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("plain derived data serializes");
    text.push('\n');
    text
}

// ---------------------------------------------------------------------------
// The digest
// ---------------------------------------------------------------------------

/// FNV-1a 64, inline, because the workspace has no hashing crate and CLAUDE.md
/// carries a forbidden-dependency list.
///
/// This is a **drift detector against accidents**, not a security boundary: it
/// catches a hand-edited `bundle.json`, a truncated sidecar or a stale
/// regeneration, which is all the freeze needs. If the owner ever wants a real
/// digest, `sha2` is the standard answer and is a one-line dev-dependency —
/// flagged in `fixtures/decide/README.md`, not decided here.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One manifest row: the digest and the byte length, because a length mismatch
/// is the failure a reader can act on without recomputing anything.
pub fn digest_entry(bytes: &[u8]) -> Value {
    serde_json::json!({
        "bytes": bytes.len(),
        "fnv1a64": format!("0x{:016x}", fnv1a64(bytes)),
    })
}

/// Every file inside one case directory, as paths relative to that directory,
/// sorted. `expected.json` is included when it exists — the manifest test is
/// what decides whether it is allowed to be unlisted.
pub fn case_files(case: &str) -> Vec<String> {
    let root = case_dir(case);
    let mut out = Vec::new();
    collect_files(&root, &root, &mut out);
    out.sort();
    out
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("child of the case directory")
                .to_string_lossy()
                // The manifest is a committed wire format; it spells paths with
                // forward slashes on every platform.
                .replace('\\', "/");
            out.push(relative);
        }
    }
}

// ---------------------------------------------------------------------------
// Sidecars
// ---------------------------------------------------------------------------

/// Read a little-endian raw-f64 sidecar. Same bytes, same order, same
/// assertion as `crates/paraeq-dsp/tests/common/mod.rs::Case::array`.
pub fn read_f64_le(path: &Path, declared_len: usize) -> Vec<f64> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        bytes.len(),
        declared_len * 8,
        "{}: declared len {declared_len} against {} bytes",
        path.display(),
        bytes.len()
    );
    bytes
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().expect("eight bytes")))
        .collect()
}

pub fn write_f64_le(path: &Path, samples: &[f64]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{}: {e}", parent.display()));
    }
    let mut bytes = Vec::with_capacity(samples.len() * 8);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

/// Replace one `ir.samples` reference block with the arrays it names.
///
/// The block is an ARRAY of `{file, len, shape}` envelopes, one per channel —
/// the repo's own single-array convention, repeated per row, so the channel
/// count stays visible in `bundle.json` and the sidecar names stay
/// `ir/pos<NN>_ch<N>.f64` as the plan's file list spells them.
fn hydrate_ir(ir: &mut Value, dir: &Path) {
    let refs = ir["samples"]
        .as_array()
        .expect("ir.samples is an array of sidecar references")
        .clone();
    let mut channels = Vec::with_capacity(refs.len());
    for reference in &refs {
        let file = reference["file"]
            .as_str()
            .expect("a sidecar reference carries `file`");
        let len = reference["len"]
            .as_u64()
            .expect("a sidecar reference carries `len`") as usize;
        let shape: Vec<u64> = reference["shape"]
            .as_array()
            .expect("a sidecar reference carries `shape`")
            .iter()
            .map(|v| v.as_u64().expect("shape entries are counts"))
            .collect();
        assert_eq!(
            shape,
            vec![len as u64],
            "{file}: one channel per sidecar, so shape is [len]"
        );
        channels.push(Value::from(read_f64_le(&dir.join(file), len)));
    }
    ir["samples"] = Value::Array(channels);
}

/// The inverse: write each channel to its sidecar and leave the reference.
fn dehydrate_ir(ir: &mut Value, dir: &Path, stem: &str) {
    let channels = ir["samples"]
        .as_array()
        .expect("ir.samples is an array of channels")
        .clone();
    let mut refs = Vec::with_capacity(channels.len());
    for (channel, samples) in channels.iter().enumerate() {
        let values: Vec<f64> = samples
            .as_array()
            .expect("one channel is an array of samples")
            .iter()
            .map(|v| v.as_f64().expect("samples are finite f64"))
            .collect();
        let file = format!("ir/{stem}_ch{channel}.f64");
        write_f64_le(&dir.join(&file), &values);
        refs.push(serde_json::json!({
            "file": file,
            "len": values.len(),
            "shape": [values.len()],
        }));
    }
    ir["samples"] = Value::Array(refs);
}

/// Hydrate every IR in one `bundle.json` envelope, in place.
pub fn hydrate(envelope: &mut Value, dir: &Path) {
    let positions = envelope["positions"]
        .as_array_mut()
        .expect("a bundle carries `positions`");
    for position in positions.iter_mut() {
        hydrate_ir(&mut position["ir"], dir);
    }
    if envelope["verification"].is_object() {
        hydrate_ir(&mut envelope["verification"]["ir"], dir);
    }
}

/// Dehydrate every IR in a serialized bundle, writing the sidecars under `dir`.
pub fn dehydrate(envelope: &mut Value, dir: &Path) {
    let positions = envelope["positions"]
        .as_array_mut()
        .expect("a bundle carries `positions`");
    for (index, position) in positions.iter_mut().enumerate() {
        dehydrate_ir(&mut position["ir"], dir, &format!("pos{index:02}"));
    }
    if envelope["verification"].is_object() {
        dehydrate_ir(&mut envelope["verification"]["ir"], dir, "verify");
    }
}

// ---------------------------------------------------------------------------
// One case
// ---------------------------------------------------------------------------

pub struct GoldenCase {
    pub dir: PathBuf,
    /// The `bundle.json` envelope, IRs still as `{file, len, shape}`.
    pub envelope: Value,
    pub name: &'static str,
}

impl GoldenCase {
    pub fn load(name: &'static str) -> GoldenCase {
        let dir = case_dir(name);
        let path = dir.join("bundle.json");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        GoldenCase {
            envelope: serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            dir,
            name,
        }
    }

    /// The hydrated bundle `decide()` is run over.
    pub fn bundle(&self) -> MeasurementBundle {
        let mut envelope = self.envelope.clone();
        hydrate(&mut envelope, &self.dir);
        serde_json::from_value(envelope)
            .unwrap_or_else(|e| panic!("{}/bundle.json is not a MeasurementBundle: {e}", self.name))
    }

    pub fn expected_path(&self) -> PathBuf {
        self.dir.join("expected.json")
    }

    /// `None` before B10's bless. Every assertion over it must say so and pass.
    pub fn expected_text(&self) -> Option<String> {
        std::fs::read_to_string(self.expected_path()).ok()
    }
}

// ---------------------------------------------------------------------------
// The bless
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum BlessMode {
    /// The default, and the only mode CI can reach.
    Off,
    /// Rewrite `fixtures/decide/<case>/expected.json` and the manifest.
    Repo,
    /// Write `<dir>/<case>/expected.json` and nothing else. A dry run.
    To(PathBuf),
}

/// The bless gate, resolved from an arbitrary environment so a test can drive
/// it without mutating the process's own (which is racy across test threads).
///
/// A variable that is set but empty, or set to `0`, is OFF: an inherited empty
/// value must not re-bless a freeze.
pub fn bless_mode(lookup: &dyn Fn(&str) -> Option<OsString>) -> BlessMode {
    let truthy = |name: &str| {
        lookup(name)
            .map(|v| {
                let v = v.to_string_lossy().into_owned();
                !v.is_empty() && v != "0"
            })
            .unwrap_or(false)
    };
    if let Some(dir) = lookup(BLESS_OUT_VAR) {
        if !dir.is_empty() {
            return BlessMode::To(PathBuf::from(dir));
        }
    }
    if truthy(BLESS_VAR) || truthy(BLESS_ALIAS) {
        return BlessMode::Repo;
    }
    BlessMode::Off
}

pub fn bless_mode_from_env() -> BlessMode {
    bless_mode(&|name: &str| std::env::var_os(name))
}

/// Where this bless writes `<case>/expected.json`, or `None` when it is off.
pub fn bless_root(mode: &BlessMode) -> Option<PathBuf> {
    match mode {
        BlessMode::Off => None,
        BlessMode::Repo => Some(decide_dir()),
        BlessMode::To(dir) => Some(dir.clone()),
    }
}

/// Rebuild `manifest.json` from what is on disk.
///
/// The bless rewrites `expected.json` **and** the manifest in one pass, so the
/// PR shows the policy change and the re-freeze together rather than as two
/// commits a reviewer has to correlate. `frozen_at` and `reviewed_by` are the
/// owner's to fill at the bless; the generator leaves them null.
pub fn build_manifest(frozen_at: Option<&str>, reviewed_by: Option<&str>) -> Value {
    let mut cases = serde_json::Map::new();
    for case in CASES {
        let dir = case_dir(case);
        let mut files = BTreeMap::new();
        for relative in case_files(case) {
            let bytes = std::fs::read(dir.join(&relative))
                .unwrap_or_else(|e| panic!("{case}/{relative}: {e}"));
            files.insert(relative, digest_entry(&bytes));
        }
        cases.insert(
            case.to_string(),
            Value::Object(files.into_iter().collect::<serde_json::Map<_, _>>()),
        );
    }
    serde_json::json!({
        "algorithm": "fnv1a64",
        "cases": Value::Object(cases),
        "frozen_at": frozen_at,
        "reviewed_by": reviewed_by,
        "schema_version": 1,
    })
}

pub fn read_manifest() -> Value {
    let path = manifest_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
