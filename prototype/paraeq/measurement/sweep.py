"""Logarithmic sine sweep generation and inverse filter for Farina method."""

import numpy as np


def generate_sweep(
    duration: float,
    sample_rate: int,
    f_start: float = 20.0,
    f_end: float = 20000.0,
) -> np.ndarray:
    """Generate a logarithmic sine sweep."""
    n_samples = int(duration * sample_rate)
    t = np.arange(n_samples) / sample_rate
    rate = f_end / f_start
    phase = (
        2.0 * np.pi * f_start * duration / np.log(rate) * (rate ** (t / duration) - 1.0)
    )
    sweep = np.sin(phase)
    return sweep


def generate_inverse_sweep(
    sweep: np.ndarray,
    sample_rate: int,
    f_start: float = 20.0,
    f_end: float = 20000.0,
) -> np.ndarray:
    """Generate the inverse filter for a log sweep (Farina method).

    The inverse is the time-reversed sweep with an amplitude envelope that
    decays at 6 dB/octave (compensating for the log sweep's pink spectrum).
    """
    n_samples = len(sweep)
    duration = n_samples / sample_rate
    rate = f_end / f_start
    inverse = sweep[::-1].copy()
    t_original = np.arange(n_samples)[::-1] / sample_rate
    envelope = rate ** (-t_original / duration)
    inverse *= envelope
    inverse /= np.sum(sweep * sweep[::-1] * envelope) / n_samples
    return inverse
