"""Load and apply microphone/jig calibration compensation files."""

import csv
import logging
from pathlib import Path

import numpy as np
from scipy.interpolate import interp1d

logger = logging.getLogger(__name__)


def load_compensation(filepath: Path) -> tuple[np.ndarray, np.ndarray]:
    """Load a calibration compensation file.

    Two formats are supported and auto-detected:

    1. **ParaEQ CSV** — comma-separated ``frequency_hz, gain_db`` rows. Lines
       starting with ``#`` and blank lines are ignored.
    2. **miniDSP REW-style** (e.g. EARS calibration) — two leading quoted-string
       header lines, ``*``-prefixed comment lines, and whitespace-separated
       ``Freq(Hz) SPL(dB) Phase(degrees)`` data rows. Phase is discarded.

    Args:
        filepath: Path to the compensation file.

    Returns:
        Tuple of (freqs_hz, gains_db) as float64 numpy arrays.
    """
    with open(filepath, "r") as f:
        first_line = f.readline().lstrip()

    if first_line.startswith('"'):
        data = np.loadtxt(filepath, skiprows=2, comments="*", usecols=(0, 1))
        freqs = data[:, 0].astype(np.float64)
        gains = data[:, 1].astype(np.float64)
    else:
        freqs_list: list[float] = []
        gains_list: list[float] = []
        with open(filepath, "r") as f:
            reader = csv.reader(f)
            for row in reader:
                if not row or row[0].startswith("#"):
                    continue
                freqs_list.append(float(row[0].strip()))
                gains_list.append(float(row[1].strip()))
        freqs = np.array(freqs_list, dtype=np.float64)
        gains = np.array(gains_list, dtype=np.float64)

    logger.debug(
        "Loaded compensation file",
        extra={"filepath": str(filepath), "n_points": len(freqs)},
    )
    return freqs, gains


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
