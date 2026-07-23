//! logf.rs tests — Tier 2 (np.interp-direct fixture) + Tier 3 (analytic).
//!
//! Tier 2 pins `resample_db_to_log_grid` at `Prefilter::None` against
//! `np.interp` on the grid-formula axis at 1e-12 (fixtures/logf). Tier 3 pins
//! the grid's closed form (f[0] bit-equal, the 2^(1/ppo) ratio law, N = 957)
//! and the anti-comb prefilter's reason to exist: aliased vs bounded ripple on
//! a synthetic comb, asserted as the DIFFERENCE between the two settings.

mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::logf::{resample_complex_to_log_grid, resample_db_to_log_grid, LogGrid, Prefilter};
use paraeq_dsp::{Complex, DspError};

// ---------------------------------------------------------------- Tier 2

#[test]
fn resample_db_none_matches_np_interp_fixture() {
    let c = Case::load("logf", "resample_db");
    let grid = LogGrid::new(
        c.param_f64("f_min"),
        c.param_f64("f_max"),
        c.param_u64("ppo") as u32,
    )
    .unwrap();
    // The fixture's query points are the grid formula itself — LogGrid must
    // reproduce them. Measured bit-identical to numpy on this toolchain; the
    // ulp-level slack is only for libm pow differences across platforms.
    assert_allclose(grid.freqs(), &c.array("grid_freqs"), 1e-15, 0.0, "grid");
    let out = resample_db_to_log_grid(
        &c.array("freqs_linear"),
        &c.array("mag_db"),
        &grid,
        Prefilter::None,
    )
    .unwrap();
    assert_allclose(&out, &c.array("resampled"), 0.0, 1e-12, "resample_db");
}

// ---------------------------------------------------------------- Tier 3: grid law

#[test]
fn standard_grid_matches_the_closed_form() {
    let g = LogGrid::standard();
    assert_eq!(g.len(), 957);
    assert!(!g.is_empty());
    // f[0] == f_min EXACTLY (bit-equal): 2^(0/ppo) is exactly 1.
    assert_eq!(g.freqs()[0], 20.0);
    assert_eq!(g.f_min(), 20.0);
    assert_eq!(g.f_max(), 20000.0);
    assert_eq!(g.points_per_octave(), 96);
    // The last bin deliberately falls short of f_max — the grid is the largest
    // set of ppo-spaced points that does not exceed f_max.
    let last = g.freqs()[956];
    assert!(last < 20000.0, "last bin {last} must fall short of f_max");
    assert!(
        last * 2f64.powf(1.0 / 96.0) > 20000.0,
        "one more bin would exceed f_max; otherwise N is wrong"
    );
}

#[test]
fn adjacent_bin_ratio_is_2_to_the_1_over_ppo() {
    let g = LogGrid::standard();
    let r = 2f64.powf(1.0 / 96.0);
    for (i, w) in g.freqs().windows(2).enumerate() {
        let ratio = w[1] / w[0];
        // Spec says 1e-15, but the pinned per-index-pow sequence itself
        // deviates by 1.11e-15 (5 ulps) at bin 796 — measured identically in
        // numpy (the fixture oracle) and Apple libm, whose grids are
        // bit-equal. 2e-15 (~9 ulps) keeps the law's intent without failing
        // the contractual float sequence.
        assert!(
            (ratio / r - 1.0).abs() <= 2e-15,
            "bin {i}: ratio {ratio:.17} vs 2^(1/96) {r:.17}"
        );
    }
}

#[test]
fn octave_axis_is_i_over_ppo() {
    let g = LogGrid::new(20.0, 20000.0, 96).unwrap();
    let ax = g.octave_axis();
    assert_eq!(ax.len(), g.len());
    for (i, &v) in ax.iter().enumerate() {
        assert_eq!(v, i as f64 / 96.0, "octave_axis[{i}]");
    }
    // standard() is exactly new(20, 20000, 96)
    assert_eq!(g.freqs(), LogGrid::standard().freqs());
}

// ---------------------------------------------------------------- Tier 3: flat inputs

#[test]
fn flat_db_input_resamples_flat_under_both_prefilters() {
    let grid = LogGrid::standard();
    let df = 48000.0 / 4096.0;
    let freqs: Vec<f64> = (0..2049).map(|i| i as f64 * df).collect();
    let flat = vec![3.25; freqs.len()];
    for pf in [Prefilter::None, Prefilter::AntiComb { fraction: 48 }] {
        let out = resample_db_to_log_grid(&freqs, &flat, &grid, pf).unwrap();
        assert_eq!(out.len(), grid.len());
        for (i, &v) in out.iter().enumerate() {
            assert!((v - 3.25).abs() < 1e-12, "{pf:?} bin {i}: {v} != 3.25");
        }
    }
}

