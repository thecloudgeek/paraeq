//! Tier 3 — analytic physics. The two-path identity is closed-form, so this
//! test cannot inherit an oracle's bugs; there is nothing here a Python
//! transcription could get wrong that the physics would not catch.
//!
//! Un-ignore when `gating.rs` lands: room-dsp/4, Stage 3 ("`window.rs` +
//! `gating.rs` against pre-written Tier-3 tests" —
//! docs/plans/2026-07-16-rescope-implementation.md).

use paraeq_dsp::{
    fr,
    gating::{self, GateSpec, ImpulseResponse},
    window::{WindowKind, WindowSpec},
};
use std::f64::consts::PI;

const SR: u32 = 48_000;
/// 10 ms — early enough to be realistic (deconvolve puts the peak at ~46–64 ms)
/// while leaving room for a 5 ms left window that does not clamp.
const PEAK: usize = 480;
/// 1 ms.
const TAU_SAMPLES: usize = 48;
const G: f64 = 0.5;
/// 4800 @ 48 kHz => Δf = 10 Hz, so the comb's 1000 Hz peaks and its nulls at
/// 500 Hz + k·1000 land on exact bins rather than between them.
const N: usize = 4800;

fn two_path_ir() -> ImpulseResponse {
    let mut samples = vec![0.0; N];
    samples[PEAK] = 1.0;
    samples[PEAK + TAU_SAMPLES] = G;
    ImpulseResponse {
        samples,
        peak: PEAK as f64,
        sample_rate: SR,
    }
}

fn tau_s() -> f64 {
    TAU_SAMPLES as f64 / SR as f64
}

/// `h = δ(t₀) + g·δ(t₀+τ)` ⇒ `|H| = |1 + g·e^{−jωτ}|`, independent of t₀.
fn comb_db(f: f64) -> f64 {
    let w = 2.0 * PI * f * tau_s();
    20.0 * (1.0 + G * w.cos()).hypot(G * w.sin()).log10()
}

#[test]
#[ignore = "gating.rs lands in Stage 3 (room-dsp/4)"]
fn gate_kills_comb() {
    let ir = two_path_ir();

    // 1. Ungated, the two-path identity holds exactly: a comb with peaks 1+g,
    //    nulls 1−g, spacing 1/τ.
    let (freqs, mag_db) = fr::compute_frequency_response(&ir.samples, SR, None).unwrap();
    for (f, m) in freqs.iter().zip(&mag_db) {
        let e = comb_db(*f);
        assert!(
            (m - e).abs() < 1e-12,
            "ungated |H| at {f} Hz: {m} dB vs {e} dB"
        );
    }
    // Peaks at k/τ (bins 0, 100, ...), nulls halfway between (bins 50, 150, ...).
    let peak_db = 20.0 * (1.0 + G).log10();
    let null_db = 20.0 * (1.0 - G).log10();
    for k in 0..4 {
        let pk = 100 * k;
        let nl = 50 + 100 * k;
        assert!((freqs[pk] - 1000.0 * k as f64).abs() < 1e-9);
        assert!(
            (mag_db[pk] - peak_db).abs() < 1e-12,
            "comb peak at bin {pk}"
        );
        assert!(
            (mag_db[nl] - null_db).abs() < 1e-12,
            "comb null at bin {nl}"
        );
    }

    // 2. A right window SHORTER than τ excludes the reflection entirely, so the
    //    gated slice is a lone impulse and the comb is gone — exactly flat.
    let spec = GateSpec {
        left_ms: 5.0,  // < peak_time_ms (10.0), so no clamp
        right_ms: 0.5, // < τ (1.0 ms)
        sweep: None,
        window: WindowSpec {
            left: WindowKind::Rect,
            right: WindowKind::Rect,
        },
    };
    let (gated, report) = gating::apply_gate(&ir, &spec).unwrap();

    let left_samples = 240; // 5.0 ms
    let right_samples = 24; // 0.5 ms
    assert_eq!(gated.len(), left_samples + right_samples + 1);
    assert!(
        (gated[left_samples] - 1.0).abs() < 1e-12,
        "peak sits at left_samples"
    );

    let (_, gated_db) = fr::compute_frequency_response(&gated, SR, None).unwrap();
    for (i, m) in gated_db.iter().enumerate() {
        assert!(
            m.abs() < 1e-12,
            "gated |H| must be flat 0 dB; bin {i} = {m} dB"
        );
    }

    // 3. The gate reports honestly.
    assert!((report.applied_left_ms - 5.0).abs() < 1e-12);
    assert!((report.applied_right_ms - 0.5).abs() < 1e-12);
    assert!(report.clamped_left.is_none(), "5 ms < 10 ms peak: no clamp");
    assert!(
        report.harmonic_bound_ms.is_none(),
        "sweep: None skips the Farina check"
    );
    // 1/T_right — the absolute floor.
    assert!((report.min_valid_freq_hz - 2000.0).abs() < 1e-9);
    assert!(
        report.resolution_limit_hz >= report.min_valid_freq_hz,
        "the 1/N-octave limit is always >= the 1/T floor"
    );
}
