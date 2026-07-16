#!/usr/bin/env python3
"""Golden-fixture generator: dumps input->output pairs from the Python
prototype (the numerical oracle) for the Rust port's parity tests.

Run:  source .venv/bin/activate && python prototype/tools/generate_fixtures.py
Deterministic: seeded RNG, no timestamps. Rerunning must be byte-identical.

Toolchain pinned: numpy/scipy must match PINNED_VERSIONS (declared as the
`fixtures` extra in prototype/pyproject.toml) or generation is refused —
a drifted scipy/numpy could silently rewrite the goldens. Override with
--allow-version-drift, which stamps "drift": true into fixtures/manifest.json.
The manifest is a provenance record of what ran, never the pin.
"""
import argparse
import json
import platform
import shutil
import sys
from pathlib import Path

import numpy as np
import scipy

from paraeq.correction.auto_fit import auto_fit_parametric_eq
from paraeq.correction.biquad import (
    biquad_frequency_response,
    biquad_high_shelf,
    biquad_low_shelf,
    biquad_notch,
    biquad_peaking,
)
from paraeq.correction.fir_filter import design_fir_correction
from paraeq.correction.parametric_eq import EQBand, ParametricEQ
from paraeq.correction.target_curves import (
    ANCHOR_FREQS,
    build_anchor_target,
    list_builtin_targets,
    match_closest_target,
)
from paraeq.engine.convolver import OverlapAddConvolver
from paraeq.engine.iir_processor import IIRProcessor
from paraeq.measurement.compensation import apply_compensation
from paraeq.measurement.deconvolution import deconvolve
from paraeq.measurement.frequency_response import (
    average_measurements,
    compute_frequency_response,
    fractional_octave_smooth,
    normalize_to_reference_band,
)
from paraeq.measurement.sweep import generate_inverse_sweep, generate_sweep

ROOT = Path(__file__).resolve().parent.parent.parent  # repo root
OUT = ROOT / "fixtures"
# Must equal the `fixtures` extra in prototype/pyproject.toml — that extra is
# the declaration, this dict is the enforcement, and fixtures/manifest.json is
# only the record of what actually ran.
PINNED_VERSIONS = {"numpy": "2.5.0", "scipy": "1.18.0"}
SR = 48000


def save_case(stage: str, name: str, params: dict, arrays: dict, scalars: dict | None = None):
    d = OUT / stage
    d.mkdir(parents=True, exist_ok=True)
    case = {"params": params, "scalars": scalars or {}, "arrays": {}}
    for key, arr in arrays.items():
        arr = np.ascontiguousarray(np.asarray(arr, dtype=np.float64))
        fname = f"{name}.{key}.f64"
        (d / fname).write_bytes(arr.astype("<f8").tobytes())
        case["arrays"][key] = {"file": fname, "len": int(arr.size), "shape": list(arr.shape)}
    (d / f"{name}.json").write_text(json.dumps(case, indent=1, sort_keys=True) + "\n")


def gen_sweep():
    sweep = generate_sweep(0.25, SR)
    inverse = generate_inverse_sweep(sweep, SR)
    save_case("sweep", "basic", {"duration": 0.25, "sample_rate": SR, "f_start": 20.0, "f_end": 20000.0},
              {"sweep": sweep, "inverse": inverse})


def gen_deconvolution():
    rng = np.random.default_rng(42)
    sweep = generate_sweep(0.25, SR)
    ir_true = np.zeros(256)
    ir_true[32] = 1.0
    ir_true[33:97] = rng.standard_normal(64) * 0.05 * np.exp(-np.arange(64) / 16.0)
    recorded = np.convolve(sweep, ir_true)[: len(sweep)]
    ir_out = deconvolve(recorded, sweep, SR)
    save_case("deconvolution", "delta_plus_tail", {"sample_rate": SR},
              {"sweep": sweep, "recorded": recorded, "ir_true": ir_true, "ir_out": ir_out})


def gen_frequency_response():
    rng = np.random.default_rng(43)
    ir = rng.standard_normal((1024, 2)) * np.exp(-np.arange(1024) / 128.0)[:, None]
    freqs, mag_db = compute_frequency_response(ir, SR)
    save_case("fr", "stereo_decay", {"sample_rate": SR, "n_fft": 1024},
              {"ir": ir, "freqs": freqs, "mag_db": mag_db})

    mono = mag_db[:, 0]
    for fraction in (3, 6, 12):
        sm = fractional_octave_smooth(mono, freqs, fraction=fraction)
        save_case("fr", f"smooth_1_{fraction}", {"fraction": fraction},
                  {"freqs": freqs, "mag_db": mono, "smoothed": sm})

    a, b = mono, mono + 6.0
    save_case("fr", "average", {}, {"a": a, "b": b, "avg": average_measurements([a, b])})
    save_case("fr", "normalize", {"low_hz": 200.0, "high_hz": 1000.0},
              {"freqs": freqs, "mag_db": mono,
               "normalized": normalize_to_reference_band(freqs, mono)})