#[test]
fn flat_complex_input_resamples_flat_under_both_prefilters() {
    let grid = LogGrid::standard();
    let df = 48000.0 / 4096.0;
    let freqs: Vec<f64> = (0..2049).map(|i| i as f64 * df).collect();
    let c = Complex::new(0.5, -1.25);
    let flat = vec![c; freqs.len()];
    for pf in [Prefilter::None, Prefilter::AntiComb { fraction: 48 }] {
        let out = resample_complex_to_log_grid(&freqs, &flat, &grid, pf).unwrap();
        assert_eq!(out.len(), grid.len());
        for (i, v) in out.iter().enumerate() {
            assert!(
                (v.re - c.re).abs() < 1e-12 && (v.im - c.im).abs() < 1e-12,
                "{pf:?} bin {i}: {v} != {c}"
            );
        }
    }
}

// ---------------------------------------------------------------- Tier 3: the comb

/// |1 + 0.7·e^{-jωτ}| in dB on the given axis.
fn comb_db(freqs: &[f64], tau: f64) -> Vec<f64> {
    freqs
        .iter()
        .map(|&f| {
            let wt = 2.0 * std::f64::consts::PI * f * tau;
            let re = 1.0 + 0.7 * wt.cos();
            let im = -0.7 * wt.sin();
            20.0 * (re * re + im * im).sqrt().log10()
        })
        .collect()
}

fn linear_axis(sr: f64, n_fft: usize) -> Vec<f64> {
    let df = sr / n_fft as f64;
    (0..n_fft / 2 + 1).map(|i| i as f64 * df).collect()
}

/// Peak-to-peak of `vals` over grid bins whose frequency lies in [lo, hi].
fn peak_to_peak(grid: &LogGrid, vals: &[f64], lo: f64, hi: f64) -> f64 {
    let band: Vec<f64> = grid
        .freqs()
        .iter()
        .zip(vals)
        .filter(|(&f, _)| f >= lo && f <= hi)
        .map(|(_, &v)| v)
        .collect();
    assert!(!band.is_empty());
    let max = band.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min = band.iter().cloned().fold(f64::INFINITY, f64::min);
    max - min
}

/// The prefilter's reason to exist. A 20 ms reflection combs at a 50 Hz period;
/// near 20 kHz adjacent log bins are ~145 Hz apart, so naive point-sampling
/// picks quasi-random comb phases and aliases the full ±(comb depth) into the
/// log curve. The 1/48-octave average decimates properly. The assertion is the
/// DIFFERENCE between the two settings — the only form of the claim that
/// means anything.
#[test]
fn anti_comb_bounds_aliased_ripple_on_synthetic_comb() {
    let freqs = linear_axis(48000.0, 32768);
    let mag_db = comb_db(&freqs, 0.020);
    let grid = LogGrid::standard();
    let none = resample_db_to_log_grid(&freqs, &mag_db, &grid, Prefilter::None).unwrap();
    let anti =
        resample_db_to_log_grid(&freqs, &mag_db, &grid, Prefilter::AntiComb { fraction: 48 })
            .unwrap();
    // Band where the 1/48-oct window spans >= ~3 comb periods.
    let pp_none = peak_to_peak(&grid, &none, 10_000.0, 19_900.0);
    let pp_anti = peak_to_peak(&grid, &anti, 10_000.0, 19_900.0);
    // Measured on the oracle-equivalent implementation: none ~ 14.9 dB
    // (nearly the comb's full 15.1 dB swing), anti ~ 1.06 dB.
    assert!(pp_anti < 2.0, "anti-comb ripple not bounded: {pp_anti} dB");
    assert!(
        pp_none > 10.0,
        "naive sampling failed to alias: {pp_none} dB"
    );
    assert!(
        pp_none > 5.0 * pp_anti,
        "prefilter margin gone: none {pp_none} dB vs anti {pp_anti} dB"
    );
}

