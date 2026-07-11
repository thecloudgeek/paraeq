# ParaEQ Rust Port — Design Spec

**Date:** 2026-07-02
**Status:** Approved (brainstormed section-by-section with project owner)
**Supersedes:** the "Phase 2: Tauri + Rust (Future)" sketch in `docs/specs/2026-04-22-paraeq-design.md`

## Summary

Port ParaEQ from the Python/PyQt6 prototype to its final form: a Rust Cargo workspace with a Tauri 2 + React desktop app, macOS-only, built on **Core Audio process taps** instead of BlackHole or a custom HAL driver. The milestone is **full functionality parity** with the prototype plus two fixes the prototype could not deliver: native hardware volume keys and an explicit output-device picker. The visual redesign comes after parity, on top of a working app.

The Python prototype is retained in-repo as the **numerical oracle**: a fixture-generation script dumps input→output pairs for every DSP stage, and the Rust DSP core is TDD'd against those golden fixtures.

## Decisions Log

Settled with the project owner during brainstorming on 2026-07-02:

| Decision | Choice | Alternatives rejected |
|---|---|---|
| UI stack | Tauri 2 + React (TypeScript, Vite) | Tauri+Svelte/Solid; egui (design ceiling too low); Slint/iced (immature charting/tray) |
| Audio acquisition | Core Audio process taps (macOS 14.4+) | Custom libASPL HAL driver (initially chosen, reversed on research findings — see below); BlackHole+aggregate port (kept as designed fallback) |
| Platform scope | macOS-only, OS-specific code isolated in one crate | Windows/Linux in this port |
| Python fate | Reference implementation + golden-fixture oracle; no PyO3 bindings | pip-installable wrapper via PyO3; deleting Python now |
| Repo layout | Same repo; Python moves to `prototype/`; Rust workspace takes top level | `rust/` subdirectory; fresh repo |
| Parity scope | Everything the prototype does + volume keys + output picker | Core-EQ-first subset; feature-set rethink |
| Process topology | Multi-crate workspace, engine in-process with Tauri, **daemon-ready seams** (owner: "1 now, 2 later") | Headless daemon + UI client now; single-crate monolith |
| Signing | Owner has a paid Apple Developer account; Developer ID + notarization used from the start | — |

