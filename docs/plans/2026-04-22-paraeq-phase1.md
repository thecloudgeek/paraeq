# ParaEQ Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a headphone measurement, correction, and EQ app for macOS with a full PyQt6 GUI, system-wide real-time audio processing, and profile management.

**Architecture:** Python DSP library (`paraeq/`) with a separate PyQt6 GUI (`app/`). Audio routed through BlackHole virtual device → Python overlap-add convolution engine → headphone output. Library is pip-installable for scripting; GUI wraps it with measurement wizard, target curve editor, spectrum analyzer, manual EQ, and profile management.

**Tech Stack:** Python 3.11+, NumPy, SciPy, sounddevice, PyQt6, pyqtgraph, BlackHole (bundled), platformdirs

---

## File Map

```
paraeq/
├── pyproject.toml
├── README.md
├── LICENSE
├── paraeq/
│   ├── __init__.py
│   ├── measurement/
│   │   ├── __init__.py
│   │   ├── sweep.py              # Log sine sweep generation & inverse filter
│   │   ├── deconvolution.py      # Farina method IR extraction
│   │   ├── compensation.py       # Load & apply calibration files
│   │   └── frequency_response.py # FFT, smoothing, averaging
│   ├── correction/
│   │   ├── __init__.py
│   │   ├── target_curves.py      # Load/interpolate/manage target curves
│   │   ├── fir_filter.py         # FIR correction filter design
│   │   ├── biquad.py             # Biquad filter coefficient math
│   │   ├── parametric_eq.py      # Parametric EQ bands & composite response
│   │   └── auto_fit.py           # Auto-fit PEQ bands to correction curve
│   ├── engine/
│   │   ├── __init__.py
│   │   ├── convolver.py          # Overlap-add FIR convolution
│   │   └── iir_processor.py      # Cascaded biquad IIR processing
│   ├── audio/
│   │   ├── __init__.py
│   │   ├── devices.py            # Audio device discovery & selection
│   │   └── stream.py             # Playback, recording, pass-through streams
│   └── profiles/
│       ├── __init__.py
│       └── profile.py            # Profile save/load/export/import
├── app/
│   ├── __init__.py
│   ├── main.py                   # Entry point, QApplication setup
│   ├── main_window.py            # Main window with tab navigation
│   ├── wizard/
│   │   ├── __init__.py
│   │   └── measurement_wizard.py # Step-by-step measurement flow
│   ├── editor/
│   │   ├── __init__.py
│   │   └── target_curve_editor.py # Interactive target curve editing
│   ├── analyzer/
│   │   ├── __init__.py
│   │   └── spectrum_analyzer.py  # Real-time FFT display
│   ├── eq/
│   │   ├── __init__.py
│   │   └── manual_eq_editor.py   # Manual parametric EQ interface
│   ├── profiles/
│   │   ├── __init__.py
│   │   └── profile_manager.py    # Profile list, import/export UI
│   ├── menu_bar/
│   │   ├── __init__.py
│   │   └── tray.py               # System tray / menu bar icon
│   └── setup/
│       ├── __init__.py
│       └── setup_wizard.py       # First-launch BlackHole setup
├── targets/
│   ├── harman_ie_2019.csv
│   ├── harman_oe_2018.csv
│   ├── diffuse_field.csv
│   └── flat.csv
├── tests/
│   ├── __init__.py
│   ├── test_sweep.py
│   ├── test_deconvolution.py
│   ├── test_compensation.py
│   ├── test_frequency_response.py
│   ├── test_target_curves.py
│   ├── test_fir_filter.py
│   ├── test_biquad.py
│   ├── test_parametric_eq.py
│   ├── test_auto_fit.py
│   ├── test_convolver.py
│   ├── test_iir_processor.py
│   ├── test_devices.py
│   ├── test_stream.py
│   └── test_profile.py
└── installer/
    └── README.md
```

---

### Task 1: Project Scaffolding

**Files:**
- Create: `pyproject.toml`
- Create: `paraeq/__init__.py`
- Create: `LICENSE`
- Create: `.gitignore`

- [ ] **Step 1: Create pyproject.toml**

```toml
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[project]
name = "paraeq"
version = "0.1.0"
description = "Open-source headphone measurement and correction EQ"
readme = "README.md"
license = "MIT"
requires-python = ">=3.11"
dependencies = [
    "numpy>=1.24",
    "scipy>=1.10",
    "sounddevice>=0.4",
    "platformdirs>=3.0",
]

[project.optional-dependencies]
gui = [
    "PyQt6>=6.5",
    "pyqtgraph>=0.13",
]
dev = [
    "pytest>=7.0",
    "pytest-cov>=4.0",
]

[tool.pytest.ini_options]
testpaths = ["tests"]
```

- [ ] **Step 2: Create package init**

```python
# paraeq/__init__.py
"""ParaEQ - Open-source headphone measurement and correction EQ."""

__version__ = "0.1.0"
```

- [ ] **Step 3: Create LICENSE (MIT)**

