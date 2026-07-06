mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::fir::{design_fir_correction, FirPhase};

fn run_case(name: &str, phase: FirPhase, atol: f64) {
    let c = Case::load("fir", name);
    let taps = design_fir_correction(
        &c.array("correction_db"),
        c.param_u64("n_taps") as usize,
        phase,
    );
    assert_allclose(&taps, &c.array("taps"), 0.0, atol, name);
}

#[test]
fn linear_phase_matches_oracle() {
    run_case("linear_512", FirPhase::Linear, 1e-9);
    run_case("linear_4096", FirPhase::Linear, 1e-9);
}

#[test]
fn minimum_phase_taps_match_oracle() {
    // log/exp cepstrum amplifies FFT rounding: 1e-6 abs per spec
    run_case("minimum_512", FirPhase::Minimum, 1e-6);
    run_case("minimum_4096", FirPhase::Minimum, 1e-6);
}

/// The perceptually true criterion (spec): the min-phase filter's magnitude
/// response must match the oracle taps' response within 0.01 dB, 20 Hz-20 kHz.
#[test]
fn minimum_phase_response_within_001_db() {
    for name in ["minimum_512", "minimum_4096"] {
        let c = Case::load("fir", name);
        let ours = design_fir_correction(
            &c.array("correction_db"),
            c.param_u64("n_taps") as usize,
            FirPhase::Minimum,
        );
        let theirs = c.array("taps");
        let sr = 48000.0;
        for k in 0..200 {
            // 200-point log grid, 20 Hz .. 20 kHz
            let f = 20.0 * (20000.0f64 / 20.0).powf(k as f64 / 199.0);
            let a = fir_mag_db(&ours, f, sr);
            let b = fir_mag_db(&theirs, f, sr);
            assert!(
                (a - b).abs() < 0.01,
                "{name} @ {f:.1} Hz: {a:.5} vs {b:.5} dB"
            );
        }
    }
}

fn fir_mag_db(taps: &[f64], f: f64, sr: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f / sr;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, t) in taps.iter().enumerate() {
        re += t * (w * n as f64).cos();
        im -= t * (w * n as f64).sin();
    }
    20.0 * (re * re + im * im).sqrt().max(1e-30).log10()
}
