"""Load and apply microphone/jig calibration compensation files."""

import logging
import math
from pathlib import Path

import numpy as np
from scipy.interpolate import interp1d

logger = logging.getLogger(__name__)


# This module and crates/paraeq-dsp/src/compensation.rs must agree row for row
# (see load_compensation), so the file grammar is pinned to the Rust parser's
# rather than inherited from CPython, which is the looser of the two. Unicode is
# the tie-breaker, so it is CPython that gives way in both cases below.
_C0_SEPARATORS = frozenset("\x1c\x1d\x1e\x1f")
_FLOAT_CHARS = frozenset("+-.0123456789Ee")


def _parse_number(token: str) -> float | None:
    """Return the token's value, or None if it is not a number.

    The grammar is the C-locale float that Rust's ``f64::from_str`` accepts:
    ASCII only, no ``_`` digit separators. float() also takes ``1_000`` and
    Arabic-Indic digits and Rust takes neither, so the alphabet is gated first.

    float() accepts "nan"/"inf"/"Infinity" and returns inf on overflow, so a
    prose line could otherwise present itself as a data row and 1e400 could ride
    into the curve. Only finite values are numbers here. Rust reaches the same
    verdict on every one of these by parsing and then filtering on is_finite.
    """
    if not _FLOAT_CHARS.issuperset(token):
        return None
    try:
        value = float(token)
    except ValueError:
        return None
    return value if math.isfinite(value) else None


def _is_column_separator(char: str) -> bool:
    """A comma or a Unicode White_Space char -- Rust's ``char::is_whitespace``.

    str.split() also breaks on the C0 information separators (U+001C-U+001F),
    which Unicode does not give the White_Space property; Rust keeps them inside
    the token, where they make it fail to parse. Excluded so both agree.
    """
    return char == "," or (char.isspace() and char not in _C0_SEPARATORS)


def _columns(line: str) -> list[str]:
    """Split on a comma or whitespace: UMIK-1 rows are tab-separated and ParaEQ
    CSV is comma-separated, and one rule now serves both.

    Separators are mapped to U+0020 and split on explicitly, because bare
    ``.split()`` would break on the C0 separators this rule deliberately keeps.
    Leading and trailing separators fall out as empty tokens, so the caller owes
    this no ``strip`` -- which would itself have eaten those C0 chars.
    """
    spaced = "".join(" " if _is_column_separator(c) else c for c in line)
    return [token for token in spaced.split(" ") if token]


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
    ignored where present. Lines end at ``\\r\\n``, ``\\r`` or ``\\n`` (text mode's
    universal newlines, which parse_cal reproduces by hand); the tokenizer and
    number grammar are pinned to Rust's, per the module comment above.

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
    # utf-8-sig strips one leading BOM if present. U+FEFF is not whitespace, so
    # it would otherwise ride into the first token, make a headerless file's
    # first row parse as a header, and edge-hold the curve from the SECOND row
    # down -- silently, and worst where correction authority is highest. The
    # explicit encoding also pins what Rust's read_to_string already assumes.
    with open(filepath, "r", encoding="utf-8-sig") as f:
        for line in f:
            columns = _columns(line)
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
