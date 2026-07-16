//! Frequency-dependent windowing: fine resolution in the bass (where modes are
//! real, correctable and need detail) and quasi-anechoic gating in the treble
//! (where reflections are position-specific junk) — from one parameter.
//! Test tier: 3 — analytic physics, with `apply_fdw_bruteforce` as the in-crate
//! oracle. Write the brute force first; ship the fast path against it.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`fdw.rs` — new".
//!
//! STUB — room-dsp/5, lands in Stage 4 (the plan sequences fdw.rs into Stage 4;
//! `tests/test_fdw.rs` is pre-written and `#[ignore]`d until then).
//!
//! # The identity, and one CORRECTION to the spec
//!
//! A Gaussian time window of half-amplitude width (FWHM) `n_c/f` centred on the
//! peak is EXACTLY a Gaussian smoothing of the complex spectrum in LINEAR
//! frequency with `σ_f/f = √(2 ln 2)/(π·n_c)`. That much is an exact Fourier
//! pair, and it is what the spec's "< 2e-15 dB" verification measured. For an
//! impulse train it is exact analytically:
//!
//! ```text
//!   window:  Σ_k a_k · exp(−t_k²/(2σ_t²)) · e^{−j2πf t_k},   σ_t = 1/(2πσ_f)
//!   smooth:  Σ_k a_k · exp(−2π²σ_f² t_k²) · e^{−j2πf t_k}      ← identical
//! ```
//!
//! The spec then maps that kernel to octave coordinates `u = log2(f/f_min)` and
//! asserts it becomes a **Gaussian in `u`** of constant `σ_u = (σ_f/f)/ln2`.
//! **That step is a first-order approximation and it is false at the spec's own
//! tolerance.** Measured against the brute force it errs by 2.5e-1 dB at
//! n_c = 3, 5.6e-2 dB at n_c = 15 (REW's default) — and the error does NOT
//! shrink with `ppo`, because it is a kernel-shape error, not a sampling error.
//!
//! The O(N log N) architecture survives intact, because the EXACT kernel is
//! still shift-invariant in `u`. Change variables (`ν = f_min·2^u`,
//! `dν = ν·ln2·du`) with `c = σ_f/f`:
//!
//! ```text
//!   S(u₀) = ∫ H(u₀+v) · k(v) dv        (a CORRELATION — see the note below)
//!   k(v)  ∝ 2^v · exp( −(2^v − 1)² / (2c²) )
//! ```
//!
//! `k` is independent of `u₀` ⇒ shift-invariant ⇒ FFT-convolvable, O(N log N).
//! It is skewed, not Gaussian: a Gaussian in linear `f` of width `c·f` reaches
//! down to `f(1 − kc)` and up to `f(1 + kc)`, which in octaves is a long lower
//! tail and a short upper one. With this kernel, fast-vs-brute lands at
//! **≤ 1e-11 dB** across n_c ∈ {3, 5, 15, 30, 61} — vindicating the spec's
//! intent, at a tolerance ~4 orders looser than its stated 2e-15 dB.
//!
//! **Orientation matters.** `S(u₀) = ∫ H(u₀+v)k(v)dv` is a correlation. A
//! convolution routine flips its kernel, so it must be handed `k(−v)`. A
//! Gaussian hides this; `k` does not. Getting it backwards costs ~4.5e-1 dB.
//!
//! # Two guards, one of which the spec is missing
//!
//! 1. **Kernel sampling** (spec has this): require `σ_bins = σ_u·ppo ≥ 1.0`, i.e.
//!    `n_c ≤ ppo·√(2 ln 2)/(π·ln 2) = 0.5407·ppo`. At ppo = 96 that caps
//!    n_c ≤ 51. `apply_fdw` errors when `σ_bins < 1.0`, naming the minimum `ppo`.
//! 2. **Grid Nyquist vs. delay spread** (spec is MISSING this, and it bites):
//!    a path at delay `t` makes `H(ν)` oscillate with period `1/t`. The log
//!    grid's coarsest spacing is at `f_max`: `Δν = f_max·(2^(1/ppo) − 1)`.
//!    Representing the IR needs `Δν < 1/(2·t_max)`. At f_max = 20 kHz, ppo = 192,
//!    `Δν = 72.3 Hz` ⇒ `t_max < 6.9 ms`. An 11 ms reflection aliases and the
//!    fast path degrades to ~1.7e-9 dB even with the exact kernel. This is
//!    exactly what `logf::Prefilter::AntiComb` exists to bound in product code —
//!    but the prefilter is a lowpass, so it changes the answer and cannot be
//!    engaged in the fast-vs-brute test. `tests/test_fdw.rs` therefore uses a
//!    4 ms delay spread, which ppo = 192 represents without aliasing.