/// Where the log grid out-resolves the linear axis (low frequencies), the
/// 1/48-oct neighbourhood holds at most one point and AntiComb must fall back
/// to plain interpolation — bit-identical to Prefilter::None, no empty-average
/// NaNs.
#[test]
fn anti_comb_falls_back_to_interp_where_grid_outresolves_input() {
    let freqs = linear_axis(48000.0, 32768);
    let df = freqs[1] - freqs[0];
    let mag_db = comb_db(&freqs, 0.020);
    let grid = LogGrid::standard();
    let none = resample_db_to_log_grid(&freqs, &mag_db, &grid, Prefilter::None).unwrap();
    let anti =
        resample_db_to_log_grid(&freqs, &mag_db, &grid, Prefilter::AntiComb { fraction: 48 })
            .unwrap();
    let width = 2f64.powf(1.0 / 96.0) - 2f64.powf(-1.0 / 96.0);
    let mut checked = 0usize;
    for (i, &f) in grid.freqs().iter().enumerate() {
        if f * width < df {
            // window narrower than the linear spacing -> at most one point
            assert!(
                anti[i] == none[i],
                "bin {i} ({f} Hz): fallback must equal plain interpolation"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 100,
        "fallback region unexpectedly small: {checked}"
    );
}

// ---------------------------------------------------------------- error paths

fn is_invalid(e: DspError) -> bool {
    matches!(e, DspError::InvalidInput(_))
}

#[test]
fn grid_rejects_bad_params() {
    assert!(is_invalid(LogGrid::new(0.0, 20000.0, 96).unwrap_err()));
    assert!(is_invalid(LogGrid::new(-1.0, 20000.0, 96).unwrap_err()));
    assert!(is_invalid(LogGrid::new(f64::NAN, 20000.0, 96).unwrap_err()));
    assert!(is_invalid(LogGrid::new(20.0, 20.0, 96).unwrap_err()));
    assert!(is_invalid(LogGrid::new(20.0, 10.0, 96).unwrap_err()));
    assert!(is_invalid(LogGrid::new(20.0, f64::NAN, 96).unwrap_err()));
    assert!(is_invalid(
        LogGrid::new(20.0, f64::INFINITY, 96).unwrap_err()
    ));
    assert!(is_invalid(LogGrid::new(20.0, 20000.0, 0).unwrap_err()));
}

#[test]
fn resamplers_reject_bad_inputs() {
    let grid = LogGrid::standard();
    let good_f = [10.0, 100.0, 1000.0, 24000.0];
    let good_d = [0.0, 1.0, 2.0, 3.0];
    let good_c: Vec<Complex<f64>> = good_d.iter().map(|&v| Complex::new(v, -v)).collect();

    // mismatched lengths
    assert!(is_invalid(
        resample_db_to_log_grid(&good_f, &good_d[..3], &grid, Prefilter::None).unwrap_err()
    ));
    assert!(is_invalid(
        resample_complex_to_log_grid(&good_f, &good_c[..3], &grid, Prefilter::None).unwrap_err()
    ));

    // empty input
    assert!(is_invalid(
        resample_db_to_log_grid(&[], &[], &grid, Prefilter::None).unwrap_err()
    ));
    assert!(is_invalid(
        resample_complex_to_log_grid(&[], &[], &grid, Prefilter::None).unwrap_err()
    ));

    // freqs not strictly increasing (equal, then decreasing)
    let equal_f = [10.0, 100.0, 100.0, 24000.0];
    let decr_f = [10.0, 100.0, 50.0, 24000.0];
    assert!(is_invalid(
        resample_db_to_log_grid(&equal_f, &good_d, &grid, Prefilter::None).unwrap_err()
    ));
    assert!(is_invalid(
        resample_db_to_log_grid(&decr_f, &good_d, &grid, Prefilter::None).unwrap_err()
    ));
    assert!(is_invalid(
        resample_complex_to_log_grid(&decr_f, &good_c, &grid, Prefilter::None).unwrap_err()
    ));

    // non-finite freqs / data
    let nan_f = [10.0, 100.0, f64::NAN, 24000.0];
    let nan_d = [0.0, 1.0, f64::NAN, 3.0];
    let mut nan_c = good_c.clone();
    nan_c[2] = Complex::new(1.0, f64::NAN);
    assert!(is_invalid(
        resample_db_to_log_grid(&nan_f, &good_d, &grid, Prefilter::None).unwrap_err()
    ));
    assert!(is_invalid(
        resample_db_to_log_grid(&good_f, &nan_d, &grid, Prefilter::None).unwrap_err()
    ));
    assert!(is_invalid(
        resample_complex_to_log_grid(&good_f, &nan_c, &grid, Prefilter::None).unwrap_err()
    ));

    // AntiComb fraction 0 would make the neighbourhood infinite
    assert!(is_invalid(
        resample_db_to_log_grid(&good_f, &good_d, &grid, Prefilter::AntiComb { fraction: 0 })
            .unwrap_err()
    ));
    assert!(is_invalid(
        resample_complex_to_log_grid(&good_f, &good_c, &grid, Prefilter::AntiComb { fraction: 0 })
            .unwrap_err()
    ));
}
