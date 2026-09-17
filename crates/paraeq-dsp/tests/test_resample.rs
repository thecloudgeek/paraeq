//! Falsifiers for [`paraeq_dsp::resample::resample_ratio`] — the time-domain
//! ratio resampler the two-clock correction needs (decision doc §Q6: "resample
//! the capture to correct it — default on").
//!
//! The one that matters most is `resample_ratio_index_convention_is_output_clock`:
//! the caller holds a `t0` measured in the INPUT clock and slices the OUTPUT
//! array, and those are different axes. Get it wrong and the deconvolution is
//! handed a window that starts `t0·(ratio − 1)` samples late — 4.4 samples on a
//! 7.7 s file at 12 ppm, which is exactly the regime the resampler exists for.

mod common;

use common::{assert_allclose, Case};
use paraeq_dsp::resample::resample_ratio;
use std::f64::consts::PI;

const RATE: f64 = 48_000.0;

/// A linear chirp `f0 → f1` over `n` samples, evaluated at an arbitrary
/// (fractional) input-sample index. This is the CONTINUOUS signal the
/// resampler is supposed to be reconstructing, so it doubles as the oracle.
fn chirp_at(input_index: f64, n: usize, f0: f64, f1: f64) -> f64 {
    let t = input_index / RATE;
    let k = (f1 - f0) / (n as f64 / RATE);
    (2.0 * PI * (f0 * t + 0.5 * k * t * t)).sin()
}

#[test]
fn resample_ratio_is_identity_in_band_at_ratio_one() {
    let x: Vec<f64> = (0..512)
        .map(|i| (2.0 * PI * 0.07 * i as f64).sin())
        .collect();

    let y = resample_ratio(&x, 1.0, 0.0);

    assert_eq!(y.len(), x.len(), "ratio 1 neither adds nor drops samples");
    // Not "close": at ratio 1 with zero phase every kernel tap lands on an
    // integer, where sinc is 1 at the centre and 0 everywhere else, so the
    // answer is the input back to float rounding. 1e-9 is the plan's bar; the
    // real figure is ~1e-16.
    assert_allclose(&y, &x, 0.0, 1e-9, "ratio-one identity");
}

#[test]
fn resample_ratio_preserves_a_chirps_group_delay_to_a_hundredth_of_a_sample() {
    // A chirp sweeps the band, so "the delay is the same at every frequency"
    // (flat group delay) is what this grades — the property that makes the
    // marker intercept meaningful after a resample.
    let n = 8192;
    let (f0, f1) = (1_000.0, 4_000.0);
    let x: Vec<f64> = (0..n).map(|i| chirp_at(i as f64, n, f0, f1)).collect();

    for (ratio, phase) in [(1.000_25, 0.37), (0.999_8, 0.0), (1.5, 0.25)] {
        let y = resample_ratio(&x, ratio, phase);
        // The kernel reaches 16 input samples either side, so the first and
        // last few output samples see the zero-padded edge rather than the
        // signal. Grade the interior.
        let margin = 64;
        let mut worst = 0.0f64;
        for (j, &actual) in y.iter().enumerate().take(y.len() - margin).skip(margin) {
            let expected = chirp_at((j as f64 + phase) / ratio, n, f0, f1);
            worst = worst.max((actual - expected).abs());
        }
        // An amplitude error `e` on a unit sinusoid of frequency `f` is what a
        // timing error of `e / (2π·f)` samples would produce, so the worst-case
        // timing error is the worst amplitude error divided by the steepest
        // slope in the band. That conversion is the whole point: the bar is
        // stated in samples, which is the unit t = 0 is measured in.
        let implied_delay_samples = worst / (2.0 * PI * f1 / RATE);
        assert!(
            implied_delay_samples < 0.01,
            "ratio {ratio} phase {phase}: worst amplitude error {worst:e} \
             implies {implied_delay_samples} samples of timing error"
        );
    }
}