**Why taps, not a custom driver.** The driver-from-day-one decision was reversed after ecosystem research (2026-07-02): `AudioHardwareCreateProcessTap` (macOS 14.4+) captures the system mix, mutes it at the device, and lets the app play processed audio to the physical output — which **stays the system default**, so hardware volume keys and the volume HUD work natively with zero extra code. No driver install, no admin prompt, no reboot (macOS 14.4 removed installers' ability to restart `coreaudiod`), single notarized .app, one "System Audio Recording" TCC prompt. Validation: Rogue Amoeba deprecated their ACE driver engine for taps (Audio Hijack 4.5.9, May 2026, supports macOS 14.4–26); iQualize ships as an open-source tap-based system EQ; Rust bindings exist in `objc2-core-audio`. The custom-driver path additionally turned out to have no Rust ecosystem (it would be a separate C++ codebase via libASPL), and eqMac's issue tracker documents years of driver-exposed-volume fragility. Taps deliver the driver's entire promised UX at a fraction of the effort.

## Architecture

```
Every app on the Mac
      │  plays audio normally to the system default output
      │  (the REAL headphone device — no virtual device exists,
      │   so volume keys / HUD keep working natively)
      ▼
Core Audio process tap  (macOS 14.4+, mutedWhenTapped)
      │  macOS hands ParaEQ the full system mix, muted at the device.
      │  The tap EXCLUDES ParaEQ's own process (prevents feedback,
      │  and lets measurement sweeps play cleanly).
      ▼
┌─ ParaEQ.app — one Tauri process, Cargo workspace ─────────────┐
│                                                               │
│  crates/paraeq-coreaudio   all unsafe CoreAudio FFI:          │
│                            tap lifecycle, device enumeration,  │
│                            default-output watcher, output      │
│                            stream, volume scalar. The ONLY     │
│                            macOS-specific crate.               │
│                                                               │
│  crates/paraeq-engine      realtime graph: tap source → ring  │
│                            → FIR convolver / IIR cascade →    │
│                            gain → output sink. Lock-free       │
│                            param swaps. Zero Tauri deps —      │
│                            extractable into a daemon later.    │
│                                                               │
│  crates/paraeq-dsp         pure math, zero platform deps:     │
│                            FFT, biquads, FIR + min-phase,      │
│                            splines, smoothing, sweep/deconv.   │
│                            Golden-fixture-tested vs Python.    │
│                                                               │
│  desktop/src-tauri         Tauri backend: commands, state,    │
│                            AutoEQ client, profile store,       │
│                            measurement runner                  │
│  desktop/ui                React + TS webview: tabs, tray     │
│                            menus, wizards, canvas plots        │
└───────────────────────────────────────────────────────────────┘
      ▼
Physical output device (headphones / DAC) — stays system default
```

Constraints that define the shape:

- **`paraeq-dsp` has zero platform dependencies** (successor to the prototype's `paraeq/`-must-stay-GUI-free rule). `paraeq-engine` depends on `paraeq-dsp` only. `paraeq-coreaudio` is the single home of unsafe FFI.
- **Daemon-ready seams**: `paraeq-engine` exposes a narrow control API (commands in, serializable state snapshots out) and never imports Tauri. Extracting it into a headless `paraeqd` later (owner-requested evolution) means moving the crate behind a socket, not a rewrite.
- **Acquisition behind a trait**: the engine consumes an `AudioSource` trait. The parity implementation is the process tap; a BlackHole+aggregate source (the prototype's proven architecture, fully documented in `docs/CONTEXT.md`) is the designed fallback if taps fail the spike. Everything downstream — DSP, engine graph, IPC, UI — is identical in both worlds.

## Realtime Audio Pipeline

**Threads and data flow:**

- **Tap IOProc** (CoreAudio realtime thread): receives system-mix blocks from the muted tap; pushes into a lock-free SPSC ring buffer.
- **Output IOProc** (CoreAudio realtime thread): pulls a block from the ring, runs the DSP chain, writes to the physical device. Chain per block: `bypass? → correction (FIR convolver | biquad cascade) → trim gain → safety clamp`. The safety clamp is a hard limit to ±1.0 full scale — a last-resort protection against filter overshoot beyond the preamp headroom, not a mastering limiter (the AutoEQ preamp convention remains the real headroom mechanism, as in the prototype).
- **Analyzer feed**: pre- and post-EQ copies pushed to a second ring; consumed off-thread; frames are dropped when the ring is full — the UI can never backpressure audio.
- **Control plane** (normal threads): engine controller owns tap/stream lifecycle and applies commands; a config builder designs FIR/biquad coefficients via `paraeq-dsp` off-thread and hands ready-to-run processors to an atomic pointer swap (`arc-swap`); retired configs are freed off-thread. **No locks, no allocation, ever, on the realtime lane.**

**Key decisions:**

- **One clock domain.** The tap captures the system default output device, and ParaEQ plays to that same device — both IOProcs run on one hardware clock, so there is no drift and no resampler at parity. The **output picker works by setting the system default output** (tap and output stream follow it), not by cross-routing to a second device. Cross-device redirect (à la SoundSource) is a post-parity feature that would add rubato rate-matching.
- **Latency budget:** tap block + ring + output block ≈ 20–30 ms added at 512-frame buffers / 48 kHz. Fine for music; under the ~45 ms lip-sync threshold. Block size becomes a setting post-parity.
- **Sample formats:** samples stay f32 (CoreAudio native); filter *design* math and biquad *state* are f64 (numerical parity with numpy/scipy; avoids low-frequency biquad quantization).
- **Two bypass levels.** *DSP bypass*: audio still flows through ParaEQ, correction skipped — instant glitch-free A/B (tray toggle, prototype parity). *Full disable*: destroy the tap, device unmutes, system exactly as if ParaEQ never ran — used on quit and as the panic path.
- **Rebuild on change.** Property listeners on `kAudioHardwarePropertyDefaultOutputDevice`, device-alive, and stream format: any change tears down and rebuilds tap + output stream (~100 ms mute blip). Covers unplug, AirPods handoff, sample-rate switches — and fixes the prototype's launch-time output lock-in gotcha.
- **Fail-safe ordering.** Every exit path — command, drop, panic — runs the full teardown sequence ending in tap destruction (stop IOProc → destroy IOProc → destroy aggregate → destroy tap, the spike-validated order): the system must never be left muted. TCC permission revoked mid-run → full disable + UI prompt.

## DSP Core and Numerical Parity

**Port map** (prototype module → Rust home):

| Prototype | Rust | How |
|---|---|---|
| `measurement/sweep.py` | `dsp::sweep` | hand-roll (closed-form log sweep + Farina inverse) |
| `measurement/deconvolution.py` | `dsp::deconvolution` | realfft + ~20 lines Wiener division `conj(S)/(|S|²+ε)`. Gotcha: rustfft does not normalize inverse FFTs — explicit 1/n |
| `measurement/frequency_response.py` | `dsp::fr` | realfft + hand-rolled fractional-octave smoothing (~25 lines); averaging stays in dB domain |
| `measurement/compensation.py` | `dsp::compensation` | CSV parse + linear interp (~10 lines) |
| `correction/target_curves.py` | `dsp::targets` | hand-rolled **not-a-knot cubic spline** in log-f space (~120 lines; no trustworthy crate); anchor deviation-layer model + `match_closest` ported 1:1; same six bundled CSVs |
| `correction/fir_filter.py` | `dsp::fir` | hand-rolled frequency-sampling design + **homomorphic minimum-phase** (~120 lines, ported from scipy's algorithm including the M²/half-magnitude trick documented in CONTEXT.md) |
| `correction/biquad.py` | `dsp::biquad` | hand-rolled RBJ Audio EQ Cookbook coefficients (~100 lines, exact parity, zero deps) + `sosfreqz` equivalent (~15 lines). The `biquad` crate is used *in tests only* as an independent cross-check |
| `correction/parametric_eq.py` | `dsp::peq` | EQBand / ParametricEQ / composite response / AutoEQ text export |
| `correction/auto_fit.py` | `dsp::autofit` | greedy peak-picking, direct port |
| `engine/convolver.py` | `engine::convolver` | hand-rolled overlap-add mirroring Python block-for-block; per-channel FIRs (the mono-FIR IndexError from CONTEXT.md becomes a regression test). `fft-convolver` partitioned upgrade later if CPU/latency demands |
| `engine/iir_processor.py` | `engine::iir` | DF2T cascade, per-channel f64 state carried across blocks (scipy `sosfilt` + `zi` semantics) |
| `audio/*`, `audio/aggregate.py` | `paraeq-coreaudio` | replaced by tap architecture; aggregate knowledge retained in CONTEXT.md as the fallback path |
| `correction/autoeq_db.py`, `profiles/` | `desktop/src-tauri` | networking and persistence live in the Tauri backend, not the DSP crate |

**Dependencies:** `rustfft`/`realfft` (FFT, auto-NEON on Apple Silicon), `hound` (WAV), `serde`, `thiserror`, `arc-swap`, a lock-free SPSC ring (e.g. `rtrb`). Deliberately **no ndarray** (all-1D workload; plain `Vec<f64>` avoids the 0.15/0.16/0.17 ecosystem split) and **no scipy-in-Rust mega-crates** (`scirs2` audited: ~2.9M LOC by 2 contributors, AI-generated-bulk risk profile — not for a realtime audio product's dependency tree).

**Golden-fixture parity pipeline** — the Python is the oracle:

1. **`prototype/tools/generate_fixtures.py`** (new): run against the pinned Python env; dumps input→output pairs for every DSP stage into `fixtures/` (JSON + WAV). Coverage: sweep samples; synthetic recording → deconvolved FR; smoothing at 1/3, 1/6, 1/12 octave; spline evaluations on dense log grids including extrapolation/clamp edges; biquad coefficient matrix (type × f0 × Q × gain); multi-block `sosfilt` streaming outputs (verifies state carry across block boundaries); FIR designs linear + minimum-phase at several tap counts (including odd `n_proto`); convolver block-by-block outputs; anchor deviation targets; autofit results.
2. **`fixtures/` is committed** to the repo.
3. **`cargo test` in `paraeq-dsp`** consumes fixtures via the `approx` crate. TDD: fixture tests are written failing-first, then the module is ported until green.

**Tolerances by class:** closed-form coefficient math ≤ 1e-12; single-FFT paths ~1e-9 relative (FFT summation-order differences); the minimum-phase chain ~1e-6 on taps **and** magnitude response within 0.01 dB over 20 Hz–20 kHz — the perceptually true criterion.

**Property-based tests** (`proptest`) for oracle-free invariants: OLA output equals direct convolution; splines pass through anchors; designed filters are stable (poles inside the unit circle); smoothing preserves flat spectra.

**Deliberate-divergence log:** a short doc listing every place the Rust intentionally differs from the Python (normalization points, epsilon floors) so a red parity test is always actionable.

## UI, IPC, and Feature Surfaces

**The contract: Rust owns all state; the UI renders it.**

- **Commands (request/response):** typed Tauri `invoke` handlers grouped by domain — engine (bypass, gain, output pick), eq (bands, preamp, import/export), targets (select, anchors, generate FIR/PEQ), measure (list devices, run sweep), profiles (CRUD, activate), autoeq (search, fetch preset).
- **State snapshots (event):** one serialized `AppState` pushed on every change — engine status, active profile, EQ bands, routing, permission state. React holds no forked state beyond ephemeral drag interactions.
- **Spectrum stream (channel):** binary Tauri v2 Channel, ~30 fps pre/post FFT frames (~8 KB each — comfortably inside measured IPC budgets), drawn in a canvas rAF loop outside the React render cycle. Measurement progress uses the same mechanism.
- **One shared `FrequencyPlot` canvas component** (log-x, dB-y, drag interactions) powers the EQ curve, target editor, analyzer, and measurement views. The exact implementation (hand-rolled canvas vs uPlot-assisted) is an implementation-plan detail.
- **Styling at parity:** Tailwind + shadcn/ui tokens — deliberately plain, but a real component system the post-parity modern-design pass can restyle wholesale without rewiring.

**Parity surfaces:**

| Surface | Behavior |
|---|---|
| Setup wizard | First launch: explain → trigger the one-time System Audio Recording permission → confirm output device → done. The BlackHole install step is deleted entirely. |
| EQ tab | Band table + draggable curve + preamp; AutoEQ text import/export; Browse AutoEQ DB searchable dialog. Same single-processor contract as the prototype: last-applied correction (PEQ or FIR) is what runs. |
| Target tab | Preset dropdown (same six bundled curves), CSV import/export, 16 draggable anchors with the deviation-layer model, "Match closest", Generate FIR / Generate PEQ, ~70 ms debounced live minimum-phase-FIR preview. |
| Measure tab | EARS input picker + sweep params + multi-measurement averaging, FR plot with compensation. Tap-era simplification: the global tap excludes ParaEQ's own process, so the sweep plays cleanly on a direct output stream while the EARS records — correction state cannot contaminate the measurement. Play/record run on separate devices; the Farina method tolerates their small clock skew (prototype proved it). |
| Analyzer tab | Live pre/post overlay, octave-smoothing dropdown, dB range — canvas fed by the spectrum channel. |
| Profiles tab | List / activate / duplicate / rename / delete / import / export. New clean storage format (serde JSON + WAV impulses via hound) in the app-data dir. No compatibility with prototype profiles (prototype is throwaway; re-measure or re-import). |
| Menubar / tray | Profile switching, bypass toggle, open window, quit. Closing the window keeps audio running (tray-resident app). |
| AutoEQ client (backend) | Rust port of the prototype's hardening (INDEX.md as index, both preset-filename casings, percent-encoding, cache-first under app-data) plus research-driven upgrades: jsDelivr CDN primary with raw-GitHub fallback, configurable base URL, pinned-commit option — unauthenticated raw.githubusercontent gets rate-limited and upstream has gone dormant for ~12 months before. Rig folder names are parsed from paths, never assumed to be a fixed enum. |

## Repo Migration

One-time restructure commit (git history preserved via `git mv`):

```
paraeq/
├── Cargo.toml            # workspace root
├── crates/
│   ├── paraeq-dsp/
│   ├── paraeq-coreaudio/
│   └── paraeq-engine/
├── desktop/              # Tauri app
│   ├── src-tauri/        # Rust backend
│   └── ui/               # React + TS + Vite
├── targets/              # curve CSVs (shared with prototype)
├── fixtures/             # Python-generated golden fixtures
├── prototype/            # ← paraeq/, app/, tests/, pyproject.toml move here
│   └── tools/generate_fixtures.py   # new: the oracle dump script
└── docs/                 # CONTEXT.md, specs, plans
```

`CLAUDE.md` is rewritten for the new world (cargo/npm commands primary; prototype venv commands kept for regenerating fixtures). The prototype's `targets/` path reference gets a one-time fixup.

## Testing Strategy

- **paraeq-dsp:** golden-fixture tests (TDD, failing-first) + proptest invariants — the heart of correctness.
- **paraeq-engine:** unit tests on synthetic blocks — block-boundary state carry, atomic swap under churn, bypass semantics, the stereo-FIR regression.
- **paraeq-coreaudio:** hardware-dependent integration tests behind `#[ignore]`, run locally (CI has no audio devices).
- **UI:** TypeScript strict + vitest for pure logic; visual surfaces manual-smoke (same policy as the prototype).
- **CI:** GitHub Actions macOS runner — `cargo test` + `cargo clippy` + UI typecheck/build on every push.

## Packaging and Distribution

No driver, no installer, no admin rights: `tauri build` produces a signed + notarized **ParaEQ.app in a DMG** using the owner's Developer ID (Tauri automates signing/notarization via environment credentials). `Info.plist` carries `NSAudioCaptureUsageDescription`; **minimum system version macOS 14.4**. Distribution: GitHub Releases at parity; Homebrew cask and auto-updater post-parity. This deletes three items from the old Phase-1 backlog (installer pipeline, PyInstaller bundling, BlackHole bundling/licensing).

## Port Order

Each stage gets its own implementation plan (writing-plans → TDD execution). Stages:

0. **Tap spike** *(go/no-go gate)* — standalone Rust binary: global tap → hardcoded biquad → output on the default device. Measures added latency, CPU, device-switch behavior, sample-rate-change behavior. Failure ⇒ swap `AudioSource` to the BlackHole+aggregate fallback before anything depends on taps.
1. **Repo restructure + scaffold** — the tree above; workspace compiles; CI green; fixtures generated and committed.
2. **paraeq-dsp** — TDD against fixtures: biquad → FIR/min-phase → targets/splines → sweep/deconvolution/FR → PEQ/autofit.
3. **paraeq-coreaudio + paraeq-engine** — production tap engine, IIR/FIR processors, controller, state snapshots, rebuild-on-change, fail-safe teardown.
4. **Tauri shell + EQ tab** — window/tray/state plumbing, FrequencyPlot component, manual EQ end-to-end: first audible corrected audio in the real app.
5. **Target editor + profiles + AutoEQ browser** — anchors, live preview, profile store, DB client.
6. **Analyzer + measurement wizard** — spectrum channel; EARS sweep flow last (needs the owner's hardware in the loop).
7. **Ship** — signed DMG, README/docs, r/audiophile-ready release. Then: the modern-design pass on a working app.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Tap API youth (macOS 14.4+; one zero-stream report on a macOS 26.5 *beta*) | Stage-0 spike is a hard gate; `AudioSource` trait keeps the proven BlackHole+aggregate fallback a contained swap; track macOS 26.x point releases before raising the supported-OS ceiling |
| Measurement play/record clock skew (two devices, two clocks) | Farina sweep deconvolution tolerates small skew — empirically proven by the prototype with the same topology |
| IPC throughput for the analyzer | ~8 KB × 30 fps is far inside Tauri v2 Channel budgets; escape hatch is a custom URI-scheme protocol handler serving binary |
| Hand-rolled DSP correctness | Golden fixtures + per-class tolerances + proptest invariants + test-only cross-checks against independent crates |
| eqMac-class lifecycle bugs (hot-plug, sleep/wake, device switch) | Rebuild-on-change listeners are designed in from the start; fail-safe teardown ordering (unmute first) is a stated invariant; these become explicit engine test scenarios |

## Out of Scope (this port)

- Windows/Linux support (clean seams only: `paraeq-coreaudio` is the single platform crate).
- Cross-device redirect / per-app EQ / output groups (SoundSource-class features).
- Custom HAL driver (contingency only).
- PyO3 bindings / pip package.
- The modern-design UI overhaul (explicitly sequenced after parity).
- Auto-updater, Homebrew cask (post-parity distribution work).
- Parameterized targets (baseline + tilt/bass/treble preference adjustments) and newer 5128-era target curves (JM-1, IEF 2025) — noted from research as the direction the measurement community has moved; post-parity backlog.

## Research Basis (2026-07-02)

Five-agent ecosystem research informed this design; key verified facts: Tauri 2.10/CLI 2.11 with first-class tray/menubar and automated notarization; cpal 0.17 still lacks duplex/channel-maps/device-notifications (unsuitable); `objc2-core-audio` 0.3.x exposes the tap and HAL APIs; `coreaudio-rs` 0.14 wraps AUHAL; mozilla `cubeb-coreaudio-rs` (ISC) is the reference for aggregate-device patterns if the fallback is needed; rustfft 6.4/realfft 3.5 auto-NEON; the `biquad` crate is RBJ-cookbook-based (test cross-check); no crate exists for scipy-equivalent minimum-phase or not-a-knot splines (hand-roll ~250 lines total); BlackHole binaries cannot be bundled with a non-GPL app; Audio Hijack 4.5.9 and iQualize ship on process taps; AutoEq's repo layout is unchanged since 2022 but dormant since 2025-07 (mirror + pin accordingly).
