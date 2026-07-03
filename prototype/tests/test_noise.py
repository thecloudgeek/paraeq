import numpy as np

from paraeq.measurement.noise import generate_pink_noise


def test_generate_pink_noise_returns_correct_length():
    noise = generate_pink_noise(duration=1.0, sample_rate=48000)
    assert noise.shape == (48000,)


def test_generate_pink_noise_amplitude_bounded():
    """Pink noise must stay within [-1, 1] so it can be played without clipping."""
    noise = generate_pink_noise(duration=2.0, sample_rate=48000)
    assert np.max(np.abs(noise)) <= 1.0


def test_generate_pink_noise_has_negative_spectral_slope():
    """Pink noise has 1/f power spectrum (~-3 dB/octave magnitude slope).

    Compute average magnitude in two log-spaced bands (100-200 Hz, 4000-8000 Hz)
    and confirm the higher band is ~12 dB lower (5 octaves * ~3 dB/oct).
    Tolerance is wide because of single-realization variance.
    """
    noise = generate_pink_noise(duration=4.0, sample_rate=48000, seed=42)
    spectrum = np.abs(np.fft.rfft(noise))
    freqs = np.fft.rfftfreq(len(noise), 1.0 / 48000)

    band_low = (freqs >= 100) & (freqs <= 200)
    band_high = (freqs >= 4000) & (freqs <= 8000)

    db_low = 20 * np.log10(np.mean(spectrum[band_low]))
    db_high = 20 * np.log10(np.mean(spectrum[band_high]))

    slope_db = db_high - db_low
    # ~5.3 octaves between band centers; expect ~-15 dB; allow generous tolerance.
    assert -22 < slope_db < -8


def test_generate_pink_noise_seed_is_deterministic():
    a = generate_pink_noise(duration=0.5, sample_rate=48000, seed=123)
    b = generate_pink_noise(duration=0.5, sample_rate=48000, seed=123)
    assert np.array_equal(a, b)
