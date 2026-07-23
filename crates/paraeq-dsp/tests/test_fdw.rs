//! Tier 3 — analytic physics, with `fdw::apply_fdw_bruteforce` as the in-crate
//! oracle. The brute force was written first; the fast path ships against it.
//!
//! Carries the room-dsp spec's full `fdw.rs` test list: fast-vs-brute across
//! n_c (the master spec's Tier-3 test #2), large n_c ⇒ ungated, two-path comb
//! plus small n_c ⇒ flat agreeing with `gating.rs`'s fixed gate, the σ_bins
//! guard erring rather than lying, complex-vs-magnitude discrimination at the
//! nulls, and the asymmetry error bound that decides `splice.rs`'s fate
//! (`asymmetric_truth_vs_fixed_pre_gate_plus_symmetric_fast_fdw` — measured
//! worst case 0.077 dB against the 0.1 dB bound, so `splice.rs` stays a stub).
//!
//! # Read `fdw.rs`'s header before touching tolerances
//!
//! A Gaussian in OCTAVE coordinates — the kernel the room-dsp spec names — fails
//! the fast-vs-brute test by ~5.6e-2 dB at n_c = 15 and ~2.5e-1 dB at n_c = 3,
//! and the error does not shrink with `ppo` because it is a kernel-shape error.
//! The exact kernel is still shift-invariant in `u` (so O(N log N) stands) but
//! it is skewed: `k(v) ∝ 2^v·exp(−(2^v − 1)²/(2c²))`, handed to a convolution
//! as `k(−v)`. With it, the numbers below hold.
//!
//! Tolerance: the spec says < 2e-15 dB. That figure belongs to the LINEAR-f
//! identity (Gaussian time window ↔ Gaussian frequency kernel), which is exact.
//! The realized fast-vs-brute floor is ~1e-11 dB, set by n_c = 3, where the
//! kernel's lower tail runs into ν → 0 and must be truncated. 1e-10 dB is the
//! honest contract and is still ~9 orders tighter than anything audible.

use paraeq_dsp::{
    fdw::{self, FdwSpec},
    gating::{self, GateSpec, ImpulseResponse, LeftClamp, SweepParams},
    logf::LogGrid,
    window::{WindowKind, WindowSpec},
    Complex, DspError,
};
use std::f64::consts::{LN_2, PI};

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

/// The exact derotated ungated spectrum of a post-peak tap set, evaluated
/// analytically on the grid.
///
/// Fed to `apply_fdw` directly rather than via
/// `logf::resample_complex_to_log_grid` so that these tests isolate the FDW
/// identity from resampling error. The resampler has its own Tier-2 test.
fn exact_derotated_spectrum(grid: &LogGrid, paths: &[(usize, f64)]) -> Vec<Complex<f64>> {
    grid.freqs()
        .iter()
        .map(|&f| {
            let mut acc = Complex::new(0.0, 0.0);
            for &(d, a) in paths {
                let ph = -2.0 * PI * f * (d as f64 / SR as f64);
                acc += Complex::new(a * ph.cos(), a * ph.sin());
            }
            acc
        })
        .collect()
}

/// Derotated single-frequency DFT of a sample buffer, phase referenced to
/// `peak_index`. Skips exact zeros, so it is exact AND fast on the sparse tap
/// IRs these tests use.
fn derotated_dft(samples: &[f64], peak_index: usize, sr: f64, freqs: &[f64]) -> Vec<Complex<f64>> {
    freqs
        .iter()
        .map(|&f| {
            let mut acc = Complex::new(0.0, 0.0);
            for (i, &h) in samples.iter().enumerate() {
                if h == 0.0 {
                    continue;
                }
                let tau = (i as f64 - peak_index as f64) / sr;
                let ph = -2.0 * PI * f * tau;
                acc += Complex::new(h * ph.cos(), h * ph.sin());
            }
            acc
        })
        .collect()
}

fn db(c: Complex<f64>) -> f64 {
    20.0 * c.norm().log10()
}

