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
