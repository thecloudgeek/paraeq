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

/// The sub-sample refinement must SURVIVE into `ir.peak` — a `peak.round()`
/// anywhere in the chain would silently discard it, and every other test here
/// uses an integer delay where rounding is a no-op. A half-sample fractional
/// delay (equal energy at t₀ and t₀+1) deconvolves to a kernel symmetric about
/// t₀+0.5, so the refined peak must carry a ~0.5 fractional part.
#[test]
fn time_axis_preserves_sub_sample_refinement() {
    const SR: u32 = 48_000;
    const T0: usize = 333;
    let sweep = generate_sweep(0.5, SR, 20.0, 20_000.0);
    // recorded = ½·δ(T0) ⊛ sweep + ½·δ(T0+1) ⊛ sweep: a half-sample delay.
    let mut recorded = vec![0.0; sweep.len() + T0 + 1];
    for (i, &s) in sweep.iter().enumerate() {
        recorded[T0 + i] += 0.5 * s;
        recorded[T0 + 1 + i] += 0.5 * s;
    }
    let ir = deconvolve_ir(&recorded, &sweep, SR).unwrap();
    assert!(
        (ir.peak - (T0 as f64 + 0.5)).abs() < 0.05,
        "peak {} should refine to ~{}.5",
        ir.peak,
        T0
    );
    assert!(
        (ir.peak - ir.peak.round()).abs() > 0.3,
        "peak {} lost its fractional part — refinement was rounded away",
        ir.peak
    );
}

/// The 1 ms fallback-lead threshold must scale with the sample rate: at 96 kHz
/// a 70-sample (0.73 ms) pre-peak arrival is INSIDE 1 ms and must NOT trip the
/// fallback, but a rate-blind 48-sample lead (1 ms at 48 kHz only) would fire
/// spuriously and shift t₀ onto the earlier arrival. Every other test runs at
/// 48 kHz where 48 == sample_rate/1000, so the constant hides.
#[test]
fn fallback_lead_scales_with_sample_rate() {
    const SR: u32 = 96_000;
    const LEAD: usize = 70; // 0.73 ms at 96 kHz — inside 1 ms (96 samples).
    let sweep = generate_sweep(0.25, SR, 20.0, 20_000.0);
    // A 0.7 early arrival (crosses 0.5·max) then a 1.0 later arrival (argmax).
    let mut recorded = vec![0.0; sweep.len() + 200 + LEAD];
    for (i, &s) in sweep.iter().enumerate() {
        recorded[200 + i] += 0.7 * s;
        recorded[200 + LEAD + i] += 1.0 * s;
    }
    let (ir, fallback) = deconvolve_ir_with_fallback(&recorded, &sweep, SR).unwrap();
    assert!(
        fallback.is_none(),
        "0.73 ms lead is inside 1 ms at 96 kHz; fallback must not fire: {fallback:?}"
    );
    assert!(
        (ir.peak - (200.0 + LEAD as f64)).abs() < 1.0,
        "peak {} should stay on the argmax arrival at ~{}",
        ir.peak,
        200 + LEAD
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
