//! Tier 3 — analytic physics. The two-path identity is closed-form, so this
//! test cannot inherit an oracle's bugs; there is nothing here a Python
//! transcription could get wrong that the physics would not catch.

use paraeq_dsp::{
    fr,
    gating::{self, GateSpec, ImpulseResponse, LeftClamp, SweepParams},
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

// ---- detect_peak ------------------------------------------------------------

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Band-limited fractional-delay sinc (cutoff 0.1·fs, so the main lobe is wide
/// enough to be locally parabolic): the refined peak lands within 0.01 sample
/// of the true fractional delay.
#[test]
fn detect_peak_recovers_fractional_delay_sinc() {
    for p in [99.75, 100.3, 100.49] {
        let samples: Vec<f64> = (0..201).map(|i| sinc(0.1 * (i as f64 - p))).collect();
        let peak = gating::detect_peak(&samples).unwrap();
        assert!(
            (peak - p).abs() < 0.01,
            "sinc at {p}: detected {peak} (err {})",
            (peak - p).abs()
        );
        // A clean band-limited arrival must not trip the 0.5·max fallback: the
        // half-max crossing sits ~6 samples before the argmax, far under 1 ms.
        let det = gating::detect_peak_with_lead(&samples, SR as f64 / 1000.0).unwrap();
        assert!(det.fallback.is_none(), "sinc at {p} tripped the fallback");
    }
}

/// A lone impulse refines to exactly its own index (δ numerator is 0).
#[test]
fn detect_peak_exact_on_integer_delay() {
    let mut samples = vec![0.0; 256];
    samples[100] = -0.8; // sign must not matter: detection runs on |h|
    let peak = gating::detect_peak(&samples).unwrap();
    assert_eq!(peak, 100.0);
}

/// The spec's fallback case: argmax on a reflection 2 ms after a smaller
/// direct arrival. The first 0.5·max crossing (the direct sound) precedes the
/// argmax by 96 samples > 1 ms, so the fallback fires and the reported peak is
/// the direct arrival, with the structured warning returned (not raised).
#[test]
fn peak_fallback_fires_on_reflection_argmax() {
    let mut samples = vec![0.0; N];
    samples[1000] = 0.6; // direct: above 0.5·max = 0.5
    samples[1096] = 1.0; // reflection 2 ms later, the argmax
    let ir_peak = gating::detect_peak(&samples).unwrap();
    assert_eq!(ir_peak, 1000.0, "fallback must pick the direct arrival");

    let det = gating::detect_peak_with_lead(&samples, SR as f64 / 1000.0).unwrap();
    assert_eq!(det.peak, 1000.0);
    let fb = det.fallback.expect("gate.peak_fallback must be reported");
    assert_eq!(fb.argmax_index, 1096);
    assert_eq!(fb.crossing_index, 1000);
}

#[test]
fn peak_fallback_silent_on_clean_ir() {
    let mut samples = vec![0.0; N];
    samples[1000] = 0.45; // pre-ring below 0.5·max: not a crossing
    samples[1096] = 1.0;
    let det = gating::detect_peak_with_lead(&samples, SR as f64 / 1000.0).unwrap();
    assert!(
        det.fallback.is_none(),
        "clean IR must not trip the fallback"
    );
    assert_eq!(det.peak, 1096.0);
}

/// "Precedes by MORE than 1 ms" is strict: a crossing exactly one lead ahead
/// of the argmax does not fire; one sample further does.
#[test]
fn peak_fallback_boundary_is_strict() {
    let lead = 48.0; // 1 ms at 48 kHz
    let mut at_boundary = vec![0.0; N];
    at_boundary[1000] = 0.6;
    at_boundary[1048] = 1.0; // exactly 48 samples: NOT more than 1 ms
    let det = gating::detect_peak_with_lead(&at_boundary, lead).unwrap();
    assert!(det.fallback.is_none());
    assert_eq!(det.peak, 1048.0);

    let mut past_boundary = vec![0.0; N];
    past_boundary[1000] = 0.6;
    past_boundary[1049] = 1.0; // 49 samples: fires
    let det = gating::detect_peak_with_lead(&past_boundary, lead).unwrap();
    assert!(det.fallback.is_some());
    assert_eq!(det.peak, 1000.0);
}

#[test]
fn detect_peak_rejects_degenerate_input() {
    assert!(gating::detect_peak(&[]).is_err(), "empty IR");
    assert!(gating::detect_peak(&[0.0; 16]).is_err(), "all-zero IR");
    assert!(
        gating::detect_peak(&[1.0, f64::NAN, 0.0]).is_err(),
        "non-finite IR"
    );
}

// ---- apply_gate: the left clamp ---------------------------------------------

/// The ParaEQ-specific trap: deconvolve() puts the peak at ~46 ms, so REW's
/// 125 ms default left window is physically impossible — the clamp must bind
/// at the peak time and the slice must remain non-empty.
#[test]
fn left_clamp_peak_too_early() {
    let mut samples = vec![0.0; N];
    samples[2208] = 1.0; // 46 ms at 48 kHz
    let ir = ImpulseResponse {
        samples,
        peak: 2208.0,
        sample_rate: SR,
    };
    let spec = GateSpec {
        left_ms: 125.0,
        right_ms: 10.0,
        sweep: None,
        window: WindowSpec {
            left: WindowKind::Rect,
            right: WindowKind::Rect,
        },
    };
    let (gated, report) = gating::apply_gate(&ir, &spec).unwrap();

    assert_eq!(report.applied_left_ms, 46.0);
    match report.clamped_left {
        Some(LeftClamp::PeakTooEarly { peak_ms }) => {
            assert!((peak_ms - 46.0).abs() < 1e-12);
        }
        other => panic!("expected PeakTooEarly, got {other:?}"),
    }
    assert!(!gated.is_empty(), "clamped slice must be non-empty");
    assert_eq!(gated.len(), 2208 + 480 + 1); // 46 ms + 10 ms + the peak
    assert_eq!(gated[2208], 1.0, "peak sits at left_samples");
    assert!((report.min_valid_freq_hz - 100.0).abs() < 1e-9); // 1000 / 10 ms
                                                              // apply_gate reports the 1/6-octave-aware limit for the applied right gate.
    assert!(
        (report.resolution_limit_hz - gating::resolution_limit_hz(0.010, 6)).abs() < 1e-9,
        "resolution_limit_hz must be the 1/6-octave figure for T_right"
    );
}

/// With sweep params given, the Farina H2 arrival bounds the left window; when
/// it does not bind it is still reported.
#[test]
fn left_clamp_harmonic_bound() {
    let mut samples = vec![0.0; N];
    samples[2208] = 1.0; // 46 ms
    let ir = ImpulseResponse {
        samples,
        peak: 2208.0,
        sample_rate: SR,
    };
    let window = WindowSpec {
        left: WindowKind::Rect,
        right: WindowKind::Rect,
    };

    // Binding: a 0.1 s sweep puts H2 only ~10.03 ms ahead of the peak, tighter
    // than both the 125 ms request and the 46 ms peak time.
    let sweep = SweepParams {
        duration_s: 0.1,
        f1: 20.0,
        f2: 20000.0,
    };
    let dt2_ms = gating::farina_h2_bound_s(&sweep) * 1000.0;
    assert!(dt2_ms < 46.0);
    let spec = GateSpec {
        left_ms: 125.0,
        right_ms: 10.0,
        sweep: Some(sweep),
        window,
    };
    let (gated, report) = gating::apply_gate(&ir, &spec).unwrap();
    assert!(!gated.is_empty());
    assert!((report.applied_left_ms - dt2_ms).abs() < 1e-12);
    match report.clamped_left {
        Some(LeftClamp::HarmonicBound { dt2_ms: d }) => assert!((d - dt2_ms).abs() < 1e-12),
        other => panic!("expected HarmonicBound, got {other:?}"),
    }
    assert!((report.harmonic_bound_ms.unwrap() - dt2_ms).abs() < 1e-12);

    // Not binding: a 5 s sweep's H2 bound (~502 ms) is looser than the 30 ms
    // request; no clamp, but the bound is still reported.
    let sweep = SweepParams {
        duration_s: 5.0,
        f1: 20.0,
        f2: 20000.0,
    };
    let dt2_ms = gating::farina_h2_bound_s(&sweep) * 1000.0;
    let spec = GateSpec {
        left_ms: 30.0,
        right_ms: 10.0,
        sweep: Some(sweep),
        window,
    };
    let (_, report) = gating::apply_gate(&ir, &spec).unwrap();
    assert!((report.applied_left_ms - 30.0).abs() < 1e-12);
    assert!(report.clamped_left.is_none());
    assert!((report.harmonic_bound_ms.unwrap() - dt2_ms).abs() < 1e-12);
}

// ---- apply_gate: tapers and slicing -----------------------------------------

/// Gating an all-ones IR reads the applied window back verbatim: left kind on
/// the pre-peak side, right kind post-peak, both with their inner edge (1.0)
/// at the peak and declining outward. Pins orientation AND independence.
#[test]
fn tapers_are_oriented_and_independent() {
    let ir = ImpulseResponse {
        samples: vec![1.0; 1000],
        peak: 500.0,
        sample_rate: SR,
    };
    let spec = GateSpec {
        left_ms: 2.0,  // 96 samples
        right_ms: 1.0, // 48 samples
        sweep: None,
        window: WindowSpec {
            left: WindowKind::Hann,
            right: WindowKind::BlackmanHarris,
        },
    };
    let (gated, report) = gating::apply_gate(&ir, &spec).unwrap();
    assert!(report.clamped_left.is_none());

    let (l, r) = (96, 48);
    assert_eq!(gated.len(), l + r + 1);
    assert_eq!(gated[l], 1.0, "peak sample carries weight 1.0");
    let left_taper = WindowKind::Hann.half_taper(l + 1);
    let right_taper = WindowKind::BlackmanHarris.half_taper(r + 1);
    for k in 1..=l {
        assert_eq!(gated[l - k], left_taper[k], "left taper at k={k}");
    }
    for k in 1..=r {
        assert_eq!(gated[l + k], right_taper[k], "right taper at k={k}");
    }
    // Outer edges: Hann reaches exactly 0, Blackman-Harris its ~6e-5 floor.
    assert_eq!(gated[0], 0.0);
    assert!(gated[l + r] > 0.0 && gated[l + r] < 1e-4);
}

/// A right window running past the recording's end zero-fills: the contract
/// fixes the output length at left + right + 1 regardless.
#[test]
fn gate_zero_fills_past_recording_end() {
    let samples: Vec<f64> = (0..100).map(|i| (i + 1) as f64).collect();
    let ir = ImpulseResponse {
        samples: samples.clone(),
        peak: 90.0,
        sample_rate: SR,
    };
    let spec = GateSpec {
        left_ms: 0.5,  // 24 samples
        right_ms: 1.0, // 48 samples, but only 9 exist after the peak
        sweep: None,
        window: WindowSpec {
            left: WindowKind::Rect,
            right: WindowKind::Rect,
        },
    };
    let (gated, _) = gating::apply_gate(&ir, &spec).unwrap();
    assert_eq!(gated.len(), 24 + 48 + 1);
    assert_eq!(gated[24], samples[90], "peak at left_samples");
    assert_eq!(gated[33], samples[99], "last real sample");
    assert!(
        gated[34..].iter().all(|&v| v == 0.0),
        "past the end is zero-filled"
    );
}

#[test]
fn apply_gate_rejects_degenerate_input() {
    let ok_ir = ImpulseResponse {
        samples: vec![0.0, 1.0, 0.0, 0.0],
        peak: 1.0,
        sample_rate: SR,
    };
    let ok_spec = GateSpec {
        left_ms: 0.0,
        right_ms: 0.02,
        sweep: None,
        window: WindowSpec::default(),
    };
    assert!(gating::apply_gate(&ok_ir, &ok_spec).is_ok());

    let mut spec = ok_spec;
    spec.right_ms = 0.0;
    assert!(gating::apply_gate(&ok_ir, &spec).is_err(), "right_ms == 0");
    let mut spec = ok_spec;
    spec.left_ms = -1.0;
    assert!(gating::apply_gate(&ok_ir, &spec).is_err(), "negative left");
    let mut spec = ok_spec;
    spec.sweep = Some(SweepParams {
        duration_s: 5.0,
        f1: 20000.0,
        f2: 20.0,
    });
    assert!(gating::apply_gate(&ok_ir, &spec).is_err(), "f2 <= f1");

    let mut ir = ok_ir.clone();
    ir.peak = 10.0; // beyond the last sample
    assert!(gating::apply_gate(&ir, &ok_spec).is_err(), "peak past end");
    let mut ir = ok_ir.clone();
    ir.samples.clear();
    assert!(gating::apply_gate(&ir, &ok_spec).is_err(), "empty IR");
}

// ---- the reported numbers ---------------------------------------------------

#[test]
fn scalar_formulas_match_spec() {
    // A 6 ms gate is blind below ~167 Hz — the modal region.
    assert!((gating::min_valid_freq(6.0) - 1000.0 / 6.0).abs() < 1e-12);
    assert!((gating::min_valid_freq(6.0) - 166.67).abs() < 0.01);

    // A 10 ms gate is only 1/6-octave-valid above ~865 Hz.
    let expected = (1.0 / 0.010) / (2f64.powf(1.0 / 12.0) - 2f64.powf(-1.0 / 12.0));
    assert!((gating::resolution_limit_hz(0.010, 6) - expected).abs() < 1e-9);
    assert!((gating::resolution_limit_hz(0.010, 6) - 865.136).abs() < 0.01);

    // A 5 s, 20 Hz–20 kHz sweep puts H2 only ~502 ms before the peak.
    let sweep = SweepParams {
        duration_s: 5.0,
        f1: 20.0,
        f2: 20000.0,
    };
    let dt2 = gating::farina_h2_bound_s(&sweep);
    assert!((dt2 - 5.0 * 2f64.ln() / 1000f64.ln()).abs() < 1e-12);
    assert!((dt2 - 0.5017).abs() < 1e-4);
}
