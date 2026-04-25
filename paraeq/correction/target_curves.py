"""Target curve loading, interpolation, and correction computation."""

import logging
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from scipy.interpolate import CubicSpline


logger = logging.getLogger(__name__)

TARGETS_DIR = Path(__file__).parent.parent.parent / "targets"


@dataclass
class TargetCurve:
    name: str
    frequencies: np.ndarray
    gains_db: np.ndarray

    def interpolate(self, query_freqs: np.ndarray) -> np.ndarray:
        """Interpolate target gains at arbitrary query frequencies.

        Uses cubic spline interpolation in log-frequency space. Values outside
        the defined frequency range are held constant at the boundary gain.

        Args:
            query_freqs: Frequencies in Hz at which to evaluate the curve.

        Returns:
            Interpolated gain values in dB, one per query frequency.
        """
        log_freqs = np.log10(np.maximum(self.frequencies, 1e-1))
        log_query = np.log10(np.maximum(query_freqs, 1e-1))
        cs = CubicSpline(log_freqs, self.gains_db, extrapolate=True)
        result = cs(log_query)
        result = np.where(query_freqs < self.frequencies[0], self.gains_db[0], result)
        result = np.where(query_freqs > self.frequencies[-1], self.gains_db[-1], result)
        logger.debug(
            "Interpolated target '%s' at %d frequencies (range %.1f–%.1f Hz)",
            self.name,
            len(query_freqs),
            float(query_freqs[0]),
            float(query_freqs[-1]),
        )
        return result


def load_target_csv(filepath: Path) -> TargetCurve:
    """Load a target curve from a CSV file.

    Lines beginning with '#' and blank lines are ignored. Each data line must
    be ``frequency_hz,gain_db``.

    Args:
        filepath: Path to the CSV file.

    Returns:
        A :class:`TargetCurve` with ``name`` set to the file stem.

    Raises:
        FileNotFoundError: If ``filepath`` does not exist.
        ValueError: If a data line cannot be parsed as two floats.
    """
    freqs: list[float] = []
    gains: list[float] = []
    with open(filepath, "r") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(",")
            freqs.append(float(parts[0]))
            gains.append(float(parts[1]))
    name = filepath.stem
    logger.info("Loaded target curve '%s' with %d points from %s", name, len(freqs), filepath)
    return TargetCurve(
        name=name,
        frequencies=np.array(freqs, dtype=np.float64),
        gains_db=np.array(gains, dtype=np.float64),
    )


def list_builtin_targets() -> list[TargetCurve]:
    """Return all built-in target curves found in the targets/ directory.

    Curves are returned in alphabetical order by filename stem.

    Returns:
        List of :class:`TargetCurve` objects, one per CSV file.
    """
    curves: list[TargetCurve] = []
    for csv_path in sorted(TARGETS_DIR.glob("*.csv")):
        curves.append(load_target_csv(csv_path))
    logger.info("Listed %d built-in target curves from %s", len(curves), TARGETS_DIR)
    return curves


def compute_correction(measured_db: np.ndarray, target_db: np.ndarray) -> np.ndarray:
    """Compute the correction needed to bring a measured response to a target.

    Each element of the result is ``target_db[i] - measured_db[i]``, so
    applying the correction to the measured response yields the target.

    Args:
        measured_db: Measured frequency response in dB.
        target_db: Desired target frequency response in dB (same length).

    Returns:
        Per-frequency correction values in dB.
    """
    correction = target_db - measured_db
    logger.debug(
        "Computed correction: min=%.2f dB, max=%.2f dB, rms=%.2f dB",
        float(correction.min()),
        float(correction.max()),
        float(np.sqrt(np.mean(correction**2))),
    )
    return correction
