"""Stimulus noise signals (pink noise for level setting)."""

import numpy as np


def generate_pink_noise(
    duration: float,
    sample_rate: int,
    seed: int | None = None,
) -> np.ndarray:
    """Generate pink (1/f) noise via FFT spectral shaping.

    White noise's flat spectrum is multiplied by a 1/sqrt(f) magnitude envelope
    so the resulting power spectrum is 1/f (a.k.a. -3 dB/octave). The signal is
    normalized to a peak of 1.0 so the caller can apply any output amplitude.

    Args:
        duration: Length in seconds.
        sample_rate: Samples per second.
        seed: Optional RNG seed for deterministic output.

    Returns:
        Float64 array of shape (duration * sample_rate,), peak ≤ 1.0.
    """
    rng = np.random.default_rng(seed)
    n_samples = int(duration * sample_rate)
    white = rng.standard_normal(n_samples)

    spectrum = np.fft.rfft(white)
    freqs = np.fft.rfftfreq(n_samples, 1.0 / sample_rate)
    envelope = np.zeros_like(freqs)
    envelope[1:] = 1.0 / np.sqrt(freqs[1:])  # avoid div-by-zero at DC
    spectrum *= envelope

    pink = np.fft.irfft(spectrum, n=n_samples)
    peak = np.max(np.abs(pink))
    if peak > 0:
        pink = pink / peak
    return pink
