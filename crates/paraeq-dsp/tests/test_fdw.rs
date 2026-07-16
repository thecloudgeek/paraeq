//! Tier 3 — analytic physics, with `apply_fdw_bruteforce` as the in-crate
//! oracle. Write the brute force first; ship the fast path against it.
//!
//! Un-ignore when `fdw.rs` lands: room-dsp/5, Stage 4 ("`fdw.rs` (bruteforce
//! oracle first, then O(N log N))" —
//! docs/plans/2026-07-16-rescope-implementation.md).
//!
//! Scope: this file carries the master spec's Tier-3 test #2 only. The room-dsp
//! spec lists four more `fdw.rs` tests (large-n_c ⇒ ungated; two-path comb plus
//! small n_c ⇒ flat; the σ_bins guard errors rather than lying; complex-vs-
//! magnitude discrimination of at least 3 dB at the nulls) and the asymmetry
//! error bound that decides `splice.rs`'s fate. Those are Stage 4's to add —
//! none of their pinned values are verified here.
//!
//! # Read `fdw.rs`'s header before making this pass
//!
//! A Gaussian in OCTAVE coordinates — the kernel the room-dsp spec names — fails
//! this test by ~5.6e-2 dB at n_c = 15 and ~2.5e-1 dB at n_c = 3, and the error
//! does not shrink with `ppo` because it is a kernel-shape error. The exact
//! kernel is still shift-invariant in `u` (so O(N log N) stands) but it is
//! skewed: `k(v) ∝ 2^v·exp(−(2^v − 1)²/(2c²))`, handed to a convolution as
//! `k(−v)`. With it, the numbers below hold.
//!
//! Tolerance: the spec says < 2e-15 dB. That figure belongs to the LINEAR-f
//! identity (Gaussian time window ↔ Gaussian frequency kernel), which is exact.
//! The realized fast-vs-brute floor is ~1e-11 dB, set by n_c = 3, where the
//! kernel's lower tail runs into ν → 0 and must be truncated. 1e-10 dB is the
//! honest contract and is still ~9 orders tighter than anything audible.

use paraeq_dsp::{
    fdw::{self, FdwSpec},
    gating::ImpulseResponse,
    logf::LogGrid,
    Complex,
};
use std::f64::consts::PI;

const SR: u32 = 48_000;
/// 100 ms — plenty of pre-peak room; the brute force windows symmetrically.
const PEAK: usize = 4800;
const PPO: u32 = 192;
const F_MIN: f64 = 20.0;
const F_MAX: f64 = 20_000.0;

/// Direct + three reflections at 1.0, 2.5 and 4.0 ms.
///
/// The 4 ms spread is load-bearing, not arbitrary. A path at delay `t` makes
/// `H(ν)` oscillate with period `1/t`; the log grid's coarsest spacing is at
/// `f_max`, `Δν = f_max·(2^(1/ppo) − 1)` = 72.3 Hz here. Representing the IR
/// needs `Δν < 1/(2·t_max)`, i.e. `t_max < 6.9 ms` at ppo = 192. An 11 ms
/// reflection aliases on the grid and drags fast-vs-brute out to ~1.7e-9 dB even
/// with the correct kernel — the fast path would be blamed for the grid's error.
/// `logf::Prefilter::AntiComb` is what bounds this in product code, but it is a
/// lowpass: it changes the answer, so it cannot be engaged here.
const PATHS: [(usize, f64); 4] = [(0, 1.0), (48, 0.4), (120, -0.3), (192, 0.25)];

/// Interior comparison band. The kernel runs off both ends of the grid, so bins
/// within ~a kernel half-width of f_min/f_max cannot agree with the brute force
/// whatever edge handling `apply_fdw` picks — that is an edge-policy question,
/// not the identity under test. At n_c = 3 the kernel reaches ~3 octaves down
/// and ~1 octave up, which these bounds clear on a 20 Hz–20 kHz grid.
const CMP_LO: f64 = 160.0;
const CMP_HI: f64 = 10_000.0;

const TOL_DB: f64 = 1e-10;

fn multi_reflection_ir() -> ImpulseResponse {
    let mut samples = vec![0.0; 16_384];
    for (d, a) in PATHS {
        samples[PEAK + d] = a;
    }
    ImpulseResponse {
        samples,
        peak: PEAK as f64,
        sample_rate: SR,
    }
}

/// The exact derotated ungated spectrum, evaluated analytically on the grid.
///
/// Fed to `apply_fdw` directly rather than via
/// `logf::resample_complex_to_log_grid` so that this test isolates the FDW
/// identity from resampling error. The resampler has its own Tier-2 test.
fn exact_derotated_spectrum(grid: &LogGrid) -> Vec<Complex<f64>> {
    grid.freqs()
        .iter()
        .map(|&f| {
            let mut acc = Complex::new(0.0, 0.0);
            for (d, a) in PATHS {
                let ph = -2.0 * PI * f * (d as f64 / SR as f64);
                acc += Complex::new(a * ph.cos(), a * ph.sin());
            }
            acc
        })
        .collect()
}

#[test]
#[ignore = "fdw.rs lands in Stage 4 (room-dsp/5)"]
fn fdw_fast_matches_bruteforce() {
    let ir = multi_reflection_ir();
    let grid = LogGrid::new(F_MIN, F_MAX, PPO).unwrap();
    let spectrum = exact_derotated_spectrum(&grid);
    assert_eq!(spectrum.len(), grid.len());

    for nc in [3.0, 5.0, 15.0, 30.0, 61.0] {
        // The brute force windows symmetrically on `post_cycles`; `pre_cycles`
        // is a pre-peak noise gate, not a resolution control, and does not
        // enter here.
        let spec = FdwSpec {
            pre_cycles: nc,
            post_cycles: nc,
        };
        let fast = fdw::apply_fdw(&spectrum, &grid, &spec).unwrap();
        let brute = fdw::apply_fdw_bruteforce(&ir, &grid, &spec).unwrap();
        assert_eq!(fast.len(), grid.len(), "n_c={nc}");
        assert_eq!(brute.len(), grid.len(), "n_c={nc}");

        let mut worst = 0.0f64;
        let mut worst_f = 0.0f64;
        for (i, &f) in grid.freqs().iter().enumerate() {
            if !(CMP_LO..=CMP_HI).contains(&f) {
                continue;
            }
            let d = (20.0 * fast[i].norm().log10() - 20.0 * brute[i].norm().log10()).abs();
            if d > worst {
                worst = d;
                worst_f = f;
            }
        }
        assert!(
            worst < TOL_DB,
            "n_c={nc}: fast vs bruteforce worst {worst:e} dB at {worst_f:.1} Hz (tol {TOL_DB:e})"
        );
    }
}
