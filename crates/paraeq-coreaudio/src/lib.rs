//! All unsafe CoreAudio FFI lives here — the ONLY macOS-specific crate
//! (spec constraint). Tap lifecycle, devices, listeners land in stage 3.

#![warn(clippy::undocumented_unsafe_blocks)]

pub mod backend;
pub mod devices;
pub mod error;
pub mod ioproc;
pub mod listeners;
pub mod measure_aggregate;
pub mod properties;
pub mod render;
pub mod tap;
pub mod two_clock;
pub mod volume;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The verification-helper re-export surface.
///
/// `crates/paraeq-stimulus` is the helper child process. It may depend on this
/// crate and must not depend on `paraeq-measure` (measurement-suite's helper
/// rule and decision record D-I; this crate's own unconditional
/// `paraeq-measure` dependency makes that rule untestable as a `cargo tree`
/// property, which is an open owner question — the child's MANIFEST honours it
/// to the letter).
///
/// So every `paraeq-measure` item the child names is re-exported HERE, and the
/// child names it as `paraeq_coreaudio::...`. This list is exhaustive by
/// construction: the child carries no `paraeq-measure` dependency line, so any
/// path into that crate is a build error, and CI asserts both. Adding a name to
/// the child means adding an item here. (The child's *doc comments* do name
/// `paraeq-measure` FILES, deliberately — that pointer is the anti-drift
/// device, and the CI gate is a dependency gate, not a prose gate.)
///
/// Each item has exactly ONE definition, in `paraeq-measure`, so parent and
/// child cannot drift:
///
/// | item | definition |
/// |---|---|
/// | `ABSOLUTE_MAX_DBFS_RMS` | `crates/paraeq-measure/src/level.rs` (= -3.0) |
/// | `ABORT_RAMP_MS` | `crates/paraeq-measure/src/session.rs` (= 5.0) |
/// | `abort_envelope(ramp_len, out_len) -> Vec<f64>` | `crates/paraeq-measure/src/ramp.rs` — TWO arguments; the second is the padded length |
/// | `abort_ramp_len` | `crates/paraeq-measure/src/ramp.rs` |
/// | `MeasureError` | `crates/paraeq-measure/src/lib.rs` |
/// | `RenderSink` | `crates/paraeq-measure/src/seam.rs` |
/// | `StreamFormat` | `crates/paraeq-measure/src/seam.rs` |
pub use paraeq_measure::{
    abort_envelope, abort_ramp_len, MeasureError, RenderSink, StreamFormat, ABORT_RAMP_MS,
    ABSOLUTE_MAX_DBFS_RMS,
};
