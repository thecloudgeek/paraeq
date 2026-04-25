"""End-to-end integration test: sweep → measure → correct → filter."""

import numpy as np
from paraeq.measurement.sweep import generate_sweep
from paraeq.measurement.deconvolution import deconvolve
from paraeq.measurement.frequency_response import compute_frequency_response, fractional_octave_smooth
from paraeq.correction.target_curves import list_builtin_targets, compute_correction
from paraeq.correction.fir_filter import design_fir_correction
from paraeq.correction.auto_fit import auto_fit_parametric_eq
from paraeq.correction.parametric_eq import ParametricEQ
from paraeq.engine.convolver import OverlapAddConvolver
from paraeq.engine.iir_processor import IIRProcessor
from paraeq.correction.biquad import biquad_peaking
from scipy.signal import sosfilt


def test_full_measurement_to_fir_pipeline():
    """Simulate: colored headphone → sweep → deconvolve → FIR correction."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    # Simulate colored headphone: +6dB resonance at 3kHz
    colored_sos = biquad_peaking(fc=3000.0, gain_db=6.0, q=2.0, sample_rate=sr)
    recorded = sosfilt(colored_sos, sweep)
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    mag_db = fractional_octave_smooth(mag_db, freqs, fraction=6)
    targets = list_builtin_targets()
    flat = next(t for t in targets if t.name == "flat")
    target_interp = flat.interpolate(freqs)
    correction = compute_correction(mag_db, target_interp)
    fir = design_fir_correction(correction, freqs, n_taps=4096, phase="minimum")
    assert len(fir) == 4096
    # Apply FIR and verify it reduces the coloration
    convolver = OverlapAddConvolver(fir, block_size=512)
    t = np.arange(4096) / sr
    test_tone = np.sin(2 * np.pi * 3000 * t) * 0.5
    colored_tone = sosfilt(colored_sos, test_tone)
    corrected = np.concatenate([convolver.process(colored_tone[i:i+512]) for i in range(0, len(colored_tone), 512)])
    colored_error = np.sqrt(np.mean((colored_tone[2048:] - test_tone[2048:]) ** 2))
    corrected_error = np.sqrt(np.mean((corrected[2048:] - test_tone[2048:]) ** 2))
    assert corrected_error < colored_error


def test_full_measurement_to_peq_pipeline():
    """Simulate measurement → auto-fit parametric EQ."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    colored_sos = biquad_peaking(fc=3000.0, gain_db=6.0, q=2.0, sample_rate=sr)
    recorded = sosfilt(colored_sos, sweep)
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    mag_db = fractional_octave_smooth(mag_db, freqs, fraction=6)
    targets = list_builtin_targets()
    flat = next(t for t in targets if t.name == "flat")
    target_interp = flat.interpolate(freqs)
    correction = compute_correction(mag_db, target_interp)
    bands = auto_fit_parametric_eq(correction, freqs, sample_rate=sr, max_bands=10)
    assert len(bands) >= 1
    eq = ParametricEQ(bands=bands, sample_rate=sr)
    sos = eq.combined_sos()
    assert sos.shape[0] >= 1
    proc = IIRProcessor(sample_rate=sr)
    proc.set_sos(sos)
    test_block = np.random.RandomState(42).randn(512) * 0.1
    output = proc.process(test_block)
    assert output.shape == test_block.shape


def test_profile_round_trip():
    """Create profile with EQ bands, save, load, verify."""
    import tempfile
    from pathlib import Path
    from paraeq.profiles.profile import Profile, ProfileManager

    with tempfile.TemporaryDirectory() as tmpdir:
        manager = ProfileManager(profiles_dir=Path(tmpdir))
        p = Profile(name="SE846-Harman", headphone_model="Shure SE846", target_curve_name="harman_ie_2019")
        p.set_eq_bands([
            {"filter_type": "peaking", "fc": 3000.0, "gain_db": -4.0, "q": 2.0},
            {"filter_type": "low_shelf", "fc": 100.0, "gain_db": 2.0, "q": 0.707},
        ])
        ir = np.random.randn(4096, 2)
        p.set_measurement(ir, sample_rate=48000)
        manager.save(p)
        loaded = manager.load("SE846-Harman")
        assert loaded.headphone_model == "Shure SE846"
        assert len(loaded.eq_bands) == 2
        assert loaded.impulse_response is not None
