"""Audio playback, recording, and real-time pass-through streams."""

from collections.abc import Callable
from typing import Any

import numpy as np
import sounddevice as sd


def play_signal(signal: np.ndarray, sample_rate: int, device_index: int | None = None) -> None:
    sd.play(signal, samplerate=sample_rate, device=device_index)
    sd.wait()


def record_signal(duration: float, sample_rate: int, channels: int = 2, device_index: int | None = None) -> np.ndarray:
    n_samples = int(duration * sample_rate)
    recording = sd.rec(n_samples, samplerate=sample_rate, channels=channels, device=device_index, dtype=np.float64)
    sd.wait()
    return recording


class AudioPassThrough:
    def __init__(self, input_device: int, output_device: int, sample_rate: int = 48000, block_size: int = 512, channels: int = 2):
        self.input_device = input_device
        self.output_device = output_device
        self.sample_rate = sample_rate
        self.block_size = block_size
        self.channels = channels
        self._processor: Callable[[np.ndarray], np.ndarray] | None = None
        self._stream: sd.Stream | None = None
        self._bypass = False
        self._pre_callback: Callable[[np.ndarray], None] | None = None
        self._post_callback: Callable[[np.ndarray], None] | None = None

    def set_processor(self, processor: Callable[[np.ndarray], np.ndarray]):
        self._processor = processor

    def set_bypass(self, bypass: bool):
        self._bypass = bypass

    def set_pre_callback(self, callback: Callable[[np.ndarray], None]):
        self._pre_callback = callback

    def set_post_callback(self, callback: Callable[[np.ndarray], None]):
        self._post_callback = callback

    def _audio_callback(self, indata: np.ndarray, outdata: np.ndarray, frames: int, time: Any, status: sd.CallbackFlags):
        if self._pre_callback is not None:
            self._pre_callback(indata.copy())
        if self._bypass or self._processor is None:
            outdata[:] = indata
        else:
            outdata[:] = self._processor(indata)
        if self._post_callback is not None:
            self._post_callback(outdata.copy())

    def start(self):
        self._stream = sd.Stream(
            samplerate=self.sample_rate, blocksize=self.block_size,
            device=(self.input_device, self.output_device),
            channels=self.channels, dtype=np.float64,
            callback=self._audio_callback,
        )
        self._stream.start()

    def stop(self):
        if self._stream is not None:
            self._stream.stop()
            self._stream.close()
            self._stream = None
