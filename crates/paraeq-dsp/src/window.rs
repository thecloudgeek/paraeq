//! Taper functions for `gating.rs`, with independent left/right kinds — the
//! left window rejects pre-peak junk (harmonic products, deconvolution
//! pre-ringing) while the right window sets frequency resolution. Different
//! jobs, independently selectable.
//! Test tier: 2 + 3. Tier 2 pins the four kinds against
//! `scipy.signal.windows.{blackmanharris, hann, tukey}` at n ∈ {8, 9, 64, 4096}
//! to 1e-12 (odd and even — the `n−1` denominator is where off-by-ones live);
//! Tier 3 pins symmetry, `Tukey{0.0} == Rect`, `Tukey{1.0} == Hann`.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`window.rs` — new".
//!
//! STUB — room-dsp/3, lands in Stage 3.
//!
//! Formulas use scipy's `sym=True` convention, denominator `n−1`, matching the
//! existing `fir.rs:92` Hann (`np.hanning`). Do not mix conventions in-crate.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowKind {
    BlackmanHarris,
    Hann,
    Rect,
    Tukey { alpha: f64 },
}

impl WindowKind {
    /// The standard gating compromise: a rectangular gate rings badly in
    /// frequency, a full Hann discards half the gated energy and widens the
    /// effective window.
    pub const TUKEY_DEFAULT: WindowKind = WindowKind::Tukey { alpha: 0.25 };

    /// Half-window taper of length `n`, index 0 at the gate's INNER edge
    /// (nearest the peak, value 1.0) running to the taper's outer end.
    /// `half_taper(0)` is empty, not a panic.
    pub fn half_taper(&self, n: usize) -> Vec<f64> {
        let _ = n;
        unimplemented!("room-dsp/3 — lands in Stage 3")
    }
}

/// Independent left/right half-windows around the IR peak.
#[derive(Clone, Copy, Debug)]
pub struct WindowSpec {
    pub left: WindowKind,
    pub right: WindowKind,
}

impl Default for WindowSpec {
    fn default() -> Self {
        WindowSpec {
            left: WindowKind::TUKEY_DEFAULT,
            right: WindowKind::TUKEY_DEFAULT,
        }
    }
}
