import numpy as np
from paraeq.engine.iir_processor import IIRProcessor
from paraeq.correction.biquad import biquad_peaking


def test_iir_passthrough_with_no_filters():
    proc = IIRProcessor(sample_rate=48000)
    block = np.random.RandomState(42).randn(512).astype(np.float64)
    output = proc.process(block)
    np.testing.assert_array_almost_equal(output, block)


def test_iir_applies_filter():
    sr = 48000
    sos = biquad_peaking(fc=1000.0, gain_db=6.0, q=1.0, sample_rate=sr)
    proc = IIRProcessor(sample_rate=sr)
    proc.set_sos(sos)
    t = np.arange(4096) / sr
    signal = np.sin(2 * np.pi * 1000 * t) * 0.5
    output = np.concatenate([
        proc.process(signal[i:i+512]) for i in range(0, len(signal), 512)
    ])
    input_rms = np.sqrt(np.mean(signal[2048:] ** 2))
    output_rms = np.sqrt(np.mean(output[2048:] ** 2))
    gain_db = 20 * np.log10(output_rms / input_rms)
    assert abs(gain_db - 6.0) < 1.0


def test_iir_stereo():
    sr = 48000
    sos_l = biquad_peaking(fc=1000.0, gain_db=6.0, q=1.0, sample_rate=sr)
    sos_r = biquad_peaking(fc=1000.0, gain_db=-6.0, q=1.0, sample_rate=sr)
    proc = IIRProcessor(sample_rate=sr)
    proc.set_sos(sos_l, channel=0)
    proc.set_sos(sos_r, channel=1)
    block = np.ones((512, 2)) * 0.5
    output = proc.process(block)
    assert output.shape == (512, 2)


def test_iir_reset():
    sr = 48000
    sos = biquad_peaking(fc=1000.0, gain_db=12.0, q=1.0, sample_rate=sr)
    proc = IIRProcessor(sample_rate=sr)
    proc.set_sos(sos)
    proc.process(np.ones(512))
    proc.reset()
    output = proc.process(np.zeros(512))
    assert np.max(np.abs(output)) < 0.01
