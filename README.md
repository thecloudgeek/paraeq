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
