"""Audio device discovery and selection via sounddevice."""

import logging
from dataclasses import dataclass

import sounddevice as sd

logger = logging.getLogger(__name__)


@dataclass
class AudioDevice:
    index: int
    name: str
    channels_in: int
    channels_out: int
    sample_rate: float

    @property
    def is_input(self) -> bool:
        return self.channels_in > 0

    @property
    def is_output(self) -> bool:
        return self.channels_out > 0


def _query_devices() -> list[AudioDevice]:
    devices = []
    raw = sd.query_devices()
    # DeviceList is iterable but not a plain list; a single-device system may
    # return a bare dict — normalise both cases into a sequence of dicts.
    if isinstance(raw, dict):
        raw = [raw]
    for dev in raw:
        devices.append(AudioDevice(
            index=dev["index"],
            name=dev["name"],
            channels_in=dev["max_input_channels"],
            channels_out=dev["max_output_channels"],
            sample_rate=dev["default_samplerate"],
        ))
    logger.debug("queried %d audio devices", len(devices))
    return devices


def list_input_devices() -> list[AudioDevice]:
    devices = [d for d in _query_devices() if d.is_input]
    logger.debug("found %d input devices", len(devices))
    return devices


def list_output_devices() -> list[AudioDevice]:
    devices = [d for d in _query_devices() if d.is_output]
    logger.debug("found %d output devices", len(devices))
    return devices


def find_blackhole_device() -> AudioDevice | None:
    for dev in _query_devices():
        if "blackhole" in dev.name.lower():
            logger.info("found BlackHole device: %s (index=%d)", dev.name, dev.index)
            return dev
    logger.debug("no BlackHole device found")
    return None
