"""Load and apply microphone/jig calibration compensation files."""

import csv
import logging
from pathlib import Path

import numpy as np
from scipy.interpolate import interp1d

logger = logging.getLogger(__name__)


def load_compensation(filepath: Path) -> tuple[np.ndarray, np.ndarray]:
    """Load a calibration compensation file (CSV: frequency,gain_db).

    Args:
        filepath: Path to a CSV file with rows of (frequency_hz, gain_db).
                  Lines starting with '#' and blank lines are ignored.

    Returns:
        Tuple of (freqs_hz, gains_db) as float64 numpy arrays.
    """
    freqs = []
    gains = []
    with open(filepath, "r") as f:
        reader = csv.reader(f)
        for row in reader:
            if not row or row[0].startswith("#"):
                continue
            freqs.append(float(row[0].strip()))
            gains.append(float(row[1].strip()))

    logger.debug(
        "Loaded compensation file",
        extra={"filepath": str(filepath), "n_points": len(freqs)},
    )
    return np.array(freqs, dtype=np.float64), np.array(gains, dtype=np.float64)


def apply_compensation(
    magnitude_db: np.ndarray,
    freqs_fft: np.ndarray,
    comp_freqs: np.ndarray,
    comp_gains_db: np.ndarray,
) -> np.ndarray:
    """Subtract a compensation curve from a magnitude spectrum.

    Interpolates the compensation curve onto the FFT frequency grid and
    subtracts it from the measured magnitude, correcting for microphone or
    jig coloration.

    Args:
        magnitude_db: Measured magnitude spectrum in dB, shape (n,).
        freqs_fft: Frequency axis for magnitude_db in Hz, shape (n,).
        comp_freqs: Compensation curve frequency points in Hz.
        comp_gains_db: Compensation curve gain values in dB.

    Returns:
        Corrected magnitude spectrum in dB, same shape as magnitude_db.
    """
    interpolator = interp1d(
        comp_freqs,
        comp_gains_db,
        kind="linear",
        bounds_error=False,
        fill_value=(comp_gains_db[0], comp_gains_db[-1]),
    )
    comp_interpolated = interpolator(freqs_fft)

    logger.debug(
        "Applied compensation curve",
        extra={
            "n_fft_bins": len(freqs_fft),
            "comp_freq_range": (float(comp_freqs[0]), float(comp_freqs[-1])),
        },
    )
    return magnitude_db - comp_interpolated
