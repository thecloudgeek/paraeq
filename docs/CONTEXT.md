# ParaEQ Project Context

This document captures the why, the goals, what's been built, and what's left. It exists so future sessions (Claude or human) can pick up the work without rebuilding context from scratch.

**Last updated:** 2026-04-27

---

## The Why

The project owner (Ronak) has Dirac Live for speaker correction on his AVR using a UMIK calibration mic — that experience is great. He also has a miniDSP EARS jig (an artificial-ear measurement device for headphones with built-in calibrated mics) and high-end headphones (Shure SE846 IEMs and B&W P9 over-ears). There is no equivalent of "Dirac for headphones" on macOS that he can easily use with his existing measurement hardware.

**The vision:** Build a Mac app that gives audiophiles Dirac-equivalent headphone correction capabilities — measurement, target curve matching, and real-time system-wide EQ — using open-source DSP. Open-source it for the r/audiophile community since this need is widely shared and currently unmet on Mac.

## The Goals

1. **Functional parity with Dirac for headphones**: measurement → target curve → correction filter → real-time playback.
2. **Lower the barrier**: support both users *with* measurement hardware (full wizard) and users *without* (manual EQ + AutoEQ preset import).
3. **r/audiophile-friendly distribution**: single installer, no Python dependency hell, BlackHole bundled.
4. **Long-term cross-platform**: Phase 1 is Python/macOS to validate the approach; Phase 2 ports DSP to Rust + Tauri for Mac/Windows/Linux.

## Phased Strategy (and Why)

We chose a hybrid approach rather than going straight to Rust:
- **Python first** because SciPy/NumPy is the lingua franca of audio DSP — fast iteration on filter design, target curves, measurement algorithms is critical to getting the science right.
- **Then port to Rust** once algorithms are proven, because real-world distribution (size, performance, cross-platform) is much better with Tauri + a Rust DSP core.
- **The Python lib stays available** as `pip install paraeq` for power users who want to script measurements.

## Current State (as of 2026-04-25)

**Phase 1 is COMPLETE and merged to main.** All 26 planned tasks done. 67 tests passing.

### What's Built

**DSP Library (`paraeq/`):**

| Module | Responsibility |
|--------|---------------|
| `measurement/sweep.py` | Logarithmic sine sweep + Farina inverse filter |
| `measurement/deconvolution.py` | Wiener spectral-division IR extraction (deviation from plan: implementer chose this over time-domain convolution for numerical stability — strictly better, mathematically equivalent) |
| `measurement/compensation.py` | Load and apply mic/jig calibration CSVs |
| `measurement/frequency_response.py` | FFT, fractional-octave smoothing, multi-measurement averaging (averaging is in dB domain, not linear — see test) |
| `correction/target_curves.py` | TargetCurve dataclass, built-in presets (Harman IE 2019, Harman OE 2018, Diffuse Field, Flat), cubic spline interp in log-frequency space |
| `correction/fir_filter.py` | FIR design via frequency sampling (linear and minimum phase). Note: minimum_phase requires squaring the magnitude beforehand (cepstral homomorphic method takes sqrt) |
| `correction/biquad.py` | Audio EQ Cookbook formulas: peaking, low_shelf, high_shelf, notch |
| `correction/parametric_eq.py` | EQBand dataclass + ParametricEQ container with composite response and AutoEQ-format export |
| `correction/auto_fit.py` | Greedy peak-picking algorithm to fit PEQ bands to a correction curve |
| `engine/convolver.py` | Overlap-add FIR convolution (block-based, per-channel) |
| `engine/iir_processor.py` | Cascaded biquad IIR with persistent per-channel state via scipy.signal.sosfilt |
| `audio/devices.py` | sounddevice wrapper, BlackHole detection by name |
| `audio/stream.py` | play / record / AudioPassThrough with pre/post callbacks for spectrum analyzer |
| `profiles/profile.py` | Profile dataclass + ProfileManager (JSON for metadata, WAV for IRs, .npy for FIR coefficients) |

**GUI App (`app/`):**

| Module | Responsibility |
|--------|---------------|
| `main.py` | QApplication entry, first-launch setup wizard check, tray icon, audio engine startup |
| `main_window.py` | Tabbed window (Measure / Target / EQ / Analyzer / Profiles), signal wiring, audio engine lifecycle |
| `wizard/measurement_wizard.py` | Device pickers, compensation loader, MeasurementWorker QThread (sd.playrec → deconvolve → FR plot), multi-measurement averaging |
| `editor/target_curve_editor.py` | Preset dropdown, CSV import/export, "Generate FIR" / "Generate PEQ" buttons, 3-trace plot (measured/target/correction-delta) |
| `eq/manual_eq_editor.py` | QTableWidget for bands, drag-on-plot editing, AutoEQ format import/export |
| `analyzer/spectrum_analyzer.py` | Live FFT, pre/post correction overlay, octave smoothing dropdown, dB range slider |
| `profiles/profile_manager.py` | List view + Activate/Duplicate/Rename/Delete/Import/Export |
| `menu_bar/tray.py` | System tray with profile switching, bypass toggle |
| `setup/setup_wizard.py` | First-launch QWizard: BlackHole check → output device pick → done |

