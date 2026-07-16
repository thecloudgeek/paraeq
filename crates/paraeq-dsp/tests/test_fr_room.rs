//! Tier 3 — analytic physics, for `fr.rs`'s additive room path.
//!
//! Separate from `test_fr.rs` on purpose: that file is Tier 1 (frozen prototype
//! fixtures) and `averaging_is_in_db_domain` is the contract that stops a future
//! session "unifying" the two averaging paths. It stays untouched.
//!
//! Un-ignore when the `fr.rs` room additions land: room-dsp/8, Stage 4
//! ("`fr.rs` additive rework (align_spl, RMS averaging, σ(f), Alvarez-Mazorra)"
//! — docs/plans/2026-07-16-rescope-implementation.md).

use paraeq_dsp::fr;

/// A bin at 60 Hz to null, and four bins inside the 200–2000 Hz alignment band.
const FREQS: [f64; 9] = [
    20.0, 60.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0,
];
/// 60 Hz — deliberately OUTSIDE the alignment band, so `align_spl` cannot move
/// it and the power average is observed uncontaminated.
const NULL_BIN: usize = 1;
const BAND: (f64, f64) = (200.0, 2000.0);

/// `n` curves, flat at 0 dB, with `k` of them nulled by `d` dB at 60 Hz only.
/// Flat inside the alignment band ⇒ every band mean is 0 ⇒ every offset is 0 ⇒
/// `align_spl` is a no-op here by construction.
fn nulled_set(n: usize, k: usize, d: f64) -> Vec<Vec<f64>> {
    (0..n)
        .map(|j| {
            let mut m = vec![0.0; FREQS.len()];
            if j < k {
                m[NULL_BIN] = d;
            }
            m
        })
        .collect()
}

fn power_avg_at_null(n: usize, k: usize, d: f64) -> f64 {
    let set = fr::align_spl(&nulled_set(n, k, d), &FREQS, BAND).unwrap();
    fr::average_measurements_rms(&set)[NULL_BIN]
}

/// The EXACT formula, not the refuted "null-immune" one:
/// `P_dB = 10·log10( ((N−k) + k·10^(d/10)) / N )`.
fn expected_db(n: usize, k: usize, d: f64) -> f64 {
    10.0 * ((((n - k) as f64) + (k as f64) * 10f64.powf(d / 10.0)) / n as f64).log10()
}

#[test]
#[ignore = "fr.rs room additions land in Stage 4 (room-dsp/8)"]
fn power_averaging_floors_correctly() {
    // Power averaging is null-RESISTANT, not null-immune. Each assertion below
    // is one clause of that claim.
    let floor = 10.0 * (4.0f64 / 5.0).log10(); // 10·log10((N−k)/N), N=5, k=1
    assert!((floor - (-0.969)).abs() < 5e-4, "floor pin: {floor}");

    // 1. The value at a FINITE depth. N=5, k=1, d=−6 dB is −0.705 dB — NOT the
    //    −0.969 floor. The floor is an asymptote, and at shallow depths the
    //    difference is most of the answer.
    let v = power_avg_at_null(5, 1, -6.0);
    assert!((v - (-0.705)).abs() < 5e-4, "N=5 k=1 d=−6 dB: {v}");
    assert!((v - expected_db(5, 1, -6.0)).abs() < 1e-12);
    assert!(v > floor, "a −6 dB null must sit well above the floor");

    // 2. Monotone approach to the floor, never attaining it.
    let mut prev = f64::INFINITY;
    for d in [-1.0, -3.0, -6.0, -10.0, -15.0, -20.0, -30.0, -60.0, -120.0] {
        let p = power_avg_at_null(5, 1, d);
        assert!(
            (p - expected_db(5, 1, d)).abs() < 1e-12,
            "closed form at d={d}"
        );
        assert!(p < prev, "must decrease monotonically with depth; d={d}");
        assert!(p > floor, "must never attain the floor; d={d} gave {p}");
        prev = p;
    }

    // 3. The floor is reached within 0.01 dB by ≈ −20 dB depth. Exactly −20 dB
    //    leaves 0.0108 dB, so the spec's "≈" is doing real work — −21 dB is the
    //    first integer depth strictly inside 0.01 dB.
    assert!(power_avg_at_null(5, 1, -20.0) - floor < 0.02);
    assert!(power_avg_at_null(5, 1, -21.0) - floor < 0.01);

    // 4. k = N — a null common to EVERY position (floor bounce, SBIR, a driver
    //    notch) passes through exactly. This is correct and desirable: a null at
    //    every seat is a real feature, not spatial scatter. The bound above is
    //    benign only because k/N is normally small.
    for d in [-6.0, -20.0, -40.0] {
        let p = power_avg_at_null(5, 5, d);
        assert!(
            (p - d).abs() < 1e-12,
            "k=N must pass {d} dB through exactly; got {p}"
        );
    }
}
