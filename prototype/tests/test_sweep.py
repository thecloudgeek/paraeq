import numpy as np
from paraeq.measurement.sweep import generate_sweep, generate_inverse_sweep


def test_generate_sweep_length():
    """Sweep should have exactly duration * sample_rate samples."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    assert sweep.shape == (144000,)
    assert sweep.dtype == np.float64


def test_generate_sweep_amplitude_bounded():
    """Sweep amplitude should stay within [-1, 1]."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    assert np.max(np.abs(sweep)) <= 1.0


def test_generate_sweep_starts_low_frequency():
    """First few cycles should be near f_start (20 Hz)."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    chunk = sweep[:4800]
    fft_mag = np.abs(np.fft.rfft(chunk))
    freqs = np.fft.rfftfreq(len(chunk), 1.0 / 48000)
    peak_freq = freqs[np.argmax(fft_mag[1:]) + 1]
    assert 10.0 < peak_freq < 50.0


def test_generate_inverse_sweep_length():
    """Inverse sweep should match the original sweep length."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    inverse = generate_inverse_sweep(sweep, sample_rate=48000, f_start=20.0, f_end=20000.0)
    assert inverse.shape == sweep.shape


def test_sweep_inverse_deconvolution_peak():
    """Convolving sweep with its inverse should produce a sharp impulse."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr, f_start=20.0, f_end=20000.0)
    inverse = generate_inverse_sweep(sweep, sample_rate=sr, f_start=20.0, f_end=20000.0)
    n = len(sweep) + len(inverse) - 1
    n_fft = int(2 ** np.ceil(np.log2(n)))
    result = np.fft.irfft(np.fft.rfft(sweep, n_fft) * np.fft.rfft(inverse, n_fft), n_fft)
    result = result / np.max(np.abs(result))
    peak_idx = np.argmax(np.abs(result))
    non_peak = np.delete(np.abs(result), range(max(0, peak_idx - 10), peak_idx + 10))
    assert np.max(np.abs(result)) > 10 * np.mean(non_peak)
