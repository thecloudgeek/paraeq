mod common;
use common::Case;
use paraeq_dsp::autofit::auto_fit_parametric_eq;

#[test]
fn greedy_fit_matches_oracle_bands() {
    let c = Case::load("autofit", "two_peaks");
    let fitted = auto_fit_parametric_eq(
        &c.array("correction_db"),
        &c.array("freqs"),
        c.param_f64("sample_rate"),
        c.param_u64("max_bands") as usize,
        0.5,
    );
    let expected = c.scalar("fitted").as_array().unwrap();
    assert_eq!(fitted.len(), expected.len(), "band count");
    for (b, e) in fitted.iter().zip(expected) {
        assert_eq!(b.filter_type.as_str(), e["filter_type"].as_str().unwrap());
        let (fc, gain, q) = (
            e["fc"].as_f64().unwrap(),
            e["gain_db"].as_f64().unwrap(),
            e["q"].as_f64().unwrap(),
        );
        assert!(
            (b.fc - fc).abs() <= 1e-9 * fc.abs().max(1.0),
            "fc {} vs {fc}",
            b.fc
        );
        assert!(
            (b.gain_db - gain).abs() <= 1e-9 * gain.abs().max(1.0),
            "gain {} vs {gain}",
            b.gain_db
        );
        assert!(
            (b.q - q).abs() <= 1e-9 * q.abs().max(1.0),
            "q {} vs {q}",
            b.q
        );
    }
}
