"""Load and apply microphone/jig calibration compensation files."""

import logging
import math
from pathlib import Path

import numpy as np
from scipy.interpolate import interp1d

logger = logging.getLogger(__name__)


def _parse_number(token: str) -> float | None:
    """Return the token's value, or None if it is not a number.

    float() accepts "nan"/"inf"/"Infinity", so a prose line could otherwise
    present itself as a data row. Only finite values are numbers here.
    """
    try:
        value = float(token)
    except ValueError:
        return None
    return value if math.isfinite(value) else None


def _columns(line: str) -> list[str]:
    """Split on whitespace or commas: UMIK-1 rows are tab-separated and ParaEQ
    CSV is comma-separated, and one rule now serves both."""
    return line.replace(",", " ").split()


def load_compensation(filepath: Path) -> tuple[np.ndarray, np.ndarray]:
    """Load a calibration compensation file.

    REW's rule verbatim -- *"only lines which begin with a number are loaded,
    others are ignored"* -- replaces all format sniffing. One rule covers EARS
    (two quoted headers), UMIK-1 0-degree (one header), UMIK-1 90-degree (two),
    unquoted legacy files, and ``*``- or ``#``-commented files, with no dispatch.

    The rule this replaced sniffed a leading double-quote and then hardcoded
    ``skiprows=2``, silently eating the first data row of every single-header
    file. This function is the lockstep half of the same fix in
    crates/paraeq-dsp/src/compensation.rs (parse_cal); the two parsers must
    agree row for row, so they change together or not at all.

    Columns 0 and 1 are frequency (Hz) and gain (dB); column 2 (phase) is
    ignored where present.

    Args:
        filepath: Path to the compensation file.

    Returns:
        Tuple of (freqs_hz, gains_db) as float64 numpy arrays.

    Raises:
        ValueError: if the file holds no data rows, or if a row whose first
            column is a number lacks a valid second column.
    """
    freqs_list: list[float] = []
    gains_list: list[float] = []
    with open(filepath, "r") as f:
        for line in f:
            columns = _columns(line.strip())
            if not columns:
                continue
            freq = _parse_number(columns[0])
            if freq is None:
                continue  # header, comment or prose: not a data row
            gain = _parse_number(columns[1]) if len(columns) > 1 else None
            if gain is None:
                raise ValueError(
                    f"malformed data row in {filepath}: {line.strip()!r}"
                )
            freqs_list.append(freq)
            gains_list.append(gain)

    if not freqs_list:
        raise ValueError(f"no data rows in compensation file: {filepath}")

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
    jig coloration. The curve is applied as-is: never normalize it to 0 dB at
    a reference frequency, because a jig's per-channel offset (the EARS
    capsules differ by a real 2.1 dB) is measured data, and erasing it would
    bake that imbalance into every correction.

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