fn nearest_bin(grid: &LogGrid, f: f64) -> usize {
    let mut best = 0;
    let mut best_d = f64::INFINITY;
    for (i, &g) in grid.freqs().iter().enumerate() {
        let d = (g - f).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

#[test]
fn fdw_fast_matches_bruteforce() {
    let ir = multi_reflection_ir();
    let grid = LogGrid::new(F_MIN, F_MAX, PPO).unwrap();
    let spectrum = exact_derotated_spectrum(&grid, &PATHS);
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

/// n_c → very large (well past any physical window) returns the ungated
/// response.
///
/// n_c = 100 at 192 ppo sits just under the grid guard (cap ppo/1.849 ≈ 103)
/// and its window is far past the IR's 4 ms extent everywhere in the band
/// below: at 200 Hz the half-amplitude width is 0.5 s, 125× the delay spread.
/// Above a few hundred Hz the window starts closing in on the reflections —
/// which is FDW doing its job, not a failure of "ungated" — so the comparison
/// band is where "well past any physical window" actually holds.
#[test]
fn very_large_n_c_returns_the_ungated_response() {
    let grid = LogGrid::new(F_MIN, F_MAX, PPO).unwrap();
    let spectrum = exact_derotated_spectrum(&grid, &PATHS);
    let spec = FdwSpec {
        pre_cycles: 100.0,
        post_cycles: 100.0,
    };
    let out = fdw::apply_fdw(&spectrum, &grid, &spec).unwrap();
    for (i, &f) in grid.freqs().iter().enumerate() {
        if f > 200.0 {
            break;
        }
        let d = (db(out[i]) - db(spectrum[i])).abs();
        assert!(
            d < 0.01,
            "{f:.1} Hz: {d:e} dB away from the ungated response (tol 1e-2)"
        );
    }
}

/// Two-path comb + small n_c returns flat, agreeing with `gating.rs`'s
/// fixed-gate result on the same IR — and, at LF where the same window is
/// longer than the reflection delay, preserves the comb. One knob, both
/// behaviours.
#[test]
fn small_n_c_flattens_a_comb_to_the_fixed_gate_result() {
    /// 3 ms — inside the ppo = 192 grid's 6.9 ms Nyquist bound (see `PATHS`).
    const TAU_SAMPLES: usize = 144;
    const G: f64 = 0.5;
    let paths = [(0usize, 1.0), (TAU_SAMPLES, G)];
    let grid = LogGrid::new(F_MIN, F_MAX, PPO).unwrap();
    let spectrum = exact_derotated_spectrum(&grid, &paths);
    let spec = FdwSpec {
        pre_cycles: 3.0,
        post_cycles: 3.0,
    };
    let smoothed = fdw::apply_fdw(&spectrum, &grid, &spec).unwrap();

    // The same IR through gating.rs with a fixed 1.5 ms right gate (< 3 ms):
    // the reflection is excluded, so the gated spectrum is EXACTLY flat
    // (`gating.rs`'s own Tier-3 identity).
    let mut samples = vec![0.0; 8192];
    samples[PEAK] = 1.0;
    samples[PEAK + TAU_SAMPLES] = G;
    let ir = ImpulseResponse {
        samples,
        peak: PEAK as f64,
        sample_rate: SR,
    };
    let gate = GateSpec {
        left_ms: 1.0,
        right_ms: 1.5,
        sweep: None,
        window: WindowSpec {
            left: WindowKind::Rect,
            right: WindowKind::Rect,
        },
    };
    let (gated, _report) = gating::apply_gate(&ir, &gate).unwrap();
    let left_samples = (gate.left_ms * SR as f64 / 1000.0).round() as usize;
    let gated_spec = derotated_dft(&gated, left_samples, SR as f64, grid.freqs());

    for (i, &f) in grid.freqs().iter().enumerate() {
        if !(2_000.0..=10_000.0).contains(&f) {
            continue;
        }
        let fixed_db = db(gated_spec[i]);
        let fdw_db = db(smoothed[i]);
        assert!(
            fixed_db.abs() < 1e-9,
            "{f:.1} Hz: the fixed gate must be exactly flat, got {fixed_db:e} dB"
        );
        assert!(
            fdw_db.abs() < 0.01,
            "{f:.1} Hz: small-n_c FDW must be flat here, got {fdw_db:e} dB"
        );
        assert!(
            (fdw_db - fixed_db).abs() < 0.01,
            "{f:.1} Hz: FDW ({fdw_db:e} dB) disagrees with the fixed gate ({fixed_db:e} dB)"
        );
    }

    // At LF the very same n_c = 3 window is much longer than 3 ms and must
    // KEEP the comb: the null near 166.7 Hz stays several dB deep.
    let lf_worst = grid
        .freqs()
        .iter()
        .enumerate()
        .filter(|&(_, &f)| (100.0..=300.0).contains(&f))
        .map(|(i, _)| db(smoothed[i]).abs())
        .fold(0.0f64, f64::max);
    assert!(
        lf_worst > 3.0,
        "small n_c must still preserve LF comb detail (worst |dB| {lf_worst:.2} <= 3)"
    );
}

/// Guard: σ_bins < 1.0 returns a structured error NAMING the minimum ppo, and
/// never silently produces a wrong answer.
///
/// The boundary pins the spec's exact denominator: n_c ≤ ppo·√(2 ln 2)/(π·ln 2)
/// = ppo/1.849. The wrong constant π·ln 2 = 2.177 (dropping the √(2 ln 2)
/// numerator) would reject valid n_c in (44, 51] at ppo = 96 — so n_c = 51
/// must pass and n_c = 52 must already fail.
#[test]
fn sigma_bins_guard_errs_and_names_the_minimum_ppo() {
    let grid = LogGrid::standard(); // 96 ppo
    let flat = vec![Complex::new(1.0, 0.0); grid.len()];

    let ok = FdwSpec {
        pre_cycles: 3.0,
        post_cycles: 51.0,
    };
    assert!(
        fdw::apply_fdw(&flat, &grid, &ok).is_ok(),
        "n_c = 51 at 96 ppo is legal (cap is ppo/1.849 = 51.9, not ppo/2.177 = 44.1)"
    );

    for (nc, min_ppo) in [(52.0, 97u32), (61.0, 113)] {
        let spec = FdwSpec {
            pre_cycles: 3.0,
            post_cycles: nc,
        };
        let err = fdw::apply_fdw(&flat, &grid, &spec).unwrap_err();
        let DspError::InvalidInput(msg) = err else {
            panic!("guard for n_c = {nc} must be InvalidInput, got {err:?}");
        };
        assert!(
            msg.contains(&format!("requires ppo >= {min_ppo}")),
            "guard for n_c = {nc} must name the minimum ppo ({min_ppo}): {msg}"
        );
    }
}

/// Complex-vs-magnitude discrimination: smoothing `|H|` of a two-path comb and
/// smoothing `H` then taking `|·|` differ by > 3 dB at the null frequencies.
///
/// This test exists to make the wrong implementation fail loudly. Magnitude
/// smoothing fills the null's sharp V symmetrically (linearly in the kernel
/// width), while complex smoothing reproduces what the time window does: at
/// these LF nulls the n_c = 15 window is much longer than the 3 ms delay, so
/// the comb — and its null — survives almost untouched. If `apply_fdw` ever
/// smoothed magnitudes internally, both arms would return the same values and
/// every assertion here would fail at 0 dB difference.
#[test]
fn complex_vs_magnitude_smoothing_differ_at_the_comb_nulls() {
    /// 3 ms — nulls at odd multiples of 166.67 Hz.
    const TAU_SAMPLES: usize = 144;
    const TAU_S: f64 = TAU_SAMPLES as f64 / SR as f64;
    let paths = [(0usize, 1.0), (TAU_SAMPLES, 1.0)];
    let grid = LogGrid::new(F_MIN, F_MAX, PPO).unwrap();
    let spectrum = exact_derotated_spectrum(&grid, &paths);
    let spec = FdwSpec {
        pre_cycles: 15.0,
        post_cycles: 15.0,
    };

    let complex_smoothed = fdw::apply_fdw(&spectrum, &grid, &spec).unwrap();
    // Same kernel, applied to |H| instead of H: exactly the wrong operation.
    let magnitudes: Vec<Complex<f64>> = spectrum
        .iter()
        .map(|c| Complex::new(c.norm(), 0.0))
        .collect();
    let magnitude_smoothed = fdw::apply_fdw(&magnitudes, &grid, &spec).unwrap();

    for k in [1usize, 2, 3, 4] {
        let null_f = (2 * k + 1) as f64 / (2.0 * TAU_S); // 500, 833, 1167, 1500 Hz
        let i = nearest_bin(&grid, null_f);
        let mag_db = db(magnitude_smoothed[i]);
        let cplx_db = db(complex_smoothed[i]);
        assert!(
            mag_db - cplx_db > 3.0,
            "null at {null_f:.0} Hz: magnitude smoothing ({mag_db:.2} dB) vs complex \
             smoothing ({cplx_db:.2} dB) must differ by > 3 dB, got {:.2}",
            mag_db - cplx_db
        );
    }
}

/// THE ASYMMETRY ERROR BOUND — the test that decides `splice.rs`'s fate
/// (room-dsp spec, "Asymmetry — an honest question, now decided").
///
/// Truth: the full asymmetric per-frequency window (`pre_cycles` = 3 of window
/// before the peak, `post_cycles` = 15 after), evaluated by brute force on a
/// raw IR carrying realistic pre-peak junk. Product: `gating.rs`'s fixed left
/// gate at `left_gate_s(f_min)` followed by the SYMMETRIC fast FDW — the only
/// thing the product can fast-path, because the constant-Q identity holds for
/// symmetric windows only. Agreement within 0.1 dB over 20 Hz–20 kHz keeps the
/// two-window `splice.rs` fallback unbuilt.
///
/// The junk is designed, not sampled: deconvolution pre-ringing within 0.8 ms
/// of the peak at −44 dB, and an H2-product cluster 37–40 ms before the peak
/// at −40 dB — about the worst a decent Farina measurement leaves in the
/// captured pre-peak region (true H2 for a ≥ 2 s sweep arrives ≥ 200 ms ahead
/// of the peak, outside the ~60 ms the deconvolved capture holds; what lands
/// close is spread/noise at this level). The gate's Tukey taper is still flat
/// at −40 ms, so the product path genuinely KEEPS the H2 junk and must rely on
/// the symmetric window's wide pre-side — the honest hard case.
///
/// Two confounds are deliberately kept OUT of the comparison, because they are
/// shared by every grid-based pipeline (a two-window splice included) and so
/// cannot inform the splice decision:
/// - **Grid aliasing**: ppo = 1536 keeps the grid's coarsest spacing,
///   Δν(25.5 kHz) = 11.5 Hz, under the 1/(2·40 ms) = 12.5 Hz the −40 ms junk
///   needs. Bounding this in product code is `logf.rs`'s job
///   (`Prefilter::AntiComb`).
/// - **Edge truncation**: the smoothing grid extends one kernel reach past
///   both band edges (0.4 octave below 20 Hz, 0.35 above 20 kHz) so that every
///   asserted bin carries the full kernel. On a hard 20 Hz–20 kHz grid the top
///   bins lose the kernel's upper tail and diverge from the brute force by
///   ~0.12 dB — an `apply_fdw` edge-policy artifact the fast-vs-brute test
///   also excludes by band (see `CMP_LO`/`CMP_HI`), not an asymmetry error.
#[test]
fn asymmetric_truth_vs_fixed_pre_gate_plus_symmetric_fast_fdw() {
    const APEAK: usize = 2880; // 60 ms — where deconvolve() actually lands
    const APPO: u32 = 1536;
    // (offset from the peak in samples, amplitude)
    const POST: [(isize, f64); 3] = [(48, 0.25), (120, -0.15), (192, 0.10)];
    const PRE_RING: [(isize, f64); 4] = [(-38, 0.0015), (-24, -0.0025), (-16, 0.004), (-8, -0.006)];
    const H2: [(isize, f64); 3] = [(-1920, 0.010), (-1848, -0.007), (-1776, 0.005)];

    let mut samples = vec![0.0; 8192];
    samples[APEAK] = 1.0;
    for &(d, a) in POST.iter().chain(&PRE_RING).chain(&H2) {
        samples[(APEAK as isize + d) as usize] = a;
    }
    let ir = ImpulseResponse {
        samples,
        peak: APEAK as f64,
        sample_rate: SR,
    };
    // pre 3, post 15.
    let spec = FdwSpec::DEFAULT;
    // One kernel reach past both edges of the asserted band (see the header).
    let grid = LogGrid::new(F_MIN * 2f64.powf(-0.4), F_MAX * 2f64.powf(0.35), APPO).unwrap();

    // Truth: the asymmetric window the product cannot fast-path.
    let truth = fdw::apply_fdw_bruteforce_asymmetric(&ir, &grid, &spec).unwrap();

    // Product: fixed left gate via gating.rs ...
    let gate = GateSpec {
        left_ms: spec.left_gate_s(F_MIN) * 1000.0, // 150 ms requested
        right_ms: 50.0,
        sweep: Some(SweepParams {
            duration_s: 2.0,
            f1: 20.0,
            f2: 20_000.0,
        }), // Farina bound 200.7 ms — present but not binding
        window: WindowSpec::default(),
    };
    let (gated, report) = gating::apply_gate(&ir, &gate).unwrap();
    assert_eq!(
        report.clamped_left,
        Some(LeftClamp::PeakTooEarly { peak_ms: 60.0 }),
        "the 150 ms left request must clamp at the 60 ms peak — that clamp IS \
         the product's fixed pre-gate"
    );
    let left_samples = (report.applied_left_ms * SR as f64 / 1000.0).round() as usize;
    // ... then the symmetric fast FDW on the gated, derotated spectrum.
    let gated_spec = derotated_dft(&gated, left_samples, SR as f64, grid.freqs());
    let product = fdw::apply_fdw(&gated_spec, &grid, &spec).unwrap();

    let mut worst = 0.0f64;
    let mut worst_f = 0.0f64;
    for (i, &f) in grid.freqs().iter().enumerate() {
        if !(F_MIN..=F_MAX).contains(&f) {
            continue; // the spec's bound is over 20 Hz–20 kHz
        }
        let d = (db(truth[i]) - db(product[i])).abs();
        if d > worst {
            worst = d;
            worst_f = f;
        }
    }
    println!("asymmetry error bound: worst {worst:.5} dB at {worst_f:.1} Hz");
    assert!(
        worst < 0.1,
        "asymmetric truth vs (fixed pre-gate + symmetric fast FDW): worst \
         {worst:.4} dB at {worst_f:.1} Hz exceeds the 0.1 dB bound — build splice.rs"
    );
}

/// The `FdwSpec` closed forms: the cycles↔octave conversion is exact, and the
/// two width formulas (σ_u and FWHM in octaves) agree algebraically.
#[test]
fn fdw_spec_closed_forms() {
    // REW's 15-cycle default is N = π·15/4 = 11.78, i.e. ~1/12 octave.
    let rew = FdwSpec::from_octave_fraction(PI * 15.0 / 4.0);
    assert!((rew.post_cycles - 15.0).abs() < 1e-12);
    assert_eq!(rew.pre_cycles, FdwSpec::DEFAULT.pre_cycles);
    assert!((FdwSpec::DEFAULT.post_octave_fraction() - PI * 15.0 / 4.0).abs() < 1e-12);

    // FWHM_u = 2√(2 ln 2)·σ_u — the two closed forms agree exactly.
    for nc in [3.0, 15.0, 61.0] {
        let sigma_u = FdwSpec::sigma_over_f(nc) / LN_2;
        let fwhm = FdwSpec::fwhm_octaves(nc);
        assert!(
            (fwhm - 2.0 * (2.0 * LN_2).sqrt() * sigma_u).abs() < 1e-14,
            "n_c = {nc}: FWHM {fwhm} vs 2·sqrt(2 ln 2)·σ_u"
        );
    }

    // The spec's running example: σ_f/f = 0.025 at n_c = 15.
    assert!((FdwSpec::sigma_over_f(15.0) - 0.024_985_6).abs() < 1e-6);
    // DEFAULT's fixed left gate at f_min = 20 Hz: 3 cycles / 20 Hz = 150 ms.
    assert!((FdwSpec::DEFAULT.left_gate_s(20.0) - 0.15).abs() < 1e-15);
}
