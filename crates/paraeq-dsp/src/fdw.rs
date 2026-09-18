//! Frequency-dependent windowing: fine resolution in the bass (where modes are
//! real, correctable and need detail) and quasi-anechoic gating in the treble
//! (where reflections are position-specific junk) — from one parameter.
//! Test tier: 3 — analytic physics, with `apply_fdw_bruteforce` as the in-crate
//! oracle. The brute force was written first; the fast path ships against it
//! (`tests/test_fdw.rs`).
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`fdw.rs` — new".
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
//! (`apply_fdw` below sums the correlation form directly, so no flip appears
//! in the code — the note stands for anyone swapping in an FFT convolve.)
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
use std::f64::consts::{LN_2, PI};

/// Kernel values below this fraction of the peak (`k(0) = 1`) are dropped from
/// the fast path's support. Truncation error lands ~7 orders below the
/// 1e-10 dB fast-vs-brute contract; for n_c ≲ 5 the skewed lower tail stays
/// above the cut for many octaves, so the support degenerates to the whole
/// grid and the "truncation" is the grid edge itself.
const KERNEL_CUT: f64 = 1e-18;

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
    ///
    /// The octave fraction describes RESOLUTION, so it sets `post_cycles`
    /// only; `pre_cycles` keeps [`FdwSpec::DEFAULT`]'s noise gate, which no
    /// octave fraction describes.
    pub fn from_octave_fraction(n: f64) -> Self {
        FdwSpec {
            pre_cycles: Self::DEFAULT.pre_cycles,
            post_cycles: 4.0 * n / PI,
        }
    }

    /// `N = π·n_c/4`.
    pub fn post_octave_fraction(&self) -> f64 {
        PI * self.post_cycles / 4.0
    }

    /// `√(2 ln 2)/(π·cycles)`.
    pub fn sigma_over_f(cycles: f64) -> f64 {
        (2.0 * LN_2).sqrt() / (PI * cycles)
    }

    /// `4/(π·cycles)` octaves.
    pub fn fwhm_octaves(cycles: f64) -> f64 {
        4.0 / (PI * cycles)
    }

    /// The time-domain left gate `gating.rs` should apply for this spec:
    /// `pre_cycles / f_min`.
    pub fn left_gate_s(&self, f_min: f64) -> f64 {
        self.pre_cycles / f_min
    }
}

