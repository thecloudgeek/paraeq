//! Tier 1 (frozen prototype fixture `deconvolution/delta_plus_tail`) for the
//! Wiener core — through both the deprecated `deconvolve` and `deconvolve_ir`,
//! bit-for-bit — plus Tier 3 (analytic) for the time axis: a recording built
//! as `sweep ⊛ δ(t₀)` must deconvolve to a peak at exactly t₀; no oracle is
//! needed to know where a delta was placed.

mod common;
use common::{assert_allclose, Case};
// The deprecated Tier-1 entry point is exactly what this file pins.
#[allow(deprecated)]
use paraeq_dsp::deconvolution::deconvolve;
use paraeq_dsp::{
    deconvolution::{deconvolve_ir, deconvolve_ir_with_fallback},
    sweep::generate_sweep,
};

#[test]
fn wiener_deconvolution_matches_oracle() {
    let c = Case::load("deconvolution", "delta_plus_tail");
    #[allow(deprecated)]
    let ir = deconvolve(
        &c.array("recorded"),
        &c.array("sweep"),
        c.param_u64("sample_rate") as u32,
    );
    assert_allclose(&ir, &c.array("ir_out"), 1e-9, 1e-9, "deconvolved IR");
}

#[test]
fn recovers_the_true_delay() {
    let c = Case::load("deconvolution", "delta_plus_tail");
    #[allow(deprecated)]
    let ir = deconvolve(&c.array("recorded"), &c.array("sweep"), 48000);
    let argmax = ir
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    assert!(
        (argmax as i64 - 32).unsigned_abs() <= 10,
        "peak at {argmax}, true delay 32"
    );
}

/// Tier 1: the reshape is a reshape — `deconvolve_ir(..)?.samples` and the
/// deprecated `deconvolve(..)` are the same bits on the frozen fixture.
#[test]
fn deconvolve_ir_matches_deconvolve_bit_for_bit() {
    let c = Case::load("deconvolution", "delta_plus_tail");
    let recorded = c.array("recorded");
    let sweep = c.array("sweep");
    let sr = c.param_u64("sample_rate") as u32;
    #[allow(deprecated)]
    let old = deconvolve(&recorded, &sweep, sr);
    let new = deconvolve_ir(&recorded, &sweep, sr).unwrap();
    assert_eq!(old.len(), new.samples.len(), "length mismatch");
    for (i, (a, b)) in old.iter().zip(&new.samples).enumerate() {
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "bit mismatch at [{i}]: {a} vs {b}"
        );
    }
}

/// Tier 3: `recorded = sweep ⊛ δ(t₀)` (an integer delay of the sweep itself)
/// deconvolves to a symmetric kernel centred on t₀ — the regularized division
/// multiplies the spectrum by the real non-negative `|S|²/(|S|²+ε)`, an even
/// time kernel — so the parabolic-refined peak must land on t₀ almost exactly.
#[test]
fn time_axis_recovers_known_delay() {
    const SR: u32 = 48_000;
    const T0: usize = 333;
    let sweep = generate_sweep(0.5, SR, 20.0, 20_000.0);
    let mut recorded = vec![0.0; sweep.len() + T0];
    recorded[T0..].copy_from_slice(&sweep);

    let ir = deconvolve_ir(&recorded, &sweep, SR).unwrap();
    assert_eq!(ir.sample_rate, SR, "sample_rate must round-trip");
    assert!(
        (ir.peak - T0 as f64).abs() < 0.01,
        "peak {} vs true delay {T0}",
        ir.peak
    );
    let t0_s = T0 as f64 / SR as f64;
    assert!(
        (ir.peak_time_s() - t0_s).abs() < 1e-9,
        "peak_time_s {} vs {t0_s}",
        ir.peak_time_s()
    );
}

/// The structured `gate.peak_fallback` warning is surfaced (gating.rs requires
/// the measurement session to show it); a clean single-delta recording must
/// not raise it.
#[test]
fn clean_delta_reports_no_peak_fallback() {
    const SR: u32 = 48_000;
    const T0: usize = 100;
    let sweep = generate_sweep(0.25, SR, 20.0, 20_000.0);
    let mut recorded = vec![0.0; sweep.len() + T0];
    recorded[T0..].copy_from_slice(&sweep);

    let (ir, fallback) = deconvolve_ir_with_fallback(&recorded, &sweep, SR).unwrap();
    assert!(fallback.is_none(), "unexpected fallback: {fallback:?}");
    assert!((ir.peak - T0 as f64).abs() < 0.01, "peak {}", ir.peak);
}
