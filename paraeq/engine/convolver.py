"""Real-time overlap-add FIR convolution engine."""

import logging

import numpy as np

logger = logging.getLogger(__name__)


class OverlapAddConvolver:
    """Block-based overlap-add FIR convolver.

    Supports mono (single FIR) or stereo (list of two FIRs, one per channel).
    """

    def __init__(self, fir: np.ndarray | list[np.ndarray], block_size: int = 512):
        self.block_size = block_size

        if isinstance(fir, list):
            self.stereo = True
            self.n_channels = len(fir)
            self._fir_len = max(len(f) for f in fir)
        else:
            self.stereo = False
            self.n_channels = 1
            fir = [fir]
            self._fir_len = len(fir[0])

        self._n_fft = int(2 ** np.ceil(np.log2(block_size + self._fir_len - 1)))

        self._fir_fft = []
        for f in fir:
            padded = np.zeros(self._n_fft)
            padded[: len(f)] = f
            self._fir_fft.append(np.fft.rfft(padded))

        self._overlap = [np.zeros(self._n_fft) for _ in range(self.n_channels)]

        logger.debug(
            "OverlapAddConvolver initialized",
            extra={
                "block_size": block_size,
                "n_channels": self.n_channels,
                "fir_len": self._fir_len,
                "n_fft": self._n_fft,
                "stereo": self.stereo,
            },
        )

    def process(self, block: np.ndarray) -> np.ndarray:
        """Process a block of audio samples through the FIR filter.

        Args:
            block: Input samples. Shape (block_size,) for mono or
                   (block_size, n_channels) for stereo.

        Returns:
            Filtered output with the same shape as input.
        """
        if block.ndim == 1:
            channels = [block]
        else:
            channels = [block[:, ch] for ch in range(block.shape[1])]

        outputs = []
        for ch_idx, ch_data in enumerate(channels):
            fir_idx = min(ch_idx, len(self._fir_fft) - 1)
            padded = np.zeros(self._n_fft)
            padded[: len(ch_data)] = ch_data
            block_fft = np.fft.rfft(padded)
            conv_result = np.fft.irfft(block_fft * self._fir_fft[fir_idx], self._n_fft)
            conv_result += self._overlap[ch_idx]
            self._overlap[ch_idx] = np.zeros(self._n_fft)
            self._overlap[ch_idx][: self._n_fft - self.block_size] = conv_result[self.block_size :]
            outputs.append(conv_result[: self.block_size])

        logger.debug(
            "Processed block",
            extra={"channels": len(channels), "block_len": len(channels[0])},
        )

        if block.ndim == 1:
            return outputs[0]
        return np.column_stack(outputs)

    def reset(self):
        """Reset overlap buffers to clear any accumulated state."""
        for i in range(len(self._overlap)):
            self._overlap[i] = np.zeros(self._n_fft)
        logger.debug("OverlapAddConvolver state reset")
