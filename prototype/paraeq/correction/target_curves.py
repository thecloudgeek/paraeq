"""Target curve loading, interpolation, and correction computation."""

import logging
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from scipy.interpolate import CubicSpline


logger = logging.getLogger(__name__)

TARGETS_DIR = Path(__file__).parent.parent.parent / "targets"

_METADATA_KEYS: frozenset[str] = frozenset({"category", "description", "name", "source"})

# Fixed, log-spaced anchor frequencies for the interactive target editor.
# Only the gain axis is draggable, so frequencies stay sorted/distinct by
# construction (no CubicSpline degenerate-input risk).
ANCHOR_FREQS = np.array(
    [20, 32, 50, 80, 125, 200, 315, 500, 800, 1250, 2000, 3150, 5000, 8000, 12500, 20000],
    dtype=np.float64,
)


@dataclass
class TargetCurve:
    name: str
    frequencies: np.ndarray
    gains_db: np.ndarray
    category: str | None = None
    description: str | None = None
    source: str | None = None

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

    Lines beginning with '#' and blank lines are ignored. Each non-comment
    line must be ``frequency_hz,gain_db``. Comment lines of the form
    ``# key: value`` populate optional metadata on the returned curve;
    recognized keys (case-insensitive) are ``name``, ``category``,
    ``description``, ``source``. Unknown keys are silently ignored.
    Recognized keys with an empty value (e.g., ``# source:`` with no text
    after the colon) are treated as absent.

    Args:
        filepath: Path to the CSV file.

    Returns:
        A :class:`TargetCurve`. ``name`` falls back to the file stem if no
        ``# name:`` header is present.

    Raises:
        FileNotFoundError: If ``filepath`` does not exist.
        ValueError: If a data line cannot be parsed as two floats.
    """
    freqs: list[float] = []
    gains: list[float] = []
    metadata: dict[str, str] = {}
    with open(filepath, "r", encoding="utf-8") as f:
        for line in f:
            stripped = line.strip()
            if not stripped:
                continue
            if stripped.startswith("#"):
                # Try to parse "# key: value"
                content = stripped.lstrip("#").strip()
                if ":" in content:
                    key, _, value = content.partition(":")
                    key_lower = key.strip().lower()
                    value_stripped = value.strip()
                    if key_lower in _METADATA_KEYS and value_stripped:
                        metadata[key_lower] = value_stripped
                continue
            parts = stripped.split(",")
            if len(parts) < 2:
                raise ValueError(
                    f"Expected 'frequency,gain' but got: {stripped!r}"
                )
            freqs.append(float(parts[0]))
            gains.append(float(parts[1]))
    name = metadata.get("name", filepath.stem)
    logger.info("Loaded target curve '%s' with %d points from %s", name, len(freqs), filepath)
    return TargetCurve(
        name=name,
        frequencies=np.array(freqs, dtype=np.float64),
        gains_db=np.array(gains, dtype=np.float64),
        category=metadata.get("category"),
        description=metadata.get("description"),
        source=metadata.get("source"),
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


def build_anchor_target(
    base: TargetCurve,
    anchor_freqs: np.ndarray,
    offsets_db: np.ndarray,
    *,
    name: str = "Custom",
) -> TargetCurve:
    """Return a new TargetCurve = base + a smooth deviation through the anchors.

    The deviation is a cubic spline (in log-frequency space) through
    ``(anchor_freqs, offsets_db)``, held constant outside the anchor range.
    Sampling on the union of the base's own frequencies and the anchor
    frequencies preserves the base preset's fine detail while applying the
    user's anchor offsets. ``base`` is never mutated.

    Args:
        base: The preset being edited (offsets start at zero).
        anchor_freqs: Fixed anchor frequencies in Hz (sorted, distinct).
        offsets_db: One dB offset per anchor (same length as anchor_freqs).
        name: Name for the returned curve.

    Returns:
        A new :class:`TargetCurve`.
    """
    grid = np.unique(np.concatenate([base.frequencies, anchor_freqs]))
    base_db = base.interpolate(grid)

    cs = CubicSpline(np.log10(anchor_freqs), offsets_db, extrapolate=True)
    deviation = cs(np.log10(grid))
    deviation = np.where(grid < anchor_freqs[0], offsets_db[0], deviation)
    deviation = np.where(grid > anchor_freqs[-1], offsets_db[-1], deviation)

    logger.debug(
        "build_anchor_target",
        extra={
            "base": base.name,
            "n_grid": len(grid),
            "offset_max": float(np.max(np.abs(offsets_db))),
        },
    )
    return TargetCurve(
        name=name,
        frequencies=grid,
        gains_db=base_db + deviation,
        category=base.category,
        description=base.description,
        source=base.source,
    )


def match_closest_target(
    measured_freqs: np.ndarray,
    measured_db: np.ndarray,
    targets: list[TargetCurve],
) -> TargetCurve:
    """Return the target the measurement is already closest to.

    Scores each target by the level-aligned RMS of ``target - measured``
    (the mean is removed first because absolute level is arbitrary) and
    returns the smallest-scoring target.

    Args:
        measured_freqs: Measurement frequencies in Hz.
        measured_db: Measured response in dB (same length as measured_freqs).
        targets: Candidate target curves (must be non-empty).

    Returns:
        The closest :class:`TargetCurve`.
    """
    best: TargetCurve | None = None
    best_score = np.inf
    for t in targets:
        residual = t.interpolate(measured_freqs) - measured_db
        residual = residual - residual.mean()
        score = float(np.sqrt(np.mean(residual ** 2)))
        logger.debug("match_closest_target candidate", extra={"target": t.name, "rms": score})
        if score < best_score:
            best_score = score
            best = t
    logger.info(
        "match_closest_target chose '%s' (rms=%.3f dB)",
        best.name if best else None,
        best_score,
    )
    return best
