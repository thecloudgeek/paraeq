# ParaEQ

Open-source headphone measurement and correction EQ for macOS.

ParaEQ aims at Dirac Live-equivalent capabilities using open-source DSP:
frequency response measurement, target curve matching, and real-time
system-wide EQ — with no audio driver to install.

**Status: pre-release, run from source.** There is no packaged release yet.
The manual parametric EQ (AutoEQ preset import/export and an AutoEQ database
browser) and the first-launch setup wizard work end to end; the Measure,
Target, Analyzer and Profiles tabs are still placeholders. `docs/CONTEXT.md`
is the current account of what is built and what is left.

## How it works

ParaEQ installs no audio driver. It uses a **Core Audio process tap**
(macOS 14.4+) to capture the system mix, mutes it at the device, and plays the
corrected audio back to the physical output — which stays the system default,
so the hardware volume keys and the volume HUD keep working natively. macOS
asks once for the "System Audio Recording" permission; there is no installer,
no admin prompt and no reboot.

## Requirements

- macOS 14.4 or later (the process-tap API)
- Rust, stable channel (`rust-toolchain.toml` pins the channel and components)
- Node.js 24 — the version CI builds the frontend with
- For the measurement features: a measurement microphone or headphone coupler.
  The project is developed against a miniDSP EARS jig and a UMIK-1.

## Build and run

```bash
git clone https://github.com/yourusername/paraeq.git
cd paraeq
npm --prefix desktop/ui ci
cd desktop/ui && npm run tauri dev
```

The `tauri` npm script anchors the working directory at `desktop/`, where the
CLI finds `desktop/src-tauri/tauri.conf.json`.

Tests:

```bash
cargo test --workspace
npm --prefix desktop/ui test
```

## Layout

| Path | What it is |
|------|------------|
| `crates/` | The Rust core — DSP, the Core Audio tap engine, the measurement suite, the decision engine |
| `desktop/` | The Tauri 2 + React app (`desktop/ui` is the frontend, `desktop/src-tauri` the shell) |
| `fixtures/` | Golden numerical fixtures the DSP core is tested against |
| `targets/` | Target curves — Harman IE/OE, Diffuse Field, B&K 1974, Flat |
| `prototype/` | The retired Python/PyQt6 prototype, kept as the numerical oracle |
| `docs/` | Context, specs, plans and decision records |

## Prototype / numerical oracle

`prototype/` is not the product. It is the original Python/PyQt6 prototype,
kept because `prototype/tools/generate_fixtures.py` generates the golden
fixtures in `fixtures/` that the Rust DSP core must reproduce. Its `paraeq`
Python package is internal and is never published to PyPI.

```bash
python3 -m venv .venv
.venv/bin/pip install -e "./prototype[dev]"
.venv/bin/pytest prototype/tests -q
```

Add the `gui` extra (`"./prototype[dev,gui]"`) only if you need to run the old
PyQt6 app itself (`.venv/bin/python -m app.main`).

## License

MIT
