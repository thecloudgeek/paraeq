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
        TRANSITION_FALLBACK_HZ,
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

/// An IR with a flat early-reflection plateau (dense early energy, ~60 ms) then
/// a reverberant decay is exactly why ISO 3382 fits over [−5, −25] dB rather
/// than from 0: the plateau keeps the Schroeder curve high through the first
/// few dB, so a [0, −20] fit spans the plateau's curved integration and reports
/// a badly inflated T60, while the −5 dB skip lands the fit in the clean
/// reverberant slope. A single-exponential can't tell the two windows apart
/// (it's linear from the top); this can — measured, the wrong window nearly
/// doubles the reported T60.
#[test]
fn early_reflection_plateau_recovers_the_reverberant_tail() {
    const T60_REVERB: f64 = 0.15;
    const FLAT_MS: f64 = 60.0;
    let i_flat = (FLAT_MS / 1000.0 * SR as f64) as usize;
    let k = 1000f64.ln() / T60_REVERB;
    let n = (0.5 * SR as f64) as usize;
    let w = noise(n, 0x2545_F491_4F6C_DD1D);
    let samples: Vec<f64> = w
        .iter()
        .enumerate()
        .map(|(i, wi)| {
            let env = if i < i_flat {
                1.0
            } else {
                (-k * (i - i_flat) as f64 / SR as f64).exp()
            };
            wi * env
        })
        .collect();
    let ir = ImpulseResponse {
        samples,
        peak: 0.0,
        sample_rate: SR,
    };
    let est = estimate_t60(&ir, None).unwrap();
    // The [−5, −25] fit recovers ~T60_REVERB; a [0, −20] fit dragged through the
    // plateau reports ~0.28 s (measured), well outside this bound.
    assert!(
        (est.t60_s - T60_REVERB).abs() / T60_REVERB < 0.15,
        "plateau T60 {} vs reverberant {T60_REVERB} — the fit window spanned the plateau?",
        est.t60_s
    );
}

/// A decay with a deterministic ripple on top of the exponential bends the
/// Schroeder curve enough to drop `fit_r2` into (0.5, 0.95): the estimator must
/// call it unusable at the 0.95 gate. Pure noise is rejected over-determinedly
/// (its r² is already ~0 AND the floor probe fails), so it can't pin the
/// threshold; this marginal case can.
#[test]
fn marginal_fit_quality_is_unusable() {
    const T60: f64 = 0.35;
    let k = 1000f64.ln() / T60;
    let n = (0.45 * SR as f64) as usize;
    let w = noise(n, 0x1234_5678_9ABC_DEF1);
    let samples: Vec<f64> = w
        .iter()
        .enumerate()
        .map(|(i, wi)| {
            let t = i as f64 / SR as f64;
            // Exponential with a slow, deep amplitude ripple (deterministic, so
            // fit_r2 is reproducible): bends the decay off the fit line.
            let ripple = 1.0 + 0.85 * (2.0 * std::f64::consts::PI * 18.0 * t).sin();
            wi * (-k * t).exp() * ripple.abs()
        })
        .collect();
    let ir = ImpulseResponse {
        samples,
        peak: 0.0,
        sample_rate: SR,
    };
    let est = estimate_t60(&ir, None).unwrap();
    // The test only bites if the fit really landed in the marginal band — assert
    // that first, so a future change that moves r² can't make this vacuous.
    assert!(
        est.fit_r2 > 0.5 && est.fit_r2 < 0.95,
        "fit_r2 {} not in the marginal (0.5, 0.95) band this test needs",
        est.fit_r2
    );
    assert!(
        !est.usable,
        "a marginal-r² fit must be unusable at the 0.95 gate: {est:?}"
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

// ------------------------------------------------ The 200 Hz fallback --

/// The whole file, as text, for the two naming assertions below.
///
/// The scan stops at an inline `#[cfg(test)]` module if one is ever added: a
/// test module is entitled to spell whatever literal it is asserting, and the
/// claim here is about the SHIPPING code. There is no such module today, so
/// today this is the whole file.
fn room_rs_source_lines() -> Vec<String> {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/room.rs"))
        .expect("room.rs is readable");
    src.lines()
        .take_while(|line| !line.trim_start().starts_with("#[cfg(test)]"))
        .map(str::to_string)
        .collect()
}

/// `docs/specs/2026-07-15-wizard-design.md:566`: if excess-group-delay masking
/// slips to v1.1, "the 200 Hz **fallback constant** is what ships — and it must
/// be labelled a fallback in the code, the UI, and the log, never a rule". This
/// is the CODE leg — the number has a NAME, and the name is the only place the
/// number is spelled.
///
/// **The exclusion is stated, because the constant itself has to spell its
/// value.** Exactly one line may match `200.0` and that line must be the
/// definition; a named constant spells its number once. `100.0` and `400.0` get
/// no exclusion at all — they are `0.5 *` and `2.0 *` the constant, so a bare
/// one of either would be an unlinked literal by definition.
#[test]
fn the_transition_fallback_constant_is_named_and_used_at_every_site() {
    let code = room_rs_source_lines();

    let spelled: Vec<&String> = code.iter().filter(|line| line.contains("200.0")).collect();
    assert_eq!(
        spelled.len(),
        1,
        "`200.0` belongs on the constant's definition line and nowhere else; found {spelled:?}"
    );
    assert!(
        spelled[0].contains("pub const TRANSITION_FALLBACK_HZ"),
        "the one `200.0` in room.rs is not the constant's definition: {:?}",
        spelled[0]
    );
    for bare in ["100.0", "400.0"] {
        assert!(
            !code.iter().any(|line| line.contains(bare)),
            "`{bare}` is half/double the named constant, not a literal of its own"
        );
    }

    // The falsifier for a rename that changes behaviour: unchanged numbers,
    // named source.
    assert_eq!(TRANSITION_FALLBACK_HZ, 200.0);
    let r = transition_range(None, None);
    assert_eq!(r.low_hz, 100.0);
    assert_eq!(r.center_hz, 200.0);
    assert_eq!(r.high_hz, 400.0);
    assert_eq!(r.source, TransitionSource::Fallback);
}

/// The doc comment is half the requirement: `wizard-design.md:566` legislates
/// against the number being read as a rule, and the only thing that stops a
/// later tidy-up trimming the doc to "the transition frequency" is a test that
/// reads it.
#[test]
fn the_transition_fallback_doc_says_fallback_and_not_rule() {
    let code = room_rs_source_lines();
    let at = code
        .iter()
        .position(|line| line.contains("pub const TRANSITION_FALLBACK_HZ"))
        .expect("room.rs defines the constant");
    let doc = code[..at]
        .iter()
        .rev()
        .take_while(|line| line.trim_start().starts_with("///"))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();

    for required in ["fallback", "never a rule", "wizard-design.md:566"] {
        assert!(
            doc.contains(required),
            "the constant's doc no longer says {required:?}:\n{doc}"
        );
    }
}
