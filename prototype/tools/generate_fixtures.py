#!/usr/bin/env python3
"""Golden-fixture generator for the Rust port's parity tests.

Two tiers are written from here (docs/specs/2026-07-15-measurement-suite-design.md,
"The four tiers"):

  Tier 1 -- dumps input->output pairs from the Python prototype, the numerical
    oracle for the ten modules ported from it. FROZEN: not regenerated for
    measurement-suite work.
  Tier 2 -- scipy/numpy-direct cases for new primitives that have a library
    delegate. These import NOTHING from paraeq, on purpose: the fixture's
    provenance is the library, not our own ported code, so the Rust module
    cannot be graded against a transcription of itself. Marked `Tier 2` in each
    gen_*() docstring.

fixtures/decide/ is a fifth kind and is NOT one of them: owner-reviewed
decision bundles, frozen once accepted, which this script neither writes nor
removes. No prototype decision engine is ever written, so there is nothing here
that could regenerate them -- see PRESERVED below for how they are protected.

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
import math
import platform
import shutil
import sys
from pathlib import Path

import numpy as np
import scipy
from scipy.ndimage import gaussian_filter1d
from scipy.signal.windows import blackmanharris, boxcar, hann, tukey

from paraeq.correction.auto_fit import auto_fit_parametric_eq
from paraeq.correction.autoeq_db import parse_parametric_eq
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
# Children of fixtures/ this script must never write and never remove. Only
# fixtures/decide/ qualifies: those bundles are owner-reviewed characterization
# records, frozen once accepted, and no code here can reproduce them.
PRESERVED = frozenset({"decide"})
SR = 48000


def wipe_generated_fixtures():
    """Clear what this script owns, child by child, leaving PRESERVED alone.

    Not shutil.rmtree(OUT): that recursively deleted fixtures/decide/ too, so
    the regeneration command CLAUDE.md sanctions destroyed every owner-frozen
    decision bundle in passing. The wipe is still total for everything else --
    a renamed or deleted gen_*() case must not leave an orphan behind.
    """
    OUT.mkdir(parents=True, exist_ok=True)
    for child in sorted(OUT.iterdir()):
        if child.name in PRESERVED:
            continue
        if child.is_dir():
            shutil.rmtree(child)
        else:
            child.unlink()


def save_case(stage: str, name: str, params: dict, arrays: dict, scalars: dict | None = None):
    # Before the mkdir, so a mis-named stage cannot even create the directory.
    assert stage not in PRESERVED, f"{stage}/ is owner-frozen; no gen_*() may write it"
    d = OUT / stage
    d.mkdir(parents=True, exist_ok=True)
    case = {"params": params, "scalars": scalars or {}, "arrays": {}}
    for key, arr in arrays.items():
        arr = np.ascontiguousarray(np.asarray(arr, dtype=np.float64))
        fname = f"{name}.{key}.f64"
        (d / fname).write_bytes(arr.astype("<f8").tobytes())
        case["arrays"][key] = {"file": fname, "len": int(arr.size), "shape": list(arr.shape)}
    (d / f"{name}.json").write_text(json.dumps(case, indent=1, sort_keys=True) + "\n")


def log_grid(f_min: float = 20.0, f_max: float = 20000.0, ppo: int = 96) -> np.ndarray:
    """The logf.rs axis: f_i = f_min * 2^(i/ppo) for i in [0, N).

    N = floor(ppo*log2(f_max/f_min)) + 1 -- the largest set of ppo-spaced points
    that does not EXCEED f_max, so the top bin falls short by under one spacing.
    Deliberate; see the room-DSP spec's logf.rs section before "fixing" it.

    Two traps, both measured against this pinned toolchain rather than assumed:

    - Not np.logspace. The spec's Tier-2 wording ("np.interp on np.logspace") is
      loose: logspace's endpoint-based spacing is a DIFFERENT float sequence,
      here by up to 2.5e-11 Hz -- above the 1e-12 bar these cases are compared
      at. These are the query points LogGrid::standard() must reproduce, so the
      formula is the contract and logspace would fail a correct implementation.
    - The standard grid's top bin is 19897.0 Hz, not the 19910 Hz the spec's
      prose states -- that figure is an arithmetic slip in the spec, not a
      different grid. N = 957 and f[0] = 20.0 exactly, both as specified.
    """
    n = int(np.floor(ppo * np.log2(f_max / f_min))) + 1
    return f_min * 2.0 ** (np.arange(n) / ppo)


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


def gen_cal():
    """Cal-file parse cases for REW's leading-numeric rule (Tier 2).

    Constructive, and deliberately so: each file is RENDERED FROM the golden
    curve, so no parser supplies its own expectation and neither half of the
    rewrite grades its own homework. `single_header` and `tab_delimited` pin
    the dropped-first-row bug the old quote-sniff + skiprows=2 parsers shared
    -- under the old rule they load six rows, not seven. `two_header` and
    `comma_delimited` are the invariance witnesses: the old rule got those
    right, which is exactly why the bug survived.
    """
    freqs = np.array([20.0, 50.0, 100.0, 1000.0, 5000.0, 10000.0, 20000.0])
    gains = np.array([0.5, 0.3, 0.0, -0.2, -0.5, -1.0, -2.0])
    triples = "".join(f"{f:>10.4f} {g:>9.4f} {0.0:>9.4f}\n" for f, g in zip(freqs, gains))
    cases = {
        # ParaEQ CSV: # comment, comma-separated pairs.
        "comma_delimited": "# ParaEQ compensation curve\n"
                           + "".join(f"{f:.4f},{g:.4f}\n" for f, g in zip(freqs, gains)),
        # UMIK-1 0-degree: ONE quoted header, 3-column whitespace rows.
        "single_header": '"Sens Factor =-0.4210dB, SERNO: 7103798"\n' + triples,
        # UMIK-1 as shipped: one header, tab-separated pairs.
        "tab_delimited": '"Sens Factor =-0.4210dB, AGain =18dB, SERNO: 7103798"\n'
                         + "".join(f"{f:.4f}\t{g:.4f}\n" for f, g in zip(freqs, gains)),
        # miniDSP EARS: TWO quoted headers, * comments, 3-column rows.
        "two_header": '"Sens Factor =-0.8dB, EARS Serial 999-9999, compensation RAW V1"\n'
                      '"Use this file on the LEFT channel. Your sensitive side is RIGHT."\n'
                      "*\n* Freq(Hz) SPL(dB) Phase(degrees)\n*\n" + triples,
    }
    for name, text in cases.items():
        save_case("compensation", name, {"cal_file": f"{name}.cal.txt"},
                  {"freqs": freqs, "gains": gains})
        (OUT / "compensation" / f"{name}.cal.txt").write_text(text)


def gen_gaussian_smoothing():
    """scipy.ndimage.gaussian_filter1d on the log-f axis (Tier 2).

    The reference for fr.rs's Alvarez-Mazorra recursion, which is by construction
    an APPROXIMATION to a true Gaussian -- hence the spec's 1e-3 dB "published
    accuracy" bar rather than a parity bar.

    Boundary handling is deliberately lifted out of the comparison: the curve is
    flat over its outer 200 bins, wider than 6*sigma at the widest sigma here, and
    every sane extension -- scipy's reflect, mirror, nearest, and AM's own --
    reproduces a constant identically. AM's boundary treatment differs from
    scipy's and is not the claim under test; the kernel shape is. A case with
    structure running into the edge would fail on the extension and read as a
    kernel bug.

    sigma is in BINS on the uniform octave axis, where constant sigma IS
    constant-Q -- the coordinate trick fdw.rs shares. At 96 ppo: 2 bins = 1/48
    oct, 8 = 1/12, 32 = 1/3, spanning what fr.rs's Variable profile drives. The
    fraction->sigma mapping is fr.rs's own design decision and is NOT pinned here.
    """
    rng = np.random.default_rng(48)
    freqs = log_grid()
    flat = 200
    interior = np.cumsum(rng.standard_normal(freqs.shape[0] - 2 * flat)) * 0.3
    interior = np.clip(interior - interior.mean(), -12.0, 12.0)
    mag_db = np.concatenate([np.full(flat, interior[0]), interior, np.full(flat, interior[-1])])
    for sigma in (2.0, 8.0, 32.0):
        save_case("fr", f"gaussian_sigma{int(sigma)}",
                  {"flat_margin_bins": flat, "mode": "reflect", "ppo": 96,
                   "sigma_bins": sigma, "truncate": 4.0},
                  {"freqs": freqs, "mag_db": mag_db,
                   "smoothed": gaussian_filter1d(mag_db, sigma, mode="reflect", truncate=4.0)})


def gen_variable_smooth():
    """fr.rs Variable smoothing: per-bin truncated sampled Gaussian (Tier 2).

    The Variable profile cannot be a scipy.ndimage.gaussian_filter1d call -- its
    sigma is per-bin (1/48 oct <100 Hz, 1/6 at 1 kHz, 1/3 >10 kHz, log-f
    interpolated), which no single-sigma primitive expresses. So this is an
    INDEPENDENT numpy re-implementation of the exact operation
    `variable_gaussian_smooth` performs (truncate=4.0, normalized weights,
    clamp-to-edge extension); a transcription bug in the Rust convolution -- wrong
    fraction anchors, linear-vs-log interpolation, wrong normalization -- diverges
    from this reference. The inner accumulation is a scalar loop mirroring the
    Rust summation ORDER so parity holds to 1e-12, not just method accuracy.

    GAUSSIAN_FWHM_PER_SIGMA and the fraction anchors are fr.rs's contract,
    replicated here; the fixture freezes their product, so a change to either on
    the Rust side must move the output to disagree.
    """
    ppo = 96
    freqs = log_grid(ppo=ppo)
    n = freqs.shape[0]
    fwhm_per_sigma = 2.3548200450309493  # 2*sqrt(2 ln 2)

    def variable_fraction(f):
        frac_bass, frac_mid, frac_treble = 1.0 / 48.0, 1.0 / 6.0, 1.0 / 3.0
        if f <= 100.0:
            return frac_bass
        if f <= 1000.0:
            return frac_bass + (frac_mid - frac_bass) * math.log10(f / 100.0)
        if f <= 10000.0:
            return frac_mid + (frac_treble - frac_mid) * math.log10(f / 1000.0)
        return frac_treble

    # A structured input so mid-band bins carry gradient the smoothing acts on
    # (a flat input would pass through every profile identically and pin nothing).
    rng = np.random.default_rng(50)
    mag_db = np.cumsum(rng.standard_normal(n)) * 0.25
    mag_db = np.clip(mag_db - mag_db.mean(), -15.0, 15.0)

    out = np.empty(n, dtype=np.float64)
    for i, f in enumerate(freqs):
        sigma = variable_fraction(f) / fwhm_per_sigma * ppo
        radius = math.ceil(4.0 * sigma)
        inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma)
        num = 0.0
        den = 0.0
        for m in range(-radius, radius + 1):  # same order as the Rust loop
            w = math.exp(-(m * m) * inv_two_sigma_sq)
            j = min(max(i + m, 0), n - 1)
            num += w * float(mag_db[j])
            den += w
        out[i] = num / den

    save_case("fr", "variable_smooth",
              {"fwhm_per_sigma": fwhm_per_sigma, "ppo": ppo, "truncate": 4.0},
              {"freqs": freqs, "mag_db": mag_db, "smoothed": out})


def gen_logf():
    """resample_db_to_log_grid at Prefilter::None == np.interp (Tier 2).

    np.interp is the entire contract at this setting: linear interpolation in f
    (NOT in log f) from the linear FFT axis onto the log grid, carrying
    np.interp's edge-hold outside the source range -- which a real n_fft axis
    spanning 0..sr/2 never reaches, and which this case therefore does not
    manufacture.

    The AntiComb prefilter is NOT pinned here. It is Tier 3, asserted as the
    aliasing DIFFERENCE it exists to remove, which is the only form of the claim
    that means anything.
    """
    rng = np.random.default_rng(49)
    n_fft = 16384
    freqs_linear = np.arange(n_fft // 2 + 1) * (SR / n_fft)
    mag_db = np.cumsum(rng.standard_normal(freqs_linear.shape[0])) * 0.08
    mag_db = np.clip(mag_db - mag_db.mean(), -18.0, 18.0)
    grid_freqs = log_grid()
    save_case("logf", "resample_db",
              {"f_max": 20000.0, "f_min": 20.0, "n_fft": n_fft, "ppo": 96,
               "prefilter": "none", "sample_rate": SR},
              {"freqs_linear": freqs_linear, "grid_freqs": grid_freqs, "mag_db": mag_db,
               "resampled": np.interp(grid_freqs, freqs_linear, mag_db)})


def gen_rms_average():
    """Power/RMS spatial average and sigma(f) vs numpy (Tier 2).

    The five curves are pre-aligned in the 200-2000 Hz band -- their band means
    agree to float precision -- so align_spl is a no-op on them (offsets ~1e-15
    dB) and the set reaches the type-gated estimators without moving this
    reference off 1e-12. Removing the band offsets SHAPES THE INPUT; the expected
    outputs are numpy's alone, which is what keeps the tier honest.

    align_spl itself, and the pinned -30 dB-null numbers (dB-avg = -6.0 vs
    power-avg = -0.97), are Tier 3: those assert the two estimators' divergence,
    a claim about physics rather than about numpy.
    """
    rng = np.random.default_rng(50)
    freqs = log_grid()
    n_positions = 5
    walk = np.cumsum(rng.standard_normal((n_positions, freqs.shape[0])), axis=1) * 0.15
    meas = -0.9 * np.log2(freqs / freqs[0]) + np.clip(
        walk - walk.mean(axis=1, keepdims=True), -12.0, 12.0)
    band = (freqs >= 200.0) & (freqs <= 2000.0)
    band_means = meas[:, band].mean(axis=1)
    meas = meas - (band_means - band_means.mean())[:, None]
    save_case("fr", "rms_average",
              {"axis_order": "position_bin", "band_hz": [200.0, 2000.0], "ddof": 0,
               "n_positions": n_positions, "ppo": 96},
              {"freqs": freqs, "measurements": meas,
               "rms_db": 10.0 * np.log10(np.mean(10.0 ** (meas / 10.0), axis=0)),
               "sigma_db": np.std(meas, axis=0, ddof=0)})


def gen_schroeder():
    """Schroeder backward integration vs a numpy reverse cumsum (Tier 2).

    E(t) = integral_t^inf h^2(tau) dtau, reported as 10*log10(E(t)/E(0)). E(0) is
    the total energy and therefore the maximum, h^2 being non-negative -- so
    normalizing to E[0] and to E.max() are the same operation here. Pinned as one
    array so a reimplementation cannot quietly normalize to something else.

    The IR is exponentially-decayed noise with a KNOWN T60 of 0.12 s: the envelope
    is exp(-t*ln(1000)/T60), since -60 dB is a factor of 1e-3 in amplitude.
    Recovering that T60 from a -5..-25 dB fit is Tier 3, analytic, in room.rs --
    this case pins the integration and nothing else. The curve runs 0 -> -107.6 dB
    and is finite throughout (no sample is exactly zero, so no log10(0)).
    """
    rng = np.random.default_rng(51)
    n, t60_s = 8192, 0.12
    ir = rng.standard_normal(n) * np.exp(-(np.arange(n) / SR) * np.log(1000.0) / t60_s)
    energy = np.cumsum(ir[::-1] ** 2)[::-1]
    save_case("room", "schroeder_decay", {"n": n, "sample_rate": SR, "t60_s": t60_s},
              {"decay_db": 10.0 * np.log10(energy / energy[0]), "ir": ir})


def gen_windows():
    """scipy.signal.windows at sym=True, the convention window.rs adopts (Tier 2).

    Both length parities: the sym denominator is n-1, which is where off-by-ones
    live. Tukey rides its shipping alpha=0.25, which at n=9 degenerates to a
    taper exactly one sample wide -- the endpoints alone, [0,1,1,1,1,1,1,1,0] --
    the case a half-open taper loop gets wrong.

    Compare on an ABSOLUTE tolerance. The spec's "1e-12" is not safe read as
    relative: blackmanharris's 4-term cosine sum cancels to ~6e-5 at the edges,
    so scipy's summation order and any reimplementation's diverge by up to
    5.8e-13 RELATIVE at n=4096 while sitting at 1e-16 absolute (measured, not
    assumed). Windows are bounded in [0, 1]; absolute is the meaningful measure.

    Tukey's alpha=0 -> Rect and alpha=1 -> Hann identities are NOT pinned here.
    They hold exactly in scipy (verified via np.array_equal) and the spec assigns
    them to Tier 3; with Rect and Hann pinned below, a Rust identity assertion
    implies them against scipy anyway, so a fixture would only duplicate it.
    """
    fns = {"blackmanharris": blackmanharris, "boxcar": boxcar, "hann": hann, "tukey": tukey}
    for kind, scipy_window, kwargs in (
        ("blackmanharris", "blackmanharris", {}),
        ("hann", "hann", {}),
        ("rect", "boxcar", {}),
        ("tukey", "tukey", {"alpha": 0.25}),
    ):
        for n in (8, 9, 64, 4096):
            save_case("window", f"{kind}_{n}",
                      {"kind": kind, "n": n, "scipy_window": scipy_window, "sym": True, **kwargs},
                      {"window": fns[scipy_window](n, sym=True, **kwargs)})


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


def gen_autoeq_parser():
    # DELIBERATE layout exception: parser cases are pure text with no float
    # arrays, so they do NOT use save_case()'s per-stage-dir + `.f64` convention
    # (see save_case above). Instead one top-level list-of-cases file
    # `fixtures/autoeq_parser.json` holds every case. Do not "fix" this to the
    # save_case layout — the Rust parser test (test_autoeq_parse.rs) reads this
    # flat file directly. Oracle: paraeq.correction.autoeq_db.parse_parametric_eq.
    roundtrip_bands = [
        EQBand("peaking", 105.3, 5.04, 1.201),
        EQBand("high_shelf", 9000.0, -3.5, 0.707),
    ]
    roundtrip_input = ParametricEQ(roundtrip_bands, SR).export_autoeq_format()
    # Fixed (deterministic) case order.
    inputs = [
        (
            "standard_with_preamp",
            "Preamp: -6.5 dB\n"
            "Filter 1: ON PK Fc 105 Hz Gain 5.0 dB Q 1.20\n"
            "Filter 2: ON LSC Fc 100 Hz Gain 3.0 dB Q 0.70\n"
            "Filter 3: ON HSC Fc 10000 Hz Gain -2.5 dB Q 0.71\n"
            "Filter 4: ON NO Fc 60 Hz Gain 0.0 dB Q 10.0",
        ),
        (
            "no_preamp_line_defaults_zero",
            "Filter 1: ON PK Fc 1000 Hz Gain 6.0 dB Q 1.0",
        ),
        (
            "two_preamp_lines_last_wins",
            "Preamp: -3.0 dB\n"
            "Filter 1: ON PK Fc 1000 Hz Gain 6.0 dB Q 1.0\n"
            "Preamp: -7.5 dB",
        ),
        (
            "off_filter_skipped",
            "Preamp: 0.0 dB\n"
            "Filter 1: ON PK Fc 1000 Hz Gain 6.0 dB Q 1.0\n"
            "Filter 2: OFF PK Fc 2000 Hz Gain 3.0 dB Q 2.0\n"
            "Filter 3: ON PK Fc 3000 Hz Gain -2.0 dB Q 1.5",
        ),
        (
            "unknown_code_falls_back_to_peaking",
            "Filter 1: ON XYZ Fc 500 Hz Gain 2.0 dB Q 1.0",
        ),
        (
            "legacy_ls_hs_aliases",
            "Filter 1: ON LS Fc 100 Hz Gain 4.0 dB Q 0.70\n"
            "Filter 2: ON HS Fc 8000 Hz Gain -3.0 dB Q 0.71",
        ),
        (
            "lowercase_on_pk",
            "filter 1: on pk fc 250 Hz gain -1.5 dB q 0.9",
        ),
        (
            "junk_lines_interleaved",
            "# ParametricEq generated by AutoEq\n"
            "Preamp: -1.0 dB\n"
            "\n"
            "some random prose that should be ignored\n"
            "Filter 1: ON PK Fc 440 Hz Gain 2.5 dB Q 1.0\n"
            "==== garbage ====\n"
            "Filter 2: ON PK Fc 880 Hz Gain -1.0 dB Q 2.0",
        ),
        (
            "empty_string",
            "",
        ),
        (
            "roundtrip_from_export",
            roundtrip_input,
        ),
    ]
    cases = []
    for name, text in inputs:
        parsed = parse_parametric_eq(text)
        cases.append({
            "name": name,
            "input": text,
            "expected": {
                "preamp_db": parsed.preamp_db,
                "bands": [{"filter_type": b.filter_type, "fc": b.fc,
                           "gain_db": b.gain_db, "q": b.q} for b in parsed.bands],
            },
        })
    (OUT / "autoeq_parser.json").write_text(
        json.dumps(cases, indent=1, sort_keys=True) + "\n"
    )


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
    wipe_generated_fixtures()
    gen_sweep()
    gen_deconvolution()
    gen_frequency_response()
    gen_cal()
    gen_compensation()
    gen_targets()
    gen_fir()
    gen_biquad()
    gen_peq_autofit()
    gen_autoeq_parser()
    gen_convolver()
    gen_iir()
    # Tier 2 -- scipy/numpy-direct, no paraeq import. Their Rust consumers land
    # in stages 3-4; the fixtures come first, which is the point of the tier.
    gen_gaussian_smoothing()
    gen_logf()
    gen_rms_average()
    gen_schroeder()
    gen_variable_smooth()
    gen_windows()
    manifest = {
        "dtype": "<f8",
        "numpy": np.__version__,
        "python": platform.python_version(),
        "scipy": scipy.__version__,
    }
    if drift:
        manifest["drift"] = True
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n")
    # Count only what this run produced: the PRESERVED children were already
    # there and were deliberately left untouched.
    n = sum(1 for p in OUT.rglob("*") if p.relative_to(OUT).parts[0] not in PRESERVED)
    print(f"wrote {n} files under {OUT}")


if __name__ == "__main__":
    main()
