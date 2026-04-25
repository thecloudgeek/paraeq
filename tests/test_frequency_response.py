import numpy as np
from paraeq.measurement.frequency_response import (
    average_measurements,
    compute_frequency_response,
    fractional_octave_smooth,
)


def test_compute_fr_shape():
    sr = 48000
    ir = np.zeros(4096)
    ir[0] = 1.0
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    expected_len = 4096 // 2 + 1
    assert freqs.shape == (expected_len,)
    assert mag_db.shape == (expected_len,)


def test_compute_fr_flat_impulse():
    sr = 48000
    ir = np.zeros(4096)
    ir[0] = 1.0
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    assert np.allclose(mag_db[1:], 0.0, atol=0.01)


def test_compute_fr_stereo():
    sr = 48000
    ir = np.zeros((4096, 2))
    ir[0, 0] = 1.0
    ir[0, 1] = 0.5
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    assert mag_db.ndim == 2
    assert mag_db.shape[1] == 2
    assert np.allclose(mag_db[1:, 1] - mag_db[1:, 0], -6.02, atol=0.1)


def test_fractional_octave_smooth_reduces_noise():
    np.random.seed(42)
    n = 2049
    noisy = np.random.randn(n) * 5.0
    freqs = np.linspace(0, 24000, n)
    smoothed = fractional_octave_smooth(noisy, freqs, fraction=6)
    assert np.std(smoothed[10:]) < np.std(noisy[10:])


def test_average_measurements():
    n = 2049
    m1 = np.zeros(n)
    m2 = np.full(n, 6.0)
    avg = average_measurements([m1, m2])
    assert np.allclose(avg, 3.0, atol=0.01)
