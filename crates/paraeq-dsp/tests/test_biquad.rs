mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::biquad;

#[test]
fn coefficient_matrix_matches_oracle() {
    let c = Case::load("biquad", "matrix");
    let sr = c.param_f64("sample_rate");
    let cases = c.scalar("cases").as_array().expect("cases array");
    assert!(!cases.is_empty());
    for case in cases {
        let kind = case["kind"].as_str().unwrap();
        let fc = case["fc"].as_f64().unwrap();
        let gain = case["gain_db"].as_f64().unwrap();
        let q = case["q"].as_f64().unwrap();
        let expected: Vec<f64> = case["sos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        let sos = match kind {
            "peaking" => biquad::peaking(fc, gain, q, sr),
            "low_shelf" => biquad::low_shelf(fc, gain, q, sr),
            "high_shelf" => biquad::high_shelf(fc, gain, q, sr),
            "notch" => biquad::notch(fc, q, sr),
            other => panic!("unknown kind {other}"),
        };
        assert_allclose(
            &sos,
            &expected,
            0.0,
            1e-12,
            &format!("{kind} fc={fc} q={q}"),
        );
    }
}

#[test]
fn cascade_response_matches_oracle() {
    let c = Case::load("biquad", "matrix");
    let sr = c.param_f64("sample_rate");
    let freqs = c.array("resp_freqs");
    let sos = biquad::peaking(1000.0, 6.0, 1.0, sr); // the case the generator used
    let resp = biquad::sos_frequency_response_db(&[sos], &freqs, sr);
    assert_allclose(&resp, &c.array("resp_db"), 1e-9, 1e-12, "sosfreqz response");
}

/// Independent cross-check against the `biquad` crate (spec: test-only).
/// NOTE: the dev-dep crate and our module share the name `biquad` — use the
/// leading-`::` extern-crate path for the dev-dep to disambiguate.
#[test]
fn peaking_agrees_with_biquad_crate() {
    let coeffs = ::biquad::Coefficients::<f64>::from_params(
        ::biquad::Type::PeakingEQ(6.0),
        ::biquad::Hertz::<f64>::from_hz(48000.0).unwrap(),
        ::biquad::Hertz::<f64>::from_hz(1000.0).unwrap(),
        1.0,
    )
    .unwrap();
    let ours = paraeq_dsp::biquad::peaking(1000.0, 6.0, 1.0, 48000.0);
    for (a, b) in [coeffs.b0, coeffs.b1, coeffs.b2, 1.0, coeffs.a1, coeffs.a2]
        .iter()
        .zip(&ours)
    {
        assert!((a - b).abs() < 1e-12, "crate {a} vs ours {b}");
    }
}
