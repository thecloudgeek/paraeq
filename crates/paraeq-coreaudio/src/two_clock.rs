//! Re-export of [`paraeq_dsp::two_clock`], which is where this module now
//! lives (Stage 6, build plan B12).
//!
//! The analysis moved down to the pure-math crate because `paraeq-measure`
//! needs marker recovery for the verification pass while the MS-1 dependency
//! gate in `.github/workflows/ci.yml` forbids that crate from depending on this
//! one. The module's own header had always invited the move — "No FFI, no
//! unsafe — it lives in this crate only because the `#[ignore]`d hardware
//! harness (tests/test_measure_hardware.rs) that feeds it real captures lives
//! here; it can migrate to a policy crate later."
//!
//! This shim exists so `crates/paraeq-coreaudio/tests/test_measure_hardware.rs`
//! is untouched by the move: it names `paraeq_coreaudio::two_clock::{self,
//! SkewEstimate}` and keeps naming it. The alternative — repointing the harness
//! at `paraeq_dsp::two_clock` — would have pulled a rig-test file into a DSP
//! refactor for no gain.

pub use paraeq_dsp::two_clock::*;
