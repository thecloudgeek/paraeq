import numpy as np
from unittest.mock import patch, MagicMock
from paraeq.audio.stream import play_signal, record_signal, AudioPassThrough


def test_play_signal_calls_sounddevice():
    with patch("paraeq.audio.stream.sd") as mock_sd:
        mock_sd.wait = MagicMock()
        signal = np.zeros(48000)
        play_signal(signal, sample_rate=48000, device_index=1)
        mock_sd.play.assert_called_once()
        args, kwargs = mock_sd.play.call_args
        np.testing.assert_array_equal(args[0], signal)
        assert kwargs["samplerate"] == 48000
        assert kwargs["device"] == 1


def test_record_signal_calls_sounddevice():
    with patch("paraeq.audio.stream.sd") as mock_sd:
        mock_sd.rec.return_value = np.zeros((48000, 2))
        mock_sd.wait = MagicMock()
        result = record_signal(duration=1.0, sample_rate=48000, channels=2, device_index=2)
        mock_sd.rec.assert_called_once()
        _, kwargs = mock_sd.rec.call_args
        assert kwargs["samplerate"] == 48000
        assert kwargs["channels"] == 2
        assert kwargs["device"] == 2


def test_passthrough_creation():
    pt = AudioPassThrough(input_device=0, output_device=1, sample_rate=48000, block_size=512, channels=2)
    assert pt.sample_rate == 48000
    assert pt.block_size == 512
    assert pt._processor is None


def test_passthrough_set_processor():
    pt = AudioPassThrough(input_device=0, output_device=1, sample_rate=48000, block_size=512, channels=2)
    callback = lambda block: block * 0.5
    pt.set_processor(callback)
    assert pt._processor is not None


def test_set_gain_stores_and_clamps_negative():
    pt = AudioPassThrough(input_device=0, output_device=1)
    assert pt._gain == 1.0  # unity by default
    pt.set_gain(0.5)
    assert pt._gain == 0.5
    pt.set_gain(-2.0)  # negatives clamp to mute, never invert phase
    assert pt._gain == 0.0


def test_callback_applies_gain_to_output():
    pt = AudioPassThrough(input_device=0, output_device=1, block_size=4)
    pt.set_gain(0.5)
    indata = np.ones((4, 2), dtype=np.float32)
    outdata = np.zeros((4, 2), dtype=np.float32)
    pt._audio_callback(indata, outdata, frames=4, time=None, status=None)
    np.testing.assert_allclose(outdata, 0.5)


def test_callback_applies_processor_before_gain():
    pt = AudioPassThrough(input_device=0, output_device=1, block_size=4)
    pt.set_processor(lambda block: block * 2.0)
    pt.set_gain(0.5)
    indata = np.ones((4, 2), dtype=np.float32)
    outdata = np.zeros((4, 2), dtype=np.float32)
    pt._audio_callback(indata, outdata, frames=4, time=None, status=None)
    # processed (x2) then gained (x0.5) == x1.0
    np.testing.assert_allclose(outdata, 1.0)


def test_callback_bypass_passes_input_through_unprocessed():
    pt = AudioPassThrough(input_device=0, output_device=1, block_size=4)
    pt.set_processor(lambda block: block * 0.0)  # would zero everything
    pt.set_bypass(True)
    indata = np.full((4, 2), 0.3, dtype=np.float32)
    outdata = np.zeros((4, 2), dtype=np.float32)
    pt._audio_callback(indata, outdata, frames=4, time=None, status=None)
    np.testing.assert_allclose(outdata, 0.3)


def test_passthrough_stores_channel_maps():
    out_map = [-1] * 16 + [0, 1]
    pt = AudioPassThrough(
        input_device=8,
        output_device=8,
        input_channel_map=[0, 1],
        output_channel_map=out_map,
    )
    assert pt.input_channel_map == [0, 1]
    assert pt.output_channel_map == out_map


def test_start_passes_coreaudio_channel_maps_as_extra_settings():
    with patch("paraeq.audio.stream.sd") as mock_sd:
        pt = AudioPassThrough(
            input_device=8,
            output_device=8,
            input_channel_map=[0, 1],
            output_channel_map=[-1, -1, 0, 1],
        )
        pt.start()

        # One CoreAudioSettings per direction, with the right channel maps.
        ca_calls = mock_sd.CoreAudioSettings.call_args_list
        assert len(ca_calls) == 2
        assert ca_calls[0].kwargs["channel_map"] == [0, 1]
        assert ca_calls[1].kwargs["channel_map"] == [-1, -1, 0, 1]

        # extra_settings is an (input, output) tuple passed to Stream.
        _, stream_kwargs = mock_sd.Stream.call_args
        assert "extra_settings" in stream_kwargs
        assert len(stream_kwargs["extra_settings"]) == 2


def test_start_omits_extra_settings_when_no_channel_maps():
    with patch("paraeq.audio.stream.sd") as mock_sd:
        pt = AudioPassThrough(input_device=0, output_device=1, dtype="float32")
        pt.start()
        _, stream_kwargs = mock_sd.Stream.call_args
        assert "extra_settings" not in stream_kwargs
        assert stream_kwargs["dtype"] == "float32"
        mock_sd.CoreAudioSettings.assert_not_called()
