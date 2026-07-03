from paraeq.audio.devices import AudioDevice, find_blackhole_device, list_input_devices, list_output_devices


def test_audio_device_dataclass():
    dev = AudioDevice(index=0, name="Test Device", channels_in=2, channels_out=0, sample_rate=48000.0)
    assert dev.name == "Test Device"
    assert dev.channels_in == 2
    assert dev.is_input
    assert not dev.is_output


def test_list_input_devices_returns_list():
    devices = list_input_devices()
    assert isinstance(devices, list)


def test_list_output_devices_returns_list():
    devices = list_output_devices()
    assert isinstance(devices, list)


def test_find_blackhole_returns_none_or_device():
    result = find_blackhole_device()
    assert result is None or isinstance(result, AudioDevice)
