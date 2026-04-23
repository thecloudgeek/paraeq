# ParaEQ Design Spec

**Date:** 2026-04-22
**Status:** Draft
**Author:** Ronak Patel

## Overview

ParaEQ is an open-source headphone measurement, correction, and equalization app for macOS. It provides Dirac Live-equivalent capabilities — frequency response measurement, target curve matching, and real-time system-wide EQ — using open-source DSP tools.

The project ships as two artifacts:
1. **`paraeq` Python library** — pip-installable DSP engine for measurement, filter generation, and real-time convolution. Usable standalone via scripts.
2. **ParaEQ GUI** — PyQt6 desktop app wrapping the library with a full measurement wizard, target curve editor, real-time spectrum analyzer, and profile manager.

### Phased Delivery

- **Phase 1 (this spec):** Python + PyQt6 macOS app. Prove out all DSP algorithms, ship a usable product.
- **Phase 2 (future):** Port DSP core to Rust, wrap in Tauri + React for cross-platform single-binary distribution. Publish the Python library separately for power users.

### Target Users

- Audiophiles with calibration hardware (miniDSP EARS, UMIK)
- Audiophiles without measurement gear who want manual EQ / AutoEQ preset import
- r/audiophile community (open-source, MIT licensed)

### Reference Hardware

- miniDSP EARS headphone measurement jig (USB audio input with calibrated mics)
- miniDSP UMIK-1 calibration microphone
- Shure SE846 (IEM)
- Bowers & Wilkins P9 (over-ear)

---

## Project Structure

```
paraeq/
├── paraeq/                  # Python package (pip-installable DSP library)
│   ├── measurement/         # Sweep generation, IR capture, FR computation
│   ├── correction/          # Filter design (FIR/IIR), target curves
│   ├── engine/              # Real-time convolution engine (overlap-add)
│   ├── audio/               # Audio I/O (sounddevice), device management
│   └── profiles/            # Profile save/load (JSON + filter coefficients)
├── app/                     # PyQt6 GUI application
│   ├── wizard/              # Measurement wizard screens
│   ├── editor/              # Target curve editor widget
│   ├── analyzer/            # Real-time spectrum analyzer widget
│   ├── profiles/            # Profile manager UI
│   └── main_window.py
├── calibration/             # EARS jig + UMIK compensation files
├── targets/                 # Built-in target curves (Harman IE/OE, diffuse field, flat)
├── tests/
├── docs/
└── installer/               # DMG/pkg packaging, bundled BlackHole HAL plugin
```

---

## Measurement Pipeline

### Signal Chain

```
Mac audio output → Headphones (on EARS jig) → EARS mics → USB audio input → ParaEQ
```

### Steps

1. **Sweep generation** — Logarithmic sine sweep, 20Hz–20kHz, ~3 seconds duration. Log sweep chosen because Farina deconvolution cleanly separates the impulse response from harmonic distortion products in the time domain.

2. **Capture** — Record the sweep through the EARS jig's left and right microphone channels simultaneously. Both channels are captured independently since headphone drivers are never perfectly matched.

3. **Deconvolution** — Convolve the recorded signal with the time-reversed inverse sweep to extract the impulse response (IR). Standard Farina method via FFT-based convolution in SciPy.

4. **Compensation** — Apply the EARS jig's calibration file to remove the jig's own frequency coloration from the measurement. miniDSP provides compensation files as CSV frequency/dB pairs. These are interpolated to match the FFT resolution and subtracted in the frequency domain.

5. **FR computation** — FFT the compensated impulse response. Apply fractional-octave smoothing (1/6 octave default, user-adjustable from 1/24 to 1/1). Display as magnitude (dB SPL) vs. frequency (Hz) on a logarithmic frequency axis.

6. **Averaging** — Support multiple measurements with headphone repositioning between each. Average in the frequency domain (magnitude). This reduces placement variation, which is especially important for IEMs like the SE846 where insertion depth affects the response.

### Data Storage

Each measurement is stored as raw data:
- Impulse response (WAV)
- Sample rate
- Compensation file used
- Timestamp and metadata (headphone model, notes)

Raw IR storage allows reprocessing later with different smoothing, windowing, or compensation files without re-measuring.

---

## Correction Engine

### Target Curves

Built-in presets:
- Harman In-Ear Target (2019)
- Harman Over-Ear Target (2018)
- Diffuse Field
- Flat (0dB reference)