def gen_compensation():
    comp_freqs = np.array([20.0, 100.0, 1000.0, 10000.0, 20000.0])
    comp_gains = np.array([-2.0, 0.5, 0.0, 1.5, -3.0])
    grid = np.geomspace(10.0, 24000.0, 256)
    mag = np.zeros_like(grid)
    out = apply_compensation(mag, grid, comp_freqs, comp_gains)
    save_case("compensation", "edge_hold", {},
              {"comp_freqs": comp_freqs, "comp_gains": comp_gains, "grid": grid,
               "mag_db": mag, "compensated": out})


def gen_targets():
    targets = list_builtin_targets()
    harman = next(t for t in targets if t.name == "Harman In-Ear 2019")
    dense = np.geomspace(20.0, 20000.0, 512)
    edges = np.array([0.5, 5.0, 15.0, 25000.0, 40000.0])  # clamp/extrapolation edges
    save_case("targets", "harman_ie_interp", {"target": "Harman In-Ear 2019"},
              {"curve_freqs": harman.frequencies, "curve_gains": harman.gains_db,
               "dense": dense, "interp_dense": harman.interpolate(dense),
               "edges": edges, "interp_edges": harman.interpolate(edges)})

    rng = np.random.default_rng(44)
    offsets = np.round(rng.uniform(-4.0, 4.0, size=ANCHOR_FREQS.shape[0]), 3)
    custom = build_anchor_target(harman, ANCHOR_FREQS, offsets)
    save_case("targets", "anchor_deviation", {"base": "Harman In-Ear 2019"},
              {"anchor_freqs": ANCHOR_FREQS, "offsets_db": offsets,
               "result_freqs": custom.frequencies, "result_gains": custom.gains_db})

    measured = harman.interpolate(dense) + rng.standard_normal(dense.shape[0]) * 0.75
    best = match_closest_target(dense, measured, targets)
    save_case("targets", "match_closest", {},
              {"measured_freqs": dense, "measured_db": measured},
              scalars={"expected_name": best.name})


def gen_fir():
    rng = np.random.default_rng(45)
    n_bins = 2049
    corr = np.cumsum(rng.standard_normal(n_bins)) * 0.05
    corr -= corr.mean()
    corr = np.clip(corr, -6.0, 6.0)
    for n_taps in (512, 4096):
        for phase in ("linear", "minimum"):
            taps = design_fir_correction(corr, np.array([]), n_taps=n_taps, phase=phase)
            save_case("fir", f"{phase}_{n_taps}", {"n_taps": n_taps, "phase": phase},
                      {"correction_db": corr, "taps": taps})


def gen_biquad():
    cases = []
    grid = np.geomspace(20.0, 20000.0, 128)
    for kind, fc, gain, q in [
        ("peaking", 1000.0, 6.0, 1.0), ("peaking", 80.0, -4.5, 2.0),
        ("peaking", 12000.0, 3.0, 0.7), ("low_shelf", 100.0, 4.0, 0.707),
        ("low_shelf", 250.0, -6.0, 1.0), ("high_shelf", 8000.0, -3.0, 0.707),
        ("high_shelf", 4000.0, 5.0, 0.5), ("notch", 60.0, 0.0, 10.0),
        ("notch", 1000.0, 0.0, 4.0),
    ]:
        if kind == "peaking":
            sos = biquad_peaking(fc, gain, q, SR)
        elif kind == "low_shelf":
            sos = biquad_low_shelf(fc, gain, q, SR)
        elif kind == "high_shelf":
            sos = biquad_high_shelf(fc, gain, q, SR)
        else:
            sos = biquad_notch(fc, q, SR)
        cases.append({"kind": kind, "fc": fc, "gain_db": gain, "q": q,
                      "sos": [float(x) for x in sos[0]]})
    resp_sos = biquad_peaking(1000.0, 6.0, 1.0, SR)
    save_case("biquad", "matrix", {"sample_rate": SR},
              {"resp_freqs": grid,
               "resp_db": biquad_frequency_response(resp_sos, grid, SR)},
              scalars={"cases": cases})


