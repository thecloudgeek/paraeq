import numpy as np
from paraeq.correction.auto_fit import auto_fit_parametric_eq
from paraeq.correction.parametric_eq import ParametricEQ


def test_auto_fit_flat_target_produces_no_bands():
    sr = 48000
    freqs = np.fft.rfftfreq(4096, 1.0 / sr)
    correction_db = np.zeros_like(freqs)
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=5)
    eq = ParametricEQ(bands=bands, sample_rate=sr)
    response = eq.frequency_response(freqs[1:])
    assert np.max(np.abs(response)) < 1.0


def test_auto_fit_single_peak():
    sr = 48000
    n_fft = 4096
    freqs = np.fft.rfftfreq(n_fft, 1.0 / sr)
    correction_db = np.zeros_like(freqs)
    idx_1k = np.argmin(np.abs(freqs - 1000.0))
    sigma = 50
    for i in range(len(correction_db)):
        correction_db[i] = 6.0 * np.exp(-0.5 * ((i - idx_1k) / sigma) ** 2)
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=5)
    assert len(bands) >= 1
    band_freqs = [b.fc for b in bands if abs(b.gain_db) > 1.0]
    assert any(500 < f < 2000 for f in band_freqs)


def test_auto_fit_respects_max_bands():
    sr = 48000
    freqs = np.fft.rfftfreq(4096, 1.0 / sr)
    correction_db = np.random.RandomState(42).randn(len(freqs)) * 3.0
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=5)
    assert len(bands) <= 5


def test_auto_fit_reduces_error():
    sr = 48000
    n_fft = 4096
    freqs = np.fft.rfftfreq(n_fft, 1.0 / sr)
    correction_db = 3.0 * np.sin(2 * np.pi * np.log10(freqs[1:] + 1) / 2)
    correction_db = np.concatenate([[0.0], correction_db])
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=10)
    eq = ParametricEQ(bands=bands, sample_rate=sr)
    response = eq.frequency_response(freqs[1:])
    residual = correction_db[1:] - response
    rms_before = np.sqrt(np.mean(correction_db[1:] ** 2))
    rms_after = np.sqrt(np.mean(residual**2))
    assert rms_after < rms_before
