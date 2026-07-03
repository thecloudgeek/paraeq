import numpy as np
from paraeq.measurement.sweep import generate_sweep, generate_inverse_sweep
from paraeq.measurement.deconvolution import deconvolve


def test_deconvolve_identity():
    """Deconvolving an unmodified sweep should yield a near-perfect impulse."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    ir = deconvolve(recorded=sweep, sweep=sweep, sample_rate=sr)
    peak_idx = np.argmax(np.abs(ir))
    peak_val = np.abs(ir[peak_idx])
    mask = np.ones(len(ir), dtype=bool)
    mask[max(0, peak_idx - 20) : peak_idx + 20] = False
    noise_rms = np.sqrt(np.mean(ir[mask] ** 2))
    assert peak_val / noise_rms > 50


def test_deconvolve_delayed_system():
    """A delayed recording should produce a delayed impulse response."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    delay_samples = 480
    recorded = np.zeros(len(sweep) + delay_samples)
    recorded[delay_samples:delay_samples + len(sweep)] = sweep
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    peak_idx = np.argmax(np.abs(ir))
    assert abs(peak_idx - delay_samples) < 10


def test_deconvolve_stereo():
    """Deconvolution should handle stereo (2D) recordings."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    left = np.zeros(len(sweep) + 100)
    left[100:100 + len(sweep)] = sweep
    right = np.zeros(len(sweep) + 100)
    right[:len(sweep)] = sweep * 0.8
    recorded = np.column_stack([left, right])
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    assert ir.ndim == 2
    assert ir.shape[1] == 2
    left_peak = np.argmax(np.abs(ir[:, 0]))
    right_peak = np.argmax(np.abs(ir[:, 1]))
    assert left_peak > right_peak
