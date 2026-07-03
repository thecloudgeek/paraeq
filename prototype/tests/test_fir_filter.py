import numpy as np
from paraeq.correction.fir_filter import design_fir_correction


def test_fir_correction_length():
    freqs = np.fft.rfftfreq(4096, 1.0 / 48000)
    correction_db = np.zeros_like(freqs)
    fir = design_fir_correction(correction_db, freqs, n_taps=4096, phase="minimum")
    assert len(fir) == 4096


def test_fir_correction_flat_is_impulse():
    n_taps = 4096
    freqs = np.fft.rfftfreq(n_taps, 1.0 / 48000)
    correction_db = np.zeros_like(freqs)
    fir = design_fir_correction(correction_db, freqs, n_taps=n_taps, phase="minimum")
    peak_val = np.max(np.abs(fir))
    rms = np.sqrt(np.mean(fir**2))
    assert peak_val > 10 * rms


def test_fir_correction_applies_gain():
    n_taps = 4096
    sr = 48000
    freqs = np.fft.rfftfreq(n_taps, 1.0 / sr)
    correction_db = np.full_like(freqs, 6.0)
    fir = design_fir_correction(correction_db, freqs, n_taps=n_taps, phase="minimum")
    fir_fft = np.fft.rfft(fir, n_taps)
    fir_mag_db = 20.0 * np.log10(np.abs(fir_fft) + 1e-10)
    idx_1k = np.argmin(np.abs(freqs - 1000.0))
    assert abs(fir_mag_db[idx_1k] - 6.0) < 1.0


def test_fir_linear_phase_is_symmetric():
    n_taps = 4096
    freqs = np.fft.rfftfreq(n_taps, 1.0 / 48000)
    correction_db = np.zeros_like(freqs)
    fir = design_fir_correction(correction_db, freqs, n_taps=n_taps, phase="linear")
    np.testing.assert_array_almost_equal(fir, fir[::-1], decimal=10)
