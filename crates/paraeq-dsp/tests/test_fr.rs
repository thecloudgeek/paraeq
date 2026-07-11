mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::fr;

#[test]
fn stereo_response_matches_oracle_per_channel() {
    let c = Case::load("fr", "stereo_decay");
    let ir = c.array2("ir");
    let mag = c.array2("mag_db");
    let n_fft = c.param_u64("n_fft") as usize;
    let sr = c.param_u64("sample_rate") as u32;
    let (freqs, mag0) = fr::compute_frequency_response(&ir.col(0), sr, Some(n_fft)).unwrap();
    let (_, mag1) = fr::compute_frequency_response(&ir.col(1), sr, Some(n_fft)).unwrap();
    assert_allclose(&freqs, &c.array("freqs"), 1e-12, 1e-9, "rfftfreq");
    assert_allclose(&mag0, &mag.col(0), 1e-9, 1e-9, "mag ch0");
    assert_allclose(&mag1, &mag.col(1), 1e-9, 1e-9, "mag ch1");
}

#[test]
fn smoothing_matches_oracle_all_fractions() {
    for (case, fraction) in [("smooth_1_3", 3u32), ("smooth_1_6", 6), ("smooth_1_12", 12)] {
        let c = Case::load("fr", case);
        let out = fr::fractional_octave_smooth(&c.array("mag_db"), &c.array("freqs"), fraction);
        assert_allclose(&out, &c.array("smoothed"), 1e-9, 1e-9, case);
    }
}

#[test]
fn averaging_is_in_db_domain() {
    let c = Case::load("fr", "average");
    let out = fr::average_measurements(&[c.array("a"), c.array("b")]).unwrap();
    assert_allclose(&out, &c.array("avg"), 1e-12, 1e-12, "average");
}

#[test]
fn average_of_nothing_is_an_error() {
    assert!(fr::average_measurements(&[]).is_err());
}

#[test]
fn ragged_average_is_an_error() {
    assert!(fr::average_measurements(&[vec![0.0; 4], vec![0.0; 3]]).is_err());
}

#[test]
fn zero_length_fft_is_an_error() {
    assert!(fr::compute_frequency_response(&[], 48000, None).is_err());
    assert!(fr::compute_frequency_response(&[1.0], 48000, Some(0)).is_err());
}

#[test]
fn normalization_matches_oracle() {
    let c = Case::load("fr", "normalize");
    let out = fr::normalize_to_reference_band(
        &c.array("freqs"),
        &c.array("mag_db"),
        c.param_f64("low_hz"),
        c.param_f64("high_hz"),
    )
    .unwrap();
    assert_allclose(&out, &c.array("normalized"), 1e-12, 1e-12, "normalize");
}