```
MIT License

Copyright (c) 2026 Ronak Patel

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

- [ ] **Step 4: Create .gitignore**

```
__pycache__/
*.pyc
*.egg-info/
dist/
build/
.venv/
*.wav
!targets/*.csv
.DS_Store
```

- [ ] **Step 5: Create directory structure**

```bash
mkdir -p paraeq/measurement paraeq/correction paraeq/engine paraeq/audio paraeq/profiles
mkdir -p app/wizard app/editor app/analyzer app/eq app/profiles app/menu_bar app/setup
mkdir -p targets tests installer
touch paraeq/measurement/__init__.py paraeq/correction/__init__.py paraeq/engine/__init__.py
touch paraeq/audio/__init__.py paraeq/profiles/__init__.py
touch app/__init__.py app/wizard/__init__.py app/editor/__init__.py app/analyzer/__init__.py
touch app/eq/__init__.py app/profiles/__init__.py app/menu_bar/__init__.py app/setup/__init__.py
touch tests/__init__.py
```

- [ ] **Step 6: Install in dev mode and verify**

```bash
python -m venv .venv
source .venv/bin/activate
pip install -e ".[dev,gui]"
python -c "import paraeq; print(paraeq.__version__)"
```

Expected: `0.1.0`

- [ ] **Step 7: Commit**

```bash
git add pyproject.toml LICENSE .gitignore paraeq/ app/ targets/ tests/ installer/ docs/
git commit -m "feat: initial project scaffolding with package structure"
```

---

### Task 2: Log Sine Sweep Generation

**Files:**
- Create: `paraeq/measurement/sweep.py`
- Create: `tests/test_sweep.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_sweep.py
import numpy as np
from paraeq.measurement.sweep import generate_sweep, generate_inverse_sweep


def test_generate_sweep_length():
    """Sweep should have exactly duration * sample_rate samples."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    assert sweep.shape == (144000,)
    assert sweep.dtype == np.float64


def test_generate_sweep_amplitude_bounded():
    """Sweep amplitude should stay within [-1, 1]."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    assert np.max(np.abs(sweep)) <= 1.0


def test_generate_sweep_starts_low_frequency():
    """First few cycles should be near f_start (20 Hz)."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    # Check the first 0.1s — dominant frequency should be near 20 Hz
    chunk = sweep[:4800]
    fft_mag = np.abs(np.fft.rfft(chunk))
    freqs = np.fft.rfftfreq(len(chunk), 1.0 / 48000)
    peak_freq = freqs[np.argmax(fft_mag[1:]) + 1]  # skip DC
    assert 10.0 < peak_freq < 50.0


def test_generate_inverse_sweep_length():
    """Inverse sweep should match the original sweep length."""
    sweep = generate_sweep(duration=3.0, sample_rate=48000, f_start=20.0, f_end=20000.0)
    inverse = generate_inverse_sweep(sweep, sample_rate=48000, f_start=20.0, f_end=20000.0)
    assert inverse.shape == sweep.shape


def test_sweep_inverse_deconvolution_peak():
    """Convolving sweep with its inverse should produce a sharp impulse."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr, f_start=20.0, f_end=20000.0)
    inverse = generate_inverse_sweep(sweep, sample_rate=sr, f_start=20.0, f_end=20000.0)
    # FFT-based convolution
    n = len(sweep) + len(inverse) - 1
    n_fft = int(2 ** np.ceil(np.log2(n)))
    result = np.fft.irfft(np.fft.rfft(sweep, n_fft) * np.fft.rfft(inverse, n_fft), n_fft)
    # Normalize
    result = result / np.max(np.abs(result))
    peak_idx = np.argmax(np.abs(result))
    # Peak should be much larger than the rest
    non_peak = np.delete(np.abs(result), range(max(0, peak_idx - 10), peak_idx + 10))
    assert np.max(np.abs(result)) > 10 * np.mean(non_peak)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_sweep.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'paraeq.measurement.sweep'`

- [ ] **Step 3: Implement sweep generation**

```python
# paraeq/measurement/sweep.py
"""Logarithmic sine sweep generation and inverse filter for Farina method."""

import numpy as np


def generate_sweep(
    duration: float,
    sample_rate: int,
    f_start: float = 20.0,
    f_end: float = 20000.0,
) -> np.ndarray:
    """Generate a logarithmic sine sweep.

    Args:
        duration: Sweep length in seconds.
        sample_rate: Sample rate in Hz.
        f_start: Start frequency in Hz.
        f_end: End frequency in Hz.

    Returns:
        1D float64 array of sweep samples, amplitude in [-1, 1].
    """
    n_samples = int(duration * sample_rate)
    t = np.arange(n_samples) / sample_rate
    # Logarithmic instantaneous frequency: f(t) = f_start * (f_end/f_start)^(t/T)
    # Phase integral: phi(t) = 2*pi * f_start * T / ln(R) * (R^(t/T) - 1)
    # where R = f_end / f_start, T = duration
    rate = f_end / f_start
    phase = (
        2.0 * np.pi * f_start * duration / np.log(rate) * (rate ** (t / duration) - 1.0)
    )
    sweep = np.sin(phase)
    return sweep


def generate_inverse_sweep(
    sweep: np.ndarray,
    sample_rate: int,
    f_start: float = 20.0,
    f_end: float = 20000.0,
) -> np.ndarray:
    """Generate the inverse filter for a log sweep (Farina method).

    The inverse is the time-reversed sweep with an amplitude envelope that
    decays at 6 dB/octave (compensating for the log sweep's pink spectrum).

    Args:
        sweep: The original log sweep array.
        sample_rate: Sample rate in Hz.
        f_start: Start frequency of the sweep in Hz.
        f_end: End frequency of the sweep in Hz.

    Returns:
        1D float64 array — the inverse sweep filter.
    """
    n_samples = len(sweep)
    duration = n_samples / sample_rate
    rate = f_end / f_start
    # Time-reverse the sweep
    inverse = sweep[::-1].copy()
    # Apply amplitude envelope: decay proportional to instantaneous frequency
    # At time t in the original sweep, freq = f_start * R^(t/T)
    # In reversed array, index i corresponds to original time (N-1-i)/sr
    t_original = np.arange(n_samples)[::-1] / sample_rate
    envelope = rate ** (-t_original / duration)
    inverse *= envelope
    # Normalize so convolution produces unit impulse
    inverse /= np.sum(sweep * sweep[::-1] * envelope) / n_samples
    return inverse
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_sweep.py -v`
Expected: All 5 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/measurement/sweep.py tests/test_sweep.py
git commit -m "feat: log sine sweep generation with Farina inverse filter"
```

---

### Task 3: Deconvolution (Impulse Response Extraction)

**Files:**
- Create: `paraeq/measurement/deconvolution.py`
- Create: `tests/test_deconvolution.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_deconvolution.py
import numpy as np
from paraeq.measurement.sweep import generate_sweep, generate_inverse_sweep
from paraeq.measurement.deconvolution import deconvolve


def test_deconvolve_identity():
    """Deconvolving an unmodified sweep should yield a near-perfect impulse."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    # Simulate "recording" = sweep itself (perfect system, no coloration)
    ir = deconvolve(recorded=sweep, sweep=sweep, sample_rate=sr)
    # IR should have a clear peak
    peak_idx = np.argmax(np.abs(ir))
    peak_val = np.abs(ir[peak_idx])
    # Energy outside peak region should be much smaller
    mask = np.ones(len(ir), dtype=bool)
    mask[max(0, peak_idx - 20) : peak_idx + 20] = False
    noise_rms = np.sqrt(np.mean(ir[mask] ** 2))
    assert peak_val / noise_rms > 50  # >34 dB SNR


def test_deconvolve_delayed_system():
    """A delayed recording should produce a delayed impulse response."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    delay_samples = 480  # 10ms delay
    recorded = np.zeros(len(sweep) + delay_samples)
    recorded[delay_samples:delay_samples + len(sweep)] = sweep
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    peak_idx = np.argmax(np.abs(ir))
    # Peak should be near the delay point (within a few samples tolerance)
    assert abs(peak_idx - delay_samples) < 10


def test_deconvolve_stereo():
    """Deconvolution should handle stereo (2D) recordings."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    # Left channel: slight delay, Right channel: no delay
    left = np.zeros(len(sweep) + 100)
    left[100:100 + len(sweep)] = sweep
    right = np.zeros(len(sweep) + 100)
    right[:len(sweep)] = sweep * 0.8
    recorded = np.column_stack([left, right])
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    assert ir.ndim == 2
    assert ir.shape[1] == 2
    # Left peak should be later than right peak
    left_peak = np.argmax(np.abs(ir[:, 0]))
    right_peak = np.argmax(np.abs(ir[:, 1]))
    assert left_peak > right_peak
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_deconvolution.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement deconvolution**

```python
# paraeq/measurement/deconvolution.py
"""Impulse response extraction via Farina deconvolution."""

import numpy as np
from paraeq.measurement.sweep import generate_inverse_sweep


def deconvolve(
    recorded: np.ndarray,
    sweep: np.ndarray,
    sample_rate: int,
    f_start: float = 20.0,
    f_end: float = 20000.0,
) -> np.ndarray:
    """Extract the impulse response from a recorded sweep measurement.

    Uses FFT-based convolution of the recording with the inverse sweep filter
    (Farina method).

    Args:
        recorded: Recorded signal. 1D (mono) or 2D (samples x channels).
        sweep: The original log sweep that was played.
        sample_rate: Sample rate in Hz.
        f_start: Start frequency of the sweep.
        f_end: End frequency of the sweep.

    Returns:
        Impulse response array. Same channel count as recorded.
    """
    inverse = generate_inverse_sweep(sweep, sample_rate, f_start, f_end)
    stereo = recorded.ndim == 2
    if stereo:
        n_channels = recorded.shape[1]
    else:
        n_channels = 1
        recorded = recorded[:, np.newaxis]

    n_conv = len(recorded) + len(inverse) - 1
    n_fft = int(2 ** np.ceil(np.log2(n_conv)))
    inverse_fft = np.fft.rfft(inverse, n_fft)

    ir_channels = []
    for ch in range(n_channels):
        rec_fft = np.fft.rfft(recorded[:, ch], n_fft)
        ir_raw = np.fft.irfft(rec_fft * inverse_fft, n_fft)
        ir_channels.append(ir_raw[:n_conv])

    ir = np.column_stack(ir_channels)
    if not stereo:
        ir = ir[:, 0]
    return ir
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_deconvolution.py -v`
Expected: All 3 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/measurement/deconvolution.py tests/test_deconvolution.py
git commit -m "feat: Farina deconvolution for impulse response extraction"
```

---

### Task 4: Compensation File Loading

**Files:**
- Create: `paraeq/measurement/compensation.py`
- Create: `tests/test_compensation.py`
- Create: `tests/fixtures/test_compensation.csv`

- [ ] **Step 1: Create test fixture**

```csv
20,0.5
50,0.3
100,0.0
1000,-0.2
5000,-0.5
10000,-1.0
20000,-2.0
```

Save to `tests/fixtures/test_compensation.csv`.

- [ ] **Step 2: Write the failing tests**

```python
# tests/test_compensation.py
import numpy as np
from pathlib import Path
from paraeq.measurement.compensation import load_compensation, apply_compensation

FIXTURE_DIR = Path(__file__).parent / "fixtures"


def test_load_compensation_csv():
    """Load a compensation CSV and return frequency/dB arrays."""
    freqs, gains_db = load_compensation(FIXTURE_DIR / "test_compensation.csv")
    assert len(freqs) == 7
    assert len(gains_db) == 7
    assert freqs[0] == 20.0
    assert gains_db[0] == 0.5
    assert freqs[-1] == 20000.0
    assert gains_db[-1] == -2.0


def test_apply_compensation_subtracts():
    """Compensation should subtract the calibration curve from the FR magnitude."""
    n_fft = 4096
    sample_rate = 48000
    freqs_fft = np.fft.rfftfreq(n_fft, 1.0 / sample_rate)
    # Flat 0 dB input
    magnitude_db = np.zeros_like(freqs_fft)
    comp_freqs = np.array([20.0, 1000.0, 20000.0])
    comp_gains = np.array([1.0, 0.0, -1.0])
    corrected = apply_compensation(magnitude_db, freqs_fft, comp_freqs, comp_gains)
    # At 1000 Hz, compensation is 0 dB, so corrected should be ~0
    idx_1k = np.argmin(np.abs(freqs_fft - 1000.0))
    assert abs(corrected[idx_1k]) < 0.01
    # At 20 Hz, compensation is +1 dB, so corrected should be ~-1 dB
    idx_20 = np.argmin(np.abs(freqs_fft - 20.0))
    assert abs(corrected[idx_20] - (-1.0)) < 0.1


def test_apply_compensation_preserves_shape():
    """Output shape should match input shape."""
    n = 2049
    magnitude_db = np.zeros(n)
    freqs_fft = np.linspace(0, 24000, n)
    comp_freqs = np.array([20.0, 20000.0])
    comp_gains = np.array([0.0, 0.0])
    result = apply_compensation(magnitude_db, freqs_fft, comp_freqs, comp_gains)
    assert result.shape == (n,)
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `pytest tests/test_compensation.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 4: Implement compensation**

```python
# paraeq/measurement/compensation.py
"""Load and apply microphone/jig calibration compensation files."""

import csv
from pathlib import Path

import numpy as np
from scipy.interpolate import interp1d


def load_compensation(filepath: Path) -> tuple[np.ndarray, np.ndarray]:
    """Load a calibration compensation file (CSV: frequency,gain_db).

    Args:
        filepath: Path to the CSV file.

    Returns:
        Tuple of (frequencies_hz, gains_db) as float64 arrays.
    """
    freqs = []
    gains = []
    with open(filepath, "r") as f:
        reader = csv.reader(f)
        for row in reader:
            if not row or row[0].startswith("#"):
                continue
            freqs.append(float(row[0].strip()))
            gains.append(float(row[1].strip()))
    return np.array(freqs, dtype=np.float64), np.array(gains, dtype=np.float64)


def apply_compensation(
    magnitude_db: np.ndarray,
    freqs_fft: np.ndarray,
    comp_freqs: np.ndarray,
    comp_gains_db: np.ndarray,
) -> np.ndarray:
    """Subtract a compensation curve from a magnitude spectrum.

    Interpolates the compensation curve to match the FFT frequency bins,
    then subtracts it from the measured magnitude.

    Args:
        magnitude_db: Measured magnitude in dB (one value per FFT bin).
        freqs_fft: Frequency of each FFT bin in Hz.
        comp_freqs: Compensation file frequencies in Hz.
        comp_gains_db: Compensation file gains in dB.

    Returns:
        Corrected magnitude in dB, same shape as magnitude_db.
    """
    interpolator = interp1d(
        comp_freqs,
        comp_gains_db,
        kind="linear",
        bounds_error=False,
        fill_value=(comp_gains_db[0], comp_gains_db[-1]),
    )
    comp_interpolated = interpolator(freqs_fft)
    return magnitude_db - comp_interpolated
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest tests/test_compensation.py -v`
Expected: All 3 tests PASS

- [ ] **Step 6: Commit**

```bash
mkdir -p tests/fixtures
git add paraeq/measurement/compensation.py tests/test_compensation.py tests/fixtures/test_compensation.csv
git commit -m "feat: compensation file loading and application"
```

---

### Task 5: Frequency Response Computation & Smoothing

**Files:**
- Create: `paraeq/measurement/frequency_response.py`
- Create: `tests/test_frequency_response.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_frequency_response.py
import numpy as np
from paraeq.measurement.frequency_response import (
    compute_frequency_response,
    fractional_octave_smooth,
    average_measurements,
)


def test_compute_fr_shape():
    """FR should return frequencies and magnitudes of correct length."""
    sr = 48000
    ir = np.zeros(4096)
    ir[0] = 1.0  # Perfect impulse
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    expected_len = 4096 // 2 + 1  # rfft length
    assert freqs.shape == (expected_len,)
    assert mag_db.shape == (expected_len,)


def test_compute_fr_flat_impulse():
    """A perfect impulse should have a flat frequency response (0 dB)."""
    sr = 48000
    ir = np.zeros(4096)
    ir[0] = 1.0
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    # Should be 0 dB everywhere (within floating point tolerance)
    assert np.allclose(mag_db[1:], 0.0, atol=0.01)  # skip DC


def test_compute_fr_stereo():
    """Stereo IR should return two sets of magnitudes."""
    sr = 48000
    ir = np.zeros((4096, 2))
    ir[0, 0] = 1.0
    ir[0, 1] = 0.5
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    assert mag_db.ndim == 2
    assert mag_db.shape[1] == 2
    # Right channel should be ~-6 dB relative to left
    assert np.allclose(mag_db[1:, 1] - mag_db[1:, 0], -6.02, atol=0.1)


def test_fractional_octave_smooth_reduces_noise():
    """Smoothing should reduce the variance of a noisy spectrum."""
    np.random.seed(42)
    n = 2049
    noisy = np.random.randn(n) * 5.0
    freqs = np.linspace(0, 24000, n)
    smoothed = fractional_octave_smooth(noisy, freqs, fraction=6)
    assert np.std(smoothed[10:]) < np.std(noisy[10:])  # skip near-DC


def test_average_measurements():
    """Averaging two measurements should produce their mean magnitude."""
    n = 2049
    m1 = np.zeros(n)
    m2 = np.full(n, 6.0)
    avg = average_measurements([m1, m2])
    assert np.allclose(avg, 3.0, atol=0.01)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_frequency_response.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement frequency response computation**

```python
# paraeq/measurement/frequency_response.py
"""Frequency response computation, smoothing, and averaging."""

import numpy as np


def compute_frequency_response(
    ir: np.ndarray,
    sample_rate: int,
    n_fft: int | None = None,
) -> tuple[np.ndarray, np.ndarray]:
    """Compute frequency response magnitude from an impulse response.

    Args:
        ir: Impulse response. 1D (mono) or 2D (samples x channels).
        sample_rate: Sample rate in Hz.
        n_fft: FFT size. Defaults to IR length.

    Returns:
        Tuple of (frequencies_hz, magnitude_db).
        magnitude_db is 1D for mono, 2D (bins x channels) for stereo.
    """
    if n_fft is None:
        n_fft = ir.shape[0]

    stereo = ir.ndim == 2
    if not stereo:
        ir = ir[:, np.newaxis]

    freqs = np.fft.rfftfreq(n_fft, 1.0 / sample_rate)
    mag_db_channels = []
    for ch in range(ir.shape[1]):
        spectrum = np.fft.rfft(ir[:, ch], n_fft)
        magnitude = np.abs(spectrum)
        magnitude = np.maximum(magnitude, 1e-10)  # floor to avoid log(0)
        mag_db = 20.0 * np.log10(magnitude)
        mag_db_channels.append(mag_db)

    mag_db = np.column_stack(mag_db_channels)
    if not stereo:
        mag_db = mag_db[:, 0]
    return freqs, mag_db


def fractional_octave_smooth(
    magnitude_db: np.ndarray,
    freqs: np.ndarray,
    fraction: int = 6,
) -> np.ndarray:
    """Apply fractional-octave smoothing to a magnitude spectrum.

    Uses a variable-width moving average where the window width at each
    frequency spans 1/fraction of an octave.

    Args:
        magnitude_db: Magnitude in dB for each frequency bin.
        freqs: Frequency of each bin in Hz.
        fraction: Octave fraction (e.g., 6 for 1/6 octave).

    Returns:
        Smoothed magnitude in dB, same shape as input.
    """
    smoothed = np.copy(magnitude_db)
    # Convert to linear for averaging, smooth, convert back
    linear = 10.0 ** (magnitude_db / 20.0)
    result = np.copy(linear)

    for i in range(len(freqs)):
        if freqs[i] <= 0:
            continue
        # Window spans [f / 2^(1/2N), f * 2^(1/2N)] where N = fraction
        ratio = 2.0 ** (1.0 / (2.0 * fraction))
        f_low = freqs[i] / ratio
        f_high = freqs[i] * ratio
        mask = (freqs >= f_low) & (freqs <= f_high)
        if np.any(mask):
            result[i] = np.mean(linear[mask])

    smoothed = 20.0 * np.log10(np.maximum(result, 1e-10))
    return smoothed


def average_measurements(measurements_db: list[np.ndarray]) -> np.ndarray:
    """Average multiple frequency response measurements in dB.

    Converts to linear, averages, converts back to dB.

    Args:
        measurements_db: List of magnitude arrays in dB (all same shape).

    Returns:
        Averaged magnitude in dB.
    """
    linear = [10.0 ** (m / 20.0) for m in measurements_db]
    avg_linear = np.mean(linear, axis=0)
    return 20.0 * np.log10(np.maximum(avg_linear, 1e-10))
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_frequency_response.py -v`
Expected: All 5 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/measurement/frequency_response.py tests/test_frequency_response.py
git commit -m "feat: frequency response computation with octave smoothing and averaging"
```

---

### Task 6: Target Curve Management

**Files:**
- Create: `paraeq/correction/target_curves.py`
- Create: `tests/test_target_curves.py`
- Create: `targets/harman_ie_2019.csv`
- Create: `targets/harman_oe_2018.csv`
- Create: `targets/diffuse_field.csv`
- Create: `targets/flat.csv`

- [ ] **Step 1: Create built-in target curve CSV files**

Harman In-Ear 2019 (approximate reference points):

```csv
# targets/harman_ie_2019.csv
# Harman In-Ear Target 2019 (approximate)
# frequency_hz,gain_db
20,-0.2
30,0.0
50,0.2
100,0.3
200,0.2
500,0.0
1000,0.0
2000,0.5
3000,1.0
4000,0.0
5000,-2.0
6000,-2.5
7000,-1.5
8000,-2.0
10000,-4.0
12000,-5.5
15000,-7.0
20000,-9.0
```

Harman Over-Ear 2018 (approximate):

```csv
# targets/harman_oe_2018.csv
# Harman Over-Ear Target 2018 (approximate)
# frequency_hz,gain_db
20,-0.5
30,-0.3
50,0.0
100,0.2
200,0.3
500,0.0
1000,0.0
2000,2.0
3000,3.0
4000,1.5
5000,-1.0
6000,-2.0
7000,-1.0
8000,-3.0
10000,-5.0
12000,-7.0
15000,-9.0
20000,-12.0
```

Diffuse Field:

```csv
# targets/diffuse_field.csv
# Diffuse Field Target (approximate)
# frequency_hz,gain_db
20,0.0
50,0.0
100,0.0
200,0.0
500,0.0
1000,0.0
2000,2.0
3000,5.0
4000,4.0
5000,2.0
6000,0.0
7000,-1.0
8000,-2.0
10000,-5.0
12000,-8.0
15000,-10.0
20000,-13.0
```

Flat:

```csv
# targets/flat.csv
# Flat Target
# frequency_hz,gain_db
20,0.0
20000,0.0
```

- [ ] **Step 2: Write the failing tests**

```python
# tests/test_target_curves.py
import numpy as np
from pathlib import Path
from paraeq.correction.target_curves import (
    TargetCurve,
    load_target_csv,
    list_builtin_targets,
    compute_correction,
)


def test_load_target_csv():
    """Load a target curve CSV into a TargetCurve."""
    targets_dir = Path(__file__).parent.parent / "targets"
    curve = load_target_csv(targets_dir / "flat.csv")
    assert curve.name == "flat"
    assert len(curve.frequencies) == 2
    assert curve.frequencies[0] == 20.0
    assert curve.gains_db[0] == 0.0


def test_target_curve_interpolate():
    """Interpolation should produce values at arbitrary frequencies."""
    curve = TargetCurve(
        name="test",
        frequencies=np.array([20.0, 1000.0, 20000.0]),
        gains_db=np.array([0.0, -3.0, -6.0]),
    )
    query_freqs = np.array([20.0, 510.0, 1000.0, 10500.0, 20000.0])
    interpolated = curve.interpolate(query_freqs)
    assert len(interpolated) == 5
    assert interpolated[0] == 0.0
    assert interpolated[2] == -3.0
    assert interpolated[4] == -6.0


def test_list_builtin_targets():
    """Should find all four built-in target CSVs."""
    targets = list_builtin_targets()
    names = {t.name for t in targets}
    assert "diffuse_field" in names
    assert "flat" in names
    assert "harman_ie_2019" in names
    assert "harman_oe_2018" in names


def test_compute_correction():
    """Correction = target - measured."""
    measured = np.array([0.0, 3.0, 6.0])
    target = np.array([0.0, 0.0, 0.0])
    correction = compute_correction(measured, target)
    np.testing.assert_array_almost_equal(correction, [0.0, -3.0, -6.0])
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `pytest tests/test_target_curves.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 4: Implement target curve management**

```python
# paraeq/correction/target_curves.py
"""Target curve loading, interpolation, and correction computation."""

from dataclasses import dataclass
from pathlib import Path

import numpy as np
from scipy.interpolate import CubicSpline


TARGETS_DIR = Path(__file__).parent.parent.parent / "targets"


@dataclass
class TargetCurve:
    """A frequency response target curve."""

    name: str
    frequencies: np.ndarray
    gains_db: np.ndarray

    def interpolate(self, query_freqs: np.ndarray) -> np.ndarray:
        """Interpolate the target curve to arbitrary frequency points.

        Uses cubic spline interpolation in log-frequency space.
        Extrapolates with edge values for out-of-range frequencies.

        Args:
            query_freqs: Frequencies to interpolate to, in Hz.

        Returns:
            Interpolated gain values in dB.
        """
        log_freqs = np.log10(np.maximum(self.frequencies, 1e-1))
        log_query = np.log10(np.maximum(query_freqs, 1e-1))
        cs = CubicSpline(log_freqs, self.gains_db, extrapolate=True)
        result = cs(log_query)
        # Clamp extrapolation to edge values
        result = np.where(
            query_freqs < self.frequencies[0], self.gains_db[0], result
        )
        result = np.where(
            query_freqs > self.frequencies[-1], self.gains_db[-1], result
        )
        return result


def load_target_csv(filepath: Path) -> TargetCurve:
    """Load a target curve from a CSV file.

    CSV format: frequency_hz,gain_db (comment lines start with #).

    Args:
        filepath: Path to the CSV file.

    Returns:
        A TargetCurve instance.
    """
    freqs = []
    gains = []
    with open(filepath, "r") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(",")
            freqs.append(float(parts[0]))
            gains.append(float(parts[1]))
    name = filepath.stem
    return TargetCurve(
        name=name,
        frequencies=np.array(freqs, dtype=np.float64),
        gains_db=np.array(gains, dtype=np.float64),
    )


def list_builtin_targets() -> list[TargetCurve]:
    """List all built-in target curves from the targets/ directory.

    Returns:
        List of TargetCurve instances, sorted alphabetically by name.
    """
    curves = []
    for csv_path in sorted(TARGETS_DIR.glob("*.csv")):
        curves.append(load_target_csv(csv_path))
    return curves


def compute_correction(
    measured_db: np.ndarray, target_db: np.ndarray
) -> np.ndarray:
    """Compute the correction curve: target minus measured.

    Args:
        measured_db: Measured frequency response in dB.
        target_db: Target frequency response in dB (same shape).

    Returns:
        Correction curve in dB.
    """
    return target_db - measured_db
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest tests/test_target_curves.py -v`
Expected: All 4 tests PASS

- [ ] **Step 6: Commit**

```bash
git add paraeq/correction/target_curves.py tests/test_target_curves.py targets/
git commit -m "feat: target curve management with built-in Harman/DF/flat presets"
```

---

### Task 7: FIR Correction Filter Design

**Files:**
- Create: `paraeq/correction/fir_filter.py`
- Create: `tests/test_fir_filter.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_fir_filter.py
import numpy as np
from paraeq.correction.fir_filter import design_fir_correction


def test_fir_correction_length():
    """FIR filter should have the requested number of taps."""
    freqs = np.fft.rfftfreq(4096, 1.0 / 48000)
    correction_db = np.zeros_like(freqs)  # No correction needed
    fir = design_fir_correction(correction_db, freqs, n_taps=4096, phase="minimum")
    assert len(fir) == 4096


def test_fir_correction_flat_is_impulse():
    """A 0 dB correction should produce a near-impulse (passthrough)."""
    n_taps = 4096
    freqs = np.fft.rfftfreq(n_taps, 1.0 / 48000)
    correction_db = np.zeros_like(freqs)
    fir = design_fir_correction(correction_db, freqs, n_taps=n_taps, phase="minimum")
    # Peak should be dominant
    peak_val = np.max(np.abs(fir))
    rms = np.sqrt(np.mean(fir**2))
    assert peak_val > 10 * rms


def test_fir_correction_applies_gain():
    """A +6 dB correction should roughly double the signal amplitude."""
    n_taps = 4096
    sr = 48000
    freqs = np.fft.rfftfreq(n_taps, 1.0 / sr)
    correction_db = np.full_like(freqs, 6.0)  # +6 dB everywhere
    fir = design_fir_correction(correction_db, freqs, n_taps=n_taps, phase="minimum")
    # Check the filter's frequency response at 1 kHz
    fir_fft = np.fft.rfft(fir, n_taps)
    fir_mag_db = 20.0 * np.log10(np.abs(fir_fft) + 1e-10)
    idx_1k = np.argmin(np.abs(freqs - 1000.0))
    assert abs(fir_mag_db[idx_1k] - 6.0) < 1.0  # within 1 dB


def test_fir_linear_phase_is_symmetric():
    """A linear-phase FIR should be symmetric."""
    n_taps = 4096
    freqs = np.fft.rfftfreq(n_taps, 1.0 / 48000)
    correction_db = np.zeros_like(freqs)
    fir = design_fir_correction(correction_db, freqs, n_taps=n_taps, phase="linear")
    # Linear phase FIR is symmetric
    np.testing.assert_array_almost_equal(fir, fir[::-1], decimal=10)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_fir_filter.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement FIR filter design**

```python
# paraeq/correction/fir_filter.py
"""FIR correction filter design via frequency sampling."""

import numpy as np
from scipy.signal import minimum_phase


def design_fir_correction(
    correction_db: np.ndarray,
    freqs: np.ndarray,
    n_taps: int = 4096,
    phase: str = "minimum",
) -> np.ndarray:
    """Design a FIR correction filter from a correction curve.

    Args:
        correction_db: Desired correction in dB at each frequency bin.
        freqs: Frequency of each bin in Hz (from rfftfreq).
        n_taps: Desired FIR filter length.
        phase: "minimum" for minimum-phase or "linear" for linear-phase.

    Returns:
        FIR filter coefficients as a 1D float64 array of length n_taps.
    """
    # Convert dB correction to linear magnitude
    magnitude = 10.0 ** (correction_db / 20.0)

    if phase == "linear":
        return _design_linear_phase(magnitude, n_taps)
    else:
        return _design_minimum_phase(magnitude, n_taps)


def _design_linear_phase(magnitude: np.ndarray, n_taps: int) -> np.ndarray:
    """Design a linear-phase (symmetric) FIR from a magnitude response."""
    # Use frequency sampling: IFFT of the desired magnitude with zero phase
    # Resample magnitude to match n_taps FFT bins
    n_fft_bins = n_taps // 2 + 1
    target_mag = np.interp(
        np.linspace(0, 1, n_fft_bins),
        np.linspace(0, 1, len(magnitude)),
        magnitude,
    )
    # Create zero-phase spectrum
    spectrum = target_mag.astype(np.complex128)
    # IFFT to get impulse response
    ir = np.fft.irfft(spectrum, n_taps)
    # Circular shift to center the peak (makes it symmetric)
    ir = np.roll(ir, n_taps // 2)
    # Apply window to taper edges
    window = np.hanning(n_taps)
    ir *= window
    # Normalize
    ir /= np.max(np.abs(ir))
    # Enforce exact symmetry
    ir = (ir + ir[::-1]) / 2.0
    return ir


def _design_minimum_phase(magnitude: np.ndarray, n_taps: int) -> np.ndarray:
    """Design a minimum-phase FIR from a magnitude response."""
    # Resample magnitude to match a linear-phase prototype
    n_proto = n_taps * 2 - 1  # odd length for minimum_phase input
    n_fft_bins = n_proto // 2 + 1
    target_mag = np.interp(
        np.linspace(0, 1, n_fft_bins),
        np.linspace(0, 1, len(magnitude)),
        magnitude,
    )
    # Create linear-phase prototype via IFFT
    spectrum = target_mag.astype(np.complex128)
    proto = np.fft.irfft(spectrum, n_proto)
    proto = np.roll(proto, n_proto // 2)
    window = np.hanning(n_proto)
    proto *= window
    # Convert to minimum phase using scipy
    min_ph = minimum_phase(proto, method="homomorphic", n_fft=2 ** int(np.ceil(np.log2(n_proto * 4))))
    # Trim or pad to desired length
    if len(min_ph) >= n_taps:
        result = min_ph[:n_taps]
    else:
        result = np.zeros(n_taps)
        result[: len(min_ph)] = min_ph
    return result
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_fir_filter.py -v`
Expected: All 4 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/fir_filter.py tests/test_fir_filter.py
git commit -m "feat: FIR correction filter design (minimum and linear phase)"
```

---

### Task 8: Biquad Filter Coefficients

**Files:**
- Create: `paraeq/correction/biquad.py`
- Create: `tests/test_biquad.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_biquad.py
import numpy as np
from paraeq.correction.biquad import biquad_peaking, biquad_low_shelf, biquad_high_shelf, biquad_notch, biquad_frequency_response


def test_peaking_at_center_frequency():
    """A peaking filter should apply the specified gain at its center frequency."""
    sos = biquad_peaking(fc=1000.0, gain_db=6.0, q=1.0, sample_rate=48000)
    freqs = np.array([1000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert abs(mag_db[0] - 6.0) < 0.5


def test_peaking_unity_at_dc():
    """A peaking filter at 1 kHz should be near 0 dB at low frequencies."""
    sos = biquad_peaking(fc=1000.0, gain_db=6.0, q=2.0, sample_rate=48000)
    freqs = np.array([20.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert abs(mag_db[0]) < 1.0


def test_low_shelf_below_corner():
    """A low shelf should apply gain below the corner frequency."""
    sos = biquad_low_shelf(fc=200.0, gain_db=6.0, q=0.707, sample_rate=48000)
    freqs = np.array([20.0, 10000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert mag_db[0] > 4.0  # Near +6 dB at 20 Hz
    assert abs(mag_db[1]) < 1.0  # Near 0 dB at 10 kHz


def test_high_shelf_above_corner():
    """A high shelf should apply gain above the corner frequency."""
    sos = biquad_high_shelf(fc=5000.0, gain_db=-6.0, q=0.707, sample_rate=48000)
    freqs = np.array([100.0, 15000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert abs(mag_db[0]) < 1.0  # Near 0 dB at 100 Hz
    assert mag_db[1] < -4.0  # Near -6 dB at 15 kHz


def test_notch_attenuates_center():
    """A notch filter should attenuate sharply at center frequency."""
    sos = biquad_notch(fc=1000.0, q=10.0, sample_rate=48000)
    freqs = np.array([1000.0, 2000.0])
    mag_db = biquad_frequency_response(sos, freqs, sample_rate=48000)
    assert mag_db[0] < -20.0  # Deep notch
    assert abs(mag_db[1]) < 3.0  # Mostly recovered at 2 kHz


def test_sos_format():
    """All biquad functions should return a (1, 6) SOS array."""
    sos = biquad_peaking(fc=1000.0, gain_db=3.0, q=1.0, sample_rate=48000)
    assert sos.shape == (1, 6)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_biquad.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement biquad coefficients**

```python
# paraeq/correction/biquad.py
"""Biquad filter coefficient computation (Audio EQ Cookbook formulas)."""

import numpy as np
from scipy.signal import sosfreqz


def biquad_peaking(
    fc: float, gain_db: float, q: float, sample_rate: int
) -> np.ndarray:
    """Compute SOS coefficients for a peaking EQ filter.

    Args:
        fc: Center frequency in Hz.
        gain_db: Gain at center frequency in dB.
        q: Quality factor.
        sample_rate: Sample rate in Hz.

    Returns:
        SOS array of shape (1, 6): [b0, b1, b2, a0, a1, a2].
    """
    a_lin = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)

    b0 = 1.0 + alpha * a_lin
    b1 = -2.0 * np.cos(w0)
    b2 = 1.0 - alpha * a_lin
    a0 = 1.0 + alpha / a_lin
    a1 = -2.0 * np.cos(w0)
    a2 = 1.0 - alpha / a_lin

    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_low_shelf(
    fc: float, gain_db: float, q: float, sample_rate: int
) -> np.ndarray:
    """Compute SOS coefficients for a low shelf filter."""
    a_lin = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)
    cos_w0 = np.cos(w0)
    two_sqrt_a_alpha = 2.0 * np.sqrt(a_lin) * alpha

    b0 = a_lin * ((a_lin + 1) - (a_lin - 1) * cos_w0 + two_sqrt_a_alpha)
    b1 = 2.0 * a_lin * ((a_lin - 1) - (a_lin + 1) * cos_w0)
    b2 = a_lin * ((a_lin + 1) - (a_lin - 1) * cos_w0 - two_sqrt_a_alpha)
    a0 = (a_lin + 1) + (a_lin - 1) * cos_w0 + two_sqrt_a_alpha
    a1 = -2.0 * ((a_lin - 1) + (a_lin + 1) * cos_w0)
    a2 = (a_lin + 1) + (a_lin - 1) * cos_w0 - two_sqrt_a_alpha

    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_high_shelf(
    fc: float, gain_db: float, q: float, sample_rate: int
) -> np.ndarray:
    """Compute SOS coefficients for a high shelf filter."""
    a_lin = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)
    cos_w0 = np.cos(w0)
    two_sqrt_a_alpha = 2.0 * np.sqrt(a_lin) * alpha

    b0 = a_lin * ((a_lin + 1) + (a_lin - 1) * cos_w0 + two_sqrt_a_alpha)
    b1 = -2.0 * a_lin * ((a_lin - 1) + (a_lin + 1) * cos_w0)
    b2 = a_lin * ((a_lin + 1) + (a_lin - 1) * cos_w0 - two_sqrt_a_alpha)
    a0 = (a_lin + 1) - (a_lin - 1) * cos_w0 + two_sqrt_a_alpha
    a1 = 2.0 * ((a_lin - 1) - (a_lin + 1) * cos_w0)
    a2 = (a_lin + 1) - (a_lin - 1) * cos_w0 - two_sqrt_a_alpha

    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_notch(fc: float, q: float, sample_rate: int) -> np.ndarray:
    """Compute SOS coefficients for a notch filter."""
    w0 = 2.0 * np.pi * fc / sample_rate
    alpha = np.sin(w0) / (2.0 * q)

    b0 = 1.0
    b1 = -2.0 * np.cos(w0)
    b2 = 1.0
    a0 = 1.0 + alpha
    a1 = -2.0 * np.cos(w0)
    a2 = 1.0 - alpha

    return np.array([[b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]])


def biquad_frequency_response(
    sos: np.ndarray, freqs: np.ndarray, sample_rate: int
) -> np.ndarray:
    """Compute the magnitude response of a biquad SOS at given frequencies.

    Args:
        sos: SOS array of shape (n_sections, 6).
        freqs: Frequencies in Hz to evaluate at.
        sample_rate: Sample rate in Hz.

    Returns:
        Magnitude response in dB at each frequency.
    """
    worN = freqs * 2.0 * np.pi / sample_rate
    _, h = sosfreqz(sos, worN=worN)
    return 20.0 * np.log10(np.abs(h) + 1e-10)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_biquad.py -v`
Expected: All 6 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/biquad.py tests/test_biquad.py
git commit -m "feat: biquad filter coefficients (peaking, shelf, notch)"
```

---

### Task 9: Parametric EQ Bands & Composite Response

**Files:**
- Create: `paraeq/correction/parametric_eq.py`
- Create: `tests/test_parametric_eq.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_parametric_eq.py
import numpy as np
from paraeq.correction.parametric_eq import (
    EQBand,
    ParametricEQ,
)


def test_eq_band_creation():
    """An EQ band should store its parameters."""
    band = EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.5)
    assert band.filter_type == "peaking"
    assert band.fc == 1000.0
    assert band.gain_db == 3.0
    assert band.q == 1.5


def test_eq_band_to_sos():
    """An EQ band should produce valid SOS coefficients."""
    band = EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.5)
    sos = band.to_sos(sample_rate=48000)
    assert sos.shape == (1, 6)


def test_parametric_eq_empty():
    """An empty PEQ should have flat response."""
    eq = ParametricEQ(bands=[], sample_rate=48000)
    freqs = np.array([100.0, 1000.0, 10000.0])
    mag = eq.frequency_response(freqs)
    np.testing.assert_array_almost_equal(mag, [0.0, 0.0, 0.0], decimal=1)


def test_parametric_eq_single_band():
    """A PEQ with one peaking band should show gain at that frequency."""
    eq = ParametricEQ(
        bands=[EQBand(filter_type="peaking", fc=1000.0, gain_db=6.0, q=1.0)],
        sample_rate=48000,
    )
    freqs = np.array([1000.0])
    mag = eq.frequency_response(freqs)
    assert abs(mag[0] - 6.0) < 0.5


def test_parametric_eq_combined_sos():
    """combined_sos should stack all band SOS arrays."""
    eq = ParametricEQ(
        bands=[
            EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.0),
            EQBand(filter_type="low_shelf", fc=100.0, gain_db=2.0, q=0.707),
        ],
        sample_rate=48000,
    )
    sos = eq.combined_sos()
    assert sos.shape == (2, 6)


def test_parametric_eq_export_text():
    """Export should produce a parseable text representation."""
    eq = ParametricEQ(
        bands=[EQBand(filter_type="peaking", fc=1000.0, gain_db=3.0, q=1.0)],
        sample_rate=48000,
    )
    text = eq.export_autoeq_format()
    assert "1000" in text
    assert "3.0" in text
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_parametric_eq.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement parametric EQ**

```python
# paraeq/correction/parametric_eq.py
"""Parametric EQ with named bands and composite frequency response."""

from dataclasses import dataclass

import numpy as np

from paraeq.correction.biquad import (
    biquad_frequency_response,
    biquad_high_shelf,
    biquad_low_shelf,
    biquad_notch,
    biquad_peaking,
)

FILTER_BUILDERS = {
    "high_shelf": lambda b, sr: biquad_high_shelf(b.fc, b.gain_db, b.q, sr),
    "low_shelf": lambda b, sr: biquad_low_shelf(b.fc, b.gain_db, b.q, sr),
    "notch": lambda b, sr: biquad_notch(b.fc, b.q, sr),
    "peaking": lambda b, sr: biquad_peaking(b.fc, b.gain_db, b.q, sr),
}


@dataclass
class EQBand:
    """A single parametric EQ band."""

    filter_type: str  # "peaking", "low_shelf", "high_shelf", "notch"
    fc: float  # center/corner frequency in Hz
    gain_db: float  # gain in dB (ignored for notch)
    q: float  # quality factor

    def to_sos(self, sample_rate: int) -> np.ndarray:
        """Compute SOS coefficients for this band."""
        builder = FILTER_BUILDERS[self.filter_type]
        return builder(self, sample_rate)


class ParametricEQ:
    """A collection of EQ bands with composite response computation."""

    def __init__(self, bands: list[EQBand], sample_rate: int):
        self.bands = bands
        self.sample_rate = sample_rate

    def combined_sos(self) -> np.ndarray:
        """Stack all band SOS arrays into a single SOS matrix.

        Returns:
            SOS array of shape (n_bands, 6). Empty (0, 6) if no bands.
        """
        if not self.bands:
            return np.empty((0, 6))
        sections = [band.to_sos(self.sample_rate) for band in self.bands]
        return np.vstack(sections)

    def frequency_response(self, freqs: np.ndarray) -> np.ndarray:
        """Compute the composite frequency response of all bands.

        Args:
            freqs: Frequencies in Hz to evaluate at.

        Returns:
            Magnitude response in dB at each frequency.
        """
        if not self.bands:
            return np.zeros_like(freqs)
        sos = self.combined_sos()
        return biquad_frequency_response(sos, freqs, self.sample_rate)

    def export_autoeq_format(self) -> str:
        """Export EQ settings in AutoEQ-compatible text format.

        Returns:
            Multi-line string with one filter per line.
        """
        lines = ["Preamp: 0.0 dB"]
        for i, band in enumerate(self.bands, 1):
            filter_name = {
                "peaking": "PK",
                "low_shelf": "LSC",
                "high_shelf": "HSC",
                "notch": "NO",
            }[band.filter_type]
            lines.append(
                f"Filter {i}: ON {filter_name} Fc {band.fc:.0f} Hz "
                f"Gain {band.gain_db:.1f} dB Q {band.q:.3f}"
            )
        return "\n".join(lines)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_parametric_eq.py -v`
Expected: All 6 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/parametric_eq.py tests/test_parametric_eq.py
git commit -m "feat: parametric EQ bands with composite response and AutoEQ export"
```

---

### Task 10: Auto-Fit Parametric EQ

**Files:**
- Create: `paraeq/correction/auto_fit.py`
- Create: `tests/test_auto_fit.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_auto_fit.py
import numpy as np
from paraeq.correction.auto_fit import auto_fit_parametric_eq
from paraeq.correction.parametric_eq import ParametricEQ


def test_auto_fit_flat_target_produces_no_bands():
    """If correction is zero, auto-fit should produce bands with ~0 gain."""
    sr = 48000
    freqs = np.fft.rfftfreq(4096, 1.0 / sr)
    correction_db = np.zeros_like(freqs)
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=5)
    eq = ParametricEQ(bands=bands, sample_rate=sr)
    response = eq.frequency_response(freqs[1:])  # skip DC
    # Should be very close to 0 dB
    assert np.max(np.abs(response)) < 1.0


def test_auto_fit_single_peak():
    """Auto-fit should place a band near a single peak in the correction curve."""
    sr = 48000
    n_fft = 4096
    freqs = np.fft.rfftfreq(n_fft, 1.0 / sr)
    # Create a correction curve with a +6 dB peak at 1 kHz
    correction_db = np.zeros_like(freqs)
    idx_1k = np.argmin(np.abs(freqs - 1000.0))
    sigma = 50  # bins
    for i in range(len(correction_db)):
        correction_db[i] = 6.0 * np.exp(-0.5 * ((i - idx_1k) / sigma) ** 2)
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=5)
    assert len(bands) >= 1
    # At least one band should be near 1 kHz
    band_freqs = [b.fc for b in bands if abs(b.gain_db) > 1.0]
    assert any(500 < f < 2000 for f in band_freqs)


def test_auto_fit_respects_max_bands():
    """Auto-fit should not exceed the max_bands limit."""
    sr = 48000
    freqs = np.fft.rfftfreq(4096, 1.0 / sr)
    correction_db = np.random.RandomState(42).randn(len(freqs)) * 3.0
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=5)
    assert len(bands) <= 5


def test_auto_fit_reduces_error():
    """Auto-fit result should reduce the RMS error vs the target correction."""
    sr = 48000
    n_fft = 4096
    freqs = np.fft.rfftfreq(n_fft, 1.0 / sr)
    # Complex correction curve
    correction_db = 3.0 * np.sin(2 * np.pi * np.log10(freqs[1:] + 1) / 2)
    correction_db = np.concatenate([[0.0], correction_db])
    bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=sr, max_bands=10)
    eq = ParametricEQ(bands=bands, sample_rate=sr)
    response = eq.frequency_response(freqs[1:])
    residual = correction_db[1:] - response
    rms_before = np.sqrt(np.mean(correction_db[1:] ** 2))
    rms_after = np.sqrt(np.mean(residual**2))
    assert rms_after < rms_before
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_auto_fit.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement auto-fit**

```python
# paraeq/correction/auto_fit.py
"""Auto-fit parametric EQ bands to approximate a correction curve."""

import numpy as np
from paraeq.correction.parametric_eq import EQBand, ParametricEQ


def auto_fit_parametric_eq(
    correction_db: np.ndarray,
    freqs: np.ndarray,
    sample_rate: int,
    max_bands: int = 10,
    min_gain_db: float = 0.5,
) -> list[EQBand]:
    """Iteratively fit peaking EQ bands to a correction curve.

    Greedy algorithm: at each step, find the frequency with the largest
    residual error, place a peaking filter there, subtract its response,
    and repeat.

    Args:
        correction_db: Target correction in dB at each frequency bin.
        freqs: Frequency of each bin in Hz (from rfftfreq).
        sample_rate: Sample rate in Hz.
        max_bands: Maximum number of EQ bands to place.
        min_gain_db: Stop adding bands when peak residual is below this.

    Returns:
        List of EQBand instances.
    """
    bands: list[EQBand] = []
    residual = correction_db.copy()
    # Only fit in audible range
    audible = (freqs >= 20.0) & (freqs <= 20000.0)

    for _ in range(max_bands):
        # Find the frequency with the largest absolute residual
        masked_residual = np.where(audible, np.abs(residual), 0.0)
        peak_idx = int(np.argmax(masked_residual))
        peak_gain = residual[peak_idx]

        if abs(peak_gain) < min_gain_db:
            break

        peak_freq = freqs[peak_idx]
        if peak_freq <= 0:
            break

        # Estimate Q from the width of the residual peak
        q = _estimate_q(residual, freqs, peak_idx)

        band = EQBand(
            filter_type="peaking",
            fc=float(peak_freq),
            gain_db=float(peak_gain),
            q=float(q),
        )
        bands.append(band)

        # Subtract this band's response from the residual
        eq = ParametricEQ(bands=[band], sample_rate=sample_rate)
        band_response = eq.frequency_response(freqs)
        # Handle DC where freq=0 gives NaN
        band_response = np.nan_to_num(band_response, nan=0.0)
        residual = residual - band_response

    return bands


def _estimate_q(
    residual: np.ndarray, freqs: np.ndarray, peak_idx: int
) -> float:
    """Estimate Q factor from the shape of the residual around a peak.

    Measures the -3 dB bandwidth of the peak and converts to Q.
    Falls back to Q=2.0 if the peak shape is too irregular.
    """
    peak_val = abs(residual[peak_idx])
    half_val = peak_val * 0.5  # -6 dB point (half the dB correction)

    # Search left for half-power point
    left_idx = peak_idx
    for i in range(peak_idx - 1, 0, -1):
        if abs(residual[i]) < half_val:
            left_idx = i
            break

    # Search right for half-power point
    right_idx = peak_idx
    for i in range(peak_idx + 1, len(residual)):
        if abs(residual[i]) < half_val:
            right_idx = i
            break

    if left_idx == peak_idx or right_idx == peak_idx:
        return 2.0  # fallback

    f_low = freqs[left_idx]
    f_high = freqs[right_idx]
    f_center = freqs[peak_idx]

    if f_high <= f_low or f_center <= 0:
        return 2.0

    bandwidth = f_high - f_low
    q = f_center / bandwidth
    return np.clip(q, 0.5, 20.0)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_auto_fit.py -v`
Expected: All 4 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/auto_fit.py tests/test_auto_fit.py
git commit -m "feat: greedy auto-fit algorithm for parametric EQ bands"
```

---

### Task 11: Overlap-Add FIR Convolver

**Files:**
- Create: `paraeq/engine/convolver.py`
- Create: `tests/test_convolver.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_convolver.py
import numpy as np
from paraeq.engine.convolver import OverlapAddConvolver


def test_convolver_passthrough():
    """An impulse FIR (passthrough) should not alter the signal."""
    fir = np.zeros(256)
    fir[0] = 1.0
    conv = OverlapAddConvolver(fir, block_size=512)
    input_block = np.random.RandomState(42).randn(512)
    output = conv.process(input_block)
    np.testing.assert_array_almost_equal(output, input_block, decimal=6)


def test_convolver_gain():
    """A FIR with a DC gain of 2.0 should double the signal."""
    fir = np.zeros(64)
    fir[0] = 2.0
    conv = OverlapAddConvolver(fir, block_size=512)
    input_block = np.ones(512)
    output = conv.process(input_block)
    np.testing.assert_array_almost_equal(output, np.full(512, 2.0), decimal=6)


def test_convolver_continuity_across_blocks():
    """Processing consecutive blocks should produce continuous output."""
    # Simple delay filter
    fir = np.zeros(128)
    fir[64] = 1.0  # 64-sample delay
    conv = OverlapAddConvolver(fir, block_size=256)
    # Two blocks of a sine wave
    t = np.arange(512) / 48000.0
    signal = np.sin(2 * np.pi * 1000 * t)
    out1 = conv.process(signal[:256])
    out2 = conv.process(signal[256:])
    output = np.concatenate([out1, out2])
    # The delayed signal should match (after the initial delay settles)
    delayed = np.sin(2 * np.pi * 1000 * (t - 64 / 48000.0))
    # Compare middle section where both are settled
    np.testing.assert_array_almost_equal(
        output[128:384], delayed[128:384], decimal=3
    )


def test_convolver_stereo():
    """Convolver should handle stereo input (samples x 2)."""
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
    """Reset should clear internal state."""
    fir = np.zeros(128)
    fir[64] = 1.0
    conv = OverlapAddConvolver(fir, block_size=256)
    conv.process(np.ones(256))
    conv.reset()
    # After reset, overlap buffer should be zeroed
    output = conv.process(np.zeros(256))
    assert np.max(np.abs(output)) < 1e-10
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_convolver.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement overlap-add convolver**

```python
# paraeq/engine/convolver.py
"""Real-time overlap-add FIR convolution engine."""

import numpy as np


class OverlapAddConvolver:
    """Block-based overlap-add FIR convolver for real-time audio.

    Supports mono (single FIR) or stereo (list of two FIRs, one per channel).

    Args:
        fir: FIR filter. 1D array for mono, or list of two 1D arrays for stereo.
        block_size: Number of samples per processing block.
    """

    def __init__(self, fir: np.ndarray | list[np.ndarray], block_size: int = 512):
        self.block_size = block_size

        if isinstance(fir, list):
            self.stereo = True
            self.n_channels = len(fir)
            self._fir_len = max(len(f) for f in fir)
        else:
            self.stereo = False
            self.n_channels = 1
            fir = [fir]
            self._fir_len = len(fir[0])

        # FFT size: next power of 2 >= block_size + fir_len - 1
        self._n_fft = int(2 ** np.ceil(np.log2(block_size + self._fir_len - 1)))

        # Pre-compute FFT of each FIR filter
        self._fir_fft = []
        for f in fir:
            padded = np.zeros(self._n_fft)
            padded[: len(f)] = f
            self._fir_fft.append(np.fft.rfft(padded))

        # Overlap buffers (one per channel)
        self._overlap = [np.zeros(self._n_fft) for _ in range(self.n_channels)]

    def process(self, block: np.ndarray) -> np.ndarray:
        """Process one block of audio through the FIR filter.

        Args:
            block: Input samples. 1D (mono) or 2D (samples x channels).

        Returns:
            Filtered output, same shape as input.
        """
        if block.ndim == 1:
            channels = [block]
        else:
            channels = [block[:, ch] for ch in range(block.shape[1])]

        outputs = []
        for ch_idx, ch_data in enumerate(channels):
            fir_idx = min(ch_idx, len(self._fir_fft) - 1)
            # FFT the input block (zero-padded to n_fft)
            padded = np.zeros(self._n_fft)
            padded[: len(ch_data)] = ch_data
            block_fft = np.fft.rfft(padded)

            # Multiply and IFFT
            conv_result = np.fft.irfft(block_fft * self._fir_fft[fir_idx], self._n_fft)

            # Add overlap from previous block
            conv_result += self._overlap[ch_idx]

            # Save tail as overlap for next block
            self._overlap[ch_idx] = np.zeros(self._n_fft)
            self._overlap[ch_idx][: self._n_fft - self.block_size] = conv_result[
                self.block_size :
            ]

            outputs.append(conv_result[: self.block_size])

        if block.ndim == 1:
            return outputs[0]
        return np.column_stack(outputs)

    def reset(self):
        """Clear all internal overlap buffers."""
        for i in range(len(self._overlap)):
            self._overlap[i] = np.zeros(self._n_fft)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_convolver.py -v`
Expected: All 5 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/engine/convolver.py tests/test_convolver.py
git commit -m "feat: overlap-add FIR convolution engine"
```

---

### Task 12: IIR Processor (Cascaded Biquads)

**Files:**
- Create: `paraeq/engine/iir_processor.py`
- Create: `tests/test_iir_processor.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_iir_processor.py
import numpy as np
from paraeq.engine.iir_processor import IIRProcessor
from paraeq.correction.biquad import biquad_peaking


def test_iir_passthrough_with_no_filters():
    """No filters loaded should pass audio through unchanged."""
    proc = IIRProcessor(sample_rate=48000)
    block = np.random.RandomState(42).randn(512).astype(np.float64)
    output = proc.process(block)
    np.testing.assert_array_almost_equal(output, block)


def test_iir_applies_filter():
    """A +6 dB peaking filter at 1 kHz should boost a 1 kHz sine."""
    sr = 48000
    sos = biquad_peaking(fc=1000.0, gain_db=6.0, q=1.0, sample_rate=sr)
    proc = IIRProcessor(sample_rate=sr)
    proc.set_sos(sos)
    # Generate 1 kHz sine
    t = np.arange(4096) / sr
    signal = np.sin(2 * np.pi * 1000 * t) * 0.5
    # Process in blocks
    output = np.concatenate([
        proc.process(signal[i:i+512]) for i in range(0, len(signal), 512)
    ])
    # After settling, output amplitude should be ~2x input (6 dB)
    input_rms = np.sqrt(np.mean(signal[2048:] ** 2))
    output_rms = np.sqrt(np.mean(output[2048:] ** 2))
    gain_db = 20 * np.log10(output_rms / input_rms)
    assert abs(gain_db - 6.0) < 1.0


def test_iir_stereo():
    """IIR processor should handle stereo blocks."""
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
    """Reset should clear filter state."""
    sr = 48000
    sos = biquad_peaking(fc=1000.0, gain_db=12.0, q=1.0, sample_rate=sr)
    proc = IIRProcessor(sample_rate=sr)
    proc.set_sos(sos)
    proc.process(np.ones(512))
    proc.reset()
    output = proc.process(np.zeros(512))
    # After reset + zero input, output should be near-zero
    assert np.max(np.abs(output)) < 0.01
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_iir_processor.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement IIR processor**

```python
# paraeq/engine/iir_processor.py
"""Real-time cascaded biquad IIR filter processor."""

import numpy as np
from scipy.signal import sosfilt


class IIRProcessor:
    """Block-based IIR filter processor using cascaded biquad sections.

    Supports mono or stereo with independent filter chains per channel.

    Args:
        sample_rate: Sample rate in Hz.
    """

    def __init__(self, sample_rate: int):
        self.sample_rate = sample_rate
        # Per-channel: sos array and filter state (zi)
        self._sos: dict[int, np.ndarray] = {}
        self._zi: dict[int, np.ndarray] = {}

    def set_sos(self, sos: np.ndarray, channel: int = 0):
        """Set the SOS filter coefficients for a channel.

        Args:
            sos: SOS array of shape (n_sections, 6).
            channel: Channel index (0=left/mono, 1=right).
        """
        self._sos[channel] = sos.copy()
        # Initialize filter state to zeros
        self._zi[channel] = np.zeros((sos.shape[0], 2))

    def process(self, block: np.ndarray) -> np.ndarray:
        """Process a block of audio samples.

        Args:
            block: Input. 1D for mono, 2D (samples x channels) for stereo.

        Returns:
            Filtered output, same shape as input.
        """
        if block.ndim == 1:
            if 0 not in self._sos:
                return block.copy()
            output, self._zi[0] = sosfilt(
                self._sos[0], block, zi=self._zi[0]
            )
            return output

        n_channels = block.shape[1]
        outputs = []
        for ch in range(n_channels):
            if ch not in self._sos:
                outputs.append(block[:, ch].copy())
            else:
                out, self._zi[ch] = sosfilt(
                    self._sos[ch], block[:, ch], zi=self._zi[ch]
                )
                outputs.append(out)
        return np.column_stack(outputs)

    def reset(self):
        """Clear all filter states (but keep the filter coefficients)."""
        for ch in self._zi:
            self._zi[ch] = np.zeros_like(self._zi[ch])
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_iir_processor.py -v`
Expected: All 4 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/engine/iir_processor.py tests/test_iir_processor.py
git commit -m "feat: cascaded biquad IIR processor for real-time parametric EQ"
```

---

### Task 13: Audio Device Discovery

**Files:**
- Create: `paraeq/audio/devices.py`
- Create: `tests/test_devices.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_devices.py
import numpy as np
from paraeq.audio.devices import (
    AudioDevice,
    list_input_devices,
    list_output_devices,
    find_blackhole_device,
)


def test_audio_device_dataclass():
    """AudioDevice should store device info."""
    dev = AudioDevice(
        index=0,
        name="Test Device",
        channels_in=2,
        channels_out=0,
        sample_rate=48000.0,
    )
    assert dev.name == "Test Device"
    assert dev.channels_in == 2
    assert dev.is_input
    assert not dev.is_output


def test_list_input_devices_returns_list():
    """list_input_devices should return a list (may be empty in CI)."""
    devices = list_input_devices()
    assert isinstance(devices, list)


def test_list_output_devices_returns_list():
    """list_output_devices should return a list (may be empty in CI)."""
    devices = list_output_devices()
    assert isinstance(devices, list)


def test_find_blackhole_returns_none_or_device():
    """find_blackhole_device should return None or an AudioDevice."""
    result = find_blackhole_device()
    assert result is None or isinstance(result, AudioDevice)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_devices.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement device discovery**

```python
# paraeq/audio/devices.py
"""Audio device discovery and selection via sounddevice."""

from dataclasses import dataclass

import sounddevice as sd


@dataclass
class AudioDevice:
    """Represents an audio device."""

    index: int
    name: str
    channels_in: int
    channels_out: int
    sample_rate: float

    @property
    def is_input(self) -> bool:
        return self.channels_in > 0

    @property
    def is_output(self) -> bool:
        return self.channels_out > 0


def _query_devices() -> list[AudioDevice]:
    """Query all audio devices from the system."""
    devices = []
    raw = sd.query_devices()
    if not isinstance(raw, list):
        raw = [raw]
    for i, dev in enumerate(raw):
        devices.append(
            AudioDevice(
                index=i,
                name=dev["name"],
                channels_in=dev["max_input_channels"],
                channels_out=dev["max_output_channels"],
                sample_rate=dev["default_samplerate"],
            )
        )
    return devices


def list_input_devices() -> list[AudioDevice]:
    """List all audio input devices (microphones, EARS jig, etc.)."""
    return [d for d in _query_devices() if d.is_input]


def list_output_devices() -> list[AudioDevice]:
    """List all audio output devices (headphone jack, DAC, etc.)."""
    return [d for d in _query_devices() if d.is_output]


def find_blackhole_device() -> AudioDevice | None:
    """Find the BlackHole virtual audio device, if installed.

    Returns:
        The BlackHole AudioDevice, or None if not found.
    """
    for dev in _query_devices():
        if "blackhole" in dev.name.lower():
            return dev
    return None
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_devices.py -v`
Expected: All 4 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/audio/devices.py tests/test_devices.py
git commit -m "feat: audio device discovery with BlackHole detection"
```

---

### Task 14: Audio Streams (Playback, Recording, Pass-Through)

**Files:**
- Create: `paraeq/audio/stream.py`
- Create: `tests/test_stream.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_stream.py
import numpy as np
from unittest.mock import patch, MagicMock
from paraeq.audio.stream import (
    play_signal,
    record_signal,
    AudioPassThrough,
)


def test_play_signal_calls_sounddevice():
    """play_signal should call sd.play with the signal and sample rate."""
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
    """record_signal should call sd.rec with correct parameters."""
    with patch("paraeq.audio.stream.sd") as mock_sd:
        mock_sd.rec.return_value = np.zeros((48000, 2))
        mock_sd.wait = MagicMock()
        result = record_signal(
            duration=1.0, sample_rate=48000, channels=2, device_index=2
        )
        mock_sd.rec.assert_called_once()
        _, kwargs = mock_sd.rec.call_args
        assert kwargs["samplerate"] == 48000
        assert kwargs["channels"] == 2
        assert kwargs["device"] == 2


def test_passthrough_creation():
    """AudioPassThrough should initialize without errors."""
    pt = AudioPassThrough(
        input_device=0,
        output_device=1,
        sample_rate=48000,
        block_size=512,
        channels=2,
    )
    assert pt.sample_rate == 48000
    assert pt.block_size == 512
    assert pt._processor is None


def test_passthrough_set_processor():
    """Setting a processor callback should store it."""
    pt = AudioPassThrough(
        input_device=0, output_device=1, sample_rate=48000, block_size=512, channels=2
    )
    callback = lambda block: block * 0.5
    pt.set_processor(callback)
    assert pt._processor is not None
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_stream.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement audio streams**

```python
# paraeq/audio/stream.py
"""Audio playback, recording, and real-time pass-through streams."""

from collections.abc import Callable
from typing import Any

import numpy as np
import sounddevice as sd


def play_signal(
    signal: np.ndarray,
    sample_rate: int,
    device_index: int | None = None,
) -> None:
    """Play an audio signal through the specified output device.

    Blocks until playback is complete.

    Args:
        signal: Audio samples (1D mono or 2D stereo).
        sample_rate: Sample rate in Hz.
        device_index: Output device index (None for default).
    """
    sd.play(signal, samplerate=sample_rate, device=device_index)
    sd.wait()


def record_signal(
    duration: float,
    sample_rate: int,
    channels: int = 2,
    device_index: int | None = None,
) -> np.ndarray:
    """Record audio from the specified input device.

    Blocks until recording is complete.

    Args:
        duration: Recording duration in seconds.
        sample_rate: Sample rate in Hz.
        channels: Number of channels to record.
        device_index: Input device index (None for default).

    Returns:
        Recorded audio as a 2D array (samples x channels).
    """
    n_samples = int(duration * sample_rate)
    recording = sd.rec(
        n_samples,
        samplerate=sample_rate,
        channels=channels,
        device=device_index,
        dtype=np.float64,
    )
    sd.wait()
    return recording


class AudioPassThrough:
    """Real-time audio pass-through with optional processing callback.

    Routes audio from an input device to an output device, optionally
    applying a processing function to each block.

    Args:
        input_device: Input device index.
        output_device: Output device index.
        sample_rate: Sample rate in Hz.
        block_size: Samples per processing block.
        channels: Number of audio channels.
    """

    def __init__(
        self,
        input_device: int,
        output_device: int,
        sample_rate: int = 48000,
        block_size: int = 512,
        channels: int = 2,
    ):
        self.input_device = input_device
        self.output_device = output_device
        self.sample_rate = sample_rate
        self.block_size = block_size
        self.channels = channels
        self._processor: Callable[[np.ndarray], np.ndarray] | None = None
        self._stream: sd.Stream | None = None
        self._bypass = False
        self._pre_callback: Callable[[np.ndarray], None] | None = None
        self._post_callback: Callable[[np.ndarray], None] | None = None

    def set_processor(self, processor: Callable[[np.ndarray], np.ndarray]):
        """Set the audio processing callback.

        Args:
            processor: Function that takes a block (samples x channels)
                       and returns the processed block (same shape).
        """
        self._processor = processor

    def set_bypass(self, bypass: bool):
        """Enable or disable bypass (passthrough without processing)."""
        self._bypass = bypass

    def set_pre_callback(self, callback: Callable[[np.ndarray], None]):
        """Set a callback that receives input audio (for spectrum analyzer)."""
        self._pre_callback = callback

    def set_post_callback(self, callback: Callable[[np.ndarray], None]):
        """Set a callback that receives processed audio (for spectrum analyzer)."""
        self._post_callback = callback

    def _audio_callback(
        self,
        indata: np.ndarray,
        outdata: np.ndarray,
        frames: int,
        time: Any,
        status: sd.CallbackFlags,
    ):
        """sounddevice stream callback — runs in audio thread."""
        if self._pre_callback is not None:
            self._pre_callback(indata.copy())

        if self._bypass or self._processor is None:
            outdata[:] = indata
        else:
            outdata[:] = self._processor(indata)

        if self._post_callback is not None:
            self._post_callback(outdata.copy())

    def start(self):
        """Start the pass-through stream."""
        self._stream = sd.Stream(
            samplerate=self.sample_rate,
            blocksize=self.block_size,
            device=(self.input_device, self.output_device),
            channels=self.channels,
            dtype=np.float64,
            callback=self._audio_callback,
        )
        self._stream.start()

    def stop(self):
        """Stop the pass-through stream."""
        if self._stream is not None:
            self._stream.stop()
            self._stream.close()
            self._stream = None
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_stream.py -v`
Expected: All 4 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/audio/stream.py tests/test_stream.py
git commit -m "feat: audio playback, recording, and pass-through streams"
```

---

### Task 15: Profile Management

**Files:**
- Create: `paraeq/profiles/profile.py`
- Create: `tests/test_profile.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_profile.py
import json
import tempfile
from pathlib import Path

import numpy as np
from paraeq.profiles.profile import Profile, ProfileManager


def test_profile_creation():
    """A profile should store headphone name and metadata."""
    p = Profile(
        name="SE846 - Harman IE",
        headphone_model="Shure SE846",
        target_curve_name="harman_ie_2019",
    )
    assert p.name == "SE846 - Harman IE"
    assert p.headphone_model == "Shure SE846"


def test_profile_set_measurement():
    """Setting measurement data should store the IR and sample rate."""
    p = Profile(name="test", headphone_model="test")
    ir = np.random.randn(4096, 2)
    p.set_measurement(ir, sample_rate=48000)
    assert p.impulse_response is not None
    np.testing.assert_array_equal(p.impulse_response, ir)
    assert p.sample_rate == 48000


def test_profile_set_eq_bands():
    """Profile should store parametric EQ band data."""
    p = Profile(name="test", headphone_model="test")
    bands_data = [
        {"filter_type": "peaking", "fc": 1000.0, "gain_db": 3.0, "q": 1.5},
        {"filter_type": "low_shelf", "fc": 100.0, "gain_db": 2.0, "q": 0.707},
    ]
    p.set_eq_bands(bands_data)
    assert len(p.eq_bands) == 2


def test_profile_export_import_json(tmp_path):
    """Profiles should round-trip through JSON export/import."""
    p = Profile(
        name="SE846 - Harman IE",
        headphone_model="Shure SE846",
        target_curve_name="harman_ie_2019",
    )
    p.set_eq_bands([
        {"filter_type": "peaking", "fc": 1000.0, "gain_db": 3.0, "q": 1.5},
    ])
    filepath = tmp_path / "test_profile.json"
    p.export_json(filepath)
    loaded = Profile.import_json(filepath)
    assert loaded.name == "SE846 - Harman IE"
    assert loaded.headphone_model == "Shure SE846"
    assert len(loaded.eq_bands) == 1
    assert loaded.eq_bands[0]["fc"] == 1000.0


def test_profile_manager_save_load(tmp_path):
    """ProfileManager should save and load profiles from a directory."""
    manager = ProfileManager(profiles_dir=tmp_path)
    p = Profile(name="test_profile", headphone_model="Test HP")
    manager.save(p)
    loaded = manager.load("test_profile")
    assert loaded.name == "test_profile"


def test_profile_manager_list(tmp_path):
    """ProfileManager should list all saved profiles."""
    manager = ProfileManager(profiles_dir=tmp_path)
    manager.save(Profile(name="alpha", headphone_model="A"))
    manager.save(Profile(name="beta", headphone_model="B"))
    names = manager.list_profiles()
    assert names == ["alpha", "beta"]


def test_profile_manager_delete(tmp_path):
    """ProfileManager should delete a profile."""
    manager = ProfileManager(profiles_dir=tmp_path)
    manager.save(Profile(name="to_delete", headphone_model="X"))
    manager.delete("to_delete")
    assert "to_delete" not in manager.list_profiles()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_profile.py -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement profile management**

```python
# paraeq/profiles/profile.py
"""Profile management for headphone correction configurations."""

import json
import shutil
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from platformdirs import user_data_dir


@dataclass
class Profile:
    """A headphone correction profile."""

    name: str
    headphone_model: str
    target_curve_name: str = "flat"
    impulse_response: np.ndarray | None = field(default=None, repr=False)
    sample_rate: int = 48000
    fir_coefficients: np.ndarray | None = field(default=None, repr=False)
    eq_bands: list[dict] = field(default_factory=list)
    correction_mode: str = "fir"  # "fir" or "parametric"
    notes: str = ""

    def set_measurement(self, ir: np.ndarray, sample_rate: int):
        """Store a measurement impulse response."""
        self.impulse_response = ir
        self.sample_rate = sample_rate

    def set_eq_bands(self, bands: list[dict]):
        """Store parametric EQ band definitions."""
        self.eq_bands = bands

    def export_json(self, filepath: Path):
        """Export the profile to a JSON file (excludes large arrays)."""
        data = {
            "name": self.name,
            "headphone_model": self.headphone_model,
            "target_curve_name": self.target_curve_name,
            "sample_rate": self.sample_rate,
            "eq_bands": self.eq_bands,
            "correction_mode": self.correction_mode,
            "notes": self.notes,
        }
        with open(filepath, "w") as f:
            json.dump(data, f, indent=2)

    @classmethod
    def import_json(cls, filepath: Path) -> "Profile":
        """Import a profile from a JSON file."""
        with open(filepath, "r") as f:
            data = json.load(f)
        p = cls(
            name=data["name"],
            headphone_model=data["headphone_model"],
            target_curve_name=data.get("target_curve_name", "flat"),
            sample_rate=data.get("sample_rate", 48000),
            correction_mode=data.get("correction_mode", "fir"),
            notes=data.get("notes", ""),
        )
        p.eq_bands = data.get("eq_bands", [])
        return p

    def save_ir_wav(self, filepath: Path):
        """Save the impulse response as a WAV file."""
        if self.impulse_response is None:
            return
        import scipy.io.wavfile as wav

        # Normalize to prevent clipping
        ir = self.impulse_response
        max_val = np.max(np.abs(ir))
        if max_val > 0:
            ir = ir / max_val
        wav.write(str(filepath), self.sample_rate, ir.astype(np.float32))

    def load_ir_wav(self, filepath: Path):
        """Load an impulse response from a WAV file."""
        import scipy.io.wavfile as wav

        sr, ir = wav.read(str(filepath))
        self.sample_rate = sr
        self.impulse_response = ir.astype(np.float64)


class ProfileManager:
    """Manages saving, loading, and listing profiles on disk."""

    def __init__(self, profiles_dir: Path | None = None):
        if profiles_dir is None:
            profiles_dir = Path(user_data_dir("ParaEQ")) / "profiles"
        self.profiles_dir = profiles_dir
        self.profiles_dir.mkdir(parents=True, exist_ok=True)

    def save(self, profile: Profile):
        """Save a profile to disk."""
        profile_dir = self.profiles_dir / profile.name
        profile_dir.mkdir(parents=True, exist_ok=True)
        profile.export_json(profile_dir / "profile.json")
        if profile.impulse_response is not None:
            profile.save_ir_wav(profile_dir / "impulse_response.wav")
        if profile.fir_coefficients is not None:
            np.save(profile_dir / "fir_coefficients.npy", profile.fir_coefficients)

    def load(self, name: str) -> Profile:
        """Load a profile from disk."""
        profile_dir = self.profiles_dir / name
        profile = Profile.import_json(profile_dir / "profile.json")
        ir_path = profile_dir / "impulse_response.wav"
        if ir_path.exists():
            profile.load_ir_wav(ir_path)
        fir_path = profile_dir / "fir_coefficients.npy"
        if fir_path.exists():
            profile.fir_coefficients = np.load(fir_path)
        return profile

    def list_profiles(self) -> list[str]:
        """List all saved profile names, sorted alphabetically."""
        names = []
        for p in sorted(self.profiles_dir.iterdir()):
            if p.is_dir() and (p / "profile.json").exists():
                names.append(p.name)
        return names

    def delete(self, name: str):
        """Delete a profile from disk."""
        profile_dir = self.profiles_dir / name
        if profile_dir.exists():
            shutil.rmtree(profile_dir)

    def duplicate(self, name: str, new_name: str):
        """Duplicate an existing profile with a new name."""
        src = self.profiles_dir / name
        dst = self.profiles_dir / new_name
        shutil.copytree(src, dst)
        # Update the name in the copied JSON
        profile = self.load(new_name)
        profile.name = new_name
        self.save(profile)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_profile.py -v`
Expected: All 7 tests PASS

- [ ] **Step 5: Commit**

```bash
git add paraeq/profiles/profile.py tests/test_profile.py
git commit -m "feat: profile management with JSON export/import and WAV IR storage"
```

---

### Task 16: GUI Main Window Shell

**Files:**
- Create: `app/main.py`
- Create: `app/main_window.py`

- [ ] **Step 1: Implement the main entry point**

```python
# app/main.py
"""ParaEQ application entry point."""

import sys

from PyQt6.QtWidgets import QApplication
from app.main_window import MainWindow


def main():
    app = QApplication(sys.argv)
    app.setApplicationName("ParaEQ")
    app.setOrganizationName("ParaEQ")
    window = MainWindow()
    window.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Implement the main window with tabs**

```python
# app/main_window.py
"""Main application window with tab-based navigation."""

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QMainWindow,
    QTabWidget,
    QVBoxLayout,
    QWidget,
    QLabel,
    QStatusBar,
)


class MainWindow(QMainWindow):
    """Main ParaEQ window with tab navigation."""

    def __init__(self):
        super().__init__()
        self.setWindowTitle("ParaEQ")
        self.setMinimumSize(1024, 768)
        self._setup_ui()

    def _setup_ui(self):
        central = QWidget()
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)

        self._tabs = QTabWidget()
        layout.addWidget(self._tabs)

        # Placeholder tabs — each will be replaced by real widgets in later tasks
        self._tabs.addTab(
            self._placeholder("Measurement Wizard"), "Measure"
        )
        self._tabs.addTab(
            self._placeholder("Target Curve Editor"), "Target"
        )
        self._tabs.addTab(
            self._placeholder("Manual EQ"), "EQ"
        )
        self._tabs.addTab(
            self._placeholder("Spectrum Analyzer"), "Analyzer"
        )
        self._tabs.addTab(
            self._placeholder("Profile Manager"), "Profiles"
        )

        self._status_bar = QStatusBar()
        self.setStatusBar(self._status_bar)
        self._status_bar.showMessage("Ready")

    def _placeholder(self, text: str) -> QWidget:
        widget = QWidget()
        layout = QVBoxLayout(widget)
        label = QLabel(text)
        label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(label)
        return widget
```

- [ ] **Step 3: Verify the GUI launches**

```bash
cd /Users/ronakpatel/code/paraeq && source .venv/bin/activate
python -c "
from PyQt6.QtWidgets import QApplication
import sys
app = QApplication(sys.argv)
from app.main_window import MainWindow
w = MainWindow()
print(f'Window title: {w.windowTitle()}')
print(f'Tab count: {w._tabs.count()}')
print('GUI shell OK')
app.quit()
"
```

Expected: `Window title: ParaEQ`, `Tab count: 5`, `GUI shell OK`

- [ ] **Step 4: Commit**

```bash
git add app/main.py app/main_window.py
git commit -m "feat: main window shell with tab navigation"
```

---

### Task 17: Measurement Wizard GUI

**Files:**
- Create: `app/wizard/measurement_wizard.py`
- Modify: `app/main_window.py`

- [ ] **Step 1: Implement the measurement wizard widget**

```python
# app/wizard/measurement_wizard.py
"""Step-by-step headphone measurement wizard."""

from pathlib import Path

import numpy as np
from PyQt6.QtCore import Qt, QThread, pyqtSignal
from PyQt6.QtWidgets import (
    QComboBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QProgressBar,
    QPushButton,
    QSpinBox,
    QStackedWidget,
    QVBoxLayout,
    QWidget,
)
import pyqtgraph as pg

from paraeq.audio.devices import list_input_devices, list_output_devices
from paraeq.audio.stream import play_signal, record_signal
from paraeq.measurement.compensation import apply_compensation, load_compensation
from paraeq.measurement.deconvolution import deconvolve
from paraeq.measurement.frequency_response import (
    average_measurements,
    compute_frequency_response,
    fractional_octave_smooth,
)
from paraeq.measurement.sweep import generate_sweep


class MeasurementWorker(QThread):
    """Background thread for sweep playback and recording."""

    finished = pyqtSignal(np.ndarray)  # emits the recorded signal
    progress = pyqtSignal(int)  # 0-100

    def __init__(
        self,
        sweep: np.ndarray,
        sample_rate: int,
        output_device: int,
        input_device: int,
        duration: float,
    ):
        super().__init__()
        self.sweep = sweep
        self.sample_rate = sample_rate
        self.output_device = output_device
        self.input_device = input_device
        self.duration = duration

    def run(self):
        self.progress.emit(10)
        # Play sweep and record simultaneously by using sd directly
        import sounddevice as sd

        n_rec_samples = int((self.duration + 0.5) * self.sample_rate)
        recording = sd.playrec(
            self.sweep[:, np.newaxis] if self.sweep.ndim == 1 else self.sweep,
            samplerate=self.sample_rate,
            channels=2,
            input_mapping=[1, 2],
            output_mapping=[1],
            device=(self.input_device, self.output_device),
            dtype=np.float64,
        )
        self.progress.emit(50)
        sd.wait()
        self.progress.emit(90)
        self.finished.emit(recording)


class MeasurementWizard(QWidget):
    """Multi-step measurement wizard."""

    measurement_complete = pyqtSignal(dict)  # emits measurement result

    def __init__(self):
        super().__init__()
        self._sample_rate = 48000
        self._sweep_duration = 3.0
        self._sweep = None
        self._comp_freqs = None
        self._comp_gains = None
        self._measurements: list[np.ndarray] = []  # list of magnitude_db arrays
        self._measurement_irs: list[np.ndarray] = []
        self._setup_ui()

    def _setup_ui(self):
        layout = QVBoxLayout(self)

        # --- Device selection ---
        device_group = QWidget()
        device_layout = QVBoxLayout(device_group)

        # Output device
        out_row = QHBoxLayout()
        out_row.addWidget(QLabel("Output Device:"))
        self._output_combo = QComboBox()
        for dev in list_output_devices():
            self._output_combo.addItem(dev.name, dev.index)
        out_row.addWidget(self._output_combo)
        device_layout.addLayout(out_row)

        # Input device
        in_row = QHBoxLayout()
        in_row.addWidget(QLabel("Input Device (EARS):"))
        self._input_combo = QComboBox()
        for dev in list_input_devices():
            self._input_combo.addItem(dev.name, dev.index)
        in_row.addWidget(self._input_combo)
        device_layout.addLayout(in_row)

        # Compensation file
        comp_row = QHBoxLayout()
        comp_row.addWidget(QLabel("Compensation File:"))
        self._comp_label = QLabel("None")
        comp_row.addWidget(self._comp_label)
        self._comp_btn = QPushButton("Load...")
        self._comp_btn.clicked.connect(self._load_compensation)
        comp_row.addWidget(self._comp_btn)
        device_layout.addLayout(comp_row)

        # Headphone name
        name_row = QHBoxLayout()
        name_row.addWidget(QLabel("Headphone Model:"))
        self._hp_name = QLineEdit()
        self._hp_name.setPlaceholderText("e.g., Shure SE846")
        name_row.addWidget(self._hp_name)
        device_layout.addLayout(name_row)

        layout.addWidget(device_group)

        # --- Controls ---
        ctrl_row = QHBoxLayout()
        self._measure_btn = QPushButton("Start Measurement")
        self._measure_btn.clicked.connect(self._start_measurement)
        ctrl_row.addWidget(self._measure_btn)

        self._again_btn = QPushButton("Measure Again (Average)")
        self._again_btn.clicked.connect(self._start_measurement)
        self._again_btn.setEnabled(False)
        ctrl_row.addWidget(self._again_btn)

        self._finish_btn = QPushButton("Save to Profile")
        self._finish_btn.clicked.connect(self._finish)
        self._finish_btn.setEnabled(False)
        ctrl_row.addWidget(self._finish_btn)

        layout.addLayout(ctrl_row)

        # Progress bar
        self._progress = QProgressBar()
        self._progress.setVisible(False)
        layout.addWidget(self._progress)

        # --- FR Plot ---
        self._plot = pg.PlotWidget(title="Frequency Response")
        self._plot.setLogMode(x=True, y=False)
        self._plot.setLabel("bottom", "Frequency", units="Hz")
        self._plot.setLabel("left", "Magnitude", units="dB")
        self._plot.showGrid(x=True, y=True)
        layout.addWidget(self._plot)

        # Measurement count
        self._count_label = QLabel("Measurements: 0")
        layout.addWidget(self._count_label)

    def _load_compensation(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Load Compensation File", "", "CSV Files (*.csv);;All Files (*)"
        )
        if path:
            self._comp_freqs, self._comp_gains = load_compensation(Path(path))
            self._comp_label.setText(Path(path).name)

    def _start_measurement(self):
        self._sweep = generate_sweep(
            duration=self._sweep_duration,
            sample_rate=self._sample_rate,
        )
        output_dev = self._output_combo.currentData()
        input_dev = self._input_combo.currentData()

        self._progress.setVisible(True)
        self._progress.setValue(0)
        self._measure_btn.setEnabled(False)

        self._worker = MeasurementWorker(
            sweep=self._sweep,
            sample_rate=self._sample_rate,
            output_device=output_dev,
            input_device=input_dev,
            duration=self._sweep_duration,
        )
        self._worker.progress.connect(self._progress.setValue)
        self._worker.finished.connect(self._on_recording_complete)
        self._worker.start()

    def _on_recording_complete(self, recording: np.ndarray):
        self._progress.setValue(100)
        self._measure_btn.setEnabled(True)

        # Deconvolve
        ir = deconvolve(
            recorded=recording,
            sweep=self._sweep,
            sample_rate=self._sample_rate,
        )
        self._measurement_irs.append(ir)

        # Compute FR
        freqs, mag_db = compute_frequency_response(ir, self._sample_rate)

        # Apply compensation if loaded
        if self._comp_freqs is not None:
            if mag_db.ndim == 2:
                for ch in range(mag_db.shape[1]):
                    mag_db[:, ch] = apply_compensation(
                        mag_db[:, ch], freqs, self._comp_freqs, self._comp_gains
                    )
            else:
                mag_db = apply_compensation(
                    mag_db, freqs, self._comp_freqs, self._comp_gains
                )

        # Smooth
        if mag_db.ndim == 2:
            for ch in range(mag_db.shape[1]):
                mag_db[:, ch] = fractional_octave_smooth(mag_db[:, ch], freqs, fraction=6)
            avg_mag = np.mean(mag_db, axis=1)
        else:
            avg_mag = fractional_octave_smooth(mag_db, freqs, fraction=6)

        self._measurements.append(avg_mag)

        # Update plot
        self._plot.clear()
        # Plot each individual measurement (dimmed)
        for i, m in enumerate(self._measurements):
            pen = pg.mkPen(color=(100, 100, 255, 80), width=1)
            self._plot.plot(freqs[1:], m[1:], pen=pen)

        # Plot the average (bright)
        if len(self._measurements) > 1:
            avg = average_measurements(self._measurements)
            pen = pg.mkPen(color=(50, 150, 255), width=2)
            self._plot.plot(freqs[1:], avg[1:], pen=pen)

        self._count_label.setText(f"Measurements: {len(self._measurements)}")
        self._again_btn.setEnabled(True)
        self._finish_btn.setEnabled(True)
        self._progress.setVisible(False)

    def _finish(self):
        if not self._measurements:
            return
        freqs = np.fft.rfftfreq(
            self._measurement_irs[0].shape[0], 1.0 / self._sample_rate
        )
        avg_mag = (
            average_measurements(self._measurements)
            if len(self._measurements) > 1
            else self._measurements[0]
        )
        self.measurement_complete.emit({
            "headphone_model": self._hp_name.text() or "Unknown",
            "frequencies": freqs,
            "magnitude_db": avg_mag,
            "impulse_responses": self._measurement_irs,
            "sample_rate": self._sample_rate,
        })
```

- [ ] **Step 2: Replace the placeholder tab in main_window.py**

Replace the "Measure" placeholder tab in `app/main_window.py`:

```python
# In app/main_window.py, update imports at top:
from app.wizard.measurement_wizard import MeasurementWizard

# In _setup_ui, replace the first addTab line:
        self._measurement_wizard = MeasurementWizard()
        self._tabs.addTab(self._measurement_wizard, "Measure")
```

The full updated `_setup_ui` method:

```python
    def _setup_ui(self):
        central = QWidget()
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)

        self._tabs = QTabWidget()
        layout.addWidget(self._tabs)

        self._measurement_wizard = MeasurementWizard()
        self._tabs.addTab(self._measurement_wizard, "Measure")
        self._tabs.addTab(
            self._placeholder("Target Curve Editor"), "Target"
        )
        self._tabs.addTab(
            self._placeholder("Manual EQ"), "EQ"
        )
        self._tabs.addTab(
            self._placeholder("Spectrum Analyzer"), "Analyzer"
        )
        self._tabs.addTab(
            self._placeholder("Profile Manager"), "Profiles"
        )

        self._status_bar = QStatusBar()
        self.setStatusBar(self._status_bar)
        self._status_bar.showMessage("Ready")
```

- [ ] **Step 3: Verify it loads**

```bash
python -c "
from PyQt6.QtWidgets import QApplication
import sys
app = QApplication(sys.argv)
from app.main_window import MainWindow
w = MainWindow()
print(f'Tab 0: {w._tabs.tabText(0)}')
print('Measurement wizard loaded OK')
app.quit()
"
```

Expected: `Tab 0: Measure`, `Measurement wizard loaded OK`

- [ ] **Step 4: Commit**

```bash
git add app/wizard/measurement_wizard.py app/main_window.py
git commit -m "feat: measurement wizard GUI with sweep playback, recording, and FR display"
```

---

### Task 18: Target Curve Editor GUI

**Files:**
- Create: `app/editor/target_curve_editor.py`
- Modify: `app/main_window.py`

- [ ] **Step 1: Implement the target curve editor**

```python
# app/editor/target_curve_editor.py
"""Interactive target curve editor with draggable control points."""

from pathlib import Path

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QComboBox,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from paraeq.correction.fir_filter import design_fir_correction
from paraeq.correction.target_curves import (
    TargetCurve,
    compute_correction,
    list_builtin_targets,
    load_target_csv,
)


class DraggablePoint(pg.ScatterPlotItem):
    """A draggable control point on the target curve."""

    point_moved = pyqtSignal()

    def __init__(self, x, y, **kwargs):
        super().__init__([x], [y], size=12, pen="w", brush="orange", **kwargs)
        self.setAcceptHoverEvents(True)
        self._dragging = False

    def mouseDragEvent(self, ev):
        if ev.button() != pg.QtCore.Qt.MouseButton.LeftButton:
            ev.ignore()
            return
        ev.accept()
        pos = ev.pos()
        self.setData([pos.x()], [pos.y()])
        self.point_moved.emit()


class TargetCurveEditor(QWidget):
    """Target curve editor with preset selection and interactive editing."""

    correction_generated = pyqtSignal(np.ndarray)  # emits FIR coefficients

    def __init__(self):
        super().__init__()
        self._measured_freqs: np.ndarray | None = None
        self._measured_db: np.ndarray | None = None
        self._target_curve: TargetCurve | None = None
        self._builtin_targets = list_builtin_targets()
        self._setup_ui()

    def _setup_ui(self):
        layout = QVBoxLayout(self)

        # Controls row
        ctrl = QHBoxLayout()

        ctrl.addWidget(QLabel("Preset:"))
        self._preset_combo = QComboBox()
        for t in self._builtin_targets:
            self._preset_combo.addItem(t.name)
        self._preset_combo.currentIndexChanged.connect(self._on_preset_changed)
        ctrl.addWidget(self._preset_combo)

        self._import_btn = QPushButton("Import CSV...")
        self._import_btn.clicked.connect(self._import_csv)
        ctrl.addWidget(self._import_btn)

        self._export_btn = QPushButton("Export CSV...")
        self._export_btn.clicked.connect(self._export_csv)
        ctrl.addWidget(self._export_btn)

        self._gen_fir_btn = QPushButton("Generate FIR Filter")
        self._gen_fir_btn.clicked.connect(self._generate_fir)
        self._gen_fir_btn.setEnabled(False)
        ctrl.addWidget(self._gen_fir_btn)

        self._gen_peq_btn = QPushButton("Generate Parametric EQ")
        self._gen_peq_btn.clicked.connect(self._generate_peq)
        self._gen_peq_btn.setEnabled(False)
        ctrl.addWidget(self._gen_peq_btn)

        layout.addLayout(ctrl)

        # Plot
        self._plot = pg.PlotWidget(title="Target Curve Editor")
        self._plot.setLogMode(x=True, y=False)
        self._plot.setLabel("bottom", "Frequency", units="Hz")
        self._plot.setLabel("left", "Magnitude", units="dB")
        self._plot.showGrid(x=True, y=True)
        self._plot.setYRange(-20, 10)
        layout.addWidget(self._plot)

        # Load initial preset
        if self._builtin_targets:
            self._on_preset_changed(0)

    def set_measurement(self, freqs: np.ndarray, magnitude_db: np.ndarray):
        """Set the measured frequency response data for display."""
        self._measured_freqs = freqs
        self._measured_db = magnitude_db
        self._gen_fir_btn.setEnabled(True)
        self._gen_peq_btn.setEnabled(True)
        self._update_plot()

    def _on_preset_changed(self, index: int):
        if 0 <= index < len(self._builtin_targets):
            self._target_curve = self._builtin_targets[index]
            self._update_plot()

    def _import_csv(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Import Target Curve", "", "CSV Files (*.csv)"
        )
        if path:
            self._target_curve = load_target_csv(Path(path))
            self._update_plot()

    def _export_csv(self):
        if self._target_curve is None:
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export Target Curve", "", "CSV Files (*.csv)"
        )
        if path:
            with open(path, "w") as f:
                for freq, gain in zip(
                    self._target_curve.frequencies, self._target_curve.gains_db
                ):
                    f.write(f"{freq},{gain}\n")

    def _update_plot(self):
        self._plot.clear()

        # Plot measured FR
        if self._measured_freqs is not None and self._measured_db is not None:
            pen = pg.mkPen(color=(50, 150, 255), width=2)
            self._plot.plot(
                self._measured_freqs[1:], self._measured_db[1:],
                pen=pen, name="Measured"
            )

        # Plot target curve
        if self._target_curve is not None:
            pen = pg.mkPen(color=(255, 165, 0), width=2)
            self._plot.plot(
                self._target_curve.frequencies,
                self._target_curve.gains_db,
                pen=pen, name="Target",
            )

            # Plot correction delta
            if self._measured_freqs is not None and self._measured_db is not None:
                target_interp = self._target_curve.interpolate(self._measured_freqs)
                correction = compute_correction(self._measured_db, target_interp)
                pen = pg.mkPen(color=(0, 200, 100), width=1, style=pg.QtCore.Qt.PenStyle.DashLine)
                self._plot.plot(
                    self._measured_freqs[1:], correction[1:],
                    pen=pen, name="Correction",
                )

    def _generate_fir(self):
        if self._measured_freqs is None or self._target_curve is None:
            return
        target_interp = self._target_curve.interpolate(self._measured_freqs)
        correction = compute_correction(self._measured_db, target_interp)
        fir = design_fir_correction(
            correction, self._measured_freqs, n_taps=4096, phase="minimum"
        )
        self.correction_generated.emit(fir)

    def _generate_peq(self):
        if self._measured_freqs is None or self._target_curve is None:
            return
        from paraeq.correction.auto_fit import auto_fit_parametric_eq

        target_interp = self._target_curve.interpolate(self._measured_freqs)
        correction = compute_correction(self._measured_db, target_interp)
        bands = auto_fit_parametric_eq(
            correction, self._measured_freqs, sample_rate=48000, max_bands=10
        )
        # Store bands for profile — emit via signal
        bands_data = [
            {"filter_type": b.filter_type, "fc": b.fc, "gain_db": b.gain_db, "q": b.q}
            for b in bands
        ]
        self.correction_generated.emit(np.array(bands_data, dtype=object))
```

- [ ] **Step 2: Wire into main window**

Update `app/main_window.py` imports and `_setup_ui`:

```python
# Add import
from app.editor.target_curve_editor import TargetCurveEditor

# In _setup_ui, replace Target placeholder:
        self._target_editor = TargetCurveEditor()
        self._tabs.addTab(self._target_editor, "Target")
```

- [ ] **Step 3: Verify it loads**

```bash
python -c "
from PyQt6.QtWidgets import QApplication
import sys
app = QApplication(sys.argv)
from app.editor.target_curve_editor import TargetCurveEditor
w = TargetCurveEditor()
print(f'Preset count: {w._preset_combo.count()}')
print('Target editor OK')
app.quit()
"
```

Expected: `Preset count: 4`, `Target editor OK`

- [ ] **Step 4: Commit**

```bash
git add app/editor/target_curve_editor.py app/main_window.py
git commit -m "feat: target curve editor with preset selection and FIR/PEQ generation"
```

---

### Task 19: Manual EQ Editor GUI

**Files:**
- Create: `app/eq/manual_eq_editor.py`
- Modify: `app/main_window.py`

- [ ] **Step 1: Implement the manual EQ editor**

```python
# app/eq/manual_eq_editor.py
"""Manual parametric EQ editor with interactive band manipulation."""

from pathlib import Path

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QComboBox,
    QDoubleSpinBox,
    QFileDialog,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QPushButton,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from paraeq.correction.parametric_eq import EQBand, ParametricEQ


class ManualEQEditor(QWidget):
    """Manual parametric EQ interface with band table and frequency response plot."""

    eq_changed = pyqtSignal(list)  # emits list of EQBand

    def __init__(self, sample_rate: int = 48000):
        super().__init__()
        self._sample_rate = sample_rate
        self._bands: list[EQBand] = []
        self._setup_ui()

    def _setup_ui(self):
        layout = QVBoxLayout(self)

        # Controls
        ctrl = QHBoxLayout()
        self._add_btn = QPushButton("Add Band")
        self._add_btn.clicked.connect(self._add_band)
        ctrl.addWidget(self._add_btn)

        self._remove_btn = QPushButton("Remove Selected")
        self._remove_btn.clicked.connect(self._remove_band)
        ctrl.addWidget(self._remove_btn)

        self._import_btn = QPushButton("Import Preset...")
        self._import_btn.clicked.connect(self._import_preset)
        ctrl.addWidget(self._import_btn)

        self._export_btn = QPushButton("Export...")
        self._export_btn.clicked.connect(self._export_preset)
        ctrl.addWidget(self._export_btn)

        layout.addLayout(ctrl)

        # Band table
        self._table = QTableWidget(0, 4)
        self._table.setHorizontalHeaderLabels(["Type", "Freq (Hz)", "Gain (dB)", "Q"])
        self._table.horizontalHeader().setSectionResizeMode(
            QHeaderView.ResizeMode.Stretch
        )
        self._table.cellChanged.connect(self._on_table_changed)
        layout.addWidget(self._table)

        # Frequency response plot
        self._plot = pg.PlotWidget(title="EQ Response")
        self._plot.setLogMode(x=True, y=False)
        self._plot.setLabel("bottom", "Frequency", units="Hz")
        self._plot.setLabel("left", "Gain", units="dB")
        self._plot.showGrid(x=True, y=True)
        self._plot.setYRange(-15, 15)
        layout.addWidget(self._plot)

    def _add_band(self):
        band = EQBand(filter_type="peaking", fc=1000.0, gain_db=0.0, q=1.0)
        self._bands.append(band)
        self._sync_table()
        self._update_plot()

    def _remove_band(self):
        row = self._table.currentRow()
        if 0 <= row < len(self._bands):
            self._bands.pop(row)
            self._sync_table()
            self._update_plot()

    def _sync_table(self):
        """Sync table display with the bands list."""
        self._table.blockSignals(True)
        self._table.setRowCount(len(self._bands))
        for i, band in enumerate(self._bands):
            type_item = QTableWidgetItem(band.filter_type)
            self._table.setItem(i, 0, type_item)
            self._table.setItem(i, 1, QTableWidgetItem(f"{band.fc:.1f}"))
            self._table.setItem(i, 2, QTableWidgetItem(f"{band.gain_db:.1f}"))
            self._table.setItem(i, 3, QTableWidgetItem(f"{band.q:.3f}"))
        self._table.blockSignals(False)

    def _on_table_changed(self, row: int, col: int):
        """Update band parameters when user edits the table."""
        if row >= len(self._bands):
            return
        try:
            item = self._table.item(row, col)
            if item is None:
                return
            val = item.text()
            if col == 0:
                if val in ("peaking", "low_shelf", "high_shelf", "notch"):
                    self._bands[row].filter_type = val
            elif col == 1:
                self._bands[row].fc = float(val)
            elif col == 2:
                self._bands[row].gain_db = float(val)
            elif col == 3:
                self._bands[row].q = float(val)
        except ValueError:
            pass
        self._update_plot()
        self.eq_changed.emit(self._bands)

    def _update_plot(self):
        self._plot.clear()
        if not self._bands:
            return
        freqs = np.logspace(np.log10(20), np.log10(20000), 500)
        eq = ParametricEQ(bands=self._bands, sample_rate=self._sample_rate)
        response = eq.frequency_response(freqs)

        # Plot composite
        pen = pg.mkPen(color=(50, 150, 255), width=2)
        self._plot.plot(freqs, response, pen=pen)

        # Plot individual bands (dimmed)
        for band in self._bands:
            single_eq = ParametricEQ(bands=[band], sample_rate=self._sample_rate)
            single_resp = single_eq.frequency_response(freqs)
            pen = pg.mkPen(color=(150, 150, 150, 100), width=1)
            self._plot.plot(freqs, single_resp, pen=pen)

        self.eq_changed.emit(self._bands)

    def _import_preset(self):
        """Import EQ bands from an AutoEQ-format text file."""
        path, _ = QFileDialog.getOpenFileName(
            self, "Import EQ Preset", "", "Text Files (*.txt);;CSV Files (*.csv);;All Files (*)"
        )
        if not path:
            return
        self._bands = self._parse_autoeq_file(Path(path))
        self._sync_table()
        self._update_plot()

    def _parse_autoeq_file(self, filepath: Path) -> list[EQBand]:
        """Parse AutoEQ-format text into EQ bands."""
        bands = []
        with open(filepath, "r") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("Preamp") or line.startswith("#"):
                    continue
                parts = line.split()
                # Format: Filter N: ON PK Fc XXXX Hz Gain X.X dB Q X.XXX
                try:
                    filter_type_map = {"PK": "peaking", "LSC": "low_shelf", "HSC": "high_shelf", "NO": "notch"}
                    ft_str = parts[3]
                    fc = float(parts[5])
                    gain = float(parts[8])
                    q = float(parts[11])
                    ft = filter_type_map.get(ft_str, "peaking")
                    bands.append(EQBand(filter_type=ft, fc=fc, gain_db=gain, q=q))
                except (IndexError, ValueError):
                    continue
        return bands

    def _export_preset(self):
        if not self._bands:
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export EQ Preset", "", "Text Files (*.txt)"
        )
        if path:
            eq = ParametricEQ(bands=self._bands, sample_rate=self._sample_rate)
            with open(path, "w") as f:
                f.write(eq.export_autoeq_format())

    def get_bands(self) -> list[EQBand]:
        """Return the current list of EQ bands."""
        return list(self._bands)

    def set_bands(self, bands: list[EQBand]):
        """Set the EQ bands externally."""
        self._bands = list(bands)
        self._sync_table()
        self._update_plot()
```

- [ ] **Step 2: Wire into main window**

Update `app/main_window.py`:

```python
# Add import
from app.eq.manual_eq_editor import ManualEQEditor

# In _setup_ui, replace EQ placeholder:
        self._manual_eq = ManualEQEditor()
        self._tabs.addTab(self._manual_eq, "EQ")
```

- [ ] **Step 3: Commit**

```bash
git add app/eq/manual_eq_editor.py app/main_window.py
git commit -m "feat: manual parametric EQ editor with band table and AutoEQ import/export"
```

---

### Task 20: Real-Time Spectrum Analyzer GUI

**Files:**
- Create: `app/analyzer/spectrum_analyzer.py`
- Modify: `app/main_window.py`

- [ ] **Step 1: Implement the spectrum analyzer**

```python
# app/analyzer/spectrum_analyzer.py
"""Real-time FFT spectrum analyzer widget."""

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import QTimer
from PyQt6.QtWidgets import (
    QComboBox,
    QHBoxLayout,
    QLabel,
    QSlider,
    QVBoxLayout,
    QWidget,
)
from PyQt6.QtCore import Qt

from paraeq.measurement.frequency_response import fractional_octave_smooth


class SpectrumAnalyzer(QWidget):
    """Real-time dual-trace spectrum analyzer (pre/post correction)."""

    def __init__(self, sample_rate: int = 48000, fft_size: int = 4096):
        super().__init__()
        self._sample_rate = sample_rate
        self._fft_size = fft_size
        self._smoothing_fraction = 6  # 1/6 octave
        self._pre_buffer = np.zeros(fft_size)
        self._post_buffer = np.zeros(fft_size)
        self._pre_write_pos = 0
        self._post_write_pos = 0
        self._freqs = np.fft.rfftfreq(fft_size, 1.0 / sample_rate)
        self._setup_ui()
        self._start_timer()

    def _setup_ui(self):
        layout = QVBoxLayout(self)

        # Controls
        ctrl = QHBoxLayout()

        ctrl.addWidget(QLabel("Smoothing:"))
        self._smooth_combo = QComboBox()
        for label, val in [("1/24 oct", 24), ("1/12 oct", 12), ("1/6 oct", 6), ("1/3 oct", 3), ("1/1 oct", 1)]:
            self._smooth_combo.addItem(label, val)
        self._smooth_combo.setCurrentIndex(2)  # 1/6 default
        self._smooth_combo.currentIndexChanged.connect(self._on_smoothing_changed)
        ctrl.addWidget(self._smooth_combo)

        ctrl.addWidget(QLabel("dB Range:"))
        self._range_slider = QSlider(Qt.Orientation.Horizontal)
        self._range_slider.setRange(20, 120)
        self._range_slider.setValue(60)
        self._range_slider.valueChanged.connect(self._on_range_changed)
        ctrl.addWidget(self._range_slider)

        layout.addLayout(ctrl)

        # Plot
        self._plot = pg.PlotWidget(title="Spectrum Analyzer")
        self._plot.setLogMode(x=True, y=False)
        self._plot.setLabel("bottom", "Frequency", units="Hz")
        self._plot.setLabel("left", "Magnitude", units="dB")
        self._plot.showGrid(x=True, y=True)
        self._plot.setYRange(-60, 0)
        self._plot.setXRange(np.log10(20), np.log10(20000))
        layout.addWidget(self._plot)

        # Pre- and post-correction curves
        self._pre_curve = self._plot.plot(
            pen=pg.mkPen(color=(150, 150, 255, 120), width=1), name="Pre"
        )
        self._post_curve = self._plot.plot(
            pen=pg.mkPen(color=(50, 200, 100), width=2), name="Post"
        )

        # VU meters
        vu_row = QHBoxLayout()
        vu_row.addWidget(QLabel("L:"))
        self._vu_left = pg.BarGraphItem(x=[0], height=[0], width=0.3, brush="g")
        vu_plot_l = pg.PlotWidget()
        vu_plot_l.setMaximumHeight(30)
        vu_plot_l.setYRange(-60, 0)
        vu_plot_l.hideAxis("left")
        vu_plot_l.hideAxis("bottom")
        vu_plot_l.addItem(self._vu_left)
        vu_row.addWidget(vu_plot_l)

        vu_row.addWidget(QLabel("R:"))
        self._vu_right = pg.BarGraphItem(x=[0], height=[0], width=0.3, brush="g")
        vu_plot_r = pg.PlotWidget()
        vu_plot_r.setMaximumHeight(30)
        vu_plot_r.setYRange(-60, 0)
        vu_plot_r.hideAxis("left")
        vu_plot_r.hideAxis("bottom")
        vu_plot_r.addItem(self._vu_right)
        vu_row.addWidget(vu_plot_r)

        layout.addLayout(vu_row)

    def _start_timer(self):
        self._timer = QTimer()
        self._timer.timeout.connect(self._update_plot)
        self._timer.start(50)  # 20 fps

    def feed_pre(self, audio_block: np.ndarray):
        """Feed pre-correction audio data (called from audio thread)."""
        mono = audio_block[:, 0] if audio_block.ndim == 2 else audio_block
        n = len(mono)
        start = self._pre_write_pos % self._fft_size
        if start + n <= self._fft_size:
            self._pre_buffer[start:start + n] = mono
        else:
            first = self._fft_size - start
            self._pre_buffer[start:] = mono[:first]
            self._pre_buffer[:n - first] = mono[first:]
        self._pre_write_pos += n

    def feed_post(self, audio_block: np.ndarray):
        """Feed post-correction audio data (called from audio thread)."""
        mono = audio_block[:, 0] if audio_block.ndim == 2 else audio_block
        n = len(mono)
        start = self._post_write_pos % self._fft_size
        if start + n <= self._fft_size:
            self._post_buffer[start:start + n] = mono
        else:
            first = self._fft_size - start
            self._post_buffer[start:] = mono[:first]
            self._post_buffer[:n - first] = mono[first:]
        self._post_write_pos += n

    def _update_plot(self):
        # Compute FFTs
        window = np.hanning(self._fft_size)

        pre_fft = np.abs(np.fft.rfft(self._pre_buffer * window))
        pre_db = 20.0 * np.log10(np.maximum(pre_fft, 1e-10))
        pre_db = fractional_octave_smooth(
            pre_db, self._freqs, fraction=self._smoothing_fraction
        )

        post_fft = np.abs(np.fft.rfft(self._post_buffer * window))
        post_db = 20.0 * np.log10(np.maximum(post_fft, 1e-10))
        post_db = fractional_octave_smooth(
            post_db, self._freqs, fraction=self._smoothing_fraction
        )

        # Update curves (skip DC bin)
        self._pre_curve.setData(self._freqs[1:], pre_db[1:])
        self._post_curve.setData(self._freqs[1:], post_db[1:])

    def _on_smoothing_changed(self, index: int):
        self._smoothing_fraction = self._smooth_combo.currentData()

    def _on_range_changed(self, value: int):
        self._plot.setYRange(-value, 0)
```

- [ ] **Step 2: Wire into main window**

Update `app/main_window.py`:

```python
# Add import
from app.analyzer.spectrum_analyzer import SpectrumAnalyzer

# In _setup_ui, replace Analyzer placeholder:
        self._analyzer = SpectrumAnalyzer()
        self._tabs.addTab(self._analyzer, "Analyzer")
```

- [ ] **Step 3: Commit**

```bash
git add app/analyzer/spectrum_analyzer.py app/main_window.py
git commit -m "feat: real-time spectrum analyzer with pre/post correction traces"
```

---

### Task 21: Profile Manager GUI

**Files:**
- Create: `app/profiles/profile_manager.py`
- Modify: `app/main_window.py`

- [ ] **Step 1: Implement the profile manager widget**

```python
# app/profiles/profile_manager.py
"""Profile management UI — list, switch, import/export, delete."""

from pathlib import Path

from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QFileDialog,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from paraeq.profiles.profile import Profile, ProfileManager as ProfileStore


class ProfileManagerWidget(QWidget):
    """Profile list with management controls."""

    profile_activated = pyqtSignal(str)  # emits profile name

    def __init__(self, profile_store: ProfileStore | None = None):
        super().__init__()
        self._store = profile_store or ProfileStore()
        self._setup_ui()
        self._refresh_list()

    def _setup_ui(self):
        layout = QVBoxLayout(self)

        layout.addWidget(QLabel("Saved Profiles"))

        self._list = QListWidget()
        self._list.itemDoubleClicked.connect(self._on_activate)
        layout.addWidget(self._list)

        # Buttons
        btn_row = QHBoxLayout()

        self._activate_btn = QPushButton("Activate")
        self._activate_btn.clicked.connect(
            lambda: self._on_activate(self._list.currentItem())
        )
        btn_row.addWidget(self._activate_btn)

        self._duplicate_btn = QPushButton("Duplicate")
        self._duplicate_btn.clicked.connect(self._on_duplicate)
        btn_row.addWidget(self._duplicate_btn)

        self._rename_btn = QPushButton("Rename")
        self._rename_btn.clicked.connect(self._on_rename)
        btn_row.addWidget(self._rename_btn)

        self._delete_btn = QPushButton("Delete")
        self._delete_btn.clicked.connect(self._on_delete)
        btn_row.addWidget(self._delete_btn)

        layout.addLayout(btn_row)

        # Import/Export
        ie_row = QHBoxLayout()

        self._import_btn = QPushButton("Import JSON...")
        self._import_btn.clicked.connect(self._on_import)
        ie_row.addWidget(self._import_btn)

        self._export_btn = QPushButton("Export JSON...")
        self._export_btn.clicked.connect(self._on_export)
        ie_row.addWidget(self._export_btn)

        layout.addLayout(ie_row)

    def _refresh_list(self):
        self._list.clear()
        for name in self._store.list_profiles():
            self._list.addItem(QListWidgetItem(name))

    def _selected_name(self) -> str | None:
        item = self._list.currentItem()
        return item.text() if item else None

    def _on_activate(self, item: QListWidgetItem | None):
        if item:
            self.profile_activated.emit(item.text())

    def _on_duplicate(self):
        name = self._selected_name()
        if not name:
            return
        new_name, ok = QInputDialog.getText(
            self, "Duplicate Profile", "New name:", text=f"{name} copy"
        )
        if ok and new_name:
            self._store.duplicate(name, new_name)
            self._refresh_list()

    def _on_rename(self):
        name = self._selected_name()
        if not name:
            return
        new_name, ok = QInputDialog.getText(
            self, "Rename Profile", "New name:", text=name
        )
        if ok and new_name and new_name != name:
            self._store.duplicate(name, new_name)
            self._store.delete(name)
            self._refresh_list()

    def _on_delete(self):
        name = self._selected_name()
        if not name:
            return
        reply = QMessageBox.question(
            self,
            "Delete Profile",
            f"Delete profile '{name}'? This cannot be undone.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if reply == QMessageBox.StandardButton.Yes:
            self._store.delete(name)
            self._refresh_list()

    def _on_import(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Import Profile", "", "JSON Files (*.json)"
        )
        if path:
            profile = Profile.import_json(Path(path))
            self._store.save(profile)
            self._refresh_list()

    def _on_export(self):
        name = self._selected_name()
        if not name:
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export Profile", f"{name}.json", "JSON Files (*.json)"
        )
        if path:
            profile = self._store.load(name)
            profile.export_json(Path(path))

    def add_profile(self, profile: Profile):
        """Add a new profile (called from other widgets)."""
        self._store.save(profile)
        self._refresh_list()
```

- [ ] **Step 2: Wire into main window**

Update `app/main_window.py`:

```python
# Add import
from app.profiles.profile_manager import ProfileManagerWidget

# In _setup_ui, replace Profiles placeholder:
        self._profile_manager = ProfileManagerWidget()
        self._tabs.addTab(self._profile_manager, "Profiles")
```

- [ ] **Step 3: Commit**

```bash
git add app/profiles/profile_manager.py app/main_window.py
git commit -m "feat: profile manager GUI with activate, duplicate, rename, delete, import/export"
```

---

### Task 22: Menu Bar / System Tray

**Files:**
- Create: `app/menu_bar/tray.py`
- Modify: `app/main.py`

- [ ] **Step 1: Implement the system tray**

```python
# app/menu_bar/tray.py
"""System tray / menu bar icon for ParaEQ."""

from PyQt6.QtGui import QAction, QIcon
from PyQt6.QtWidgets import QMenu, QSystemTrayIcon, QWidget


class ParaEQTray(QSystemTrayIcon):
    """Menu bar / system tray icon with quick controls."""

    def __init__(self, parent: QWidget | None = None):
        super().__init__(parent)
        self._current_profile = "None"
        self._bypass = False
        self._profiles: list[str] = []
        self._on_profile_switch = None
        self._on_bypass_toggle = None
        self._on_show_window = None
        self._on_quit = None
        self._setup_menu()
        self.setToolTip("ParaEQ")

    def _setup_menu(self):
        menu = QMenu()

        # Current profile display
        self._profile_label = QAction("Profile: None", menu)
        self._profile_label.setEnabled(False)
        menu.addAction(self._profile_label)

        menu.addSeparator()

        # Profile submenu
        self._profile_menu = menu.addMenu("Switch Profile")

        # Bypass toggle
        self._bypass_action = QAction("Bypass", menu)
        self._bypass_action.setCheckable(True)
        self._bypass_action.triggered.connect(self._toggle_bypass)
        menu.addAction(self._bypass_action)

        menu.addSeparator()

        # Show window
        self._show_action = QAction("Show Window", menu)
        self._show_action.triggered.connect(self._show_window)
        menu.addAction(self._show_action)

        menu.addSeparator()

        # Quit
        self._quit_action = QAction("Quit ParaEQ", menu)
        self._quit_action.triggered.connect(self._quit)
        menu.addAction(self._quit_action)

        self.setContextMenu(menu)

    def set_callbacks(
        self,
        on_profile_switch=None,
        on_bypass_toggle=None,
        on_show_window=None,
        on_quit=None,
    ):
        """Set callback functions for tray actions."""
        self._on_profile_switch = on_profile_switch
        self._on_bypass_toggle = on_bypass_toggle
        self._on_show_window = on_show_window
        self._on_quit = on_quit

    def update_profiles(self, profiles: list[str], current: str):
        """Update the profile switch submenu."""
        self._profiles = profiles
        self._current_profile = current
        self._profile_label.setText(f"Profile: {current}")
        self._profile_menu.clear()
        for name in profiles:
            action = self._profile_menu.addAction(name)
            action.setCheckable(True)
            action.setChecked(name == current)
            action.triggered.connect(lambda checked, n=name: self._switch_profile(n))

    def _switch_profile(self, name: str):
        self._current_profile = name
        self._profile_label.setText(f"Profile: {name}")
        if self._on_profile_switch:
            self._on_profile_switch(name)

    def _toggle_bypass(self, checked: bool):
        self._bypass = checked
        if self._on_bypass_toggle:
            self._on_bypass_toggle(checked)

    def _show_window(self):
        if self._on_show_window:
            self._on_show_window()

    def _quit(self):
        if self._on_quit:
            self._on_quit()
```

- [ ] **Step 2: Wire tray into app/main.py**

```python
# app/main.py
"""ParaEQ application entry point."""

import sys

from PyQt6.QtWidgets import QApplication
from app.main_window import MainWindow
from app.menu_bar.tray import ParaEQTray


def main():
    app = QApplication(sys.argv)
    app.setApplicationName("ParaEQ")
    app.setOrganizationName("ParaEQ")
    app.setQuitOnLastWindowClosed(False)

    window = MainWindow()

    tray = ParaEQTray()
    tray.set_callbacks(
        on_show_window=window.show,
        on_quit=app.quit,
    )
    tray.show()

    window.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
```

- [ ] **Step 3: Commit**

```bash
git add app/menu_bar/tray.py app/main.py
git commit -m "feat: system tray with profile switching, bypass toggle, and quick access"
```

---

### Task 23: Setup Wizard (BlackHole Detection)

**Files:**
- Create: `app/setup/setup_wizard.py`
- Modify: `app/main.py`

- [ ] **Step 1: Implement the setup wizard**

```python
# app/setup/setup_wizard.py
"""First-launch setup wizard for BlackHole configuration."""

from pathlib import Path

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QComboBox,
    QDialog,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QVBoxLayout,
    QWizard,
    QWizardPage,
)

from paraeq.audio.devices import find_blackhole_device, list_output_devices


class BlackHoleCheckPage(QWizardPage):
    """Check if BlackHole is installed."""

    def __init__(self):
        super().__init__()
        self.setTitle("Audio Setup")
        self.setSubTitle("ParaEQ needs a virtual audio device to process system audio.")
        layout = QVBoxLayout(self)

        self._status_label = QLabel()
        layout.addWidget(self._status_label)

        self._check_btn = QPushButton("Check for BlackHole")
        self._check_btn.clicked.connect(self._check)
        layout.addWidget(self._check_btn)

        self._info_label = QLabel(
            "BlackHole is bundled with ParaEQ. If it's not detected, "
            "it may need to be installed from the ParaEQ installer package."
        )
        self._info_label.setWordWrap(True)
        layout.addWidget(self._info_label)

        self._found = False

    def _check(self):
        bh = find_blackhole_device()
        if bh:
            self._status_label.setText(f"BlackHole found: {bh.name}")
            self._status_label.setStyleSheet("color: green;")
            self._found = True
        else:
            self._status_label.setText(
                "BlackHole not found. Please install it from the ParaEQ installer."
            )
            self._status_label.setStyleSheet("color: red;")
            self._found = False
        self.completeChanged.emit()

    def isComplete(self) -> bool:
        return self._found


class OutputDevicePage(QWizardPage):
    """Select the headphone output device."""

    def __init__(self):
        super().__init__()
        self.setTitle("Output Device")
        self.setSubTitle(
            "Select your headphone output device. This is where ParaEQ will send corrected audio."
        )
        layout = QVBoxLayout(self)

        layout.addWidget(QLabel("Headphone Output:"))
        self._combo = QComboBox()
        for dev in list_output_devices():
            if "blackhole" not in dev.name.lower():
                self._combo.addItem(dev.name, dev.index)
        layout.addWidget(self._combo)

    def get_device_index(self) -> int | None:
        return self._combo.currentData()


class SetupCompletePage(QWizardPage):
    """Setup complete confirmation."""

    def __init__(self):
        super().__init__()
        self.setTitle("Setup Complete")
        layout = QVBoxLayout(self)
        layout.addWidget(
            QLabel(
                "ParaEQ is configured.\n\n"
                "Set your macOS system audio output to 'BlackHole 2ch' in "
                "System Settings > Sound.\n\n"
                "ParaEQ will route audio from BlackHole through your "
                "correction filters to your headphones."
            )
        )


class SetupWizard(QWizard):
    """First-launch setup wizard."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowTitle("ParaEQ Setup")
        self.setMinimumSize(500, 400)

        self._bh_page = BlackHoleCheckPage()
        self._output_page = OutputDevicePage()
        self._complete_page = SetupCompletePage()

        self.addPage(self._bh_page)
        self.addPage(self._output_page)
        self.addPage(self._complete_page)

    def get_output_device(self) -> int | None:
        return self._output_page.get_device_index()
```

- [ ] **Step 2: Add first-launch check to app/main.py**

```python
# app/main.py
"""ParaEQ application entry point."""

import sys
from pathlib import Path

from platformdirs import user_data_dir
from PyQt6.QtWidgets import QApplication

from app.main_window import MainWindow
from app.menu_bar.tray import ParaEQTray
from app.setup.setup_wizard import SetupWizard


def _is_first_launch() -> bool:
    config_dir = Path(user_data_dir("ParaEQ"))
    return not (config_dir / "setup_complete").exists()


def _mark_setup_complete():
    config_dir = Path(user_data_dir("ParaEQ"))
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "setup_complete").touch()


def main():
    app = QApplication(sys.argv)
    app.setApplicationName("ParaEQ")
    app.setOrganizationName("ParaEQ")
    app.setQuitOnLastWindowClosed(False)

    if _is_first_launch():
        wizard = SetupWizard()
        if wizard.exec():
            _mark_setup_complete()
        else:
            sys.exit(0)

    window = MainWindow()

    tray = ParaEQTray()
    tray.set_callbacks(
        on_show_window=window.show,
        on_quit=app.quit,
    )
    tray.show()

    window.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
```

- [ ] **Step 3: Commit**

```bash
git add app/setup/setup_wizard.py app/main.py
git commit -m "feat: first-launch setup wizard with BlackHole detection and output device selection"
```

---

### Task 24: Wire Everything Together in Main Window

**Files:**
- Modify: `app/main_window.py`

- [ ] **Step 1: Update main window with full wiring**

```python
# app/main_window.py
"""Main application window with tab-based navigation."""

import numpy as np
from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QLabel,
    QMainWindow,
    QStatusBar,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from app.analyzer.spectrum_analyzer import SpectrumAnalyzer
from app.editor.target_curve_editor import TargetCurveEditor
from app.eq.manual_eq_editor import ManualEQEditor
from app.profiles.profile_manager import ProfileManagerWidget
from app.wizard.measurement_wizard import MeasurementWizard
from paraeq.audio.devices import find_blackhole_device, list_output_devices
from paraeq.audio.stream import AudioPassThrough
from paraeq.correction.parametric_eq import EQBand, ParametricEQ
from paraeq.engine.convolver import OverlapAddConvolver
from paraeq.engine.iir_processor import IIRProcessor
from paraeq.profiles.profile import Profile, ProfileManager


class MainWindow(QMainWindow):
    """Main ParaEQ window with tab navigation and audio engine."""

    def __init__(self):
        super().__init__()
        self.setWindowTitle("ParaEQ")
        self.setMinimumSize(1024, 768)

        self._profile_store = ProfileManager()
        self._active_profile: Profile | None = None
        self._pass_through: AudioPassThrough | None = None
        self._convolver: OverlapAddConvolver | None = None
        self._iir_proc = IIRProcessor(sample_rate=48000)

        self._setup_ui()
        self._connect_signals()

    def _setup_ui(self):
        central = QWidget()
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)

        self._tabs = QTabWidget()
        layout.addWidget(self._tabs)

        self._measurement_wizard = MeasurementWizard()
        self._tabs.addTab(self._measurement_wizard, "Measure")

        self._target_editor = TargetCurveEditor()
        self._tabs.addTab(self._target_editor, "Target")

        self._manual_eq = ManualEQEditor()
        self._tabs.addTab(self._manual_eq, "EQ")

        self._analyzer = SpectrumAnalyzer()
        self._tabs.addTab(self._analyzer, "Analyzer")

        self._profile_manager = ProfileManagerWidget(self._profile_store)
        self._tabs.addTab(self._profile_manager, "Profiles")

        self._status_bar = QStatusBar()
        self.setStatusBar(self._status_bar)
        self._status_bar.showMessage("Ready")

    def _connect_signals(self):
        # Measurement complete → pass data to target editor and create profile
        self._measurement_wizard.measurement_complete.connect(
            self._on_measurement_complete
        )
        # Profile activated → load and apply
        self._profile_manager.profile_activated.connect(self._on_profile_activated)
        # Manual EQ changed → update IIR processor
        self._manual_eq.eq_changed.connect(self._on_eq_changed)

    def _on_measurement_complete(self, data: dict):
        freqs = data["frequencies"]
        mag_db = data["magnitude_db"]
        self._target_editor.set_measurement(freqs, mag_db)
        # Create a new profile
        profile = Profile(
            name=data["headphone_model"],
            headphone_model=data["headphone_model"],
        )
        if data["impulse_responses"]:
            avg_ir = np.mean(data["impulse_responses"], axis=0)
            profile.set_measurement(avg_ir, data["sample_rate"])
        self._profile_manager.add_profile(profile)
        self._tabs.setCurrentWidget(self._target_editor)
        self._status_bar.showMessage(
            f"Measurement complete for {data['headphone_model']}"
        )

    def _on_profile_activated(self, name: str):
        profile = self._profile_store.load(name)
        self._active_profile = profile
        if profile.eq_bands:
            bands = [
                EQBand(
                    filter_type=b["filter_type"],
                    fc=b["fc"],
                    gain_db=b["gain_db"],
                    q=b["q"],
                )
                for b in profile.eq_bands
            ]
            self._manual_eq.set_bands(bands)
        self._status_bar.showMessage(f"Active profile: {name}")

    def _on_eq_changed(self, bands: list):
        if not bands:
            return
        eq = ParametricEQ(bands=bands, sample_rate=48000)
        sos = eq.combined_sos()
        self._iir_proc.set_sos(sos, channel=0)
        self._iir_proc.set_sos(sos, channel=1)

    def start_audio_engine(self, input_device: int, output_device: int):
        """Start the real-time audio pass-through engine."""
        self._pass_through = AudioPassThrough(
            input_device=input_device,
            output_device=output_device,
            sample_rate=48000,
            block_size=512,
            channels=2,
        )
        self._pass_through.set_processor(self._process_audio)
        self._pass_through.set_pre_callback(self._analyzer.feed_pre)
        self._pass_through.set_post_callback(self._analyzer.feed_post)
        self._pass_through.start()
        self._status_bar.showMessage("Audio engine running")

    def _process_audio(self, block: np.ndarray) -> np.ndarray:
        """Process an audio block through the active correction chain."""
        if self._convolver is not None:
            block = self._convolver.process(block)
        block = self._iir_proc.process(block)
        return block

    def stop_audio_engine(self):
        """Stop the audio pass-through engine."""
        if self._pass_through is not None:
            self._pass_through.stop()
            self._pass_through = None

    def closeEvent(self, event):
        self.stop_audio_engine()
        event.accept()
```

- [ ] **Step 2: Commit**

```bash
git add app/main_window.py
git commit -m "feat: wire all GUI components together with audio engine and signal routing"
```

---

### Task 25: Built-In Target Curve Data & Final Integration Test

**Files:**
- Verify: `targets/*.csv` (created in Task 6)
- Create: `tests/test_integration.py`

- [ ] **Step 1: Write integration test**

```python
# tests/test_integration.py
"""End-to-end integration test: sweep → measure → correct → filter."""

import numpy as np
from paraeq.measurement.sweep import generate_sweep
from paraeq.measurement.deconvolution import deconvolve
from paraeq.measurement.frequency_response import (
    compute_frequency_response,
    fractional_octave_smooth,
)
from paraeq.correction.target_curves import list_builtin_targets, compute_correction
from paraeq.correction.fir_filter import design_fir_correction
from paraeq.correction.auto_fit import auto_fit_parametric_eq
from paraeq.correction.parametric_eq import ParametricEQ
from paraeq.engine.convolver import OverlapAddConvolver
from paraeq.engine.iir_processor import IIRProcessor
from paraeq.profiles.profile import Profile


def test_full_measurement_to_fir_pipeline():
    """Simulate a complete measurement → FIR correction pipeline."""
    sr = 48000

    # 1. Generate sweep
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    assert len(sweep) == sr

    # 2. Simulate a "colored" headphone by filtering the sweep
    # Apply a simple resonance at 3 kHz (+6 dB)
    from paraeq.correction.biquad import biquad_peaking
    from scipy.signal import sosfilt
    colored_sos = biquad_peaking(fc=3000.0, gain_db=6.0, q=2.0, sample_rate=sr)
    recorded = sosfilt(colored_sos, sweep)

    # 3. Deconvolve to get IR
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    assert len(ir) > 0

    # 4. Compute FR
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    mag_db = fractional_octave_smooth(mag_db, freqs, fraction=6)

    # 5. Compute correction against flat target
    targets = list_builtin_targets()
    flat = next(t for t in targets if t.name == "flat")
    target_interp = flat.interpolate(freqs)
    correction = compute_correction(mag_db, target_interp)

    # 6. Design FIR correction filter
    fir = design_fir_correction(correction, freqs, n_taps=4096, phase="minimum")
    assert len(fir) == 4096

    # 7. Apply FIR to a test signal and verify correction
    convolver = OverlapAddConvolver(fir, block_size=512)
    # Generate 3 kHz tone
    t = np.arange(4096) / sr
    test_tone = np.sin(2 * np.pi * 3000 * t) * 0.5
    # First color the tone (simulating headphone)
    colored_tone = sosfilt(colored_sos, test_tone)
    # Then correct it
    corrected = np.concatenate([
        convolver.process(colored_tone[i:i+512])
        for i in range(0, len(colored_tone), 512)
    ])
    # The corrected tone should be closer to the original than the colored one
    # (measure via RMS difference in the settled region)
    original_rms = np.sqrt(np.mean(test_tone[2048:] ** 2))
    colored_error = np.sqrt(np.mean((colored_tone[2048:] - test_tone[2048:]) ** 2))
    corrected_error = np.sqrt(np.mean((corrected[2048:] - test_tone[2048:]) ** 2))
    assert corrected_error < colored_error


def test_full_measurement_to_peq_pipeline():
    """Simulate measurement → auto-fit parametric EQ pipeline."""
    sr = 48000
    sweep = generate_sweep(duration=1.0, sample_rate=sr)
    from paraeq.correction.biquad import biquad_peaking
    from scipy.signal import sosfilt
    colored_sos = biquad_peaking(fc=3000.0, gain_db=6.0, q=2.0, sample_rate=sr)
    recorded = sosfilt(colored_sos, sweep)
    ir = deconvolve(recorded=recorded, sweep=sweep, sample_rate=sr)
    freqs, mag_db = compute_frequency_response(ir, sample_rate=sr)
    mag_db = fractional_octave_smooth(mag_db, freqs, fraction=6)

    targets = list_builtin_targets()
    flat = next(t for t in targets if t.name == "flat")
    target_interp = flat.interpolate(freqs)
    correction = compute_correction(mag_db, target_interp)

    # Auto-fit PEQ
    bands = auto_fit_parametric_eq(correction, freqs, sample_rate=sr, max_bands=10)
    assert len(bands) >= 1

    eq = ParametricEQ(bands=bands, sample_rate=sr)
    sos = eq.combined_sos()
    assert sos.shape[0] >= 1

    # Apply through IIR processor
    proc = IIRProcessor(sample_rate=sr)
    proc.set_sos(sos)
    test_block = np.random.RandomState(42).randn(512) * 0.1
    output = proc.process(test_block)
    assert output.shape == test_block.shape


def test_profile_round_trip():
    """Create a profile with EQ bands, save, load, verify."""
    import tempfile
    from pathlib import Path
    from paraeq.profiles.profile import ProfileManager

    with tempfile.TemporaryDirectory() as tmpdir:
        manager = ProfileManager(profiles_dir=Path(tmpdir))
        p = Profile(
            name="SE846-Harman",
            headphone_model="Shure SE846",
            target_curve_name="harman_ie_2019",
        )
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
```

- [ ] **Step 2: Run integration tests**

Run: `pytest tests/test_integration.py -v`
Expected: All 3 tests PASS

- [ ] **Step 3: Run full test suite**

Run: `pytest tests/ -v --tb=short`
Expected: All tests PASS

- [ ] **Step 4: Commit**

```bash
git add tests/test_integration.py
git commit -m "feat: end-to-end integration tests for measurement, correction, and profile pipelines"
```

---

### Task 26: README and Package Finalization

**Files:**
- Create: `README.md`
- Verify: all `__init__.py` files have proper exports

- [ ] **Step 1: Create README.md**

```markdown
# ParaEQ

Open-source headphone measurement and correction EQ for macOS.

ParaEQ provides Dirac Live-equivalent capabilities using open-source DSP tools:
frequency response measurement, target curve matching, and real-time system-wide EQ.

## Features

- **Measurement wizard** — Log sine sweep measurement with miniDSP EARS jig support
- **Target curve editor** — Built-in Harman IE/OE, Diffuse Field, and Flat presets
- **Manual parametric EQ** — Standalone EQ with AutoEQ preset import
- **Real-time spectrum analyzer** — Pre/post correction visualization
- **Profile manager** — Per-headphone profiles with quick switching
- **System-wide audio** — Processes all Mac audio via BlackHole virtual device

## Install

```bash
# From source (development)
git clone https://github.com/yourusername/paraeq.git
cd paraeq
python -m venv .venv
source .venv/bin/activate
pip install -e ".[dev,gui]"

# DSP library only (no GUI)
pip install paraeq
```

## Usage

```bash
# Launch the GUI
python -m app.main

# Use the library in scripts
python -c "
from paraeq.measurement.sweep import generate_sweep
sweep = generate_sweep(duration=3.0, sample_rate=48000)
print(f'Generated {len(sweep)} samples')
"
```

## Requirements

- macOS (BlackHole virtual audio device, bundled with installer)
- Python 3.11+
- miniDSP EARS headphone measurement jig (for measurement features)

## License

MIT
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: add README with install and usage instructions"
```

---

## Self-Review Checklist

1. **Spec coverage:** All spec sections mapped to tasks:
   - Measurement pipeline → Tasks 2-5
   - Correction engine (target curves, FIR, PEQ) → Tasks 6-10
   - Real-time engine (convolver, IIR) → Tasks 11-12
   - Audio I/O → Tasks 13-14
   - Profile management → Task 15
   - GUI (all 6 components) → Tasks 16-22
   - BlackHole/setup → Task 23
   - Integration wiring → Task 24
   - Integration tests → Task 25
   - Distribution/README → Task 26

2. **Placeholder scan:** No TBD/TODO found. All code steps include full implementations.

3. **Type consistency verified:**
   - `EQBand` used consistently across `biquad.py`, `parametric_eq.py`, `auto_fit.py`, and `manual_eq_editor.py`
   - `TargetCurve` dataclass used consistently across `target_curves.py` and `target_curve_editor.py`
   - `Profile` used consistently across `profile.py`, `profile_manager.py`, and `main_window.py`
   - `OverlapAddConvolver` / `IIRProcessor` interfaces match between engine and main_window usage
   - `AudioPassThrough` callbacks match spectrum analyzer `feed_pre`/`feed_post` signatures
