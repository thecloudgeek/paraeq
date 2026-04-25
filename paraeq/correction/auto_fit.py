"""Auto-fit parametric EQ bands to approximate a correction curve."""

import logging

import numpy as np

from paraeq.correction.parametric_eq import EQBand, ParametricEQ

logger = logging.getLogger(__name__)


def auto_fit_parametric_eq(
    correction_db: np.ndarray,
    freqs: np.ndarray,
    sample_rate: int,
    max_bands: int = 10,
    min_gain_db: float = 0.5,
) -> list[EQBand]:
    """Greedy: find largest residual peak, place a peaking filter, subtract, repeat."""
    bands: list[EQBand] = []
    residual = correction_db.copy()
    audible = (freqs >= 20.0) & (freqs <= 20000.0)

    logger.debug(
        "auto_fit_parametric_eq start",
        extra={
            "max_bands": max_bands,
            "min_gain_db": min_gain_db,
            "sample_rate": sample_rate,
            "num_freqs": len(freqs),
        },
    )

    for iteration in range(max_bands):
        masked_residual = np.where(audible, np.abs(residual), 0.0)
        peak_idx = int(np.argmax(masked_residual))
        peak_gain = residual[peak_idx]

        if abs(peak_gain) < min_gain_db:
            logger.debug(
                "auto_fit_parametric_eq: stopping early, peak below threshold",
                extra={"iteration": iteration, "peak_gain": float(peak_gain), "min_gain_db": min_gain_db},
            )
            break

        peak_freq = freqs[peak_idx]
        if peak_freq <= 0:
            logger.debug(
                "auto_fit_parametric_eq: stopping early, peak_freq <= 0",
                extra={"iteration": iteration, "peak_freq": float(peak_freq)},
            )
            break

        q = _estimate_q(residual, freqs, peak_idx)

        band = EQBand(filter_type="peaking", fc=float(peak_freq), gain_db=float(peak_gain), q=float(q))
        bands.append(band)

        logger.debug(
            "auto_fit_parametric_eq: placed band",
            extra={
                "iteration": iteration,
                "fc": float(peak_freq),
                "gain_db": float(peak_gain),
                "q": float(q),
            },
        )

        eq = ParametricEQ(bands=[band], sample_rate=sample_rate)
        band_response = eq.frequency_response(freqs)
        band_response = np.nan_to_num(band_response, nan=0.0)
        residual = residual - band_response

    logger.debug(
        "auto_fit_parametric_eq done",
        extra={"num_bands_placed": len(bands)},
    )

    return bands


def _estimate_q(residual: np.ndarray, freqs: np.ndarray, peak_idx: int) -> float:
    peak_val = abs(residual[peak_idx])
    half_val = peak_val * 0.5

    left_idx = peak_idx
    for i in range(peak_idx - 1, 0, -1):
        if abs(residual[i]) < half_val:
            left_idx = i
            break

    right_idx = peak_idx
    for i in range(peak_idx + 1, len(residual)):
        if abs(residual[i]) < half_val:
            right_idx = i
            break

    if left_idx == peak_idx or right_idx == peak_idx:
        return 2.0

    f_low = freqs[left_idx]
    f_high = freqs[right_idx]
    f_center = freqs[peak_idx]

    if f_high <= f_low or f_center <= 0:
        return 2.0

    bandwidth = f_high - f_low
    q = f_center / bandwidth
    return float(np.clip(q, 0.5, 20.0))
