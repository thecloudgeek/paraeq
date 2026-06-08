"""Audio playback, recording, and real-time pass-through streams."""

import logging
from collections.abc import Callable
from typing import Any

import numpy as np
import sounddevice as sd

logger = logging.getLogger(__name__)


def play_signal(signal: np.ndarray, sample_rate: int, device_index: int | None = None) -> None:
    sd.play(signal, samplerate=sample_rate, device=device_index)
    sd.wait()


def record_signal(duration: float, sample_rate: int, channels: int = 2, device_index: int | None = None) -> np.ndarray:
    n_samples = int(duration * sample_rate)
    recording = sd.rec(n_samples, samplerate=sample_rate, channels=channels, device=device_index, dtype=np.float64)
    sd.wait()
    return recording


class AudioPassThrough:
    def __init__(
        self,
        input_device: int,
        output_device: int,
        sample_rate: int = 48000,
        block_size: int = 512,
        channels: int = 2,
        dtype: str = "float32",
        input_channel_map: list[int] | None = None,
        output_channel_map: list[int] | None = None,
    ):
        self.input_device = input_device
        self.output_device = output_device
        self.sample_rate = sample_rate
        self.block_size = block_size
        self.channels = channels
        self.dtype = dtype
        self.input_channel_map = input_channel_map
        self.output_channel_map = output_channel_map
        self._processor: Callable[[np.ndarray], np.ndarray] | None = None
        self._callback_count = 0  # for periodic throughput logging
        self._stream: sd.Stream | None = None
        self._bypass = False
        self._gain: float = 1.0  # linear output gain, 0..1 (BlackHole has no system volume)
        self._pre_callback: Callable[[np.ndarray], None] | None = None
        self._post_callback: Callable[[np.ndarray], None] | None = None

    def set_processor(self, processor: Callable[[np.ndarray], np.ndarray]):
        self._processor = processor

    def set_bypass(self, bypass: bool):
        self._bypass = bypass

    def set_gain(self, gain: float):
        """Set linear output gain (0.0 = mute, 1.0 = unity)."""
        self._gain = max(0.0, float(gain))

    def set_pre_callback(self, callback: Callable[[np.ndarray], None]):
        self._pre_callback = callback

    def set_post_callback(self, callback: Callable[[np.ndarray], None]):
        self._post_callback = callback

    def _audio_callback(self, indata: np.ndarray, outdata: np.ndarray, frames: int, time: Any, status: sd.CallbackFlags):
        if status:
            logger.warning("audio callback status: %s", status)
        if self._pre_callback is not None:
            self._pre_callback(indata.copy())
        if self._bypass or self._processor is None:
            outdata[:] = indata
        else:
            outdata[:] = self._processor(indata)
        if self._gain != 1.0:
            outdata *= self._gain
        if self._post_callback is not None:
            self._post_callback(outdata.copy())

        # Throughput log roughly once per second so signal levels are visible
        # at DEBUG level — BlackHole routing is easy to get wrong, and this is
        # the fastest way to confirm signal is flowing in and back out.
        self._callback_count += 1
        if self._callback_count * frames >= self.sample_rate:
            self._callback_count = 0
            in_rms = float(np.sqrt(np.mean(indata.astype(np.float64) ** 2)))
            out_rms = float(np.sqrt(np.mean(outdata.astype(np.float64) ** 2)))
            logger.debug(
                "audio throughput",
                extra={
                    "in_dbfs": 20.0 * np.log10(max(in_rms, 1e-10)),
                    "out_dbfs": 20.0 * np.log10(max(out_rms, 1e-10)),
                    "bypass": self._bypass,
                    "processing": self._processor is not None,
                    "gain": self._gain,
                },
            )

    def start(self):
        kwargs: dict[str, Any] = {}
        if self.input_channel_map is not None or self.output_channel_map is not None:
            in_settings = (
                sd.CoreAudioSettings(channel_map=self.input_channel_map)
                if self.input_channel_map is not None
                else None
            )
            out_settings = (
                sd.CoreAudioSettings(channel_map=self.output_channel_map)
                if self.output_channel_map is not None
                else None
            )
            kwargs["extra_settings"] = (in_settings, out_settings)

        self._stream = sd.Stream(
            samplerate=self.sample_rate, blocksize=self.block_size,
            device=(self.input_device, self.output_device),
            channels=self.channels, dtype=self.dtype,
            callback=self._audio_callback,
            **kwargs,
        )
        self._stream.start()

    def stop(self):
        if self._stream is not None:
            self._stream.stop()
            self._stream.close()
            self._stream = None
