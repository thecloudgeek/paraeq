# ParaEQ Project Context

This document captures the why, the goals, what's been built, and what's left. It exists so future sessions (Claude or human) can pick up the work without rebuilding context from scratch.

**Last updated:** 2026-07-02 (Rust port foundation)

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

Phase 2 has begun — see docs/specs/2026-07-02-rust-port-design.md (process-tap architecture supersedes BlackHole+aggregate; the aggregate knowledge below is retained as the designed fallback).

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
| `audio/stream.py` | play / record / AudioPassThrough (real-time duplex with CoreAudio channel maps + software master gain + pre/post callbacks for the spectrum analyzer) |
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

**Testing:** 128 tests, all passing, ~1.7s runtime. Includes 3 end-to-end integration tests covering measurement → FIR pipeline, measurement → PEQ pipeline, and profile save/load round-trip; 8 tests covering the metadata-headered target curve loader; 6 tests for the interactive-editor library functions (`build_anchor_target` deviation model + `match_closest_target`); and 10 tests for the AutoEq database module (`parse_index`, `parse_parametric_eq` with preamp + shelf aliases, and `AutoEQClient` cache-hit/miss + both filename casings, all with networking mocked via a patched `_http_get`). GUI layers (`app/`) are manual-smoke only (no display in CI).

### Notable Implementation Decisions

- **Deconvolution uses Wiener spectral division** (`conj(S) / (|S|² + ε)`), not time-domain convolution with the inverse sweep. Mathematically equivalent to Farina, but numerically far more stable. This means `generate_inverse_sweep` from sweep.py is currently unused by the production deconvolution path — kept for now in case scripts want it.
- **Minimum-phase FIR design**: `scipy.signal.minimum_phase(method="homomorphic")` takes the square root of the magnitude in cepstral domain. To produce a filter with desired magnitude M, you must build the linear-phase prototype with magnitude M² so that sqrt(M²) = M after conversion.
- **Averaging in dB domain**, not linear, per test expectation: `(0 dB + 6 dB) / 2 = 3 dB`.
- **BlackHole is detected by name match** (`"blackhole" in device.name.lower()`). Bundling/install of the BlackHole HAL plugin into the macOS installer pkg is **not yet implemented** — currently relies on user having BlackHole pre-installed.
- **Real-time routing is one Aggregate Device used in duplex** (this is the hardest-won, most port-critical piece — read carefully before the Rust port). The user creates a macOS Aggregate Device that lists **BlackHole 16ch FIRST** (16 ch) and the physical headphone output **SECOND** (2 ch), giving 18 output / ≥16 input channels. macOS plays system audio to aggregate output ch 1-2 (= BlackHole 1-2); BlackHole is a loopback, so output ch *N* reappears on input ch *N*. ParaEQ therefore opens the aggregate as **both input and output** (`input_device == output_device`), reads input ch 0-1, applies EQ, and writes to aggregate output ch 17-18 (= the headphones) via CoreAudio channel maps: input `[0, 1]`, output `[-1]*16 + [0, 1]`. The `-1` is the CoreAudio "route this stream channel nowhere" sentinel; the output map length must equal the device's output-channel count; and the leading `16` *is* BlackHole 16ch's channel count (a BlackHole 2ch aggregate would need `[-1, -1, 0, 1]`). **BlackHole's position in the aggregate is load-bearing** — reorder it and the channel offsets break. (`paraeq/audio/stream.py` → `AudioPassThrough(... input_channel_map, output_channel_map)`; wired in `app/main_window.py` → `start_audio_engine`.)
- **AudioPassThrough provides a software master volume** (`set_gain`, square-law taper in the GUI). This is **required, not a convenience**: BlackHole exposes no system volume, so the macOS volume keys do nothing once audio is routed through the aggregate — the engine's gain is the only loudness control. **Three gain stages multiply** while routed, and only one is user-visible: (1) BlackHole's own device volume scales every sample in the loopback — `setup_routing` forces it to unity (`set_device_volume`) because a slider once dragged down in Audio MIDI Setup otherwise silently attenuates the whole system; (2) the ParaEQ slider (engine gain, the intended control); (3) the physical output's hardware volume, frozen at whatever it was before launch since the volume keys can no longer reach it. **Rust-port requirement:** hardware volume keys must work — either a media-key event tap driving the engine gain, or an eqMac-style custom HAL driver that exposes a controllable volume. The ParaEQ-slider-only prototype UX is a real usability complaint, not a nice-to-have.
- **The aggregate adopts whatever output is the system default at launch** (`setup_routing` → `default_output()`). If the speakers were default when the app started, headphones plugged in later (or simply not selected) are never wired into the chain — audio silently goes to the wrong device. Prototype rule: select the intended output in System Settings *before* launching ParaEQ. The Rust port should offer an explicit output picker and/or rebuild the aggregate when the default output changes (Core Audio publishes `kAudioHardwarePropertyDefaultOutputDevice` change notifications).
- **Target editor uses a deviation-layer model** (`build_anchor_target` in `paraeq/correction/target_curves.py`): 16 fixed log-spaced anchors carry dB *offsets* added on top of the selected preset, sampled on the union of the base's frequencies and the anchor frequencies. This preserves a dense preset's fine detail (e.g. the Harman ear-gain peak) while letting the user tweak it. Anchors are vertical-drag-only, so anchor frequencies stay sorted/distinct and CubicSpline never sees degenerate input. The pyqtgraph overlay (`app/editor/draggable_anchors.py`) works in **log10(Hz)** coordinates because the plot is `setLogMode(x=True)` — items added to a log-mode ViewBox live in log space.
- **Live target preview swaps a minimum-phase FIR** into the single `AudioEngine.set_processor` slot, debounced ~70 ms (`MainWindow._on_target_preview`). The convolver is built with one FIR **per channel** (`OverlapAddConvolver([fir, fir])`) — a mono convolver only allocates one overlap buffer and would `IndexError` on channel 1 inside the realtime callback. The slot is shared with the manual-EQ IIR path, so the previewed FIR **persists as the active correction** until something else sets the processor (there is no auto-restore of the prior manual EQ — known limitation, acceptable for "drag to taste"). The Rust port should keep the same single-processor contract.
- **AutoEq browser uses `results/INDEX.md` as the index** (not `webapp/data/*.json`), because the markdown gives each model's exact relative folder path — sidestepping AutoEq's inconsistent `results/` folder layout (full tree vs recommended-list use different depths). Preset filenames vary in casing (`ParametricEq.txt` vs `ParametricEQ.txt`); the client tries both and percent-encodes spaces. Index + presets are cached under the platformdirs data dir (`autoeq_cache/`); only first access hits the network. Networking is stdlib `urllib` behind a patchable `_http_get`, run on a `QThread` worker in the dialog. (`paraeq/correction/autoeq_db.py`, `app/eq/autoeq_browser.py`.)

