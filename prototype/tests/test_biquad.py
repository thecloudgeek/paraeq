import numpy as np
from paraeq.correction.biquad import biquad_peaking, biquad_low_shelf, biquad_high_shelf, biquad_notch, biquad_frequency_response


def test_peaking_at_center_frequency():
    sos = biquad_peaking(fc=1000.0, gain_db=6.0, q=1.0, sample_rate=48000)
    freqs = np.array([1000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert abs(mag_db[0] - 6.0) < 0.5


def test_peaking_unity_at_dc():
    sos = biquad_peaking(fc=1000.0, gain_db=6.0, q=2.0, sample_rate=48000)
    freqs = np.array([20.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert abs(mag_db[0]) < 1.0


def test_low_shelf_below_corner():
    sos = biquad_low_shelf(fc=200.0, gain_db=6.0, q=0.707, sample_rate=48000)
    freqs = np.array([20.0, 10000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert mag_db[0] > 4.0
    assert abs(mag_db[1]) < 1.0


def test_high_shelf_above_corner():
    sos = biquad_high_shelf(fc=5000.0, gain_db=-6.0, q=0.707, sample_rate=48000)
    freqs = np.array([100.0, 15000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert abs(mag_db[0]) < 1.0
    assert mag_db[1] < -4.0


def test_notch_attenuates_center():
    sos = biquad_notch(fc=1000.0, q=10.0, sample_rate=48000)
    freqs = np.array([1000.0, 2000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert mag_db[0] < -20.0
    assert abs(mag_db[1]) < 3.0


def test_sos_format():
    sos = biquad_peaking(fc=1000.0, gain_db=3.0, q=1.0, sample_rate=48000)
    assert sos.shape == (1, 6)
