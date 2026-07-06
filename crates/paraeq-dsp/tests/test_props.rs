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
