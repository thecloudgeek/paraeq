"""Frequency response computation, smoothing, and averaging."""

import logging

import numpy as np

logger = logging.getLogger(__name__)


def compute_frequency_response(
    ir: np.ndarray,
    sample_rate: int,
    n_fft: int | None = None,
) -> tuple[np.ndarray, np.ndarray]:
    """Compute frequency response magnitude from an impulse response.

    Args:
        ir: Impulse response array. Shape (n_samples,) for mono or
            (n_samples, n_channels) for multi-channel.
        sample_rate: Sample rate in Hz.
        n_fft: FFT size. Defaults to the number of samples in ir.

    Returns:
        Tuple of (freqs, mag_db) where freqs has shape (n_fft//2+1,) and
        mag_db has shape (n_fft//2+1,) for mono or (n_fft//2+1, n_channels)
        for multi-channel input.
    """
    if n_fft is None:
        n_fft = ir.shape[0]

    stereo = ir.ndim == 2
    if not stereo:
        ir = ir[:, np.newaxis]

    n_channels = ir.shape[1]
    freqs = np.fft.rfftfreq(n_fft, 1.0 / sample_rate)

    logger.debug(
        "Computing frequency response",
        extra={
            "n_fft": n_fft,
            "sample_rate": sample_rate,
            "n_channels": n_channels,
            "stereo": stereo,
        },
    )

    mag_db_channels = []
    for ch in range(n_channels):
        spectrum = np.fft.rfft(ir[:, ch], n_fft)
        magnitude = np.abs(spectrum)
        magnitude = np.maximum(magnitude, 1e-10)
        mag_db = 20.0 * np.log10(magnitude)
        mag_db_channels.append(mag_db)

    mag_db = np.column_stack(mag_db_channels)
    if not stereo:
        mag_db = mag_db[:, 0]

    logger.debug(
        "Frequency response computed",
        extra={"output_shape": mag_db.shape, "freq_range_hz": (freqs[1], freqs[-1])},
    )

    return freqs, mag_db


def fractional_octave_smooth(
    magnitude_db: np.ndarray,
    freqs: np.ndarray,
    fraction: int = 6,
) -> np.ndarray:
    """Apply fractional-octave smoothing to a magnitude spectrum.

    Variable-width moving average where each window spans 1/fraction of an
    octave centred on that bin's frequency.

    Args:
        magnitude_db: Magnitude spectrum in dB. Shape (n_bins,).
        freqs: Frequency axis in Hz. Shape (n_bins,).
        fraction: Octave fraction for smoothing window (e.g. 6 = 1/6-octave).

    Returns:
        Smoothed magnitude spectrum in dB with the same shape as magnitude_db.
    """
    logger.debug(
        "Applying fractional-octave smoothing",
        extra={"fraction": fraction, "n_bins": len(magnitude_db)},
    )

    linear = 10.0 ** (magnitude_db / 20.0)
    result = np.copy(linear)

    ratio = 2.0 ** (1.0 / (2.0 * fraction))

    for i in range(len(freqs)):
        if freqs[i] <= 0:
            continue
        f_low = freqs[i] / ratio
        f_high = freqs[i] * ratio
        mask = (freqs >= f_low) & (freqs <= f_high)
        if np.any(mask):
            result[i] = np.mean(linear[mask])

    smoothed = 20.0 * np.log10(np.maximum(result, 1e-10))

    logger.debug(
        "Smoothing complete",
        extra={"output_std_db": float(np.std(smoothed))},
    )

    return smoothed


def average_measurements(measurements_db: list[np.ndarray]) -> np.ndarray:
    """Average multiple frequency response measurements in dB.

    Computes a simple arithmetic mean directly in the dB domain so that
    0 dB and 6 dB average to 3 dB, preserving perceptually intuitive
    behaviour for equalisation work.

    Args:
        measurements_db: List of magnitude spectra in dB. Each array must have
            the same shape.

    Returns:
        Averaged magnitude spectrum in dB with the same shape as each input.
    """
    logger.debug(
        "Averaging measurements",
        extra={"n_measurements": len(measurements_db)},
    )

    averaged = np.mean(measurements_db, axis=0)

    logger.debug(
        "Averaging complete",
        extra={"output_shape": averaged.shape},
    )

    return averaged  # type: ignore[return-value]
