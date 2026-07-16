use paraeq_dsp::{biquad, fr, spline::NakSpline};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Designed peaking filters are stable: poles inside the unit circle.
    #[test]
    fn peaking_is_stable(fc in 20.0f64..20000.0, gain in -24.0f64..24.0, q in 0.1f64..20.0) {
        let sos = biquad::peaking(fc, gain, q, 48000.0);
        let (a1, a2) = (sos[4], sos[5]);
        // |poles| < 1  <=>  |a2| < 1 && |a1| < 1 + a2   (real-coefficient biquad)
        prop_assert!(a2.abs() < 1.0 + 1e-12);
        prop_assert!(a1.abs() < 1.0 + a2 + 1e-9);
    }

    /// Designed low-shelf filters are stable: poles inside the unit circle.
    #[test]
    fn low_shelf_is_stable(fc in 20.0f64..20000.0, gain in -24.0f64..24.0, q in 0.1f64..20.0) {
        let sos = biquad::low_shelf(fc, gain, q, 48000.0);
        let (a1, a2) = (sos[4], sos[5]);
        prop_assert!(a2.abs() < 1.0 + 1e-12);
        prop_assert!(a1.abs() < 1.0 + a2 + 1e-9);
    }

    /// Designed high-shelf filters are stable: poles inside the unit circle.
    #[test]
    fn high_shelf_is_stable(fc in 20.0f64..20000.0, gain in -24.0f64..24.0, q in 0.1f64..20.0) {
        let sos = biquad::high_shelf(fc, gain, q, 48000.0);
        let (a1, a2) = (sos[4], sos[5]);
        prop_assert!(a2.abs() < 1.0 + 1e-12);
        prop_assert!(a1.abs() < 1.0 + a2 + 1e-9);
    }

    /// Designed notch filters are stable: poles inside the unit circle.
    #[test]
    fn notch_is_stable(fc in 20.0f64..20000.0, q in 0.1f64..20.0) {
        let sos = biquad::notch(fc, q, 48000.0);
        let (a1, a2) = (sos[4], sos[5]);
        prop_assert!(a2.abs() < 1.0 + 1e-12);
        prop_assert!(a1.abs() < 1.0 + a2 + 1e-9);
    }

    /// Smoothing preserves a flat spectrum exactly (any fraction).
    #[test]
    fn smoothing_preserves_flat(level in -60.0f64..20.0, fraction in 1u32..24) {
        let freqs: Vec<f64> = (0..256).map(|i| i as f64 * 48000.0 / 512.0).collect();
        let flat = vec![level; 256];
        let out = fr::fractional_octave_smooth(&flat, &freqs, fraction);
        for v in &out {
            prop_assert!((v - level).abs() < 1e-9);
        }
    }

    /// Spline interpolates its knots.
    #[test]
    fn spline_passes_through_knots(n in 4usize..24, seed in 0u64..1000) {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut next = || { state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (state >> 33) as f64 / (1u64 << 31) as f64 - 1.0 };
        let x: Vec<f64> = (0..n).map(|i| i as f64 + 0.25 * next().abs()).collect();
        let y: Vec<f64> = (0..n).map(|_| 10.0 * next()).collect();
        let s = NakSpline::new(&x, &y).unwrap();
        for (xi, yi) in x.iter().zip(&y) {
            prop_assert!((s.eval(*xi) - yi).abs() < 1e-9);
        }
    }
}

/// OLA convolver output equals direct convolution (cross-crate check lives in
/// paraeq-engine's own tests; here we verify the FFT round-trip primitive the
/// fixtures already pin, so no duplicate — intentionally no OLA prop here).
#[test]
fn fir_design_impulse_is_finite() {
    let taps = paraeq_dsp::fir::design_fir_correction(
        &[0.0; 128],
        256,
        paraeq_dsp::fir::FirPhase::Minimum,
    );
    assert!(taps.iter().all(|t| t.is_finite()));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `is_stable` (strict Jury form) accepts every design inside the box
    /// the four stability props above pin — designed filters sit strictly
    /// inside the unit circle with margin, never on the slack boundary.
    #[test]
    fn is_stable_accepts_designed_box(fc in 20.0f64..20000.0, gain in -24.0f64..24.0, q in 0.1f64..20.0) {
        prop_assert!(biquad::is_stable(&biquad::peaking(fc, gain, q, 48000.0)));
        prop_assert!(biquad::is_stable(&biquad::low_shelf(fc, gain, q, 48000.0)));
        prop_assert!(biquad::is_stable(&biquad::high_shelf(fc, gain, q, 48000.0)));
        prop_assert!(biquad::is_stable(&biquad::notch(fc, q, 48000.0)));
    }
}

/// Hand-constructed sections on the wrong side of each Jury condition,
/// including the boundary itself (the strict form rejects |pole| == 1).
#[test]
fn is_stable_rejects_unstable_sections() {
    // |a2| >= 1: pole radius at or outside the unit circle.
    assert!(!biquad::is_stable(&[1.0, 0.0, 0.0, 1.0, 0.0, 1.01]));
    assert!(!biquad::is_stable(&[1.0, 0.0, 0.0, 1.0, 0.0, 1.0]));
    assert!(!biquad::is_stable(&[1.0, 0.0, 0.0, 1.0, 0.0, -1.0]));
    // |a1| >= a2 + 1: a real pole at or outside the unit circle.
    assert!(!biquad::is_stable(&[1.0, 0.0, 0.0, 1.0, 2.5, 0.9]));
    assert!(!biquad::is_stable(&[1.0, 0.0, 0.0, 1.0, 1.9, 0.9]));
    assert!(!biquad::is_stable(&[1.0, 0.0, 0.0, 1.0, -2.0, 1.0]));
}

/// Any non-finite coefficient fails — b-side too (a NaN numerator poisons
/// the DF2T state even with stable poles).
#[test]
fn is_stable_rejects_non_finite_coefficients() {
    let stable = [0.2, 0.3, 0.1, 1.0, -0.5, 0.25];
    assert!(biquad::is_stable(&stable));
    for i in 0..6 {
        for bad in [f64::INFINITY, f64::NAN, f64::NEG_INFINITY] {
            let mut sos = stable;
            sos[i] = bad;
            assert!(!biquad::is_stable(&sos), "sos[{i}] = {bad} must fail");
        }
    }
}

/// q = 0 makes `wa`'s alpha infinite and `row` divides inf/inf — the
/// designers stay infallible by design (spec R1-3: they are oracle-pinned);
/// `is_stable` is the wall that catches the NaN result.
#[test]
fn q_zero_designs_nan_coefficients_and_fails_is_stable() {
    let sos = biquad::peaking(1000.0, 6.0, 0.0, 48000.0);
    assert!(
        sos.iter().any(|c| c.is_nan()),
        "q = 0 must yield NaN coefficients, got {sos:?}"
    );
    assert!(!biquad::is_stable(&sos));
}