def gen_peq_autofit():
    bands = [EQBand("peaking", 100.0, 5.0, 1.2), EQBand("high_shelf", 9000.0, -3.5, 0.707)]
    peq = ParametricEQ(bands, SR)
    grid = np.geomspace(20.0, 20000.0, 256)
    save_case("peq", "two_band", {"sample_rate": SR},
              {"freqs": grid, "response_db": peq.frequency_response(grid)},
              scalars={"autoeq_export": peq.export_autoeq_format(),
                       "bands": [{"filter_type": b.filter_type, "fc": b.fc,
                                  "gain_db": b.gain_db, "q": b.q} for b in bands]})

    freqs = np.geomspace(20.0, 20000.0, 512)
    seed_bands = [EQBand("peaking", 150.0, 6.0, 2.0), EQBand("peaking", 3000.0, -5.0, 3.0)]
    corr = ParametricEQ(seed_bands, SR).frequency_response(freqs)
    fitted = auto_fit_parametric_eq(corr, freqs, SR, max_bands=4)
    save_case("autofit", "two_peaks", {"sample_rate": SR, "max_bands": 4},
              {"freqs": freqs, "correction_db": corr},
              scalars={"fitted": [{"filter_type": b.filter_type, "fc": float(b.fc),
                                   "gain_db": float(b.gain_db), "q": float(b.q)}
                                  for b in fitted]})


def gen_convolver():
    rng = np.random.default_rng(46)
    fir_l = rng.standard_normal(1024) * np.exp(-np.arange(1024) / 200.0)
    fir_r = fir_l * 0.5
    conv = OverlapAddConvolver([fir_l, fir_r], block_size=512)
    blocks_in = rng.standard_normal((8 * 512, 2))
    out = np.vstack([conv.process(blocks_in[i * 512:(i + 1) * 512]) for i in range(8)])
    save_case("convolver", "stereo_8_blocks", {"block_size": 512},
              {"fir_l": fir_l, "fir_r": fir_r, "input": blocks_in, "output": out})


def gen_iir():
    rng = np.random.default_rng(47)
    sos = np.vstack([biquad_peaking(80.0, 6.0, 1.0, SR),
                     biquad_peaking(1000.0, -4.0, 2.0, SR),
                     biquad_high_shelf(8000.0, 3.0, 0.707, SR)])
    proc = IIRProcessor(SR)
    proc.set_sos(sos, channel=0)
    proc.set_sos(sos, channel=1)
    blocks_in = rng.standard_normal((4 * 512, 2))
    out = np.vstack([proc.process(blocks_in[i * 512:(i + 1) * 512]) for i in range(4)])
    save_case("iir", "cascade_4_blocks", {"sample_rate": SR},
              {"sos": sos, "input": blocks_in, "output": out})


def check_pinned_versions(allow_drift: bool) -> bool:
    """Refuse to touch fixtures/ under a drifted toolchain unless overridden.

    Returns True when generation proceeds under drifted versions (the caller
    must then stamp "drift": true into the manifest).
    """
    installed = {"numpy": np.__version__, "scipy": scipy.__version__}
    drifted = sorted(name for name in PINNED_VERSIONS if installed[name] != PINNED_VERSIONS[name])
    if not drifted:
        return False
    for name in drifted:
        print(f"ERROR: {name} {installed[name]} != pinned {name}=={PINNED_VERSIONS[name]} "
              "(prototype/pyproject.toml, `fixtures` extra)", file=sys.stderr)
    if not allow_drift:
        print("Refusing to regenerate: a drifted numpy/scipy could silently rewrite the "
              "goldens. Install the pins (.venv/bin/pip install -e './prototype[fixtures]') "
              "or rerun with --allow-version-drift to proceed anyway.", file=sys.stderr)
        sys.exit(1)
    print('WARNING: --allow-version-drift: generating anyway; stamping "drift": true '
          "into fixtures/manifest.json", file=sys.stderr)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--allow-version-drift", action="store_true",
        help='generate despite numpy/scipy not matching the pins; stamps "drift": true '
             "into fixtures/manifest.json")
    args = parser.parse_args()
    drift = check_pinned_versions(args.allow_version_drift)
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()
    gen_sweep()
    gen_deconvolution()
    gen_frequency_response()
    gen_compensation()
    gen_targets()
    gen_fir()
    gen_biquad()
    gen_peq_autofit()
    gen_convolver()
    gen_iir()
    manifest = {
        "dtype": "<f8",
        "numpy": np.__version__,
        "python": platform.python_version(),
        "scipy": scipy.__version__,
    }
    if drift:
        manifest["drift"] = True
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    n = sum(1 for _ in OUT.rglob("*"))
    print(f"wrote {n} files under {OUT}")


if __name__ == "__main__":
    main()
