//! Two-window splice (Klippel AN39). **FALLBACK ONLY.**
//! Test tier: 3 — analytic physics.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`splice.rs` — new".
//!
//! STUB — room-dsp/6, lands in Stage 4 *if it lands at all*: `fdw.rs` subsumes
//! this and tracks perception better (Toole's three-zone model has the ear
//! integrating the first ~50 ms into timbre between 200 Hz and 1 kHz, which a
//! cycles-based window follows naturally — 75 ms at 200 Hz, 15 ms at 1 kHz — and
//! a two-window splice cannot). Build `fdw.rs` first: the room-dsp spec's
//! asymmetry error bound (full brute-force asymmetric-window FDW vs fixed
//! pre-gate + symmetric fast FDW, agreeing within 0.1 dB) is what decides
//! whether this module is ever built. That test is NOT yet written — it needs an
//! asymmetric brute-force entry point the spec's API does not define — and it is
//! Stage 4's first FDW task, ahead of any work here.

use crate::{logf::LogGrid, DspError};

#[derive(Clone, Copy, Debug)]
pub struct SpliceSpec {
    pub overlap_lo_hz: f64,
    pub overlap_hi_hz: f64,
}

#[derive(Clone, Debug)]
pub struct SpliceReport {
    /// `mean_dB(low) − mean_dB(high)` over the overlap, added to `high` before
    /// blending.
    pub level_offset_db: f64,
    /// 1-octave RMS error between the spliced curve and each source inside the
    /// overlap — the auto-tune objective a caller sweeps to place the crossover.
    pub rms_error_1oct_db: f64,
}

/// Raised-cosine blend weight at normalized overlap position `u ∈ [0, 1]`
/// (linear in OCTAVES across the overlap): `0.5·(1 − cos(π·u))`.
///
/// Exposed because the weights are otherwise unobservable: `splice` always level
/// matches, which removes the very difference between `low` and `high` that the
/// weights would reveal. `wt(0) == 0` and `wt(1) == 1` exactly.
pub fn blend_weight(u: f64) -> f64 {
    let _ = u;
    unimplemented!("room-dsp/6 — lands in Stage 4 (only if FDW misses)")
}

/// 1. **Level match — MANDATORY, not cosmetic.**
///    `offset = mean_dB(low) − mean_dB(high)` over the overlap; `high += offset`.
///    An unmatched splice produced a 13.28 dB step in testing.
/// 2. **Raised-cosine blend** over the overlap in log-f:
///    `out = (1−wt)·low + wt·high_matched`.
///
/// `low` is the long-window (fine LF) curve, `high` the short-window (gated HF)
/// curve; both in dB on `grid`.
pub fn splice(
    low: &[f64],
    high: &[f64],
    grid: &LogGrid,
    spec: &SpliceSpec,
) -> Result<(Vec<f64>, SpliceReport), DspError> {
    let _ = (low, high, grid, spec);
    unimplemented!("room-dsp/6 — lands in Stage 4 (only if FDW misses)")
}