/// Constant-Q smoothing of the COMPLEX spectrum on the log-f axis.
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
/// # Complexity
///
/// Direct O(N·K) correlation with the precomputed kernel (K = support at the
/// [`KERNEL_CUT`]: ~±9·σ_bins for large n_c, growing toward the full grid
/// below n_c ≈ 5, where the skewed lower tail spans octaves). At the product's
/// N ≈ 957 that is at most ~1e6 multiply-adds — microseconds, and simpler than
/// the O(N log N) FFT convolve, whose per-bin renormalization denominator
/// would need a transform of its own. Swap to FFT (with the kernel flipped —
/// see the module header) if grids ever grow 100×.
///
/// # Edge policy (Stage 4's call, made here)
///
/// The kernel is truncated at the grid edges and every bin renormalized by its
/// in-grid kernel mass, so a constant spectrum maps to itself exactly and edge
/// bins degrade gracefully toward a one-sided smooth. Interior bins are
/// unaffected (their full kernel mass is in-grid to the cut). Bins within a
/// kernel half-width of `f_min`/`f_max` still cannot agree with the brute
/// force — it integrates the true spectrum beyond the grid — which is why
/// `tests/test_fdw.rs`'s oracle comparison runs over an interior band.
pub fn apply_fdw(
    spectrum: &[Complex<f64>],
    grid: &LogGrid,
    spec: &FdwSpec,
) -> Result<Vec<Complex<f64>>, DspError> {
    validate_spec("apply_fdw", spec)?;
    if spectrum.len() != grid.len() {
        return Err(DspError::InvalidInput(format!(
            "apply_fdw: {} spectrum bins vs {} grid bins",
            spectrum.len(),
            grid.len()
        )));
    }
    if spectrum
        .iter()
        .any(|c| !c.re.is_finite() || !c.im.is_finite())
    {
        return Err(DspError::InvalidInput(
            "apply_fdw: spectrum contains non-finite values".into(),
        ));
    }

    let ppo = f64::from(grid.points_per_octave());
    let c = FdwSpec::sigma_over_f(spec.post_cycles);
    let sigma_u = c / LN_2;
    let sigma_bins = sigma_u * ppo;
    if sigma_bins < 1.0 {
        // ceil(1/σ_u): the smallest ppo whose grid samples this kernel.
        let min_ppo = (1.0 / sigma_u).ceil() as u64;
        return Err(DspError::InvalidInput(format!(
            "apply_fdw: sigma_bins = {sigma_bins:.4} < 1.0 -- a {} ppo grid cannot sample \
             the n_c = {} kernel; requires ppo >= {min_ppo} \
             (n_c <= ppo*sqrt(2 ln 2)/(pi*ln 2) = ppo/1.849)",
            grid.points_per_octave(),
            spec.post_cycles
        )));
    }

    // The exact log-f kernel (module header): k(v) = 2^v·exp(−(2^v−1)²/(2c²)),
    // v in octaves. Unimodal with k(0) = 1 and its mode a hair above v = 0, so
    // scanning outward from 0 while k ≥ KERNEL_CUT finds the whole
    // super-threshold support.
    let n = grid.len() as i64;
    let two_c2 = 2.0 * c * c;
    let kval = |m: i64| -> f64 {
        let p = 2f64.powf(m as f64 / ppo);
        let x = p - 1.0;
        p * (-x * x / two_c2).exp()
    };
    let mut m_lo = 0i64;
    while m_lo > -(n - 1) && kval(m_lo - 1) >= KERNEL_CUT {
        m_lo -= 1;
    }
    let mut m_hi = 0i64;
    while m_hi < n - 1 && kval(m_hi + 1) >= KERNEL_CUT {
        m_hi += 1;
    }
    let kernel: Vec<f64> = (m_lo..=m_hi).map(kval).collect();
    // Prefix sums make each bin's in-grid kernel mass (the renormalization
    // denominator) O(1).
    let mut prefix = Vec::with_capacity(kernel.len() + 1);
    prefix.push(0.0);
    for &k in &kernel {
        prefix.push(prefix.last().expect("non-empty by construction") + k);
    }

    let mut out = Vec::with_capacity(spectrum.len());
    for i in 0..n {
        let j_lo = (i + m_lo).max(0);
        let j_hi = (i + m_hi).min(n - 1);
        let k0 = (j_lo - (i + m_lo)) as usize;
        let count = (j_hi - j_lo + 1) as usize;
        let mut acc = Complex::new(0.0, 0.0);
        for (h, &k) in spectrum[j_lo as usize..=j_hi as usize]
            .iter()
            .zip(&kernel[k0..k0 + count])
        {
            acc += h * k;
        }
        let mass = prefix[k0 + count] - prefix[k0];
        out.push(acc / mass);
    }
    Ok(out)
}

/// The O(F·N) reference: for each grid frequency, window the IR with a symmetric
/// Gaussian of half-amplitude width `spec.post_cycles / f` centred on
/// `ir.peak`, and DFT at that one frequency. Returns DEROTATED values (phase
/// referenced to `ir.peak`), matching `apply_fdw`'s convention so the two are
/// directly comparable.
///
/// `spec.pre_cycles` does not enter: it is a pre-peak noise gate, not a
/// resolution control ([`apply_fdw_bruteforce_asymmetric`] is the oracle that
/// windows the two sides independently).
///
/// TEST ORACLE for `apply_fdw`. Never call in product code.
pub fn apply_fdw_bruteforce(
    ir: &ImpulseResponse,
    grid: &LogGrid,
    spec: &FdwSpec,
) -> Result<Vec<Complex<f64>>, DspError> {
    validate_spec("apply_fdw_bruteforce", spec)?;
    validate_ir("apply_fdw_bruteforce", ir)?;
    Ok(bruteforce(ir, grid, spec.post_cycles, spec.post_cycles))
}