**Built-in Target Curves (`targets/`):** flat.csv, harman_ie_2019.csv, harman_ie_2019_without_bass.csv, harman_oe_2018.csv, harman_oe_2018_without_bass.csv, diffuse_field.csv. High-resolution data sourced from AutoEQ (MIT licensed) at commit `7ae0f56` (full SHA `7ae0f56d53074872b028649617a22bbb4232feb7`). Each CSV carries `# name:`, `# category:`, `# description:`, `# source:` metadata headers; the loader (`paraeq/correction/target_curves.py`) populates these on the `TargetCurve` dataclass.

**Testing:** 75 tests, all passing, ~1.5s runtime. Includes 3 end-to-end integration tests covering measurement → FIR pipeline, measurement → PEQ pipeline, and profile save/load round-trip; plus 8 tests covering the metadata-headered target curve loader (added in cycle 1, including a contract-honoring test that `load_target_csv` raises `ValueError` on malformed data lines).

### Notable Implementation Decisions

- **Deconvolution uses Wiener spectral division** (`conj(S) / (|S|² + ε)`), not time-domain convolution with the inverse sweep. Mathematically equivalent to Farina, but numerically far more stable. This means `generate_inverse_sweep` from sweep.py is currently unused by the production deconvolution path — kept for now in case scripts want it.
- **Minimum-phase FIR design**: `scipy.signal.minimum_phase(method="homomorphic")` takes the square root of the magnitude in cepstral domain. To produce a filter with desired magnitude M, you must build the linear-phase prototype with magnitude M² so that sqrt(M²) = M after conversion.
- **Averaging in dB domain**, not linear, per test expectation: `(0 dB + 6 dB) / 2 = 3 dB`.
- **BlackHole is detected by name match** (`"blackhole" in device.name.lower()`). Bundling/install of the BlackHole HAL plugin into the macOS installer pkg is **not yet implemented** — currently relies on user having BlackHole pre-installed.

### Recently Completed Cycles

- **2026-04-27 — Cycle 1: Refined target curve data.** Replaced the four hand-typed approximate target CSVs with high-resolution data sourced from AutoEQ at commit `7ae0f56`. Expanded built-in set to six curves (added `harman_ie_2019_without_bass`, `harman_oe_2018_without_bass`). Extended `TargetCurve` and the CSV loader with optional metadata fields (`category`, `description`, `source`); old-style CSVs without metadata still load. See `docs/specs/2026-04-26-refined-target-curves-design.md` and `docs/plans/2026-04-26-refined-target-curves.md`.

## What's Left

### Near-term (still Phase 1 polish)

These items were specified in the design but not in the executed implementation plan:

- **Installer pipeline**: DMG + pkg installer that bundles the BlackHole HAL plugin and installs ParaEQ.app to /Applications/. Currently the app only runs from source via `python -m app.main`.
- **PyInstaller bundling**: `ParaEQ.app` build for distribution.
- **Homebrew cask**: `brew install --cask paraeq`.
- **Aggregate device creation**: programmatically create a macOS Aggregate Device combining BlackHole + headphone jack via Core Audio API. Currently the user has to set this up manually.
- **Launch-at-login**: launchd plist for auto-start.
- **Interactive draggable control points** on the target curve editor (currently you can pick a preset and import/export CSV, but in-plot editing of control points is not implemented).
- **AutoEQ database integration**: searchable headphone model picker that loads presets from the AutoEQ project. Currently you can import a preset file, but there's no built-in browser.

### Phase 2 (future, designed but not built)

Port the DSP core to Rust and wrap in Tauri + React for cross-platform single-binary distribution:

- Rust DSP library using `rustfft`, custom biquad filters
- Tauri app with React frontend
- PyO3 bindings so the Python `paraeq` package keeps working as a wrapper around the Rust core
- Cross-platform builds for macOS, Windows, Linux

## How to Pick Up the Work

1. Read this document, `CLAUDE.md`, and the design spec at `docs/specs/2026-04-22-paraeq-design.md`.
2. Decide which gap above to tackle. The most user-impactful next step is the **installer pipeline** — without it, only developers can use the app.
3. Use the brainstorming → writing-plans → subagent-driven-development workflow for substantial new work. Smaller fixes can be done directly.
4. Each major change should follow TDD where possible (DSP changes definitely; GUI changes by manual smoke test since no display in CI).

## Hardware Reference

The project owner uses for testing:
- **miniDSP EARS jig** — USB headphone measurement coupler with calibrated L/R mics. Comes with a per-unit compensation CSV.
- **miniDSP UMIK-1** — calibrated USB measurement microphone (used for room/speaker measurement, not directly used by ParaEQ but supported via compensation files).
- **Shure SE846** — reference IEM (high isolation, BA drivers, 4-way crossover).
- **Bowers & Wilkins P9** — reference closed-back over-ear.

## Project Owner Profile

- Comfortable in software engineering and audio
- Wants Dirac-quality results, not a toy
- Is the primary first user, then plans to release to r/audiophile
- Prefers asking questions over assumptions
- Prefers context7 verification over guessing on library APIs
- Prefers TDD
- Prefers structured logging
