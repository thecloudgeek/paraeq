import numpy as np
import pytest

from paraeq.measurement.frequency_response import (
    average_measurements,
    compute_frequency_response,
    fractional_octave_smooth,
    normalize_to_reference_band,
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


def test_normalize_to_reference_band_centers_band_at_zero():
    """After normalization, the mean over the reference band is 0 dB."""
    freqs = np.linspace(20.0, 20000.0, 2049)
    mag_db = np.full_like(freqs, -47.0)  # raw dBFS-ish offset
    out = normalize_to_reference_band(freqs, mag_db, low_hz=200.0, high_hz=1000.0)
    band = (freqs >= 200.0) & (freqs <= 1000.0)
    assert abs(np.mean(out[band])) < 1e-6


def test_normalize_to_reference_band_preserves_shape():
    """Only a constant offset is subtracted; relative shape is unchanged."""
    freqs = np.linspace(20.0, 20000.0, 2049)
    rng = np.random.default_rng(1)
    mag_db = rng.standard_normal(2049) * 3.0 - 50.0
    out = normalize_to_reference_band(freqs, mag_db, low_hz=200.0, high_hz=1000.0)
    diffs = (mag_db - out) - (mag_db[0] - out[0])
    assert np.allclose(diffs, 0.0, atol=1e-9)


def test_normalize_to_reference_band_raises_on_empty_band():
    """If no FFT bins fall in the requested band, raise rather than silently NaN."""
    freqs = np.array([10.0, 20.0, 30.0])
    mag_db = np.zeros(3)
    with pytest.raises(ValueError):
        normalize_to_reference_band(freqs, mag_db, low_hz=200.0, high_hz=1000.0)