use crate::{gating::ImpulseResponse, logf::LogGrid, Complex, DspError};

/// Independent pre/post cycle counts (Acourate's convention). A symmetric FDW at
/// low frequency needs pre-peak data ParaEQ does not have: 15 cycles at 50 Hz is
/// a 300 ms window, 150 ms of it before the peak, against the ~46–64 ms
/// available.
#[derive(Clone, Copy, Debug)]
pub struct FdwSpec {
    /// Cycles of window BEFORE the peak. A noise gate on pre-peak artifacts,
    /// NOT a resolution control: a deconvolved IR is causal, so the pre-side
    /// multiplies near-zero data (tap-latency noise, deconvolution pre-ringing,
    /// Farina harmonic products). Applied in the TIME domain by `gating.rs` as a
    /// fixed left gate — frequency-independent, which is what a fixed gate can
    /// express.
    pub pre_cycles: f64,
    /// Cycles AFTER the peak. This is the one that sets frequency resolution.
    pub post_cycles: f64,
}

impl FdwSpec {
    /// REW's 15-cycle default, made asymmetric to fit ParaEQ's ~46–64 ms of
    /// pre-peak headroom.
    pub const DEFAULT: FdwSpec = FdwSpec {
        pre_cycles: 3.0,
        post_cycles: 15.0,
    };

    /// `n_c = 4N/π`. REW's 15-cycle default is `N = π·15/4 = 11.78`, i.e.
    /// 1/11.8 ≈ 1/12 octave.
    pub fn from_octave_fraction(n: f64) -> Self {
        let _ = n;
        unimplemented!("room-dsp/5 — lands in Stage 4")
    }

    /// `N = π·n_c/4`.
    pub fn post_octave_fraction(&self) -> f64 {
        unimplemented!("room-dsp/5 — lands in Stage 4")
    }

    /// `√(2 ln 2)/(π·cycles)`.
    pub fn sigma_over_f(cycles: f64) -> f64 {
        let _ = cycles;
        unimplemented!("room-dsp/5 — lands in Stage 4")
    }

    /// `4/(π·cycles)` octaves.
    pub fn fwhm_octaves(cycles: f64) -> f64 {
        let _ = cycles;
        unimplemented!("room-dsp/5 — lands in Stage 4")
    }

    /// The time-domain left gate `gating.rs` should apply for this spec:
    /// `pre_cycles / f_min`.
    pub fn left_gate_s(&self, f_min: f64) -> f64 {
        let _ = f_min;
        unimplemented!("room-dsp/5 — lands in Stage 4")
    }
}

/// Constant-Q smoothing of the COMPLEX spectrum, O(N log N) on the log-f axis.
///
/// `spectrum` must be DEROTATED (phase referenced to the IR peak) and already on
/// `grid`. Returns derotated, same length as `grid`.
///
/// Smoothing `|H|` instead is a different, WRONG operation: it fills a comb's
/// nulls symmetrically, whereas smoothing `H` then taking `|·|` reproduces
/// exactly what a time window does.
///
/// Errors when `σ_bins < 1.0`, naming the minimum `ppo` required — it must never
/// silently return a wrong answer.
///
/// Edge handling is Stage 4's call and is deliberately NOT pinned here: the
/// kernel runs off both ends of the grid, so bins within roughly a kernel
/// half-width of `f_min`/`f_max` cannot agree with the brute force whatever the
/// choice. `tests/test_fdw.rs` compares over an interior band for this reason.
pub fn apply_fdw(
    spectrum: &[Complex<f64>],
    grid: &LogGrid,
    spec: &FdwSpec,
) -> Result<Vec<Complex<f64>>, DspError> {
    let _ = (spectrum, grid, spec);
    unimplemented!("room-dsp/5 — lands in Stage 4")
}

/// The O(F·N) reference: for each grid frequency, window the IR with a symmetric
/// Gaussian of half-amplitude width `spec.post_cycles / f` centred on
/// `ir.peak`, and DFT at that one frequency. Returns DEROTATED values (phase
/// referenced to `ir.peak`), matching `apply_fdw`'s convention so the two are
/// directly comparable.
///
/// TEST ORACLE for `apply_fdw`. Never call in product code.
pub fn apply_fdw_bruteforce(
    ir: &ImpulseResponse,
    grid: &LogGrid,
    spec: &FdwSpec,
) -> Result<Vec<Complex<f64>>, DspError> {
    let _ = (ir, grid, spec);
    unimplemented!("room-dsp/5 — lands in Stage 4")
}
