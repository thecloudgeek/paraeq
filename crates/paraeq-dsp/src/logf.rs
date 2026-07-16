//! Log-frequency grid + resampling — the shared axis FDW, smoothing, splice,
//! targets and authority all operate on.
//! Test tier: 2 + 3. Tier 2 pins `resample_db_to_log_grid` (Prefilter::None)
//! against `np.interp` on `np.logspace` at 1e-12; Tier 3 pins the grid's closed
//! form and the anti-comb prefilter's reason to exist (aliased vs bounded
//! ripple on a synthetic comb).
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`logf.rs` — new".
//!
//! STUB — room-dsp/2, lands in Stage 3. Signatures are the design contract;
//! bodies are `unimplemented!`. The accessors below are plumbing, not the
//! algorithm: grid construction, the `N` formula, the prefilter and both
//! resamplers are what Stage 3 must write.

use crate::{Complex, DspError};

/// `f_i = f_min · 2^(i/ppo)` for `i ∈ [0, N)`, `N = floor(ppo·log2(f_max/f_min)) + 1`.
///
/// The last bin falls short of `f_max` by less than one bin spacing — the grid is
/// the largest set of `ppo`-spaced points that does not exceed `f_max`. This is
/// deliberate; it is not an off-by-one.
pub struct LogGrid {
    freqs: Vec<f64>,
    f_min: f64,
    f_max: f64,
    points_per_octave: u32,
}

impl LogGrid {
    pub const DEFAULT_PPO: u32 = 96;

    pub fn new(f_min: f64, f_max: f64, points_per_octave: u32) -> Result<Self, DspError> {
        let _ = (f_min, f_max, points_per_octave);
        unimplemented!("room-dsp/2 — lands in Stage 3")
    }

    /// 20 Hz .. 20 kHz @ 96 ppo -> 957 points.
    pub fn standard() -> Self {
        unimplemented!("room-dsp/2 — lands in Stage 3")
    }

    pub fn freqs(&self) -> &[f64] {
        &self.freqs
    }

    pub fn len(&self) -> usize {
        self.freqs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.freqs.is_empty()
    }

    pub fn f_min(&self) -> f64 {
        self.f_min
    }

    pub fn f_max(&self) -> f64 {
        self.f_max
    }

    pub fn points_per_octave(&self) -> u32 {
        self.points_per_octave
    }

    /// Octave offset of bin `i` from `f_min`: `i / ppo`. The UNIFORM axis fdw.rs
    /// and fr.rs convolve on — this is why a log grid buys O(N log N).
    pub fn octave_axis(&self) -> Vec<f64> {
        unimplemented!("room-dsp/2 — lands in Stage 3")
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Prefilter {
    None,
    /// Average over each bin's 1/`fraction`-octave neighbourhood before
    /// decimating. Default 48 (REW's finest variable-smoothing setting, so it
    /// cannot erase anything downstream smoothing would keep). This is a
    /// lowpass-before-downsample, NOT smoothing.
    AntiComb {
        fraction: u32,
    },
}

/// Complex resample — the ONLY correct input to `fdw::apply_fdw`.
///
/// The caller must derotate (remove the bulk delay) BEFORE calling: at 20 kHz a
/// 50 ms delay winds ~1000 full turns and no log grid could sample it.
pub fn resample_complex_to_log_grid(
    freqs_linear: &[f64],
    spectrum: &[Complex<f64>],
    grid: &LogGrid,
    prefilter: Prefilter,
) -> Result<Vec<Complex<f64>>, DspError> {
    let _ = (freqs_linear, spectrum, grid, prefilter);
    unimplemented!("room-dsp/2 — lands in Stage 3")
}

/// Magnitude resample — for display and dB-domain operations only. Never feed
/// this to `fdw::apply_fdw`; smoothing `|H|` is a different, wrong operation.
pub fn resample_db_to_log_grid(
    freqs_linear: &[f64],
    magnitude_db: &[f64],
    grid: &LogGrid,
    prefilter: Prefilter,
) -> Result<Vec<f64>, DspError> {
    let _ = (freqs_linear, magnitude_db, grid, prefilter);
    unimplemented!("room-dsp/2 — lands in Stage 3")
}
