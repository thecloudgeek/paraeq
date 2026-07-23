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
//! Formulas use scipy's `sym=True` convention, denominator `n−1`, matching the
//! existing `fir.rs:92` Hann (`np.hanning`). Do not mix conventions in-crate.

use std::f64::consts::PI;

/// Blackman-Harris 4-term coefficients. Their f64 sum is exactly 1.0, so the
/// window's center — and therefore `half_taper`'s inner edge — is exactly 1.0.
const BH: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];

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

    /// Full symmetric window of length `n` (scipy `sym=True`).
    /// `full(1) == [1.0]`; `full(0)` is empty, not a panic.
    ///
    /// The second half is mirrored from the first, so `w[i] == w[n−1−i]` holds
    /// EXACTLY by construction (Tier-3 contract) while staying ~1e-16 absolute
    /// from scipy's direct evaluation, well inside the Tier-2 1e-12 budget.
    ///
    /// `Tukey` follows scipy's edge handling: `alpha <= 0` degenerates to
    /// `Rect`, `alpha >= 1` to `Hann` — scipy's own identities, which the
    /// Tier-3 tests pin bitwise.
    pub fn full(&self, n: usize) -> Vec<f64> {
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![1.0];
        }
        let denom = (n - 1) as f64;
        match *self {
            WindowKind::BlackmanHarris => symmetric(n, |i| {
                let x = 2.0 * PI * i as f64 / denom;
                BH[0] - BH[1] * x.cos() + BH[2] * (2.0 * x).cos() - BH[3] * (3.0 * x).cos()
            }),
            WindowKind::Hann => symmetric(n, |i| 0.5 - 0.5 * (2.0 * PI * i as f64 / denom).cos()),
            WindowKind::Rect => vec![1.0; n],
            WindowKind::Tukey { alpha } => {
                if alpha <= 0.0 {
                    WindowKind::Rect.full(n)
                } else if alpha >= 1.0 {
                    WindowKind::Hann.full(n)
                } else {
                    // scipy: cosine taper over indices 0..=width, flat middle;
                    // the right taper falls out of the mirror.
                    let width = (alpha * denom / 2.0).floor() as usize;
                    symmetric(n, |i| {
                        if i <= width {
                            0.5 * (1.0 + (PI * (-1.0 + 2.0 * i as f64 / alpha / denom)).cos())
                        } else {
                            1.0
                        }
                    })
                }
            }
        }
    }

    /// Half-window taper of length `n`, index 0 at the gate's INNER edge
    /// (nearest the peak, value 1.0) running to the taper's outer end.
    /// `half_taper(0)` is empty, not a panic.
    ///
    /// Derivation: the last `n` samples of the full symmetric window of length
    /// `2n−1`, so index 0 is the full window's center sample. Every kind's
    /// center is exactly 1.0 (the cosine sums land on `cos(kπ) = ±1` and the
    /// Blackman-Harris coefficients sum to 1.0 in f64), which makes the
    /// inner-edge contract exact, not approximate.
    pub fn half_taper(&self, n: usize) -> Vec<f64> {
        if n == 0 {
            return Vec::new();
        }
        let mut w = self.full(2 * n - 1);
        w.drain(..n - 1);
        w
    }
}

/// Evaluate `f` on the first ⌈n/2⌉ indices and mirror the rest.
fn symmetric(n: usize, f: impl Fn(usize) -> f64) -> Vec<f64> {
    let mut w = vec![0.0; n];
    let half = n.div_ceil(2);
    for (i, v) in w.iter_mut().enumerate().take(half) {
        *v = f(i);
    }
    for i in half..n {
        w[i] = w[n - 1 - i];
    }
    w
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
