import numpy as np
from paraeq.engine.convolver import OverlapAddConvolver


def test_convolver_passthrough():
    fir = np.zeros(256)
    fir[0] = 1.0
    conv = OverlapAddConvolver(fir, block_size=512)
    input_block = np.random.RandomState(42).randn(512)
    output = conv.process(input_block)
    np.testing.assert_array_almost_equal(output, input_block, decimal=6)


def test_convolver_gain():
    fir = np.zeros(64)
    fir[0] = 2.0
    conv = OverlapAddConvolver(fir, block_size=512)
    input_block = np.ones(512)
    output = conv.process(input_block)
    np.testing.assert_array_almost_equal(output, np.full(512, 2.0), decimal=6)


def test_convolver_continuity_across_blocks():
    fir = np.zeros(128)
    fir[64] = 1.0
    conv = OverlapAddConvolver(fir, block_size=256)
    t = np.arange(512) / 48000.0
    signal = np.sin(2 * np.pi * 1000 * t)
    out1 = conv.process(signal[:256])
    out2 = conv.process(signal[256:])
    output = np.concatenate([out1, out2])
    delayed = np.sin(2 * np.pi * 1000 * (t - 64 / 48000.0))
    np.testing.assert_array_almost_equal(output[128:384], delayed[128:384], decimal=3)


def test_convolver_stereo():
    fir_l = np.zeros(64)
    fir_l[0] = 1.0
    fir_r = np.zeros(64)
    fir_r[0] = 0.5
    conv = OverlapAddConvolver([fir_l, fir_r], block_size=256)
    input_block = np.ones((256, 2))
    output = conv.process(input_block)
    assert output.shape == (256, 2)
    np.testing.assert_array_almost_equal(output[:, 0], 1.0, decimal=6)
    np.testing.assert_array_almost_equal(output[:, 1], 0.5, decimal=6)


def test_convolver_reset():
    fir = np.zeros(128)
    fir[64] = 1.0
    conv = OverlapAddConvolver(fir, block_size=256)
    conv.process(np.ones(256))
    conv.reset()
    output = conv.process(np.zeros(256))
    assert np.max(np.abs(output)) < 1e-10