/// The asymmetric-window oracle behind the spec's "asymmetry error bound"
/// test: per grid frequency, a two-piece Gaussian centred on `ir.peak` — the
/// pre side (t < peak) the half of a Gaussian whose full half-amplitude width
/// would be `spec.pre_cycles / f`, the post side likewise from
/// `spec.post_cycles / f` — then a DFT at that one frequency. Equals
/// [`apply_fdw_bruteforce`] when `pre_cycles == post_cycles`;
/// `pre_cycles == 0` degenerates to a hard pre-peak cut (the σ → 0 limit).
///
/// A time-asymmetric window has a Hermitian-asymmetric transform, so its
/// frequency kernel phase-shifts as well as smooths — it is NOT a two-piece
/// Gaussian on the log-f axis, and no fast path exists for it. The product
/// substitutes a fixed left gate (`gating.rs`) plus the symmetric
/// [`apply_fdw`]; `tests/test_fdw.rs`'s asymmetry bound against this oracle is
/// what licenses that substitution.
///
/// TEST ORACLE ONLY. Never call in product code.
pub fn apply_fdw_bruteforce_asymmetric(
    ir: &ImpulseResponse,
    grid: &LogGrid,
    spec: &FdwSpec,
) -> Result<Vec<Complex<f64>>, DspError> {
    validate_spec("apply_fdw_bruteforce_asymmetric", spec)?;
    validate_ir("apply_fdw_bruteforce_asymmetric", ir)?;
    Ok(bruteforce(ir, grid, spec.pre_cycles, spec.post_cycles))
}

/// Shared brute-force core: per-frequency two-piece Gaussian window + DFT.
/// O(F·N) in general; exact zeros contribute nothing and are skipped, which
/// makes the oracle cheap on the sparse tap IRs the tests use.
fn bruteforce(
    ir: &ImpulseResponse,
    grid: &LogGrid,
    pre_cycles: f64,
    post_cycles: f64,
) -> Vec<Complex<f64>> {
    let sr = f64::from(ir.sample_rate);
    // FWHM = 2√(2 ln 2)·σ_t, so σ_t = (cycles/f) / width.
    let width = 2.0 * (2.0 * LN_2).sqrt();
    grid.freqs()
        .iter()
        .map(|&f| {
            let sigma_pre = pre_cycles / (f * width);
            let sigma_post = post_cycles / (f * width);
            let mut acc = Complex::new(0.0, 0.0);
            for (i, &h) in ir.samples.iter().enumerate() {
                if h == 0.0 {
                    continue;
                }
                let tau = (i as f64 - ir.peak) / sr;
                let sigma = if tau < 0.0 { sigma_pre } else { sigma_post };
                // σ = 0 is the hard-cut limit (pre_cycles = 0, τ < 0 only:
                // post_cycles is validated > 0).
                let w = if sigma > 0.0 {
                    (-tau * tau / (2.0 * sigma * sigma)).exp()
                } else {
                    0.0
                };
                if w == 0.0 {
                    continue;
                }
                let ph = -2.0 * PI * f * tau;
                acc += Complex::new(w * h * ph.cos(), w * h * ph.sin());
            }
            acc
        })
        .collect()
}

fn validate_spec(caller: &str, spec: &FdwSpec) -> Result<(), DspError> {
    if !spec.post_cycles.is_finite() || spec.post_cycles <= 0.0 {
        return Err(DspError::InvalidInput(format!(
            "{caller}: post_cycles must be finite and > 0, got {}",
            spec.post_cycles
        )));
    }
    if !spec.pre_cycles.is_finite() || spec.pre_cycles < 0.0 {
        return Err(DspError::InvalidInput(format!(
            "{caller}: pre_cycles must be finite and >= 0, got {}",
            spec.pre_cycles
        )));
    }
    Ok(())
}

fn validate_ir(caller: &str, ir: &ImpulseResponse) -> Result<(), DspError> {
    if ir.samples.is_empty() {
        return Err(DspError::InvalidInput(format!(
            "{caller}: empty impulse response"
        )));
    }
    if ir.sample_rate == 0 {
        return Err(DspError::InvalidInput(format!(
            "{caller}: sample_rate is 0"
        )));
    }
    if ir.samples.iter().any(|s| !s.is_finite()) {
        return Err(DspError::InvalidInput(format!(
            "{caller}: non-finite sample in impulse response"
        )));
    }
    if !ir.peak.is_finite() || ir.peak < 0.0 || ir.peak_index() >= ir.samples.len() {
        return Err(DspError::InvalidInput(format!(
            "{caller}: invalid peak index {} for {} samples",
            ir.peak,
            ir.samples.len()
        )));
    }
    Ok(())
}
