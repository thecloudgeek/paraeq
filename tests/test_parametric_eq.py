import numpy as np
from paraeq.correction.parametric_eq import EQBand, ParametricEQ


def test_eq_band_creation():
    band = EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.5)
    assert band.filter_type == "peaking"
    assert band.fc == 1000.0
    assert band.gain_db == 3.0
    assert band.q == 1.5


def test_eq_band_to_sos():
    band = EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.5)
    sos = band.to_sos(sample_rate=48000)
    assert sos.shape == (1, 6)


def test_parametric_eq_empty():
    eq = ParametricEQ(bands=[], sample_rate=48000)
    freqs = np.array([100.0, 1000.0, 10000.0])
    mag = eq.frequency_response(freqs)
    np.testing.assert_array_almost_equal(mag, [0.0, 0.0, 0.0], decimal=1)


def test_parametric_eq_single_band():
    eq = ParametricEQ(
        bands=[EQBand(filter_type="peaking", fc=1000.0, gain_db=6.0, q=1.0)],
        sample_rate=48000,
    )
    freqs = np.array([1000.0])
    mag = eq.frequency_response(freqs)
    assert abs(mag[0] - 6.0) < 0.5


def test_parametric_eq_combined_sos():
    eq = ParametricEQ(
        bands=[
            EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.0),
            EQBand(filter_type="low_shelf", fc=100.0, gain_db=2.0, q=0.707),
        ],
        sample_rate=48000,
    )
    sos = eq.combined_sos()
    assert sos.shape == (2, 6)


def test_parametric_eq_export_text():
    eq = ParametricEQ(
        bands=[EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.0)],
        sample_rate=48000,
    )
    text = eq.export_autoeq_format()
    assert "1000" in text
    assert "3.0" in text
