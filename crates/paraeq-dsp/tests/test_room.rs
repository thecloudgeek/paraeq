//! Tier 2 (`fixtures/room/schroeder_decay`, a numpy reverse-cumsum reference)
//! for `schroeder_decay_db`; Tier 3 (analytic) for the rest: a synthetic
//! `noise · exp(−t·ln(1000)/T60)` IR has a KNOWN T60 (−60 dB is a factor
//! 10⁻³ in amplitude), and `f_s = 2000·√(T60/V)` is closed-form.

mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::{
    gating::ImpulseResponse,
    room::{
        estimate_t60, schroeder_decay_db, schroeder_frequency, transition_range, TransitionSource,
    },
};

const SR: u32 = 48_000;

/// Deterministic xorshift64* noise in [−1, 1): keeps the crate free of a rand
/// dev-dependency and the Tier-3 assertions reproducible. Seed must be
/// non-zero.
fn noise(n: usize, mut state: u64) -> Vec<f64> {
    (0..n)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let r = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
            (r >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        })
        .collect()
}

/// `noise · exp(−t·ln(1000)/T60)`, `duration_s` long at [`SR`].
fn synthetic_decay(t60_s: f64, duration_s: f64, seed: u64) -> ImpulseResponse {
    let n = (duration_s * SR as f64) as usize;
    let samples = noise(n, seed)
        .iter()
        .enumerate()
        .map(|(i, w)| w * (-(i as f64 / SR as f64) * 1000f64.ln() / t60_s).exp())
        .collect();
    ImpulseResponse {
        samples,
        peak: 0.0,
        sample_rate: SR,
    }
}

// ---------------------------------------------------------------- Tier 2 --

#[test]
fn schroeder_decay_matches_numpy_reverse_cumsum() {
    let c = Case::load("room", "schroeder_decay");
    let decay = schroeder_decay_db(&c.array("ir"));
    assert_allclose(&decay, &c.array("decay_db"), 0.0, 1e-12, "schroeder decay");
}

// ---------------------------------------------------------------- Tier 3 --

#[test]
fn recovers_known_t60_within_5_percent() {
    const T60: f64 = 0.3;
    let ir = synthetic_decay(T60, 0.35, 0x9E37_79B9_7F4A_7C15);
    let est = estimate_t60(&ir, None).unwrap();
    assert!(
        est.usable,
        "clean exponential decay must be usable: {est:?}"
    );
    assert!(est.fit_r2 > 0.95, "fit_r2 {}", est.fit_r2);
    assert!(
        (est.t60_s - T60).abs() / T60 < 0.05,
        "t60 {} vs known {T60}",
        est.t60_s
    );
    // T20 is the raw fitted 20 dB time; T60 is its ×3 extrapolation.
    assert_eq!(est.t60_s, 3.0 * est.t20_s);
}

/// The Tier-2 fixture's IR was generated with a known T60 of 0.12 s
/// (`gen_schroeder`); the estimator must recover the parameter it was built
/// from.
#[test]
fn fixture_ir_t60_is_recovered() {
    let c = Case::load("room", "schroeder_decay");
    let ir = ImpulseResponse {
        samples: c.array("ir"),
        peak: 0.0,
        sample_rate: c.param_u64("sample_rate") as u32,
    };
    let known = c.param_f64("t60_s");
    let est = estimate_t60(&ir, None).unwrap();
    assert!(est.usable, "{est:?}");
    assert!(
        (est.t60_s - known).abs() / known < 0.05,
        "t60 {} vs known {known}",
        est.t60_s
    );
}

/// Pure stationary noise has no decay; its Schroeder curve only reaches
/// −25 dB in the end-of-integration plunge. The estimate must disqualify
/// itself — Ok, but `usable: false`.
#[test]
fn pure_noise_is_unusable() {
    let ir = ImpulseResponse {
        samples: noise(8192, 0xD1B5_4A32_D192_ED03),
        peak: 0.0,
        sample_rate: SR,
    };
    let est = estimate_t60(&ir, None).unwrap();
    assert!(!est.usable, "pure noise must not be usable: {est:?}");
}

/// Band-limited T60 (octave-band filtering per ISO 3382) is deferred; asking
/// for it must be an explicit error, never a broadband number wearing a band
/// label.
#[test]
fn band_limited_estimate_is_an_explicit_error() {
    let ir = synthetic_decay(0.3, 0.35, 0x9E37_79B9_7F4A_7C15);
    assert!(estimate_t60(&ir, Some((500.0, 2000.0))).is_err());
}

#[test]
fn schroeder_frequency_matches_the_spec_example() {
    let f = schroeder_frequency(0.4, 50.0);
    assert_eq!(f, 2000.0 * (0.4f64 / 50.0).sqrt());
    assert!((f - 178.9).abs() < 0.1, "f_s {f} vs the spec's ≈178.9");
}

#[test]
fn measured_transition_range_brackets_f_s() {
    let r = transition_range(Some(0.4), Some(50.0));
    let f_s = schroeder_frequency(0.4, 50.0);
    assert_eq!(r.low_hz, 0.5 * f_s);
    assert_eq!(r.center_hz, f_s);
    assert_eq!(r.high_hz, 2.0 * f_s);
    assert_eq!(
        r.source,
        TransitionSource::Measured {
            t60_s: 0.4,
            volume_m3: 50.0
        }
    );
}

#[test]
fn unknown_inputs_fall_back_to_200_hz_center() {
    for (t60, vol) in [(None, None), (Some(0.4), None), (None, Some(50.0))] {
        let r = transition_range(t60, vol);
        assert_eq!(r.low_hz, 100.0);
        assert_eq!(r.center_hz, 200.0);
        assert_eq!(r.high_hz, 400.0);
        assert_eq!(r.source, TransitionSource::Fallback);
    }
}