Custom targets:
- Draw control points on an interactive curve in the editor
- Import/export as CSV (frequency, dB pairs)
- Interpolation between control points using cubic spline

Target curves are stored as frequency/dB point arrays and interpolated to full FFT resolution at correction time.

### Filter Generation

The correction curve is computed as: `correction = target - measured` (in dB).

Two filter modes:

**FIR Filter (default):**
- Frequency-domain design via frequency sampling
- Filter length: 4096–8192 taps at 48kHz (gives 1Hz resolution at low frequencies)
- Two phase options:
  - **Linear phase** — symmetric FIR, no phase distortion, higher latency (~42-85ms at 4096-8192 taps)
  - **Minimum phase** — lower latency (~5ms), slight phase shift (inaudible for EQ corrections)
- Minimum phase is the default — latency matters more than theoretical phase purity for real-time headphone listening

**Parametric EQ:**
- Auto-fit algorithm: iteratively place peaking and shelf filters to approximate the correction curve
- Configurable max number of bands (default: 10)
- Each band: center frequency, gain (dB), Q factor
- Exportable formats: AutoEQ-compatible text, EqualizerAPO config, AU parameter list
- Less precise than FIR but universally portable to other EQ software

### Per-Channel Correction

Left and right channels get independent correction filters derived from their respective measurements. A single "apply" generates two filters.

---

## Manual EQ Mode

For users without measurement hardware — a standalone parametric EQ interface.

### Features

- Add/remove parametric EQ bands (peaking, low shelf, high shelf, notch)
- Adjust frequency, gain, and Q per band by dragging on the frequency response plot or entering values directly
- Import presets from:
  - AutoEQ database (CSV/text format)
  - oratory1990 EQ presets
  - Any CSV with frequency/gain/Q columns
- No measurement required — select headphone model from a searchable list or dial in EQ by ear
- Same real-time engine applies the filters (BlackHole → ParaEQ → headphones)
- Can be layered on top of a measurement-based FIR correction as fine-tuning

### Entry Points

The app has two primary workflows:
1. **Measure & correct** — full measurement wizard → auto-generated correction filter
2. **Manual EQ** — jump straight to the parametric EQ editor, load a preset or tweak by hand

---

## Real-Time Convolution Engine

### Architecture

- Overlap-add FFT convolution using NumPy
- Block size: 512 samples at 48kHz (~10.7ms per block)
- Double-buffered: process block N+1 while block N plays out
- FIR convolution: zero-pad filter and audio block to next power of 2, multiply FFTs, IFFT, overlap-add
- Parametric EQ: cascaded biquad IIR filters (direct form II transposed) via SciPy `sosfilt`

### Audio Routing

- BlackHole 2ch virtual audio device captures all system audio
- ParaEQ reads from BlackHole input, applies correction filter, writes to headphone jack output
- Sample rate: 48kHz (matches most macOS audio)
- Bit depth: 32-bit float internal processing

### Performance

- Latency budget:
  - BlackHole: ~1ms
  - FFT convolution (512 samples at 48kHz): ~10ms
  - Audio output buffer: ~5ms
  - **Total: ~16ms** — imperceptible for music, within lip-sync tolerance (~45ms) for video
- CPU: FFT convolution of a 4096-tap FIR on a single core is negligible on Apple Silicon

---

## System Audio Routing

### BlackHole Integration

BlackHole is an open-source (MIT licensed) Core Audio HAL virtual audio driver. It creates a virtual audio device that allows routing audio between applications.

**Bundled with installer:**
- BlackHole's HAL plugin (`.driver` bundle) is packaged inside the ParaEQ installer
- Installed to `/Library/Audio/Plug-Ins/HAL/` (requires admin password during install)
- On first launch, ParaEQ checks if BlackHole is already present and skips installation if so (respects existing user installs)
- BlackHole's MIT license included in attribution/notices

**Aggregate device (optional):**
- ParaEQ can create a macOS Aggregate Device (BlackHole + headphone jack) via Core Audio API
- Presents as a single audio device in System Settings for simpler configuration
- Created/removed programmatically by the app

### Setup Wizard

First-launch wizard walks the user through:
1. Confirming BlackHole is installed (or triggering install)
2. Setting macOS system output to BlackHole
3. Selecting the headphone output device
4. Verifying audio passes through (test tone)

