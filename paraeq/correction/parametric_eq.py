"""Parametric EQ with named bands and composite frequency response."""

import logging
from dataclasses import dataclass

import numpy as np

from paraeq.correction.biquad import (
    biquad_frequency_response,
    biquad_high_shelf,
    biquad_low_shelf,
    biquad_notch,
    biquad_peaking,
)

logger = logging.getLogger(__name__)

FILTER_BUILDERS = {
    "high_shelf": lambda b, sr: biquad_high_shelf(b.fc, b.gain_db, b.q, sr),
    "low_shelf": lambda b, sr: biquad_low_shelf(b.fc, b.gain_db, b.q, sr),
    "notch": lambda b, sr: biquad_notch(b.fc, b.q, sr),
    "peaking": lambda b, sr: biquad_peaking(b.fc, b.gain_db, b.q, sr),
}

_AUTOEQ_FILTER_NAMES = {
    "high_shelf": "HSC",
    "low_shelf": "LSC",
    "notch": "NO",
    "peaking": "PK",
}


@dataclass
class EQBand:
    filter_type: str  # "peaking", "low_shelf", "high_shelf", "notch"
    fc: float
    gain_db: float
    q: float

    def to_sos(self, sample_rate: int) -> np.ndarray:
        """Return a (1, 6) SOS array for this band at the given sample rate."""
        builder = FILTER_BUILDERS[self.filter_type]
        sos = builder(self, sample_rate)
        logger.debug(
            "EQBand.to_sos",
            extra={
                "filter_type": self.filter_type,
                "fc": self.fc,
                "gain_db": self.gain_db,
                "q": self.q,
                "sample_rate": sample_rate,
                "sos_shape": sos.shape,
            },
        )
        return sos


class ParametricEQ:
    def __init__(self, bands: list[EQBand], sample_rate: int):
        self.bands = bands
        self.sample_rate = sample_rate
        logger.debug(
            "ParametricEQ created",
            extra={"num_bands": len(bands), "sample_rate": sample_rate},
        )

    def combined_sos(self) -> np.ndarray:
        """Stack all band SOS sections into a single (N, 6) array."""
        if not self.bands:
            return np.empty((0, 6))
        sections = [band.to_sos(self.sample_rate) for band in self.bands]
        sos = np.vstack(sections)
        logger.debug(
            "ParametricEQ.combined_sos",
            extra={"sos_shape": sos.shape},
        )
        return sos

    def frequency_response(self, freqs: np.ndarray) -> np.ndarray:
        """Return magnitude response in dB at each frequency in freqs."""
        if not self.bands:
            logger.debug("ParametricEQ.frequency_response: no bands, returning zeros")
            return np.zeros_like(freqs)
        sos = self.combined_sos()
        mag_db = biquad_frequency_response(sos, freqs, self.sample_rate)
        logger.debug(
            "ParametricEQ.frequency_response",
            extra={"num_freqs": len(freqs), "mag_db_min": float(mag_db.min()), "mag_db_max": float(mag_db.max())},
        )
        return mag_db

    def export_autoeq_format(self) -> str:
        """Export EQ settings as AutoEQ-compatible text."""
        lines = ["Preamp: 0.0 dB"]
        for i, band in enumerate(self.bands, 1):
            filter_name = _AUTOEQ_FILTER_NAMES[band.filter_type]
            lines.append(
                f"Filter {i}: ON {filter_name} Fc {band.fc:.0f} Hz "
                f"Gain {band.gain_db:.1f} dB Q {band.q:.3f}"
            )
        text = "\n".join(lines)
        logger.debug(
            "ParametricEQ.export_autoeq_format",
            extra={"num_bands": len(self.bands), "output_lines": len(lines)},
        )
        return text
