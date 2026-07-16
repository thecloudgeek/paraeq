//! Stimulus level policy and measurement orchestration.
//!
//! This crate exists because the policy had no home in the tree and the place
//! it obviously belonged was the one place it must not go. `paraeq-dsp`'s
//! `generate_sweep` returns an unscaled sine (peak 1.0, **-3.01 dBFS RMS** —
//! REW's absolute maximum) and is correct to: it is bit-exact against an
//! oracle that is also unscaled, and an output-level hearing-safety constant is
//! product policy, not math. The prototype's `SWEEP_AMPLITUDE = 0.5` lives one
//! layer up in a PyQt playback worker that was never ported. Nothing was
//! dropped and nothing ships hot — `generate_sweep` has zero non-test callers.
//! The exposure is the *first* playback wiring, which is why MS-1/MS-2 land
//! before any playback path exists.
//!
//! The load-bearing invariant, stated here so it is never re-litigated:
//! **whenever tap self-exclusion succeeds, the sweep ParaEQ plays from its own
//! process never passes through `RealtimeChain` and is therefore subject to
//! neither the trim gain nor either of its two ±1.0 clamp sites.** The one
//! signal in the product that can injure a person is the one signal the
//! engine's safety clamps never see. That exclusion is the design goal, not an
//! obstacle — so this crate's level control is the sole protection on the
//! stimulus path, and gets the scrutiny a safety interlock gets.
//!
//! Test tier: **Tier 3 (analytic)** — output level is policy, not DSP, and it
//! has no oracle. The prototype's one playback constant is a guess about an
//! unknown chain, and porting it would launder that guess into a fixture. What
//! is pinned instead: the caps table field-by-field, every refusal edge of
//! [`SweepLevel::new`], and a `trybuild` compile-fail proof that no other type
//! reaches a [`StimulusSink`].
//!
//! Scaffold scope (MS-1/MS-2). The level ladder itself (floor → pilot → solve →
//! envelope → ≤6 dB rungs), stimulus assembly, the session state machine with
//! its RAII volume restore, and the `paraeq-coreaudio` impls of the seam are
//! later stages; they are the callers this crate's types were written to
//! constrain.

#![forbid(unsafe_code)]

pub mod level;
pub mod seam;

pub use level::{caps_for, LevelError, SweepLevel, TransducerCaps, ABSOLUTE_MAX_DBFS_RMS};
/// Re-exported: the enum lives in `paraeq-dsp` because that is the only crate
/// every consumer may depend on. Its semantics are `paraeq-decide`'s; this
/// crate uses it as the key into the caps table and as `SweepLevel::new`'s
/// second argument.
pub use paraeq_dsp::targets::TransducerClass;
pub use seam::{CaptureSource, StimulusSink, StreamFormat};

/// Error type shared by this crate's entry points.
///
/// Distinct from [`level::LevelError`], which is the sole constructor's
/// refusal set and is deliberately `Copy + PartialEq` so a refusal can be
/// asserted on and recorded in the session log verbatim.
#[derive(Debug, thiserror::Error)]
pub enum MeasureError {
    #[error("capture source failed: {0}")]
    Capture(String),
    #[error("invalid level: {0}")]
    Level(#[from] LevelError),
    #[error("stimulus sink failed: {0}")]
    Sink(String),
}
