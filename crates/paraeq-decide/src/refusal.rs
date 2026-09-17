//! The refusal and sanity checks. **B7a ships an empty pass; B7c fills it.**
//!
//! "Refusal is what earns auto mode the right to hide everything. Each check
//! produces a typed [`Diagnostic`]; the front-ends differ only in how much of it
//! they render" (§ Refusal and Sanity Checks). Twenty table rows plus four codes
//! that come from prose — `CalHasTargetBakedIn`, `CoherentAveragingRejected`,
//! `FewPositions`, `SelfExclusionUnavailable` — and every one of them is B7c's.
//!
//! The empty pass is deliberate and is not a stub that might be forgotten: it is
//! what makes `decide()`'s two refusal invariants — `verdict == Refuse ⟺
//! correction.is_none()` and `verdict == Refuse ⟺ any diagnostic has
//! `Severity::Refuse`" — hold VACUOUSLY today and non-vacuously the moment B7c
//! returns its first row. `tests/test_invariants.rs` asserts both now, so the
//! first real check lands inside a guard rail rather than next to one.
//!
//! Three things B7c must carry in from the rulings, recorded here so they are
//! not rediscovered:
//!
//! - **§ D-M — the thresholds are this crate's, not the capture layer's.**
//!   `capture::CLIP_THRESHOLD = 1.0` and `ladder::NOISE_FLOOR_MAX_DBFS = −60.0`
//!   fire DURING a capture and refuse to emit; the decision table's numbers
//!   (0.997 with a 30 % block fraction, −0.3 dBFS, −24 dBFS, 1.5 dB) fire on the
//!   assembled bundle AFTERWARDS. They are different gates in different layers
//!   and must not be reconciled into one number. They belong here, as named
//!   constants, with the layering stated — exactly as `paraeq-measure` documents
//!   its own.
//! - **§ D-V — verification INVERTS MS-6.** The verification pass does not
//!   re-check `self_excluded`: the tap MUST see the helper. `false` on a
//!   measurement (not verification) capture is the Refuse. Write the inversion
//!   down at the check, or someone copies MS-6 into the wrong place.
//! - **§ D-Q — `TwoClock` is conditional now.** It fires as a `Warn` only when
//!   `capture.clock_skew_ppm` is `None`; `Some(ppm)` means the estimate was
//!   formed and applied, and the ppm rides on `Diagnostic::value` instead.

use crate::analysis::AnalysisProducts;
use crate::bundle::MeasurementBundle;
use crate::decisions::Decisions;
use crate::outcome::Diagnostic;

/// Every refusal and warning the bundle earns, in a stable order.
///
/// Takes the decided values as well as the bundle because most rows are graded
/// against a decision rather than against a raw input — the SNR rows against
/// `correction_range`, the variance row against `transition_hz` and
/// `low_corner_hz`, the position-count rows against `positions_default`.
pub(crate) fn diagnostics(
    bundle: &MeasurementBundle,
    decisions: &Decisions,
    analysis: &AnalysisProducts,
) -> Vec<Diagnostic> {
    let _ = (bundle, decisions, analysis);
    Vec::new()
}