---

## GUI Components

Built with PyQt6. All real-time plots use pyqtgraph (matplotlib is too slow for live audio visualization).

### 1. Measurement Wizard

Step-by-step flow:
1. Select audio output device (headphone jack)
2. Select audio input device (EARS jig USB)
3. Load EARS compensation file
4. Label the headphone (model name, e.g., "SE846" or "P9")
5. Play sweep — visual progress bar during capture
6. Display captured frequency response immediately
7. Option: "Measure again" for averaging — overlay all measurements with running average highlighted
8. Finish: save measurement to profile

### 2. Target Curve Editor

- Interactive plot canvas with three layers: measured FR (blue), target curve (orange), correction delta (green)
- Drag control points on the target curve to customize
- Preset selector dropdown (Harman IE 2019, Harman OE 2018, Diffuse Field, Flat)
- Import/export target curves as CSV
- "Auto-match" button that evaluates which built-in preset is closest to the current measurement and applies it
- Generate correction filter button (FIR or Parametric, user selects)

### 3. Real-Time Spectrum Analyzer

- Live FFT display of audio passing through the engine
- Two overlaid traces: pre-correction (dimmed) and post-correction (bright)
- Adjustable parameters: smoothing (1/24 to 1/1 octave), dB range, FFT size
- L/R channel toggle or stereo overlay
- VU meters for left and right channels

### 4. Profile Manager

- List view of saved profiles (e.g., "SE846 — Harman IE", "P9 — Custom warm")
- Each profile stores: measurement data, target curve, generated filter coefficients, metadata
- Quick-switch between profiles
- Duplicate, rename, delete profiles
- Export profile as JSON (shareable with other ParaEQ users)
- Import profile from JSON

### 5. Manual EQ Editor

- Parametric EQ band list with frequency, gain, Q, filter type per band
- Interactive frequency response plot — drag bands directly on the curve
- Import from AutoEQ / oratory1990 presets
- Searchable headphone model list for quick preset lookup
- Can be used standalone or layered on top of measurement-based correction

### 6. Menu Bar / System Tray

- Persistent menu bar icon (always running when active)
- Shows: current profile name, bypass toggle, volume control
- Profile quick-switch submenu
- Open main window
- Quit

### Startup Behavior

- Option to launch at login via launchd plist
- Remembers last active profile and resumes on launch
- Minimizes to menu bar when main window is closed

---

## Technology Stack (Phase 1)

| Component | Technology |
|-----------|------------|
| DSP / signal processing | NumPy, SciPy |
| Audio I/O | sounddevice (PortAudio wrapper) |
| Real-time plots | pyqtgraph |
| GUI framework | PyQt6 |
| Virtual audio routing | BlackHole (bundled, MIT) |
| Profile storage | JSON + WAV (impulse responses) |
| Target curves | CSV (frequency/dB pairs) |
| Packaging | PyInstaller → DMG with pkg installer |
| Distribution | GitHub releases, Homebrew cask |
| License | MIT |

### Python Dependencies

- `numpy` — array math, FFT
- `scipy` — signal processing (filter design, convolution, interpolation)
- `sounddevice` — cross-platform audio I/O via PortAudio
- `PyQt6` — GUI framework
- `pyqtgraph` — real-time plotting
- `platformdirs` — OS-appropriate config/data directories

---

## Distribution

### Installer

- DMG containing a `.pkg` installer
- The pkg installs:
  - ParaEQ.app (PyInstaller-bundled Python app) to `/Applications/`
  - BlackHole HAL plugin to `/Library/Audio/Plug-Ins/HAL/` (if not already present)
- Admin password required once at install time
- Uninstaller script included that removes both ParaEQ and the bundled HAL plugin

### Homebrew

```
brew install --cask paraeq
```

### Python Library (separate)

```
pip install paraeq
```

Installs the DSP library only (no GUI) for scripting and automation.

---

## Phase 2: Tauri + Rust (Future)

Not in scope for this spec, but the planned migration path:

- Port `paraeq` DSP library to Rust (`rustfft`, custom biquad filters)
- Tauri desktop app with React frontend
- Single binary distribution for macOS, Windows, Linux
- Python library remains available separately via PyO3 bindings to the Rust core
- Community contribution paths: Python lib for DSP experimentation, React/Rust app for UI/systems work