### Recently Completed Cycles

- **2026-04-27 — Cycle 1: Refined target curve data.** Replaced the four hand-typed approximate target CSVs with high-resolution data sourced from AutoEQ at commit `7ae0f56`. Expanded built-in set to six curves (added `harman_ie_2019_without_bass`, `harman_oe_2018_without_bass`). Extended `TargetCurve` and the CSV loader with optional metadata fields (`category`, `description`, `source`); old-style CSVs without metadata still load. See `docs/specs/2026-04-26-refined-target-curves-design.md` and `docs/plans/2026-04-26-refined-target-curves.md`.

## What's Left

### Near-term (still Phase 1 polish)

These items were specified in the design but not in the executed implementation plan:

- **Installer pipeline**: DMG + pkg installer that bundles the BlackHole HAL plugin and installs ParaEQ.app to /Applications/. Currently the app only runs from source via `python -m app.main`.
- **PyInstaller bundling**: `ParaEQ.app` build for distribution.
- **Homebrew cask**: `brew install --cask paraeq`.
- ~~**Aggregate device creation**~~ ✅ **Done.** `paraeq/audio/aggregate.py` programmatically creates the BlackHole + physical-output Aggregate Device via the Core Audio HAL (`AudioHardwareCreateAggregateDevice` through PyObjC), sets it as the system default on launch, and restores the real output + destroys the aggregate on quit. Fully automatic — no Audio MIDI Setup needed. (The Rust port calls the identical Core Audio API.)
- **Launch-at-login**: launchd plist for auto-start.
- ~~**Interactive draggable control points**~~ ✅ **Done.** The target editor overlays 16 fixed log-spaced, vertically-draggable anchors that apply a smooth dB *deviation* on top of the selected preset (`build_anchor_target`), with a debounced live minimum-phase-FIR preview to the audio engine and a "Match closest" auto-match button. See `docs/specs/2026-06-17-target-editor-autoeq-design.md` and `docs/plans/2026-06-18-target-editor-autoeq.md`.
- ~~**AutoEQ database integration**~~ ✅ **Done.** The EQ tab's "Browse AutoEQ DB…" button opens a searchable model picker backed by `paraeq/correction/autoeq_db.py` (cache-first index sync + lazy per-model preset fetch from the AutoEq GitHub repo). Single-file import still works too.

### Phase 2 (in progress)