#[test]
fn resample_ratio_index_convention_is_output_clock() {
    // A unit impulse at INPUT index t0, resampled by `ratio`, must come back at
    // OUTPUT index t0·ratio. The wrong answer — the one this test exists to
    // refuse — is "t0", i.e. slicing the resampled array at an index that was
    // measured before the resample.
    let n = 2_048;
    let t0 = 1_000usize;
    let ratio = 1.5;
    let mut x = vec![0.0; n];
    x[t0] = 1.0;

    let y = resample_ratio(&x, ratio, 0.0);

    let peak_index = y
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap()
        .0;
    assert_eq!(peak_index, 1_500, "t0·ratio = 1000·1.5, not t0");
    assert!(
        (y[1_500] - 1.0).abs() < 1e-9,
        "an integer-aligned output index samples the impulse exactly, got {}",
        y[1_500]
    );
    assert!(
        y[t0].abs() < 1e-6,
        "output index t0 holds nothing; that is the convention error, got {}",
        y[t0]
    );

    // And the phase term's SIGN: `frac_offset` advances input time, so a
    // POSITIVE offset moves a feature EARLIER in output index. Half a sample of
    // offset puts the impulse exactly between 1499 and 1500, and the kernel is
    // even, so those two samples must be equal.
    let shifted = resample_ratio(&x, ratio, 0.5);
    assert!(
        (shifted[1_499] - shifted[1_500]).abs() < 1e-12,
        "frac_offset 0.5 straddles 1499/1500: {} vs {}",
        shifted[1_499],
        shifted[1_500]
    );
    assert!(
        shifted[1_499] < y[1_500],
        "a half-sample-off impulse peaks below the on-grid one"
    );
}

/// Tier 2 — `scipy.signal.resample_poly` is the delegate for the rational-ratio
/// special case. See `gen_resample_poly()` in the generator for what the two
/// tolerances are and are not measuring: scipy's own passband ripple dominates
/// the disagreement, so this case grades the convention, the ratio and the
/// kernel's shape, not the last three digits.
#[test]
fn resample_ratio_matches_resample_poly_on_a_rational_ratio() {
    let case = Case::load("resample", "poly_rational");
    let x = case.array("x");
    let expected = case.array("y");
    let ratio = case.param_u64("up") as f64 / case.param_u64("down") as f64;

    let actual = resample_ratio(&x, ratio, 0.0);

    // scipy emits ceil(n·up/down) samples and we emit floor((n−1)·ratio)+1, so
    // the two differ by at most one sample at the very end. Compare the overlap,
    // minus the edge margin where the two filters' zero-padding differs (ours
    // reaches 16 input samples, scipy's 2·10·max(up,down)+1 taps reach 10).
    let margin = 128;
    let n = actual.len().min(expected.len());
    assert!(n > 4 * margin, "fixture too short to have an interior");
    assert_allclose(
        &actual[margin..n - margin],
        &expected[margin..n - margin],
        0.0,
        6e-3,
        "resample_ratio vs scipy.signal.resample_poly",
    );

    // A second, tighter bar on the bulk of the signal: the max above is a few
    // isolated ripple excursions, and an implementation that was merely
    // *roughly* right would fail this one.
    let err_rms = {
        let sum: f64 = actual[margin..n - margin]
            .iter()
            .zip(&expected[margin..n - margin])
            .map(|(a, e)| (a - e) * (a - e))
            .sum();
        (sum / (n - 2 * margin) as f64).sqrt()
    };
    assert!(
        err_rms < 1.5e-3,
        "RMS disagreement with resample_poly is {err_rms:e} on a unit-RMS signal"
    );
}

#[test]
fn resample_ratio_refuses_degenerate_inputs_with_an_empty_output() {
    assert!(resample_ratio(&[], 1.0, 0.0).is_empty(), "no input");
    assert!(resample_ratio(&[1.0; 8], 0.0, 0.0).is_empty(), "zero ratio");
    assert!(
        resample_ratio(&[1.0; 8], -1.0, 0.0).is_empty(),
        "negative ratio"
    );
    assert!(
        resample_ratio(&[1.0; 8], f64::NAN, 0.0).is_empty(),
        "NaN ratio"
    );
    assert!(
        resample_ratio(&[1.0; 8], f64::INFINITY, 0.0).is_empty(),
        "infinite ratio"
    );
    assert!(
        resample_ratio(&[1.0; 8], 1.0, f64::NAN).is_empty(),
        "NaN phase"
    );
    // A single sample has no span to resample across, but it is not an error.
    assert_eq!(resample_ratio(&[1.0], 1.0, 0.0).len(), 1);
}
