"""Real-time cascaded biquad IIR filter processor."""

import numpy as np
from scipy.signal import sosfilt


class IIRProcessor:
    def __init__(self, sample_rate: int):
        self.sample_rate = sample_rate
        self._sos: dict[int, np.ndarray] = {}
        self._zi: dict[int, np.ndarray] = {}

    def set_sos(self, sos: np.ndarray, channel: int = 0):
        self._sos[channel] = sos.copy()
        self._zi[channel] = np.zeros((sos.shape[0], 2))

    def process(self, block: np.ndarray) -> np.ndarray:
        if block.ndim == 1:
            if 0 not in self._sos:
                return block.copy()
            output, self._zi[0] = sosfilt(self._sos[0], block, zi=self._zi[0])
            return output

        n_channels = block.shape[1]
        outputs = []
        for ch in range(n_channels):
            if ch not in self._sos:
                outputs.append(block[:, ch].copy())
            else:
                out, self._zi[ch] = sosfilt(self._sos[ch], block[:, ch], zi=self._zi[ch])
                outputs.append(out)
        return np.column_stack(outputs)

    def reset(self):
        for ch in self._zi:
            self._zi[ch] = np.zeros_like(self._zi[ch])