In progress. Spec: docs/specs/2026-07-02-rust-port-design.md. Foundation plan: docs/plans/2026-07-02-rust-port-foundation.md. Repo restructured (Python → prototype/), fixtures committed, workspace + desktop scaffold + CI live. Tap spike findings: docs/spikes/2026-07-tap-spike.md. Stage 2 (DSP core) complete (merged 2026-07-06): all pure-DSP modules golden-matched in crates/paraeq-dsp + engine processors in crates/paraeq-engine (see crates/paraeq-dsp/DIVERGENCES.md; fixture oracle pin = scipy 1.18.0 per fixtures/manifest.json). Stage 3 (production tap engine) complete (2026-07-11): `TapBackend` in crates/paraeq-coreaudio behind the engine's `AudioBackend` seam; `EngineController` with a command API + serialized `EngineState` snapshots (arc-swap + subscribe channel); rebuild-on-change driven by CoreAudio property listeners; fail-safe teardown (stop IOProc → destroy aggregate → destroy tap) on every exit path, including catch_unwind+abort in the FFI trampolines. Plan: docs/plans/2026-07-06-rust-port-tap-engine.md — all five stage-2 review carry-forwards landed there too (unified processor output contracts, shelf/notch stability proptests, dsp input-wiring validation, silence watchdog, and the spike's unsafe patterns are now enforced-against in code, not just documented). The engine now fails open (2026-07-12): `NoInputDetected` persisting past `fail_open_after_ms` (default 15 s) triggers the same path as Disable — tap destroyed, un-EQ'd audio restored — publishing `AutoDisabledNoInput`, with retries only via an explicit `Enable` (post-`Running` states `Idle`/`InputSilent` never fail open — pausing music must never disable the EQ). Next: stage 4 — Tauri shell + EQ tab.

**Stage-3 findings and open items (2026-07-11):**
- Watchdog semantics: `Running` is gated on the first nonzero input block, never on start returning; ~5 s engage tolerance, then `NoInputDetected` — the "check System Audio Recording permission or play some audio" hint, because TCC silent-failure is indistinguishable from silence. `InputSilent` and `Idle` are informational states, never failures.
- **Hardware finding:** the tap aggregate's IOProc only cycles while the system renders audio — 0 cb/s idle, ~94 cb/s during playback. A frozen callback counter is normal idling (status `Idle`), NOT engine death; rebuilds trigger only on device events (default-output change, device death, format change).
- Latency (measured, release build, 48 kHz BuiltInSpeakerDevice): 512 frames = 62.3 ms, 256 = 51.6 ms, 128 = 46.3 ms — a ~41 ms fixed tap-path floor. The spec's original 20–30 ms budget (since revised to the measured numbers) is NOT reachable by buffer sizing alone; the measured number is exposed honestly in `EngineState.latency_ms` (SoundSource-style, per the spike obligation). CPU is negligible (IIR ≈0%; 4096-tap min-phase FIR 0.2–0.4%).
- Dev-loop caveat: unsigned CLI binaries never get the TCC prompt — grant manually (System Settings → Privacy & Security → Screen & System Audio Recording → add the terminal, then fully relaunch it). The dev terminal used for this stage currently runs in silent-zeros mode.
- **Outstanding manual owner checklist** (needs a TCC-granted, ears-on session): music audibly EQ'd via `cargo run -p paraeq-coreaudio --example tap_engine`; native volume keys + HUD while processing; `--bypass-every` A/B is clean; Ctrl-C restores audio; `kill -9` restores audio; EQ'd audio stays correct (no mic bleed into the tap input) with a mic-capable default output (AirPods / USB headset) — `TapBackend`'s input-stream identification assumes the tap buffers come first (KNOWN LIMITATION doc in `backend.rs`).
- **Stage-4 carry-forward:** on a sample-rate change the engine republishes state but cannot redesign SOS/FIR coefficients — stage 4 must re-send corrections on rate change. Also: the setup wizard should verify capture with a deterministic probe — play a chime via a helper child process (ParaEQ's own audio is excluded from its tap by design; the hardware tests' `afplay` helper in `crates/paraeq-coreaudio/tests/test_hardware.rs` is the reference) instead of waiting on ambient audio, so `NoInputDetected`/fail-open cleanly distinguishes a missing TCC grant from "nothing playing".

## How to Pick Up the Work

1. Read this document, `CLAUDE.md`, the Rust-port spec at `docs/specs/2026-07-02-rust-port-design.md`, and (for Phase-1 history) the original design at `docs/specs/2026-04-22-paraeq-design.md`.
2. Continue the Rust port: next stage per the spec's port order (stage 4: Tauri shell + EQ tab — stage-4 carry-forward noted above).
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
