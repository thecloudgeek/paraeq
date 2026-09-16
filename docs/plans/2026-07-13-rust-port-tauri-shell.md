# Tauri Shell + EQ Tab (Stage 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the Tauri 2 desktop shell around the stage-3 engine and the manual-EQ tab end-to-end: engine spawn/teardown wired into the app lifecycle, Rust-owned `AppState` over one snapshot event, tray-resident window behavior, the setup wizard with a deterministic chime probe, output-device picker, hand-rolled `FrequencyPlot` canvas with draggable band handles, AutoEQ text import/export plus the searchable AutoEQ-DB browse dialog, minimal profile store with tray switching — **first audible corrected audio in the real app** (spec `docs/specs/2026-07-02-rust-port-design.md:196`).

**Architecture:** Per spec:132-138 ("Rust owns all state; the UI renders it"), spec:145 (EQ tab parity row), spec:150 (tray), spec:144 + `docs/CONTEXT.md:115` (wizard + chime probe + rate-change re-send carry-forward). Owner-approved scope decisions (2026-07-13) and this plan's deliberate design decisions, numbered:

1. **Launch flow: wizard-gated, then remember.** First launch: the engine spawns DISABLED; the setup wizard explains, then the user clicks "Enable EQ" which sends `Enable` — the TCC prompt fires at an understood moment. Subsequent launches restore the persisted enabled/disabled state (a session that ended in fail-open/`Failed` still counts as "user wanted it on"). Requires an engine change: `EngineConfig.enabled: bool` — spawn honors it instead of hardcoding `enabled: true` (controller.rs:226). (Task 1, Task 7, Task 17)
2. **Gain UX: preamp only — the prototype's master-volume slider is deleted.** Hardware volume keys work natively under taps (spec:28), so the EQ tab gets a single preamp control (dB, −30..+10, step 0.5) wired to `EngineCommand::SetGainDb`. AutoEQ import (file AND DB browser) applies the parsed preamp — fixing the prototype asymmetry where file-import silently dropped it. Export writes the actual preamp value instead of the prototype's hardcoded `Preamp: 0.0 dB` (deliberate, documented divergence; the parser accepts either). (Tasks 2, 3, 14, 16)
3. **FrequencyPlot: hand-rolled canvas, no uPlot.** A pure `FreqPlotRenderer` TS class (log-x transforms, fixed audio ticks 20/50/100/…/20k, dB-y, trace drawing, hit-testing — no React, vitest-tested) plus a thin `FrequencyPlot.tsx` wrapper (ref, ResizeObserver, DPR, pointer events, rAF, two stacked canvases: static traces below, drag handles/live layer above). Rationale: drag interaction is first-class and uPlot provides zero drag support; our axis range is fixed so tick/log math is ~50 trivial lines; the same component later powers target/analyzer/measure views. (Tasks 12, 13, 15)
4. **All four stage-boundary extras are IN stage 4** (owner decision): setup wizard + deterministic chime probe; output-device picker (new unsafe FFI in paraeq-coreaudio: enumerate outputs, read names/UIDs, set system default output — the spec's "output pick" command, spec:89); tray profile switching backed by a MINIMAL profile store pulled forward from stage 5; AutoEQ DB browse dialog backed by the full Rust AutoEQ client pulled forward from stage 5 (spec:151).
5. **Scope boundary restatement.** Stage 4 now owns: profile storage format (JSON: name + bands + preamp_db), "Save Profile…" in the EQ tab, tray switching, `active_profile` in `AppState`; and the AutoEQ client backend (INDEX.md index, both preset-filename casings, percent-encoding, cache-first under app-data, jsDelivr CDN primary + raw-GitHub fallback, configurable base URL, pinned-commit option) + the searchable browse dialog. **Stage 5 keeps:** the Profiles TAB (list/activate/duplicate/rename/delete/import/export UI), FIR-impulse storage (WAV via hound), and the target editor (anchors, live FIR preview, Generate FIR/PEQ). **Stage 6 keeps:** Analyzer + Measure tabs.
6. **Spectrum Channel: deferred WHOLESALE to stage 6.** spec:136 lists it in the IPC contract, but the engine has no analyzer tap-off ring (stage 3 built none) — there is nothing to type a Channel against. No placeholder typing is shipped; stage 6 adds ring + Channel together.
7. **Engine additions (one small TDD'd task, MockBackend tests):** `EngineConfig.enabled: bool` (default `true` preserves existing behavior/tests; desktop passes `false` pre-wizard); `EngineState.enabled: bool` so the UI can distinguish user-disabled from other `Stopped`; `EngineState.frame_mismatch_blocks: u64` (the `RtShared` telemetry exists but is invisible to consumers — closes the degraded-passthrough blind spot). (Task 1)
8. **Stable wire format, pinned by golden tests.** `EngineStatus` gets `#[serde(rename_all = "snake_case", tag = "kind")]` (internally-tagged → a clean TS discriminated union). Field names stay snake_case everywhere (NO `camelCase` renames); TS mirrors them. Serialization golden tests pin the JSON shape at every layer: `EngineState`/`EngineStatus` in paraeq-engine (Task 1), `EQBand` in paraeq-dsp (Task 2), and the full `AppState` envelope in src-tauri (Task 5) — so a field rename anywhere in the hand-mirrored `ipc/types.ts` chain fails a Rust test, not the UI. TS wire types are hand-written against those pinned shapes — no codegen dependency in stage 4 (ts-rs noted as a post-parity option). (Tasks 1, 2, 5, 11)
9. **serde on paraeq-dsp EQ types:** derive `Serialize`/`Deserialize` on `FilterType`/`EQBand` (+ `PartialEq` on `EQBand` for tests). Pure serde, no platform deps — respects the crate rules. (Task 2)
10. **AutoEQ text parser lives in `paraeq-dsp::peq`** (pure `&str` → `ParsedPreset`, no I/O). Semantics match the prototype oracle `prototype/paraeq/correction/autoeq_db.py::parse_parametric_eq` exactly: preamp regex with default 0.0 if absent; only `ON` lines; unknown type codes → peaking; `LS`/`HS` legacy aliases; filter number ignored; non-matching lines skipped. Fixtures come from EXTENDING `prototype/tools/generate_fixtures.py` — fixtures are sacred, never hand-written. (Task 3)
11. **Band validation at the src-tauri command layer** (pure, unit-tested module): reject fc outside (0, sample_rate/2), q ≤ 0 or > 100, |gain_db| > 30, non-finite anything; errors return to the UI as `Result<_, String>`. Rationale: dsp/engine validate nothing — `validate_correction` (controller.rs:80-99) only rejects structurally empty configs, so NaN coefficients from a degenerate band would reach the realtime path. The UI also constrains inputs, but Rust is the enforcement point. (Task 6)
12. **Composite curve is computed at the LIVE stream sample rate** (fallback 48 kHz when stopped) — deliberate divergence from the prototype's fixed 48 kHz, documented. Curve math stays in Rust (single source of truth): an `eq_response` command takes a frequency grid and returns composite + per-band dB values (`ParametricEQ::frequency_response` costs microseconds); the UI calls it on band changes. Drag frames use the same command round-trip by default; a local TS approximation is permitted ONLY if measured latency proves annoying — measure first. The log-grid generator is a tiny TS helper (no dsp crate change). (Tasks 6, 12, 14)
13. **Drag UX for the EQ curve (new — the prototype had none):** one handle per band at (fc, gain); pointer drag moves fc (x, log-mapped) and gain (y) with clamps; wheel over a handle adjusts Q in multiplicative steps; handle colors match the per-band curve palette; table and plot are two views of the same Rust-owned state and stay in sync through it. (Task 15)
14. **State ownership & IPC:** src-tauri holds `AppState` = { engine: EngineState, eq: { bands, preamp_db }, active_profile, profiles, setup_complete, devices, default_output_uid }. One `app-state` event carries the full serialized AppState on every change; a `get_app_state` command serves the initial fetch (events emitted before `listen` are lost). React holds no forked state beyond in-flight drags. Commands grouped by domain: `engine_*` (enable, disable, set_bypass, set_preamp_db, set_default_output, list_outputs), `eq_*` (set_bands, add_band, remove_band, import_autoeq, export_autoeq, response), `profiles_*` (save, activate, list), `autoeq_*` (sync_index, search, fetch_preset), `setup_*` (probe_start, probe_stop, open_privacy_settings, complete). **`AppState` carries NO "permission state" field** despite spec:135 listing one — TCC is undetectable (status.rs:4-12); `no_input_detected`/`auto_disabled_no_input` are the only honest proxies and they live in `engine.status`. (Tasks 5, 7)
15. **Rate-change re-send (the headline stage-3 carry-forward):** a dedicated forwarder thread in src-tauri owns `handle.subscribe()`; on each snapshot it emits AppState, and if `stream.sample_rate` changed since the last snapshot AND bands are non-empty, it **re-validates the bands at the NEW rate** and then redesigns the SOS and re-sends `SetCorrection`. Re-validation is not optional: bands were validated at their apply-time rate, and fc = 23 kHz is legal at 96/48 kHz but ≥ Nyquist at 44.1 kHz — exactly the NaN/Inf-coefficient case the validation wall exists for. On validation failure the forwarder sends `ClearCorrection` (flat passthrough, never NaN into the chain) and `log::warn`s. The same path covers the first-stream trigger re-applying persisted `settings.json` bands (a hand-editable file). The redesign decision is a pure function `(last_rate, snapshot, have_bands) -> Option<f64>` with unit tests. (Tasks 6, 7)
16. **Persistence: plain `std::fs` JSON** under `app.path().app_data_dir()` — `settings.json` (setup_complete, engine_enabled, active_profile, preamp_db, bands of the unsaved working set) + `profiles/<slug>.json`. No tauri-plugin-store (unneeded dependency). Atomic writes (write temp + rename). (Tasks 5, 8)
17. **File dialogs: tauri-plugin-dialog** (native open/save) for AutoEQ import/export; paths go to Rust commands that do the file I/O (no fs plugin needed). Exact plugin API verified via context7 during execution. (Tasks 10, 14)
18. **HTTP for the AutoEQ client: `reqwest`** (rustls-tls, async via Tauri's tokio runtime — async commands; verify via context7 at execution). The client is trait-seamed (`HttpFetch`) exactly like the prototype's patchable `_http_get` — no network in CI; tests run on committed sample INDEX.md/preset text. (Task 9)
19. **Chime probe:** the wizard's verify-capture step spawns `afplay /System/Library/Sounds/Glass.aiff` in a loop from a child process (pattern: `crates/paraeq-coreaudio/tests/test_hardware.rs:19-51` — child playback needs no TCC grant; ParaEQ's own process is excluded from its tap by design, so a helper process is REQUIRED), then watches `EngineState` for `running` (success) vs `no_input_detected` persisting ~10 s (< the 15 s fail-open) → TCC guidance screen with a deep link to System Settings → Privacy & Security → Screen & System Audio Recording, and a "Re-test" button. The wizard also handles `auto_disabled_no_input` (offers Enable retry). (Task 17)
20. **Tray:** Enable/Disable EQ item (text reflects `engine.enabled`), Bypass (`CheckMenuItem`, syncs with state), Profile submenu (checkmark on active; "(no profiles)" disabled item when empty), Show Window, Quit. Left-click opens the menu (macOS default). Icon: default window icon for now (template icon is post-parity polish). Tooltip "ParaEQ" / "ParaEQ (bypassed)". (Task 10)
21. **Window/dock: Regular activation policy** (dock icon stays — parity with the prototype's window+tray model; no `LSUIElement`). Close → hide + `prevent_close()`. Quit paths (tray Quit → `app.exit(0)`, Cmd-Q → `ExitRequested` not prevented) reach `RunEvent::Exit`, where src-tauri takes the `EngineHandle` out of managed state, sends `Disable`, and drops it (joins the controller thread) BEFORE process exit — the never-leave-muted invariant. **Ctrl-C of `tauri dev` (SIGINT) is deliberately in the same class as `kill -9`:** no signal handler is installed (default signal death runs no destructors, so `RunEvent::Exit` never fires), and both rely on the stage-3 design — the tap dies with the process and macOS drops the mute; documented, and on the manual checklist. (Task 10, Task 18)
22. **Single instance:** tauri-plugin-single-instance, registered FIRST (two ParaEQ instances = two taps fighting; cheap insurance). (Task 10)
23. **Latency display:** show `EngineState.latency_ms` honestly in the EQ-tab status strip (e.g. "Engine: Running · 62 ms · 48 kHz"). NO buffer-size UI control in stage 4 (spec:90 — post-parity setting). (Task 14)
24. **shadcn/ui + Tailwind v4** (spec:138): minimal-parity usage (button, dialog, input, select, table, tabs) so the post-parity design pass can restyle tokens wholesale. The shadcn CLI's Tailwind-v4 + React 19 flow is verified via context7 at execution time, not asserted here. (Task 11)
25. **vitest** as a devDependency for pure TS logic (FreqPlotRenderer math, log-grid helper); `npx vitest run` joins the frontend CI job. (Tasks 12, 18)
26. **Single-processor contract** (spec:145): the engine holds one correction slot; last-applied wins. Stage 4 only ever applies PEQ-derived IIR, but the commands are written so stage 5's FIR preview can take the same slot. (Task 7 command design; recorded in CONTEXT.md by Task 18 for the stage-5 planner)
27. **Known-limitation surfacing:** mic-capable default outputs (AirPods/USB headsets) stay on the manual smoke checklist (backend.rs:121-130 KNOWN LIMITATION); the completion checklist includes the outstanding stage-3 ears-on items (audible EQ, volume keys+HUD, bypass A/B, Ctrl-C / kill -9 recovery) — stage 4 is where they finally get exercised in-app. (Task 18)

**Explicit deferrals** (each with rationale, so no future session "discovers" them):
- **Spectrum Channel + engine analyzer ring → stage 6** — no engine tap-off exists to type against.
- **Profiles tab UI + WAV impulse storage (hound) → stage 5** — stage 4 ships only the minimal store the tray needs.
- **Target tab → stage 5** — per port order.
- **Accessory/menu-bar-only mode, template tray icon, autostart/login item, auto-updater, signing/notarization → post-parity/ship stage** — parity keeps the prototype's Regular window+tray model.
- **Mid-run TCC revocation handling (spec:94 "revoked mid-run → full disable + UI prompt") → post-parity** — TCC is undetectable (status.rs:4-12), and once `Running`, revocation is indistinguishable from paused playback: the engine degrades to `InputSilent`/`Idle`, which deliberately never fails open (pausing music must not disable EQ). An auto-disable heuristic would false-positive on every pause. What stage 4 ships honestly: the pre-start `no_input_detected` hint and the wizard's guidance panel (Task 17); the status strip renders the real engine states (Task 14). A dedicated revocation prompt needs a signal the OS does not provide — recorded here and in CONTEXT.md (Task 18) so no future session "discovers" it.
- **Buffer-size UI control → post-parity** (spec:90).
- **CSP hardening** (`app.security.csp` is `null` in the scaffold) **→ ship stage** — noted, not silently dropped.
- **ts-rs (or other TS codegen) → post-parity** — golden serialization tests are the stage-4 enforcement point.
- **Master-volume slider → deleted permanently** (decision 2) — not deferred, removed: hardware keys own volume.

**Tech Stack:** Rust stable workspace. Existing (locked): `tauri` 2.11.5 (features `["tray-icon"]`), `tauri-build` 2.6.3, `paraeq-engine`/`paraeq-dsp`/`paraeq-coreaudio` (workspace paths), `serde`/`serde_json` (workspace), `log` 0.4. Frontend existing (locked): react 19.2.7, vite 8.1.3, typescript 6.0.3, tailwindcss + @tailwindcss/vite 4.3.2, @tauri-apps/api 2.11.1, @tauri-apps/cli 2.11.4, @vitejs/plugin-react 6.0.3, oxlint 1.72.0. **New Rust deps (desktop/src-tauri):** `paraeq-coreaudio` + `paraeq-dsp` (workspace paths), `percent-encoding` 2, `reqwest` 0.12 (default-features off, `rustls-tls`), `tauri-plugin-dialog` 2, `tauri-plugin-single-instance` 2; dev-dep `tempfile` 3. **New Rust deps (crates):** `regex` 1 (paraeq-dsp, parser only — pure Rust); `serde` (paraeq-dsp, workspace). (paraeq-engine needs NO manifest change — its `serde_json` dev-dep already exists.) **New npm deps (desktop/ui):** `@tauri-apps/plugin-dialog` (guest bindings), shadcn/ui-generated components (+ its transitive `clsx`/`tailwind-merge`/etc. per current CLI), dev-dep `vitest` (current major at execution time). Forbidden deps everywhere: ndarray, scirs2-*, fundsp.

## Global Constraints

- **Crate boundaries** (spec:43-76 + CLAUDE.md): `paraeq-dsp` pure math, zero platform deps; `paraeq-engine` gets NO Tauri deps (daemon-ready) — the engine is spoken to ONLY via `EngineHandle`/`EngineCommand`; `paraeq-coreaudio` stays the ONLY crate with unsafe CoreAudio FFI (the output-device picker FFI lands there, never in src-tauri). All Tauri code lives in `desktop/src-tauri`.
- **Realtime rules:** src-tauri never touches the realtime path. No Tauri `emit` from anywhere but the forwarder thread; correction build/warm-up stays inside the engine's control plane.
- **Fixtures are sacred** (CLAUDE.md): `fixtures/` is generated ONLY by `prototype/tools/generate_fixtures.py` (deterministic, seeded). Never edit fixtures by hand; regenerate and commit script + output together. Don't add features to the prototype beyond the fixture generator.
- **Testing policy** (spec:181, CLAUDE.md:43-45): TDD for Rust — paraeq-engine changes via MockBackend, paraeq-dsp parser via oracle fixtures, src-tauri pure modules via unit tests; vitest for pure TS logic (FreqPlotRenderer). UI surfaces are TypeScript-strict typecheck + explicit manual-smoke checklists (no display in CI). Hardware-touching coreaudio tests stay `#[ignore]`.
- **CI gates** (`.github/workflows/ci.yml`): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` (covers src-tauri — it IS a workspace member), `cargo test --workspace`, frontend `npx tsc -b` + `npm run build` (+ `npx vitest run` added by Task 18), `.venv/bin/pytest prototype/tests -q`.
- Alphabetical ordering for imports, dict keys, and dep lists where order doesn't matter functionally.
- Every commit message ends with the trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` (plus the `Claude-Session:` line if the harness supplies one).
- Gates before every commit: the task's tests green; `cargo fmt --all` + `cargo clippy --workspace --all-targets -- -D warnings` for any Rust change; `npx tsc -b && npm run build` in `desktop/ui` for any frontend change.
- Execution on a feature branch/worktree: `git worktree add .worktrees/tauri-shell -b feature/rust-port-tauri-shell`. All paths relative to the worktree root.
- **Context7 discipline:** verify 3rd-party API usage with `npx ctx7@latest library <name> "<question>"` then `npx ctx7@latest docs <id> "<question>"` before writing code against it. Tasks below name the specific verifications required (shadcn CLI, tauri-plugin-dialog, reqwest-in-tauri, vitest config); do them IN the task, don't trust this plan's sketches for those specific APIs.
- **Dev TCC caveat** (CONTEXT.md:113): unsigned dev binaries may never show the TCC prompt — grant "System Audio Recording" manually to the terminal (System Settings → Privacy & Security → Screen & System Audio Recording), then fully relaunch the terminal. `npx tauri dev` runs may need the same treatment; note it in every manual-smoke section that plays audio.

## Verified API Reference

### paraeq-engine embedding surface (verified against `crates/paraeq-engine/src/`, cites are real)

- `EngineHandle::spawn<B: AudioBackend + 'static>(backend: B, config: EngineConfig) -> EngineHandle` (controller.rs:198) — spawns the `paraeq-engine-controller` thread and (today) immediately starts the backend; Task 1 makes start conditional on `config.enabled`. Panics if `config.ring_capacity < 1`.
- `TapBackend::new() -> TapBackend` (crates/paraeq-coreaudio/src/backend.rs:73-75) — trivial constructor; one instance drives at most one tap/aggregate/IOProc set.
- `EngineHandle::send(&self, cmd: EngineCommand)` — fire-and-forget, ignored after shutdown. `EngineCommand` variants (controller.rs:119-128): `ClearCorrection`, `Disable` (full disable: session stopped, tap destroyed, device unmuted, clears fail-open latch → plain `Stopped`), `Enable` (the ONLY way out of `Failed` and `AutoDisabledNoInput`), `SetBufferFrames(usize)`, `SetBypass(bool)`, `SetCorrection(CorrectionConfig)`, `SetGainDb(f32)` (unclamped — validate desktop-side), `Shutdown` (sent automatically by `EngineHandle::drop`, which also JOINS the controller thread, controller.rs:281-288).
- `EngineHandle::state(&self) -> Arc<EngineState>` — lock-free latest snapshot. `EngineHandle::subscribe(&self) -> std::sync::mpsc::Receiver<Arc<EngineState>>` — bounded(64) sync_channel, **lossy by design** (slow consumer gets drops, never unbounded queuing); published only on effective change (latency quantized 0.1 ms, peak 1e-3) and on every command; tick default 250 ms. Consumer must be a dedicated thread doing `recv_timeout` (the tap_engine example at crates/paraeq-coreaudio/examples/tap_engine.rs:225-240 is the reference).
- `EngineState` fields (controller.rs:130-143; `Clone, Debug, PartialEq, Serialize`): `bypass: bool`, `correction: Option<String>` (descriptor, e.g. `"iir:5-band"`), `gain_db: f32`, `input_peak: f32`, `latency_ms: Option<f64>`, `status: EngineStatus`, `stream: Option<StreamInfo>` where `StreamInfo { buffer_frames: usize, channels: usize, device_uid: String, sample_rate: f64 }` (backend.rs:17-23). Task 1 adds `enabled: bool` + `frame_mismatch_blocks: u64`.
- `EngineStatus` variants (status.rs:22-57): `Stopped`, `Starting { since_ms }`, `NoInputDetected { since_ms }` (informational — TCC denial and "no music" are indistinguishable), `AutoDisabledNoInput { after_ms }` (fail-open latch, sticky, only `Enable` restarts), `Running`, `InputSilent { since_ms }`, `Idle { since_ms }` (paused playback — never fails open), `Failed { reason }` (sticky after 2 consecutive start failures; only `Enable` clears).
- `CorrectionConfig` (controller.rs:56-59): `Fir { firs: Vec<Vec<f64>> }` | `Iir { sos_per_channel: Vec<Vec<[f64; 6]>> }` — raw coefficients, **no sample-rate field**. SOS layout `[b0,b1,b2,_,a1,a2]`, a0 assumed 1. Broadcast rule: fewer entries than channels → last entry broadcast, so `vec![peq.combined_sos()]` corrects both stereo channels. On a device rate change the controller re-applies the SAME coefficients at the new rate — **the embedder must redesign and re-send** (the stage-4 carry-forward).
- Design path (paraeq-dsp): `EQBand { filter_type: FilterType, fc: f64, gain_db: f64, q: f64 }` (peq.rs:47-53), `EQBand::to_sos(sample_rate) -> [f64; 6]` (peq.rs:56), `ParametricEQ { bands, sample_rate }`, `ParametricEQ::combined_sos() -> Vec<[f64; 6]>` (peq.rs:72), `ParametricEQ::frequency_response(freqs: &[f64]) -> Vec<f64>` dB (peq.rs:91-96; empty bands → exact zeros), `ParametricEQ::export_autoeq_format(...)` (peq.rs:111-124; Task 2 changes its signature). `FilterType::{HighShelf, LowShelf, Notch, Peaking}` with `as_str()` names `"high_shelf" | "low_shelf" | "notch" | "peaking"` and AutoEQ tags `HSC/LSC/NO/PK`. Notch ignores `gain_db` (biquad.rs:56).
- Teardown contract: normal exit = `handle.send(EngineCommand::Disable)` then `drop(handle)` (joins). Controller panic-safety is internal (`StopGuard`); `TapBackend::stop` runs the full invariant order and is idempotent. Nothing protects against SIGKILL except the OS dropping the tap with the process — that IS the design.

### Tauri 2.11.5 verified facts (from v2.tauri.app + docs.rs 2.11.5 + tauri source; all confirmed present in 2.11.5)

- **Tray:** `tauri::tray::{TrayIconBuilder, TrayIconEvent, MouseButton, MouseButtonState}`, `tauri::menu::{CheckMenuItemBuilder, Menu, MenuItem, SubmenuBuilder}`. Build in `.setup(|app| …)`. `MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?`; `CheckMenuItemBuilder::with_id("bypass", "Bypass").checked(false).build(app)?`; `Menu::with_items(app, &[…])?`; `TrayIconBuilder::with_id("main-tray").menu(&menu).show_menu_on_left_click(true).icon(app.default_window_icon().unwrap().clone()).on_menu_event(|app, event| match event.id().as_ref() { … }).build(app)?`. Dynamic updates: keep item handles (managed state) and call `item.set_checked(bool)?` / `item.set_text(…)?` / `tray.set_tooltip(Some(…))?` / `tray.set_menu(Some(menu))?` later.
- **Window lifecycle:** `.on_window_event(|window, event| if let tauri::WindowEvent::CloseRequested { api, .. } = event { window.hide().unwrap(); api.prevent_close(); })` — `WindowEvent` is non-exhaustive; `if let` (or a wildcard arm) is mandatory. Re-show: `window.show()` + `window.set_focus()` + `unminimize()`. App exit: handle `tauri::RunEvent::ExitRequested { api, .. }` (call `api.prevent_exit()` only if you want to veto) and `tauri::RunEvent::Exit` (last-chance cleanup) in the closure passed to `app.run(|app_handle, event| …)`; `RunEvent` is non-exhaustive — wildcard arm required.
- **Events from threads:** `use tauri::Emitter;` then `app_handle.emit("app-state", payload)?` (payload `Serialize + Clone`). `AppHandle` is `Send + Sync + Clone` — a plain `std::thread::spawn` holding a clone is the sanctioned pattern.
- **State:** `.manage(shared)` on the Builder (needs `Send + Sync + 'static`; wrap mutability in `Mutex` yourself); commands take `state: tauri::State<'_, AppShared>`; anywhere else `app.state::<AppShared>()`.
- **Commands:** any `Deserialize` args / `Serialize` return; `Result<T, String>` idiom — JS `invoke()` rejects with the serialized error. `async fn` commands run on Tauri's tokio runtime (needed for reqwest).
- **Plugins:** `tauri_plugin_single_instance::init(|app, _args, _cwd| { /* focus main window */ })` must be the FIRST plugin registered. `tauri_plugin_dialog::init()` + capability permission `dialog:default` + npm `@tauri-apps/plugin-dialog` (`import { open, save } from '@tauri-apps/plugin-dialog'`) — **verify exact API via context7 in Tasks 10/14**.
- **macOS bundle:** an `Info.plist` file in `src-tauri/` is auto-merged (ours already carries `NSAudioCaptureUsageDescription`); `bundle.macOS.minimumSystemVersion: "14.4"` already set. `App::set_activation_policy(tauri::ActivationPolicy::Regular)` exists but Regular is the default — no call needed.

### Anti-patterns (do NOT do)

- **Do NOT spawn the engine before reading persisted state.** Spawning enabled pre-wizard engages the tap and mutes output until fail-open (15 s) on a machine with no TCC grant. Read `settings.json` first; pass `EngineConfig { enabled: settings.engine_enabled && settings.setup_complete, .. }`.
- **Do NOT emit Tauri events from anywhere near the realtime path**, and do NOT call `EngineHandle` methods from inside an emit handler loop. Only the forwarder thread consumes snapshots and emits.
- **Do NOT hold the `subscribe()` receiver on the main thread** or inside a Tauri command — it's a blocking mpsc receiver; it gets a dedicated `std::thread` doing `recv_timeout`. The channel is lossy (bounded 64): treat pushes as change notifications; `handle.state()` is always the reconciliation source.
- **Do NOT leak the `EngineHandle`** in a `ManuallyDrop`, `Box::leak`, or `static` — its `Drop` sends `Shutdown` and joins the controller thread; skipping it skips orderly teardown. Managed state holds `Mutex<Option<EngineHandle>>` precisely so `RunEvent::Exit` can `.take()` and drop it.
- **Do NOT design SOS at a hardcoded 48 kHz when a stream is live** — always `stream.sample_rate` (fallback 48_000.0 only when `stream` is `None`). And do NOT forget the re-send on rate change; the engine will happily run stale coefficients at the new rate.
- **Do NOT pass unvalidated bands into `to_sos`** — q=0 or fc≥Nyquist produces NaN/Inf coefficients that `validate_correction` accepts into the realtime chain. Validation is Task 6's module; every band-accepting command calls it first, and the forwarder's rate-resend path re-validates at the NEW rate (bands valid at 96 kHz can be ≥ Nyquist at 44.1 kHz).
- **Do NOT match Tauri's non-exhaustive enums exhaustively** (`WindowEvent`, `RunEvent`, `TrayIconEvent`) — keep wildcard arms or `if let`, or a minor Tauri bump breaks the build.
- **Do NOT register tauri-plugin-single-instance anywhere but first.**
- **Do NOT block a sync command on network** — AutoEQ commands are `async fn` on the tokio runtime.
- **Do NOT rely on events for initial UI state** — events before `listen` are lost; the UI registers the listener, THEN calls `get_app_state`.
- **Do NOT hand-edit `fixtures/`** — extend `prototype/tools/generate_fixtures.py`, regenerate, commit script + output together.
- **Do NOT put unsafe FFI in src-tauri** — device enumeration/default-output setting goes in `paraeq-coreaudio` with `// SAFETY:` comments, symbols verified against the local registry source (`~/.cargo/registry/src/*/objc2-core-audio-0.3.2/src/generated/AudioHardware.rs`).

## File Structure (end state)

```
prototype/tools/generate_fixtures.py            # modified: AutoEQ-parser cases          (Task 3)
fixtures/autoeq_parser.json                     # NEW, generated (never hand-edited;
                                                #   single-file text-only layout —
                                                #   deliberate, no .f64 payloads)         (Task 3)
crates/paraeq-engine/src/controller.rs          # modified: enabled config/state,
                                                #   frame_mismatch_blocks in snapshots +
                                                #   effectively_equal                     (Task 1)
crates/paraeq-engine/src/status.rs              # modified: serde tag attrs              (Task 1)
crates/paraeq-engine/tests/test_controller.rs   # modified: enabled-flag tests +
                                                #   fast_config() literal                 (Task 1)
crates/paraeq-engine/tests/test_status.rs       # modified: old serialization test
                                                #   removed (superseded)                  (Task 1)
crates/paraeq-engine/tests/test_wire_format.rs  # NEW: golden JSON shape test            (Task 1)
crates/paraeq-dsp/src/peq.rs                    # modified: serde derives, preamp-aware
                                                #   export, parse_autoeq + ParsedPreset  (Tasks 2-3)
crates/paraeq-dsp/tests/…                       # test_peq.rs modified; NEW
                                                #   tests/test_autoeq_parse.rs           (Tasks 2-3)
crates/paraeq-dsp/Cargo.toml                    # + serde, regex                         (Tasks 2-3)
crates/paraeq-dsp/DIVERGENCES.md                # + preamp-export divergence entry       (Tasks 2, 18)
crates/paraeq-coreaudio/src/devices.rs          # NEW: output enumeration + set-default  (Task 4)
crates/paraeq-coreaudio/src/lib.rs              # + pub mod devices;                     (Task 4)
crates/paraeq-coreaudio/tests/test_hardware.rs  # + device enumeration hardware tests    (Task 4)
Cargo.lock                                      # updated by every dep-adding task       (Tasks 2-3, 5, 9-10)
desktop/src-tauri/Cargo.toml                    # + coreaudio/dsp/reqwest/plugins/…      (Tasks 5, 9, 10)
desktop/src-tauri/capabilities/default.json     # + dialog:default                       (Task 10)
desktop/src-tauri/tauri.conf.json               # window size/min 1024×768 / 900×640;
                                                #   drop vestigial bundle.android         (Task 10)
desktop/src-tauri/src/
├── main.rs             # stock shim (unchanged)
├── lib.rs              # builder: plugins, manage, invoke_handler, setup, run-event loop (Tasks 5-10, 14, 17)
├── autoeq.rs           # AutoEQ DB client (HttpFetch seam, index, cache)                 (Task 9)
├── commands.rs         # all #[tauri::command] handlers, grouped by domain               (Tasks 7-9, 14, 17)
├── engine_bridge.rs    # spawn_engine, forwarder thread, emit_app_state                  (Tasks 7, 10)
├── eq.rs               # validate_bands/preamp, design_correction, resend_decision       (Task 6)
├── profiles.rs         # minimal profile store (slug, save/load/list)                    (Task 8)
├── settings.rs         # Settings struct + atomic load/save                              (Task 5)
├── state.rs            # AppShared, AppData, AppState, EqState, OutputDeviceInfo         (Tasks 5, 17)
└── tray.rs             # build_tray, sync_tray                                           (Task 10)
desktop/src-tauri/tests/data/                   # sample INDEX.md + preset text (hand-
                                                #   written test data, NOT fixtures/)     (Task 9)
desktop/ui/
├── components.json     # NEW: shadcn-generated                                           (Task 11)
├── index.html          # title "ParaEQ"                                                  (Task 11)
├── package.json        # + @tauri-apps/plugin-dialog, shadcn deps, vitest                (Tasks 11-12, 14)
├── tsconfig.json       # + path alias (references root)                                  (Task 11)
├── tsconfig.app.json   # + path alias (the one tsc -b compiles app code with)            (Task 11)
├── vite.config.ts      # + path alias                                                    (Task 11)
├── vitest.config.ts    # NEW                                                             (Task 12)
└── src/                # (src/App.css + stock hero/logo assets DELETED, Task 11)
    ├── App.tsx             # tab shell (Measure|Target|EQ|Analyzer|Profiles) + wizard gate (Tasks 11, 14, 17)
    ├── components/ui/…     # shadcn-generated                                             (Task 11)
    ├── components/FrequencyPlot.tsx  # canvas wrapper (2 layers, RO, DPR, pointer)        (Task 13)
    ├── dialogs/AutoEqBrowser.tsx     # searchable DB dialog                               (Task 16)
    ├── ipc/commands.ts     # typed invoke wrappers                                        (Tasks 11, 14, 16-17)
    ├── ipc/types.ts        # hand-written wire types mirroring pinned serde shapes        (Task 11)
    ├── ipc/useAppState.ts  # listen("app-state") + get_app_state bridge                   (Task 11)
    ├── plot/FreqPlotRenderer.ts      # pure renderer class + logspace helper              (Task 12)
    ├── plot/FreqPlotRenderer.test.ts # vitest                                             (Tasks 12, 15)
    ├── tabs/EqTab.tsx      # band table, preamp, toolbar, plot, status strip              (Tasks 14-16)
    ├── tabs/PlaceholderTab.tsx                                                            (Task 11)
    └── wizard/SetupWizard.tsx        # 4-step wizard + chime probe                        (Task 17)
.github/workflows/ci.yml                        # + vitest step                            (Task 18)
docs/CONTEXT.md                                 # stage-4 status                           (Task 18)
```

Reference sources (read them; they are the spec for each pattern): `crates/paraeq-coreaudio/examples/tap_engine.rs` (embedding sequence, snapshot loop, hint text), `crates/paraeq-coreaudio/tests/test_hardware.rs:19-51` (afplay helper), `prototype/paraeq/correction/autoeq_db.py` (parser + DB-client oracle), `prototype/app/eq/manual_eq_editor.py` + `prototype/app/menu_bar/tray.py` (parity reference), `docs/plans/2026-07-06-rust-port-tap-engine.md` (house style + stage-3 interfaces).

---

### Task 1: Engine — spawn-disabled config, enabled + frame_mismatch_blocks in state, pinned wire format

**Files:**
- Modify: `crates/paraeq-engine/src/controller.rs`, `crates/paraeq-engine/src/status.rs`, `crates/paraeq-engine/tests/test_controller.rs`, `crates/paraeq-engine/tests/test_status.rs` (delete `status_serializes_to_json` — it pins the OLD externally-tagged PascalCase shape and is superseded by the new golden test)
- Create: `crates/paraeq-engine/tests/test_wire_format.rs`
- No Cargo.toml change: the `serde_json` dev-dep already exists in `crates/paraeq-engine/Cargo.toml`.

**Interfaces:**
- `EngineConfig` (controller.rs:147-183) gains `pub enabled: bool`; `Default` sets `true` (preserves every existing test and the tap_engine example). `EngineHandle::spawn` initializes the controller's `enabled` from config instead of the hardcoded `enabled: true` (controller.rs:226); the run loop's initial `try_start()` (controller.rs:353-355) happens only when enabled.
- `EngineState` (controller.rs:130-143) gains `pub enabled: bool` (the controller's current enabled flag — distinguishes user-disabled from pre-start/failed `Stopped`) and `pub frame_mismatch_blocks: u64` (read from `RtShared` with `Ordering::Relaxed` at snapshot time; retains the last observed count after `Disable` tears the session down, and is zeroed by the fresh `RtShared` on every start — document that in the field's doc comment and pin it in a test). Both fields participate in publish-on-change — which is NOT derived `PartialEq` but the hand-rolled field-by-field `effectively_equal` (controller.rs:703-713); adding struct fields produces NO compile error there, so both comparisons must be added by hand. `enabled` compares exactly; `frame_mismatch_blocks` compares **quantized as `> 0`** (the counter increments once per mismatched block — comparing it raw would defeat the snapshot damping and publish a fresh snapshot + Tauri emit every tick for as long as a degraded stretch lasts; the UI only needs the boolean, Task 14).
- `EngineStatus` (status.rs:22-57) gains `#[serde(rename_all = "snake_case", tag = "kind")]` — variants serialize as `{"kind":"stopped"}`, `{"kind":"no_input_detected","since_ms":1200}`, `{"kind":"failed","reason":"…"}`, etc. NO other rename attrs anywhere (field names stay snake_case).
- Semantics that MUST hold (each is a test): spawn with `enabled: false` never calls `backend.start()` and snapshots `status: Stopped, enabled: false`; `Enable` then starts and flips `enabled: true`; `Disable` flips it back; **a flip of `enabled` ALONE publishes** (spawn disabled → `fail_next_starts(1)` → `Enable`: the start fails so `status` stays `Stopped` and every other compared field is unchanged, yet `state()`/subscribers must see `enabled: true` — this is the test that catches a missed `effectively_equal` edit); `frame_mismatch_blocks` surfaces nonzero after mismatched-frame pumping and re-zeroes on the next start; the exact JSON shape is pinned.
- Consumed by: Task 7 (spawn honoring persisted state; forwarder), Task 11 (TS types mirror the pinned shape).

- [ ] **Step 1: Failing tests.** Append to `tests/test_controller.rs` (reuse `common::mock_backend::MockBackend`, mirroring the existing tests' shortened-window config):

```rust
#[test]
fn spawn_disabled_does_not_start_until_enable() {
    let backend = MockBackend::default();
    let probe = backend.clone();
    let handle = EngineHandle::spawn(
        backend,
        EngineConfig { enabled: false, tick_ms: 20, ..EngineConfig::default() },
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(probe.start_count(), 0, "disabled spawn must not touch the backend");
    let s = handle.state();
    assert!(!s.enabled);
    assert_eq!(s.status, EngineStatus::Stopped);
    handle.send(EngineCommand::Enable);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(probe.start_count(), 1);
    assert!(handle.state().enabled);
}

#[test]
fn enabled_flip_alone_publishes() {
    // spawn disabled; probe.fail_next_starts(1); send Enable → the start
    // fails, so status stays Stopped and every OTHER effectively_equal field
    // is unchanged — the snapshot must still update to enabled: true.
    // (Guards the hand-rolled effectively_equal edit; a missed comparison
    // makes publish() return early and the UI never sees the flip.)
}

#[test]
fn frame_mismatch_blocks_reach_snapshots() {
    // spawn enabled; send a FIR correction sized for the mock's block_size;
    // pump blocks at block_size/2 → chain flags mismatches → snapshot field grows.
    // (Adapt the pump helper the existing swap tests use.)
}
```

(`MockBackend` already has the accessors needed: `start_count()` at tests/common/mock_backend.rs:114-120, plus `calls()` and `fail_next_starts(n)` — add nothing.) New file `tests/test_wire_format.rs`:

```rust
use paraeq_engine::backend::StreamInfo;
use paraeq_engine::controller::EngineState;
use paraeq_engine::status::EngineStatus;

/// Stage-4 wire contract: desktop/ui/src/ipc/types.ts mirrors THIS shape by hand.
/// If this test changes, the TS types must change in the same PR.
#[test]
fn engine_state_wire_format_is_pinned() {
    let state = EngineState {
        bypass: false,
        correction: Some("iir:2-band".into()),
        enabled: true,
        frame_mismatch_blocks: 0,
        gain_db: -3.0,
        input_peak: 0.25,
        latency_ms: Some(62.3),
        status: EngineStatus::NoInputDetected { since_ms: 1200 },
        stream: Some(StreamInfo {
            buffer_frames: 512, channels: 2,
            device_uid: "uid-1".into(), sample_rate: 48000.0,
        }),
    };
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        serde_json::json!({
            "bypass": false,
            "correction": "iir:2-band",
            "enabled": true,
            "frame_mismatch_blocks": 0,
            "gain_db": -3.0,
            "input_peak": 0.25,
            "latency_ms": 62.3,
            "status": { "kind": "no_input_detected", "since_ms": 1200 },
            "stream": { "buffer_frames": 512, "channels": 2,
                        "device_uid": "uid-1", "sample_rate": 48000.0 }
        })
    );
    assert_eq!(
        serde_json::to_value(EngineStatus::Running).unwrap(),
        serde_json::json!({ "kind": "running" })
    );
    assert_eq!(
        serde_json::to_value(EngineStatus::AutoDisabledNoInput { after_ms: 15000 }).unwrap(),
        serde_json::json!({ "kind": "auto_disabled_no_input", "after_ms": 15000 })
    );
}
```

(Adjust field-struct literal syntax to whatever `EngineState` construction the crate allows — if fields are pub it works as-is.)
- [ ] **Step 2: Run to verify failure.** `cargo test -p paraeq-engine` — compile errors (`enabled` field missing) are the expected failure mode. Note the OTHER expected compile break: `fast_config()` in `tests/test_controller.rs:29-41` spells out every `EngineConfig` field with no `..Default::default()` — add `enabled: true` there (that's the fix, not a design problem; `tap_engine.rs` and `test_hardware.rs` use `..EngineConfig::default()` and survive untouched).
- [ ] **Step 3: Implement.** `EngineConfig.enabled` + conditional initial start; `enabled` + `frame_mismatch_blocks` threaded into the snapshot builder (controller.rs:664-689 region) **AND into `effectively_equal` (controller.rs:703-713)** — it is hand-rolled field-by-field, new fields silently don't participate unless added; compare `enabled` exactly and `frame_mismatch_blocks` quantized as `> 0` (per the interface bullet); serde attrs on `EngineStatus`. Gotchas: keep the `Enable`/`Disable` handlers' existing latch-clearing behavior intact (controller.rs:390-405); `frame_mismatch_blocks` semantics are "last observed count, zeroed by the fresh `RtShared` on start" (matches the code's natural per-session counter, controller.rs:571) — document in the field's doc comment and pin in a test; delete `status_serializes_to_json` (tests/test_status.rs:221-228 — asserts the old `NoInputDetected`/`Running` PascalCase shape, superseded by `test_wire_format.rs`); the tap_engine example constructs `EngineConfig` with `..Default::default()` — confirm it still compiles under `--all-targets`.
- [ ] **Step 4: Run to verify pass.** `cargo test -p paraeq-engine` — new tests green; every existing controller/watchdog test green with only the two named mechanical edits (`fast_config()` gains `enabled: true`; `status_serializes_to_json` deleted).
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "feat(engine): spawn-disabled config, enabled + frame_mismatch_blocks in EngineState, pinned wire format"
```

---

### Task 2: dsp — serde on EQ types + preamp-aware AutoEQ export

**Files:**
- Modify: `crates/paraeq-dsp/src/peq.rs`, `crates/paraeq-dsp/Cargo.toml` (`serde = { workspace = true }` under `[dependencies]`), the existing peq test file under `crates/paraeq-dsp/tests/` (locate with `ls crates/paraeq-dsp/tests/`), `crates/paraeq-dsp/DIVERGENCES.md`

**Interfaces:**
- `FilterType` (peq.rs:7-13) derives grow `serde::Deserialize, serde::Serialize` with `#[serde(rename_all = "snake_case")]` — wire names `"high_shelf" | "low_shelf" | "notch" | "peaking"`, identical to `as_str()` (peq.rs:16-35).
- `EQBand` (peq.rs:47-53) derives grow `serde::Deserialize, PartialEq, serde::Serialize` — wire fields `filter_type, fc, gain_db, q` (natural snake_case, no attrs).
- `ParametricEQ::export_autoeq_format(&self, preamp_db: f64) -> String` — SIGNATURE CHANGE (was `&self` only, hardcoding `Preamp: 0.0 dB` at peq.rs:112). First line becomes `format!("Preamp: {:.1} dB", preamp_db)`; filter lines unchanged (`Filter {i}: ON {tag} Fc {fc:.0} Hz Gain {gain:.1} dB Q {q:.3}`, 1-based, `\n`-joined, no trailing newline).
- Semantics that MUST hold (each a test): serde round-trip of an `EQBand` vec; golden JSON string for one band (pins the wire shape for Task 11's TS); `export_autoeq_format(0.0)` byte-identical to the previous output (existing golden/fixture expectations keep passing with `0.0` passed at call sites); `export_autoeq_format(-6.5)` first line is `Preamp: -6.5 dB`.
- Consumed by: Tasks 3, 5, 6, 7, 14.

- [ ] **Step 1: Failing tests.** In the existing peq test file (`tests/test_peq.rs`) add `band_serde_roundtrip_and_golden_json` (assert `serde_json::to_value(&band)` equals `json!({"filter_type":"peaking","fc":1000.0,"gain_db":3.0,"q":1.41})`) and `export_writes_actual_preamp` (assert first line of `export_autoeq_format(-6.5)`). (The dsp crate already has the `serde_json` dev-dep — only the `serde` dependency is new.)
- [ ] **Step 2: Run to verify failure.** `cargo test -p paraeq-dsp` — compile error (no serde derives / wrong arity) expected.
- [ ] **Step 3: Implement.** Derives + signature change; update every existing call site (tests, and grep the workspace: `rg "export_autoeq_format" --type rust`) to pass `0.0` where prior behavior is wanted.
- [ ] **Step 4: DIVERGENCES.md entry** (append, keeping the numbering):

```markdown
N. **`export_autoeq_format` takes and writes the real preamp.** The prototype
   exporter hardcodes `Preamp: 0.0 dB` (parametric_eq.py:92-106) and never
   round-trips preamp. Rust takes `preamp_db: f64` and writes it (`{:.1}`).
   The parser accepts both, so cross-imports still work. Deliberate UX fix,
   owner-approved 2026-07-13.
```

- [ ] **Step 5: Run to verify pass.** `cargo test -p paraeq-dsp`.
- [ ] **Step 6: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp Cargo.lock
git commit -m "feat(dsp): serde derives on FilterType/EQBand + preamp-aware AutoEQ export (divergence logged)"
```

---

### Task 3: dsp — AutoEQ text parser against oracle fixtures

**Files:**
- Modify: `prototype/tools/generate_fixtures.py`, `crates/paraeq-dsp/src/peq.rs`, `crates/paraeq-dsp/Cargo.toml` (`regex = "1"`)
- Create: `fixtures/autoeq_parser.json` (GENERATED — never hand-edited), `crates/paraeq-dsp/tests/test_autoeq_parse.rs`

**Interfaces:**
- ```rust
  #[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
  pub struct ParsedPreset { pub bands: Vec<EQBand>, pub preamp_db: f64 }

  pub fn parse_autoeq(text: &str) -> ParsedPreset
  ```
  in `paraeq_dsp::peq`. Infallible (zero bands is a valid result — callers decide whether that's a warning). Oracle semantics (`prototype/paraeq/correction/autoeq_db.py:23-37, 82-110`) that MUST hold, each covered by a fixture case:
  - Preamp line: regex `Preamp\s*:\s*([-\d.]+)\s*dB`, case-insensitive, searched (not anchored); **default 0.0 when absent**.
  - Filter line: `Filter\s+\d+\s*:\s*ON\s+(\w+)\s+Fc\s+([\d.]+)\s+Hz\s+Gain\s+([-\d.]+)\s+dB\s+Q\s+([\d.]+)`, case-insensitive; only literal `ON` matches (`OFF` lines silently skipped); the filter number is ignored.
  - Type codes: `PK`→peaking, `NO`→notch, `LSC` and legacy `LS`→low_shelf, `HSC` and legacy `HS`→high_shelf; **unknown codes → peaking**.
  - Non-matching lines are skipped without error.
- Consumed by: Tasks 9 (client parses fetched presets), 14 (file import), 16 (DB browser).

- [ ] **Step 0: Align the oracle venv BEFORE regenerating.** The generator `rmtree`s and rebuilds the ENTIRE `fixtures/` tree (generate_fixtures.py:221-224) and stamps `manifest.json` with library versions (:235-240). The committed manifest records numpy 2.5.0 / scipy 1.18.0 / python 3.13.7; the current `.venv` has numpy 2.4.4 / scipy 1.17.1 — regenerating as-is churns `manifest.json` at minimum and risks float drift. Remediation, in order of preference: (a) `.venv/bin/pip install numpy==2.5.0 scipy==1.18.0`, re-run `.venv/bin/pytest prototype/tests -q`, then regenerate — `git diff` outside the new file should be empty; (b) if alignment is impractical, regenerate wholesale, verify oracle pytest AND `cargo test --workspace` (the Rust fixture-parity suites) green, and commit the full fixture churn deliberately, saying so in the commit body. Record which route was taken.
- [ ] **Step 1: Extend the fixture generator.** In `prototype/tools/generate_fixtures.py`, add an `autoeq_parser` section: a list of named case input strings (standard preset with preamp; preset with NO preamp line; **TWO `Preamp:` lines — the oracle is last-wins: `parse_parametric_eq` overwrites `preamp_db` on every match and `continue`s, autoeq_db.py:93-97; a first-wins Rust port would pass every other case and still diverge**; `OFF` filter mixed in; unknown code `XYZ`; legacy `LS`/`HS`; lowercase `filter 1: on pk fc …`; junk/comment lines interleaved; empty string; a round-trip case built from `ParametricEQ.export_autoeq_format()` prototype output). For each case, run the PROTOTYPE parser (`paraeq.correction.autoeq_db.parse_parametric_eq`) and dump `{"name", "input", "expected": {"preamp_db", "bands": [{"filter_type","fc","gain_db","q"}]}}` to `fixtures/autoeq_parser.json` (deterministic ordering). Layout note: this is a DELIBERATE exception to `save_case()`'s per-stage-dir + `.f64` convention (generate_fixtures.py:49-58) — parser cases are pure text with no float arrays, so one top-level list-of-cases file is used; say so in a comment beside the new generator section so nobody "fixes" it. Regenerate:

```bash
.venv/bin/python prototype/tools/generate_fixtures.py
.venv/bin/pytest prototype/tests -q   # oracle still green
git diff --stat fixtures/             # route (a): ONLY autoeq_parser.json new; route (b): full churn, accepted deliberately
```

- [ ] **Step 2: Failing Rust test.** `crates/paraeq-dsp/tests/test_autoeq_parse.rs`: deserialize `fixtures/autoeq_parser.json` (same relative-path convention as the existing fixture tests — copy it from a neighboring test file), loop cases, assert `parse_autoeq(&case.input)` equals expected (`PartialEq` from Tasks 2-3 derives; floats parsed from identical decimal strings are bit-identical — use exact equality, and only fall back to `1e-12` tolerance if a real IEEE discrepancy shows up, documenting why). Add a non-fixture unit test: export→parse round-trip via Rust's own `export_autoeq_format(-3.0)` (tolerances: fc ±0.5 from `{:.0}`, gain ±0.05 from `{:.1}`, q ±0.0005 from `{:.3}`).
- [ ] **Step 3: Run to verify failure.** `cargo test -p paraeq-dsp --test test_autoeq_parse` — `parse_autoeq` not found.
- [ ] **Step 4: Implement** in `peq.rs` with `regex` (compile the two regexes once via `std::sync::OnceLock<Regex>`; `(?i)` inline flag for case-insensitivity). Unknown-code fallback and alias table as a match statement mirroring autoeq_db.py:30-37.
- [ ] **Step 5: Run to verify pass.** `cargo test -p paraeq-dsp`.
- [ ] **Step 6: Gates + commit** (script + fixtures + code together — fixtures rule):

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp Cargo.lock prototype/tools/generate_fixtures.py fixtures/
git commit -m "feat(dsp): AutoEQ text parser (oracle-fixture parity: ON-only, aliases, unknown->peaking, preamp last-wins)"
```

---

### Task 4: coreaudio — output-device enumeration + set-default-output FFI

**Files:**
- Create: `crates/paraeq-coreaudio/src/devices.rs`
- Modify: `crates/paraeq-coreaudio/src/lib.rs` (`pub mod devices;`), `crates/paraeq-coreaudio/tests/test_hardware.rs`

**Interfaces:**
- ```rust
  #[derive(Clone, Debug, PartialEq)]
  pub struct OutputDevice { pub id: AudioObjectID, pub name: String, pub uid: String }

  /// All devices with ≥1 output channel, excluding ParaEQ's own private
  /// aggregate (defensive: skip uid prefix "com.paraeq.engine."). Sorted by name.
  pub fn list_output_devices() -> Result<Vec<OutputDevice>, CaError>

  /// Resolve uid → AudioObjectID (via the enumeration) and set the system
  /// default output. Err if the uid is unknown.
  pub fn set_default_output_device(uid: &str) -> Result<(), CaError>
  ```
  No serde here — src-tauri maps to its own serializable struct (crate stays serde-free).
- FFI plan (every symbol MUST be verified in the local registry source `~/.cargo/registry/src/*/objc2-core-audio-0.3.2/src/generated/AudioHardware.rs` before use, exactly like the stage-3 plan's discipline; record line numbers in code comments): `kAudioHardwarePropertyDevices` on `kAudioObjectSystemObject` (GetPropertyDataSize → GetPropertyData for the `AudioObjectID` array); per device `kAudioDevicePropertyStreamConfiguration` with scope `kAudioObjectPropertyScopeOutput` (sum `mNumberChannels` over the returned `AudioBufferList` — >0 means output-capable); name via `kAudioObjectPropertyName` (CFString, +1 retained — same conversion gotcha as `device_uid`, properties.rs); reuse the existing `device_uid()` getter; set-default via `AudioObjectSetPropertyData` (already verified, AudioHardware.rs:856) on `kAudioObjectSystemObject` / `kAudioHardwarePropertyDefaultOutputDevice` with an `AudioObjectID` payload. If `kAudioObjectPropertyName` is absent from the generated bindings, use `kAudioDevicePropertyDeviceNameCFString` — whichever the registry actually exports.
- Every `unsafe` block gets `// SAFETY:`; the crate's `#![warn(clippy::undocumented_unsafe_blocks)]` enforces it.
- Consumed by: Task 7 (device list in AppState + `engine_set_default_output`), Task 17 (wizard device page).

- [ ] **Step 1: Hardware tests first** (`tests/test_hardware.rs`, `#[ignore = "requires audio hardware"]`):
  - `output_enumeration_includes_default`: `list_output_devices()` non-empty; the uid of `properties::default_output_device()` appears in the list; every entry has non-empty name+uid.
  - `set_default_output_roundtrip_noop`: read the current default's uid, call `set_default_output_device(&uid)` with the SAME uid (a no-op switch — never disrupt the dev machine's audio) → `Ok`; `set_default_output_device("bogus-uid-nope")` → `Err`.
- [ ] **Step 2: Implement `devices.rs`.** Verify each new selector constant in the registry file FIRST; if a needed constant is missing from objc2-core-audio 0.3.2, stop and record it in the task report rather than hand-defining the fourcc (only fall back to a local `const` with the documented fourcc value + a `// VERIFIED against Apple headers` comment if truly absent).
- [ ] **Step 3: Run to verify.** `cargo test -p paraeq-coreaudio` (non-ignored suite green) and locally `cargo test -p paraeq-coreaudio -- --ignored` (needs the TCC-granted terminal for the pre-existing suite; the two new tests themselves need no TCC).
- [ ] **Step 4: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-coreaudio
git commit -m "feat(coreaudio): output-device enumeration + system-default-output setter (verified FFI)"
```

---

### Task 5: src-tauri — settings persistence + AppState types

**Files:**
- Create: `desktop/src-tauri/src/settings.rs`, `desktop/src-tauri/src/state.rs`
- Modify: `desktop/src-tauri/src/lib.rs` (`mod settings; mod state;` — modules private to the crate), `desktop/src-tauri/Cargo.toml` (add, alphabetical: `paraeq-coreaudio = { path = "../../crates/paraeq-coreaudio" }`, `paraeq-dsp = { path = "../../crates/paraeq-dsp" }`; `[dev-dependencies] tempfile = "3"`)

**Interfaces:**
- `settings.rs`:

```rust
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Settings {
    pub active_profile: Option<String>,
    pub bands: Vec<paraeq_dsp::peq::EQBand>,
    pub engine_enabled: bool,
    pub preamp_db: f64,
    pub setup_complete: bool,
}
impl Default for Settings { /* None, vec![], false, 0.0, false */ }

/// Missing file or corrupt JSON → Default (log::warn on corrupt, never panic).
pub fn load(path: &std::path::Path) -> Settings
/// Atomic: create parent dirs, write `<path>.tmp`, then fs::rename over path.
pub fn save(path: &std::path::Path, s: &Settings) -> std::io::Result<()>
```

- `state.rs`:

```rust
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct OutputDeviceInfo { pub name: String, pub uid: String }

#[derive(Clone, Debug, serde::Serialize)]
pub struct EqState { pub bands: Vec<EQBand>, pub preamp_db: f64 }

/// THE snapshot the UI renders. Serialized whole on every change ("app-state"
/// event) and returned by get_app_state. NO permission field — TCC is
/// undetectable; engine.status carries the honest proxies.
#[derive(Clone, Debug, serde::Serialize)]
pub struct AppState {
    pub active_profile: Option<String>,
    pub default_output_uid: Option<String>,
    pub devices: Vec<OutputDeviceInfo>,
    pub engine: paraeq_engine::controller::EngineState,
    pub eq: EqState,
    pub profiles: Vec<String>,
    pub setup_complete: bool,
}

/// Everything mutable the app owns, behind ONE Mutex (commands are rare and
/// cheap; no lock ordering to get wrong). EngineHandle lives in its own slot
/// so RunEvent::Exit can .take() it.
pub struct AppShared {
    pub data: std::sync::Mutex<AppData>,
    pub engine: std::sync::Mutex<Option<paraeq_engine::controller::EngineHandle>>,
    pub profiles_dir: std::path::PathBuf,
    pub settings_path: std::path::PathBuf,
}

#[derive(Clone, Debug)]
pub struct AppData {
    pub active_profile: Option<String>,
    pub bands: Vec<EQBand>,
    pub default_output_uid: Option<String>,
    pub devices: Vec<OutputDeviceInfo>,
    pub engine_enabled: bool,   // persisted intent (survives fail-open)
    pub preamp_db: f64,
    pub profiles: Vec<String>,
    pub setup_complete: bool,
}
impl AppData {
    pub fn from_settings(s: &Settings) -> AppData
    pub fn to_settings(&self) -> Settings
    pub fn app_state(&self, engine: &EngineState) -> AppState
}
```

- Semantics that MUST hold (each a unit test): `load` of a missing path → `Default`; `load` of garbage bytes → `Default`; save→load round-trip equality (with bands); `#[serde(default)]` tolerates old settings files missing new fields; save is atomic (after save, no `.tmp` file remains; content parses); `from_settings`/`to_settings` round-trip; **the full `AppState` envelope JSON is pinned by a golden test** (`app_state_wire_format_is_pinned` in `state.rs` tests: compose an `AppData` literal + a hand-built `EngineState` — fields are pub after Task 1 — and `assert_eq!` against a complete `serde_json::json!` literal, exactly like Task 1's test; this extends wire-format enforcement to `AppState`/`EqState`/`OutputDeviceInfo`, so a field rename in `state.rs` fails HERE instead of silently breaking the UI).
- Consumed by: Tasks 6-10, 17.

- [ ] **Step 1: Failing tests.** `#[cfg(test)] mod tests` inside each module using `tempfile::tempdir()`. Include the round-trip with a 2-band `Settings` (exercises the Task 2 serde derives end-to-end) and the `AppState` golden JSON test (a doc comment on it MUST say `desktop/ui/src/ipc/types.ts` mirrors this shape by hand, same as Task 1's).
- [ ] **Step 2: Run to verify failure.** `cargo test -p paraeq-desktop` — modules don't exist yet (compile error).
- [ ] **Step 3: Implement.** Note: `log` is already a workspace dep — add `log = { workspace = true }` to src-tauri if not present.
- [ ] **Step 4: Run to verify pass.** `cargo test -p paraeq-desktop`.
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add desktop/src-tauri Cargo.lock
git commit -m "feat(desktop): settings persistence (atomic JSON) + AppState/AppShared state model (wire shape pinned)"
```

---

### Task 6: src-tauri — band validation, correction design, rate-resend decision (pure)

**Files:**
- Create: `desktop/src-tauri/src/eq.rs`
- Modify: `desktop/src-tauri/src/lib.rs` (`mod eq;`)

**Interfaces:**

```rust
pub const GAIN_LIMIT_DB: f64 = 30.0;
pub const PREAMP_MAX_DB: f64 = 10.0;
pub const PREAMP_MIN_DB: f64 = -30.0;
pub const Q_MAX: f64 = 100.0;

/// Reject before ANY coefficient design: fc not finite or outside
/// (0, sample_rate/2) exclusive; q not finite, <= 0, or > Q_MAX; gain_db not
/// finite or |gain_db| > GAIN_LIMIT_DB. Error strings name the band index and
/// offending field (they surface verbatim in the UI).
pub fn validate_bands(bands: &[EQBand], sample_rate: f64) -> Result<(), String>

/// Not finite or outside [PREAMP_MIN_DB, PREAMP_MAX_DB] → Err.
pub fn validate_preamp(db: f64) -> Result<(), String>

/// None when bands is empty (caller sends ClearCorrection). Single SOS set —
/// the engine broadcasts it to both stereo channels.
pub fn design_correction(bands: &[EQBand], sample_rate: f64) -> Option<CorrectionConfig>
    // Some(CorrectionConfig::Iir { sos_per_channel: vec![ParametricEQ{..}.combined_sos()] })

/// The forwarder's redesign trigger, pure and unit-tested:
/// Some(new_rate) iff snapshot.stream is Some AND have_bands AND
/// last_rate != Some(stream.sample_rate). (First-ever stream also triggers —
/// covers "correction was queued before geometry was known".)
pub fn resend_decision(last_rate: Option<f64>, snapshot: &EngineState, have_bands: bool) -> Option<f64>

/// Grid + bands + rate → what the plot draws. per_band[i] is band i alone.
pub struct ResponseData { pub composite: Vec<f64>, pub per_band: Vec<Vec<f64>>, pub sample_rate: f64 }
pub fn response(bands: &[EQBand], freqs: &[f64], sample_rate: f64) -> ResponseData
```

- Semantics that MUST hold (each a unit test): fc = 0 / fc = sr/2 / fc = NaN rejected; q = 0 / q = -1 / q = 101 rejected; **the rate-revalidation case: a band with fc = 23 kHz passes `validate_bands` at 96 000/48 000 but is rejected at 44 100** (this is exactly what the Task 7 forwarder re-checks on a rate switch); gain 30.0 accepted, 30.1 rejected; empty bands validate Ok; `design_correction(&[], _)` is None; a 2-band design's SOS equals `ParametricEQ::combined_sos()` directly (no drift); `resend_decision`: `(None, stream@48k, true) → Some(48000.0)`, `(Some(48k), stream@48k, true) → None`, `(Some(48k), stream@44.1k, true) → Some(44100.0)`, `(Some(48k), stream@44.1k, false) → None`, `(Some(48k), no stream, true) → None`; `response` composite of empty bands is all-zeros; composite ≈ sum of per_band (1e-9 — same-math sanity, mirrors the prototype's per-band-sum composite path).
- Consumed by: Task 7 (every band/preamp command + forwarder), Task 14 (`eq_response`).

- [ ] **Step 1: Failing tests** (`#[cfg(test)] mod tests` in `eq.rs`; construct a minimal `EngineState` literal for the resend cases — all fields are pub after Task 1).
- [ ] **Step 2: Run to verify failure.** `cargo test -p paraeq-desktop` — compile error.
- [ ] **Step 3: Implement.** `response`: composite via one `ParametricEQ::frequency_response` over all bands; `per_band` via 1-band `ParametricEQ`s (parity with the prototype's per-band evaluation).
- [ ] **Step 4: Run to verify pass.** `cargo test -p paraeq-desktop`.
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add desktop/src-tauri
git commit -m "feat(desktop): band/preamp validation, SOS design, rate-resend decision (pure, unit-tested)"
```

---

### Task 7: src-tauri — engine wiring: spawn, forwarder thread, engine_*/eq_* commands, get_app_state

**Files:**
- Create: `desktop/src-tauri/src/engine_bridge.rs`, `desktop/src-tauri/src/commands.rs`
- Modify: `desktop/src-tauri/src/lib.rs`

**Interfaces:**
- `engine_bridge.rs`:

```rust
/// TapBackend + EngineConfig { enabled: settings.engine_enabled && settings.setup_complete,
/// ..Default::default() }. Called ONCE from setup, AFTER settings::load —
/// never spawn before reading persisted state.
pub fn spawn_engine(settings: &Settings) -> EngineHandle

/// Compose AppState from AppShared.data + the given engine snapshot, emit
/// "app-state" (tauri::Emitter), and persist Settings if data changed.
/// The single choke point every command and the forwarder call.
pub fn publish(app: &tauri::AppHandle, engine: &EngineState)

/// Dedicated std::thread owning rx = handle.subscribe() (created BEFORE the
/// handle is stored). Loop on rx.recv_timeout(500ms):
///   Ok(snapshot) →
///     1. if let Some(rate) = eq::resend_decision(last_rate, &snapshot, have_bands) {
///          match eq::validate_bands(&bands, rate) {          // REVALIDATE at the NEW rate —
///            Ok(()) =>                                        // apply-time validation used the
///              if let Some(cfg) = eq::design_correction(&bands, rate)   // OLD rate; fc can be
///                { handle.send(SetCorrection(cfg)) },                   // ≥ Nyquist after a switch
///            Err(e) => { log::warn!(…); handle.send(ClearCorrection) }, // flat passthrough,
///          }                                                            // never NaN into the chain
///          last_rate = Some(rate);
///        }
///        (The first-stream trigger runs the same path — it is how persisted
///         settings.json bands, a hand-editable file, get vetted before to_sos.)
///     2. if stream.device_uid changed → refresh data.devices via
///        paraeq_coreaudio::devices::list_output_devices()
///     3. publish(&app, &snapshot); tray::sync_tray(&app, &state)  [no-op until Task 10]
///   Err(Timeout) → continue; Err(Disconnected) → break (engine dropped).
/// It reads bands/handle through app.state::<AppShared>() — lock, copy, drop
/// the guard BEFORE send (never hold a lock across an engine call).
pub fn start_forwarder(app: tauri::AppHandle, rx: Receiver<Arc<EngineState>>)
```

- `commands.rs` — all `Result<(), String>` unless noted; every mutation ends by calling `publish` with `handle.state()` so the UI sees the change without waiting for the next engine tick:
  - `get_app_state(state: State<AppShared>) -> Result<AppState, String>` — compose from current `data` + `engine.state()` (or a `Stopped`/disabled default `EngineState` if the handle is somehow absent).
  - `engine_enable` / `engine_disable` — send `Enable`/`Disable`; set `data.engine_enabled` = true/false (persisted intent — decision 1).
  - `engine_set_bypass(bypass: bool)` — `SetBypass`.
  - `engine_set_preamp_db(db: f64)` — `eq::validate_preamp` → `SetGainDb(db as f32)` → `data.preamp_db = db`.
  - `engine_set_default_output(uid: String)` — `paraeq_coreaudio::devices::set_default_output_device(&uid)` (map err to String); the engine's own `DefaultOutputChanged` listener drives the rebuild — do NOT send any engine command; set `data.default_output_uid = Some(uid)`.
  - `engine_list_outputs() -> Result<Vec<OutputDeviceInfo>, String>` — re-enumerate, update `data.devices`, publish, return.
  - `eq_set_bands(bands: Vec<EQBand>)` — THE apply path (prototype contract: every edit applies immediately, no Apply button): `validate_bands` at the live rate (fallback 48_000.0) → `data.bands = bands` → empty ? `ClearCorrection` : `SetCorrection(design_correction(..))` → publish (which persists).
  - `eq_add_band()` — append prototype default `EQBand { filter_type: Peaking, fc: 1000.0, gain_db: 0.0, q: 1.41 }`, then the same apply path.
  - `eq_remove_band(index: Option<usize>)` — remove given index, or the LAST band when None (prototype parity: remove-with-no-selection deletes the last row); same apply path.
  - `eq_response(freqs: Vec<f64>) -> Result<ResponseData, String>` — `eq::response` at live rate (fallback 48 k).
- `lib.rs`: `.manage(AppShared { .. })` (paths from `app.path().app_data_dir()` — `settings.json`, `profiles/`), `.setup(|app| { load settings → spawn_engine → rx = handle.subscribe() → **restore persisted preamp: clamp `settings.preamp_db` into [PREAMP_MIN_DB, PREAMP_MAX_DB] (hand-editable file; `log::warn` if clamped), `handle.send(SetGainDb(clamped as f32))`, `data.preamp_db = clamped`** → store handle → start_forwarder })`, `.invoke_handler(tauri::generate_handler![…all commands…])`, and `app.run(|app_handle, event| match event { tauri::RunEvent::Exit => { if let Some(h) = shared.engine.lock().unwrap().take() { h.send(EngineCommand::Disable); drop(h); } } _ => {} })` — replacing the stock one-liner `run`. (Tray + window-event handling arrive in Task 10; Task 8 adds profile-list population to this setup; keep this task engine-only.) The preamp restore is NOT optional: bands reach the engine via the forwarder's first-stream trigger, but nothing else ever sends `SetGainDb` — without it the engine runs at 0 dB while the UI displays the persisted value (Task 18 items 6 and 11 depend on this).
- Semantics that MUST hold: no lock held across `handle.send` or `emit`; `subscribe()` called before the handle is stored (no missed early snapshots); persisted preamp reaches the engine at setup (clamped); the forwarder never designs coefficients from bands it has not validated at the target rate; Exit path takes+drops the handle exactly once. These are wiring properties — enforced by code review against this list + the Task 10 manual smoke; the pure logic they compose was tested in Tasks 5-6.
- Consumed by: Tasks 8-11, 14-17.

- [ ] **Step 1: Implement** (no TDD for the wiring itself — the pure parts landed tested in Tasks 5-6; this is composition). Follow the interface notes verbatim, especially lock discipline.
- [ ] **Step 2: Compile + dev smoke.** `cargo test -p paraeq-desktop` (still green), then `cd desktop/ui && npx tauri dev` and verify in the terminal log: app launches; with no settings file the engine spawns DISABLED (no tap engages — system audio untouched, no TCC prompt); quit with Cmd-Q exits cleanly (no hang = the controller thread joined).
- [ ] **Step 3: Temporary probe (optional but recommended):** from the webview devtools console run `window.__TAURI__.core.invoke('get_app_state').then(console.log)` — confirm the AppState JSON shape matches Task 1's pinned format (`engine.status.kind === "stopped"`, `engine.enabled === false`).
- [ ] **Step 4: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add desktop/src-tauri
git commit -m "feat(desktop): engine spawn honoring persisted state, snapshot forwarder with rate re-send, engine_*/eq_* commands"
```

---

### Task 8: src-tauri — minimal profile store + profiles_* commands

**Files:**
- Create: `desktop/src-tauri/src/profiles.rs`
- Modify: `desktop/src-tauri/src/lib.rs` (`mod profiles;` + register commands + populate `data.profiles` in `.setup`), `desktop/src-tauri/src/commands.rs`

**Interfaces:**
- `profiles.rs` (pure fs, no Tauri types — unit-testable):

```rust
/// Stage-4 minimal store (stage 5 adds the Profiles tab + WAV impulses on top
/// of THIS format — do not change it casually).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Profile { pub bands: Vec<EQBand>, pub name: String, pub preamp_db: f64 }

/// Lowercase; [a-z0-9] kept, everything else → '-'; collapse repeats; trim '-'.
/// Empty result → "profile".
pub fn slugify(name: &str) -> String
pub fn save_profile(dir: &Path, p: &Profile) -> std::io::Result<()>   // <dir>/<slug>.json, atomic like settings
pub fn load_profile(dir: &Path, name: &str) -> Option<Profile>        // by slug(name); corrupt → None + log::warn
pub fn list_profiles(dir: &Path) -> Vec<String>                       // display names read from files, sorted; missing dir → vec![]
```

- `lib.rs` setup addition (this task owns it — the module doesn't exist in Task 7): populate `data.profiles = profiles::list_profiles(&shared.profiles_dir)` BEFORE the first publish. Without it, saved profiles never load at relaunch: `Settings` carries no profile list, so `AppData::from_settings` leaves `profiles` empty while `active_profile` IS restored — the tray would show "(no profiles)" forever with an active profile that isn't in the list.
- Commands in `commands.rs`:
  - `profiles_save(name: String)` — reject empty/whitespace name; snapshot current `data.bands`/`data.preamp_db` into a `Profile`; `save_profile`; refresh `data.profiles`; set `data.active_profile = Some(name)`; publish.
  - `profiles_activate(name: String)` — `load_profile` (Err if missing); then EXACTLY the `eq_set_bands` apply path with the profile's bands + `engine_set_preamp_db` with its preamp; set `data.active_profile`; publish. (Prototype parity: activation loads bands into the EQ tab and applies.)
  - `profiles_list() -> Result<Vec<String>, String>`.
- Semantics that MUST hold (unit tests on the store): slugify cases (`"My AirPods Pro!" → "my-airpods-pro"`, `"---" → "profile"`, unicode stripped); save→list→load round-trip; two names colliding on slug — last write wins, documented (stage 5 owns collision UX); corrupt profile file → `None`, list skips it without panicking. Wiring property (Task 10 smoke verifies): after relaunch, saved profiles appear in `AppState.profiles`/the tray.
- Consumed by: Task 10 (tray submenu), Task 14 ("Save Profile…" button), Task 16.

- [ ] **Step 1: Failing tests** (`#[cfg(test)]` in `profiles.rs`, tempdir).
- [ ] **Step 2: Run to verify failure.** `cargo test -p paraeq-desktop` — compile error: `profiles` module missing.
- [ ] **Step 3: Implement** store + commands; register in `generate_handler!`; add the `list_profiles` call to `.setup` per the interface note.
- [ ] **Step 4: Run to verify pass.** `cargo test -p paraeq-desktop`.
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add desktop/src-tauri
git commit -m "feat(desktop): minimal profile store (JSON slug files) + profiles_* commands"
```

---

### Task 9: src-tauri — AutoEQ DB client (trait-seamed HTTP, cache-first) + autoeq_* commands

**Files:**
- Create: `desktop/src-tauri/src/autoeq.rs`, `desktop/src-tauri/tests/data/sample_index.md`, `desktop/src-tauri/tests/data/sample_preset.txt`
- Modify: `desktop/src-tauri/src/lib.rs` (`mod autoeq;` + commands), `desktop/src-tauri/src/commands.rs`, `desktop/src-tauri/Cargo.toml` (`percent-encoding = "2"`, `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }`)

**Interfaces:**
- **Behavioral oracle is `prototype/paraeq/correction/autoeq_db.py`** — READ IT FIRST; where this sketch and the prototype disagree on parsing/URL semantics, the prototype wins (spec:151 pins the feature list: INDEX.md index, both preset-filename casings, percent-encoding, cache-first under app-data, jsDelivr primary + raw-GitHub fallback, configurable base URL, pinned-commit option).

```rust
/// Seam identical in spirit to the prototype's patchable _http_get.
pub trait HttpFetch: Send + Sync {
    fn get(&self, url: &str) -> Result<String, String>;
}
pub struct ReqwestFetch { /* blocking-free: async client driven via the command layer,
                             or a small internal runtime handle — see Step 1 verification */ }

pub struct AutoEqClient<F: HttpFetch> {
    /// e.g. "https://cdn.jsdelivr.net/gh/jaakkopasanen/AutoEq@{rev}" primary,
    /// "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/{rev}" fallback;
    /// rev = pinned commit or default branch; base overridable (tests point at nothing).
    pub fn new(fetch: F, cache_dir: PathBuf, base_override: Option<String>, pinned_rev: Option<String>) -> Self
    /// Cache-first: serve <cache_dir>/INDEX.md if present unless force;
    /// on fetch, try primary then fallback, write cache atomically.
    pub fn sync_index(&self, force: bool) -> Result<Vec<IndexEntry>, String>
    /// Case-insensitive substring match on entry names.
    pub fn search<'a>(entries: &'a [IndexEntry], query: &str) -> Vec<&'a IndexEntry>
    /// Fetch preset text (cache-first): percent-encode path segments; try BOTH
    /// filename casings ("… ParametricEQ.txt" and "… ParametricEq.txt") ×
    /// (primary, fallback) before erroring.
    pub fn fetch_preset(&self, entry: &IndexEntry) -> Result<String, String>
}
/// The same model is measured by multiple sources/rigs in AutoEq's INDEX.md —
/// bare names are AMBIGUOUS. Carry source/rig (prototype parity:
/// AutoEQEntry keeps name/source/rig/rel_path, autoeq_db.py:40-45) and key
/// fetches by `path`, never by name. serde derives — this goes to the UI.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct IndexEntry { pub name: String, pub path: String, pub rig: String, pub source: String }
/// INDEX.md line parser — mirror the prototype's extraction exactly
/// (`- [Model](./source/rig/Model) by source on rig`; strip the leading "./";
/// skip non-matching lines — autoeq_db.py:16-18, 52-79).
pub fn parse_index(md: &str) -> Vec<IndexEntry>
```

- Commands (async — network must not block):
  - `async fn autoeq_sync_index(force: bool) -> Result<usize, String>` (entry count; index kept in `AppShared` or re-read from cache per call — implementer's choice, document it).
  - `async fn autoeq_search(query: String) -> Result<Vec<IndexEntry>, String>` (case-insensitive substring on `name`, prototype parity; full entries so the dialog can disambiguate duplicates; capped at 200).
  - `async fn autoeq_fetch_preset(path: String) -> Result<ParsedPresetDto, String>` — resolve the entry by `path` from the synced index (Err if unknown), fetch, then `ParsedPresetDto { bands: Vec<EQBand>, preamp_db: f64 }` = `paraeq_dsp::peq::parse_autoeq(&text)`; zero bands → `Err("Preset contains no parametric EQ filters")`. Fetch/parse ONLY — applying is the UI's explicit second step (Task 16).
- Semantics that MUST hold (unit tests, `MockFetch` recording requested URLs, no network): `parse_index` on `sample_index.md` (hand-written test data — copy a real ~20-line excerpt of AutoEq's `results/INDEX.md`, including at least one model that appears under TWO sources/rigs; this is NOT `fixtures/`, which stays oracle-generated) yields expected name/source/rig/path tuples, duplicates preserved as distinct entries; `fetch_preset` URL sequence tries casing A primary → casing B primary → casing A fallback → casing B fallback, stopping at first success; names with spaces/`#`/`+` are percent-encoded per segment (`/` preserved); cache hit issues ZERO fetch calls; primary failure + fallback success works; `sync_index(force=true)` refetches and rewrites cache.
- Consumed by: Task 16 (browse dialog).

- [ ] **Step 1: Verify reqwest-in-Tauri via context7 BEFORE writing `ReqwestFetch`:** `npx ctx7@latest library "reqwest" "async client inside tauri v2 async command tokio runtime"` (≤3 commands). Decide: async `reqwest::Client` awaited inside the async commands (preferred — Tauri commands already run on tokio), with `HttpFetch` kept sync for the pure logic and the async boundary living in the command layer, OR an async trait. Record the choice in a module doc comment.
- [ ] **Step 2: Failing tests** (`#[cfg(test)]` in `autoeq.rs` with `MockFetch { responses: HashMap<String, Result<String,String>>, log: Mutex<Vec<String>> }`; `include_str!` the two `tests/data/` files).
- [ ] **Step 3: Run to verify failure.** `cargo test -p paraeq-desktop` — compile error: `autoeq` module missing.
- [ ] **Step 4: Implement** client + commands. Cache under `app_data_dir()/autoeq/` (INDEX.md + presets mirrored by path). Keep `AutoEqClient` free of Tauri types.
- [ ] **Step 5: Run to verify pass.** `cargo test -p paraeq-desktop`. Optionally once, locally: a `#[ignore = "network"]` smoke test hitting the real CDN for one known preset.
- [ ] **Step 6: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add desktop/src-tauri Cargo.lock
git commit -m "feat(desktop): AutoEQ DB client (index, dual-casing fetch, percent-encoding, cache-first, CDN fallback) + autoeq_* commands"
```

---

### Task 10: src-tauri — tray, window lifecycle, single-instance, dialog plugin, exit teardown

**Files:**
- Create: `desktop/src-tauri/src/tray.rs`
- Modify: `desktop/src-tauri/src/lib.rs`, `desktop/src-tauri/src/engine_bridge.rs` (forwarder calls `sync_tray`), `desktop/src-tauri/Cargo.toml` (`tauri-plugin-dialog = "2"`, `tauri-plugin-single-instance = "2"`), `desktop/src-tauri/capabilities/default.json` (+ `"dialog:default"`), `desktop/src-tauri/tauri.conf.json` (window `width: 1024, height: 768, minWidth: 900, minHeight: 640`; drop the vestigial `bundle.android` block)

**Interfaces:**
- `tray.rs`:

```rust
/// Item handles the forwarder needs for live sync. Stored via app.manage().
pub struct TrayHandles {
    pub bypass: tauri::menu::CheckMenuItem<tauri::Wry>,
    pub toggle: tauri::menu::MenuItem<tauri::Wry>,      // "Enable EQ"/"Disable EQ"
    pub tray: tauri::tray::TrayIcon<tauri::Wry>,
    profile_names: std::sync::Mutex<Vec<String>>,        // last-built submenu, to detect rebuild need
}
/// Menu, exact order (decision 20): toggle-eq | bypass (check) | separator |
/// "Profile" submenu (CheckMenuItem per profile, id "profile:<name>", checked
/// = active; single disabled "(no profiles)" item when empty) | separator |
/// "Show Window" | "Quit ParaEQ". Icon: app.default_window_icon(). Left click
/// opens the menu (.show_menu_on_left_click(true)).
pub fn build_tray(app: &tauri::AppHandle, state: &AppState) -> tauri::Result<TrayHandles>
/// Called by the forwarder on every publish: toggle text ("Disable EQ" iff
/// engine.enabled), bypass.set_checked(engine.bypass), tooltip "ParaEQ" /
/// "ParaEQ (bypassed)", and IF profiles or active_profile changed → rebuild
/// the whole menu via tray.set_menu (coarse but simple).
pub fn sync_tray(app: &tauri::AppHandle, state: &AppState)
```

- Menu event handler (ids): `"toggle-eq"` → engine_enable/disable logic (reuse the command bodies via shared fns, don't `invoke` yourself); `"bypass"` → `SetBypass(checked)`; `"profile:<name>"` → profiles_activate logic; `"show"` → `get_webview_window("main")` → `show` + `unminimize` + `set_focus`; `"quit"` → `app.exit(0)`.
- `lib.rs` additions:
  - `.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| { /* show+focus main */ }))` — FIRST plugin.
  - `.plugin(tauri_plugin_dialog::init())` (registration only; JS usage is Task 14).
  - `.on_window_event(...)`: `CloseRequested` → `window.hide()` + `api.prevent_close()` (wildcard arm for the rest).
  - `run` closure now: `RunEvent::ExitRequested` → do NOT prevent (window close never reaches here; only real quit paths do); `RunEvent::Exit` → the Task 7 take-Disable-drop teardown; wildcard arm.
  - `.setup` builds the tray after the engine spawn (needs initial `AppState`).
- Threading note: `sync_tray` is called from the forwarder thread. **Verify via context7 whether menu-item mutation is main-thread-only in Tauri 2.11** (`npx ctx7@latest docs /websites/v2_tauri_app "update tray menu item from background thread set_checked"`); if it is, wrap the mutations in `app.run_on_main_thread(move || …)`. Record the answer in a comment.
- Consumed by: Tasks 11, 17, 18.

- [ ] **Step 1: Verify plugin registration + capability via context7** (`npx ctx7@latest library "tauri-plugin-dialog" "tauri v2 register plugin rust capability permission"`), then implement `tray.rs` + `lib.rs` wiring (no TDD — UI surface; the pure state logic is upstream).
- [ ] **Step 2: `cargo test -p paraeq-desktop`** still green (tray code compiles under clippy `-D warnings`).
- [ ] **Step 3: Manual smoke** (`cd desktop/ui && npx tauri dev`; TCC caveat from Global Constraints applies):
  1. Tray icon appears; left-click opens the menu with exact item order; profiles submenu shows "(no profiles)".
  2. "Show Window" after closing the window re-shows it (close hid it — app kept running, tray alive).
  3. Toggle "Enable EQ" → menu text flips to "Disable EQ" (engine may sit in `no_input_detected` without a TCC grant — fine; toggle back).
  4. Bypass checkmark toggles; tooltip flips to "ParaEQ (bypassed)" and back.
  5. Launch a second `npx tauri dev` instance → it exits/focuses the first (single-instance).
  6. Quit from the tray → process exits promptly (engine thread joined, no hang); system audio normal.
  7. Cmd-Q with the window focused → same clean exit.
- [ ] **Step 4: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add desktop/src-tauri Cargo.lock
git commit -m "feat(desktop): tray menu with live sync, hide-on-close, single-instance, dialog plugin, exit teardown"
```

---

### Task 11: UI — shadcn/Tailwind shell, wire types, state bridge, tab scaffold

**Files:**
- Create: `desktop/ui/src/ipc/types.ts`, `desktop/ui/src/ipc/commands.ts`, `desktop/ui/src/ipc/useAppState.ts`, `desktop/ui/src/tabs/PlaceholderTab.tsx`, shadcn-generated `desktop/ui/src/components/ui/*` + `desktop/ui/components.json`
- Modify: `desktop/ui/src/App.tsx` (replace stock landing page), `desktop/ui/index.html` (title "ParaEQ"), `desktop/ui/vite.config.ts` + `desktop/ui/tsconfig.json` + `desktop/ui/tsconfig.app.json` (path alias `@/` → `src/`; the scaffold is a project-references setup — `compilerOptions.paths` must land in `tsconfig.app.json` for `tsc -b` of app code, and the shadcn Vite guide adds it to both), `desktop/ui/package.json`
- Delete: `desktop/ui/src/App.css` and stock hero/logo assets

**Interfaces:**
- `ipc/types.ts` — hand-written mirror of the shapes pinned by Task 1's `test_wire_format.rs`, Task 2's golden band JSON, and Task 5's `app_state_wire_format_is_pinned` (a header comment MUST point at all three tests as the enforcement source):

```ts
export type FilterType = "high_shelf" | "low_shelf" | "notch" | "peaking";
export interface EQBand { fc: number; filter_type: FilterType; gain_db: number; q: number }
export type EngineStatus =
  | { kind: "auto_disabled_no_input"; after_ms: number }
  | { kind: "failed"; reason: string }
  | { kind: "idle"; since_ms: number }
  | { kind: "input_silent"; since_ms: number }
  | { kind: "no_input_detected"; since_ms: number }
  | { kind: "running" }
  | { kind: "starting"; since_ms: number }
  | { kind: "stopped" };
export interface StreamInfo { buffer_frames: number; channels: number; device_uid: string; sample_rate: number }
export interface EngineState {
  bypass: boolean; correction: string | null; enabled: boolean;
  frame_mismatch_blocks: number; gain_db: number; input_peak: number;
  latency_ms: number | null; status: EngineStatus; stream: StreamInfo | null;
}
export interface OutputDeviceInfo { name: string; uid: string }
export interface EqState { bands: EQBand[]; preamp_db: number }
export interface AppState {
  active_profile: string | null; default_output_uid: string | null;
  devices: OutputDeviceInfo[]; engine: EngineState; eq: EqState;
  profiles: string[]; setup_complete: boolean;
}
export interface IndexEntry { name: string; path: string; rig: string; source: string }
export interface ResponseData { composite: number[]; per_band: number[][]; sample_rate: number }
export interface ParsedPresetDto { bands: EQBand[]; preamp_db: number }
```

- `ipc/commands.ts` — one typed wrapper per Rust command (`export const eqSetBands = (bands: EQBand[]) => invoke<void>("eq_set_bands", { bands });` etc., alphabetical). NOTE Tauri arg-name convention: invoke args are matched to Rust parameter names — snake_case Rust params are exposed camelCase to JS by default; **verify against one real command in dev and write the mapping down in a comment** (this bites everyone once).
- `ipc/useAppState.ts`:

```ts
export function useAppState(): AppState | null {
  const [state, setState] = useState<AppState | null>(null);
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    (async () => {
      unlisten = await listen<AppState>("app-state", (e) => setState(e.payload));
      const initial = await invoke<AppState>("get_app_state");
      if (!cancelled) setState((s) => s ?? initial);   // an already-arrived event wins over the fetch
    })();
    return () => { cancelled = true; unlisten?.(); };
  }, []);
  return state;
}
```

  Register the listener FIRST, then fetch — the anti-pattern list explains why.
- `App.tsx`: shadcn `Tabs` with the prototype's exact tab order `Measure | Target | EQ | Analyzer | Profiles`; EQ renders a stub `<div>` until Task 14; the other four render `PlaceholderTab` ("Coming in stage 5"/"stage 6" per the boundary in Architecture #5). Single `useAppState()` at the top; pass slices down as props (React holds no forked state).
- Consumed by: Tasks 12-17.

- [ ] **Step 1: Verify the shadcn CLI flow via context7 BEFORE running it** (Tailwind v4 + React 19 + Vite 8 is exactly the combination that changed recently): `npx ctx7@latest library "shadcn/ui" "init with Vite Tailwind v4 React 19"`, then the docs call. Follow the CURRENT init instructions (expect: path-alias prereq in tsconfig + vite config, `npx shadcn@latest init`, then `npx shadcn@latest add button dialog input select table tabs`). If the CLI fights the scaffold, vendoring the six components by hand from the docs is acceptable — record which route was taken.
- [ ] **Step 2: Implement** types/commands/hook/shell; delete stock assets; title fix. (No TDD/unit tests this task — scaffold + declaration-only surface; the wire types are enforced by the Rust golden tests they mirror, typecheck is the gate, and the first pure TS logic arrives with Task 12's vitest.)
- [ ] **Step 3: Typecheck + build.** `cd desktop/ui && npx tsc -b && npm run build` — zero errors (TS strict).
- [ ] **Step 4: Manual smoke** (`npx tauri dev`):
  1. Window titled "ParaEQ", five tabs in prototype order, EQ selected by default is fine (pick EQ as default).
  2. Placeholders name their stage.
  3. Devtools console: no errors; `app-state` events arrive when toggling bypass from the tray, and the rendered state (a temporary `<pre>` dump on the EQ stub is a fine dev aid) updates live.
- [ ] **Step 5: Gates + commit.**

```bash
cd desktop/ui && npx tsc -b && npm run build && cd ../..
git add desktop/ui
git commit -m "feat(ui): shadcn/Tailwind shell, hand-written wire types, app-state bridge, tab scaffold"
```

---

### Task 12: UI — FreqPlotRenderer (pure canvas math) + vitest

**Files:**
- Create: `desktop/ui/src/plot/FreqPlotRenderer.ts`, `desktop/ui/src/plot/FreqPlotRenderer.test.ts`, `desktop/ui/vitest.config.ts`
- Modify: `desktop/ui/package.json` (devDep `vitest`, script `"test": "vitest run"`)

**Interfaces:**

```ts
export interface PlotRange { dbMax: number; dbMin: number; fMax: number; fMin: number }
export const EQ_PLOT_RANGE: PlotRange = { dbMax: 20, dbMin: -20, fMax: 20000, fMin: 20 }; // prototype parity
export const BAND_PALETTE = ["#EF5350", "#FFA726", "#FFEE58", "#66BB6A",
                             "#26C6DA", "#42A5F5", "#AB47BC", "#EC407A"];   // prototype palette, cycled
export interface Trace { color: string; dash?: number[]; dbs: number[]; freqs: number[]; width: number }
export interface PlotHandle { color: string; db: number; f: number; id: number }

/// n log-spaced points from fMin..fMax inclusive (the response grid; 512 = prototype parity).
export function logspace(fMin: number, fMax: number, n: number): number[]

export class FreqPlotRenderer {
  constructor(range: PlotRange)
  setSize(cssWidth: number, cssHeight: number): void   // CSS px; margins internal (left ~44, bottom ~24, top/right ~8)
  // pure transforms (CSS px):
  xForFreq(f: number): number;  freqForX(x: number): number
  yForDb(db: number): number;   dbForY(y: number): number
  freqTicks(): { f: number; label: string }[]  // FIXED audio list: 20,50,100,200,500,1k,2k,5k,10k,20k
  dbTicks(step: number): number[]              // e.g. step 5 → -20..20
  // drawing (ctx already DPR-scaled by the wrapper):
  drawGrid(ctx: CanvasRenderingContext2D): void
  drawTraces(ctx: CanvasRenderingContext2D, traces: Trace[]): void
  drawHandles(ctx: CanvasRenderingContext2D, handles: PlotHandle[], activeId: number | null): void
  hitTest(x: number, y: number, handles: PlotHandle[], radiusPx?: number): number | null  // nearest within radius (default 10), else null
  clampToRange(f: number, db: number): { db: number; f: number }
}
```

- Semantics that MUST hold (each a vitest case): `xForFreq(fMin)` = plot-area left, `xForFreq(fMax)` = right; `freqForX(xForFreq(f)) ≈ f` (1e-9 rel) across a log sweep; `xForFreq(1000)` equals the analytic value `left + w·(log10(1000/20)/log10(20000/20))`; `yForDb(0)` is the vertical center for the symmetric range; `freqTicks()` returns exactly the fixed 10-tick list with labels `"20","50","100","200","500","1k","2k","5k","10k","20k"`; `logspace(20, 20000, 512)` has 512 points, endpoints exact, ratio between neighbors constant (1e-9 rel); `hitTest` picks the nearest handle within radius, returns null outside, prefers nearest when two overlap; `clampToRange` clamps both axes.
- Drawing methods are exercised by typecheck + Task 13's manual smoke (no canvas in CI) — keep ALL math in the pure methods so the drawing bodies are trivial loops.
- Consumed by: Tasks 13, 14, 15 (and stages 5/6 — target anchors, analyzer traces reuse this class; keep it EQ-agnostic).

- [ ] **Step 1: Set up vitest.** Verify current config via context7 if anything surprises (`npx ctx7@latest library "Vitest" "config with Vite 8 TypeScript"`); minimal `vitest.config.ts` (node environment — the tests are pure math, no jsdom needed). Confirm `npx tsc -b` still passes with test files present (add `types: ["vitest/globals"]` or use explicit imports — prefer explicit `import { describe, expect, it } from "vitest"`, zero config).
- [ ] **Step 2: Failing tests** per the semantics list.
- [ ] **Step 3: Run to verify failure.** `cd desktop/ui && npx vitest run` — module not found.
- [ ] **Step 4: Implement.** Transforms: `x = left + w·(log10(f) − log10(fMin))/(log10(fMax) − log10(fMin))`; inverse via `10^(…)`; y linear in dB.
- [ ] **Step 5: Run to verify pass.** `npx vitest run` green; `npx tsc -b && npm run build` green.
- [ ] **Step 6: Gates + commit.**

```bash
cd desktop/ui && npx tsc -b && npm run build && npx vitest run && cd ../..
git add desktop/ui
git commit -m "feat(ui): FreqPlotRenderer pure canvas math (log-x transforms, fixed audio ticks, hit-test) + vitest"
```

---

### Task 13: UI — FrequencyPlot.tsx canvas wrapper

**Files:**
- Create: `desktop/ui/src/components/FrequencyPlot.tsx`

**Interfaces:**

```tsx
export interface FrequencyPlotProps {
  handles?: PlotHandle[];
  onHandleDrag?: (id: number, f: number, db: number, phase: "move" | "end" | "start") => void;
  onHandleWheel?: (id: number, deltaY: number) => void;
  range: PlotRange;
  title?: string;
  traces: Trace[];
}
export function FrequencyPlot(props: FrequencyPlotProps): JSX.Element
```

- Structure (decision 3): a relatively-positioned container div holding TWO stacked absolutely-positioned canvases — base (grid + traces; redrawn only when `traces`/size change) and overlay (handles + drag feedback; redrawn via a dirty-flag rAF loop). One `FreqPlotRenderer` instance in a ref, shared by both.
- Sizing/DPR: `ResizeObserver` on the container; on resize set each canvas `width/height = cssSize × devicePixelRatio`, `ctx.setTransform(dpr, 0, 0, dpr, 0, 0)`, `renderer.setSize(cssW, cssH)`, redraw both layers. WKWebView DPR is stable per display but listen anyway via the observer re-fire.
- Pointer protocol (consumed by Task 15; wire it now, EQ tab passes no handles until then): `onPointerDown` → `renderer.hitTest` → if hit: `setPointerCapture`, fire `onHandleDrag(id, f, db, "start")`; `onPointerMove` while captured → map through `freqForX`/`dbForY`, `clampToRange`, fire `"move"` at most once per rAF frame; `onPointerUp` → release, fire `"end"`. `onWheel` over a hit handle → `preventDefault()`, fire `onHandleWheel(id, e.deltaY)`. No hit → default behavior (no pan/zoom in stage 4).
- React discipline: the component NEVER stores traces/handles in state — props in, imperative canvas out; `useEffect` diffs redraw the layers. All math lives in the renderer (tested in Task 12); this file is deliberately dumb plumbing.
- Consumed by: Tasks 14, 15 (and stages 5/6).

- [ ] **Step 1: Implement** (no unit tests — canvas surface; the math underneath is vitest-covered).
- [ ] **Step 2: Typecheck.** `cd desktop/ui && npx tsc -b && npm run build`.
- [ ] **Step 3: Manual smoke** (temporarily mount in the EQ stub with a sine-shaped fake trace):
  1. Grid renders with the 10 audio ticks + dB lines; labels legible at both 1× and Retina (no blur — DPR handling works).
  2. Window resize re-renders sharply, no smearing or aspect distortion.
  3. Trace draws smoothly edge-to-edge.
  4. Remove the temporary mount before committing.
- [ ] **Step 4: Gates + commit.**

```bash
cd desktop/ui && npx tsc -b && npm run build && npx vitest run && cd ../..
git add desktop/ui
git commit -m "feat(ui): FrequencyPlot wrapper (two-layer canvas, ResizeObserver, DPR, pointer protocol)"
```

---

### Task 14: UI — EQ tab: band table, preamp, status strip, plot wiring, import/export, Save Profile

**Files:**
- Create: `desktop/ui/src/tabs/EqTab.tsx`
- Modify: `desktop/ui/src/App.tsx` (mount it), `desktop/ui/src/ipc/commands.ts` (wrappers for this task's commands), `desktop/ui/package.json` (`npm install @tauri-apps/plugin-dialog`), `desktop/src-tauri/src/commands.rs` (the two new commands below), `desktop/src-tauri/src/lib.rs` (register them in `generate_handler!`)

**Interfaces:**
- Layout, top to bottom (prototype parity, `prototype/app/eq/manual_eq_editor.py`):
  1. **Toolbar:** `Add Band`, `Remove Band`, spacer, `Save Profile…`, `Browse AutoEQ DB…` (disabled until Task 16 wires it), `Import AutoEQ…`, `Export AutoEQ…`.
  2. **Band table** (shadcn Table): columns `Type | Freq (Hz) | Gain (dB) | Q`. Type = shadcn Select with exactly `peaking, low_shelf, high_shelf, notch` in that order; Freq/Gain/Q = numeric inputs displaying `%.1f / %.1f / %.3f`. Row click selects (single selection). Gain stays editable for notch (parity; the engine ignores it — tooltip notes this).
  3. **Preamp row:** label + number input, −30..+10, step 0.5 (decision 2 — the master-volume slider from the prototype is GONE, deliberately).
  4. **Plot:** `FrequencyPlot` with `EQ_PLOT_RANGE`; traces = composite (white `#FFFFFF`, width 2.5, solid) + per-band (BAND_PALETTE cycled, width 1, `dash: [2, 3]`), grid `logspace(20, 20000, 512)`.
  5. **Status strip:** honest engine line, e.g. `Engine: Running · 62.3 ms · 48 kHz · stereo` from `engine.status.kind`/`latency_ms`/`stream`; when `status.kind === "no_input_detected"` append the hint "check System Audio Recording permission, or play some audio"; `"auto_disabled_no_input"`/`"failed"` render an inline `Enable` retry button (the ONLY way out — engine contract); when `frame_mismatch_blocks > 0` append "degraded (frame mismatch)"; when `bypass` show "bypassed".
- Behavior (Rust owns state — every edit is a command, no local band state):
  - Table edit commit (blur/Enter) → build the new band list from `state.eq.bands` + the edit → `eqSetBands`. Invalid input → red toast/inline error with the Rust error string; the table re-renders from unchanged AppState (no client-side drift). NOTE divergence from prototype: malformed rows are REJECTED loudly, not silently skipped — Rust-side validation makes silent-skip impossible; this is the better behavior and is documented here.
  - Add/Remove → `eqAddBand()` / `eqRemoveBand(selectedIndex ?? null)` (null → Rust removes last; parity).
  - Preamp change → `engineSetPreampDb(value)`.
  - Curve refresh: `useEffect` on `state.eq.bands` + `state.engine.stream?.sample_rate` → `eqResponse(logspace(20, 20000, 512))` → set traces. (Live-rate recompute is the documented divergence from the prototype's fixed 48 k — decision 12.)
  - Import: verify `@tauri-apps/plugin-dialog` JS API via context7 (`npx ctx7@latest library "tauri-plugin-dialog" "javascript open save file dialog"`), then `open({ filters: [{ name: "AutoEQ preset", extensions: ["txt", "csv"] }] })` → `invoke("eq_import_autoeq", { path })`. New Rust command in `commands.rs`: `eq_import_autoeq(path: String)` — read file, `parse_autoeq`, zero bands → `Err("No AutoEQ filter lines found in the file.")` (prototype's exact message); else REPLACE bands via the apply path AND apply the parsed preamp **clamped into [PREAMP_MIN_DB, PREAMP_MAX_DB]** via the preamp path (decision 2 fixes the prototype's dropped-preamp asymmetry). The clamp is the SAME policy Task 16's DB-browser apply uses — a wild file preamp must not turn into a rejected-after-bands-applied partial apply; return the clamped value/flag so the UI can note it.
  - Export: `save({ defaultPath: "eq_preset.txt", … })` → `invoke("eq_export_autoeq", { path })`. New Rust command: `eq_export_autoeq(path: String)` — write `ParametricEQ::export_autoeq_format(data.preamp_db)` for the current bands.
  - Save Profile: name prompt (shadcn Dialog + input) → `profilesSave(name)`.
- Rust-side additions this task: `eq_import_autoeq` + `eq_export_autoeq` commands (thin — parse/format is dsp, apply-path exists), registered in `generate_handler!` and capability-safe (paths come from the native dialog).
- Consumed by: Tasks 15, 16.

- [ ] **Step 1: Rust commands, failing tests first** (per Testing policy — these are src-tauri logic, not UI surface): unit tests for the zero-bands error message and the out-of-range-preamp clamp, red → implement the two thin commands → `cargo test -p paraeq-desktop` green.
- [ ] **Step 2: Implement the tab** per the layout/behavior lists.
- [ ] **Step 3: Typecheck + build.** `cd desktop/ui && npx tsc -b && npm run build`.
- [ ] **Step 4: Manual smoke** (`npx tauri dev`, TCC granted, music playing — **this is the "first audible corrected audio in the real app" moment**):
  1. Enable EQ from the tray; status strip reaches `Running` with a real latency figure and the live sample rate.
  2. Add Band → row appears with `peaking / 1000.0 / 0.0 / 1.41`; composite stays flat (0 gain).
  3. Set gain to +9, fc to 80 → bass boost is AUDIBLE within a beat; composite + band-1 curves redraw.
  4. Type `0` into Q → loud inline error naming band 1 / q; table value snaps back; audio unchanged.
  5. Remove Band with nothing selected removes the LAST row; with a row selected removes that row; empty table → flat passthrough (audibly).
  6. Preamp −10 → obvious volume drop; hardware volume keys + HUD still work on top (product requirement, CONTEXT.md:81).
  7. Export to a file; open it: first line `Preamp: -10.0 dB`, filter lines `Filter 1: ON PK Fc 80 Hz …`. Re-import the same file → identical table AND preamp −10 restored (the fixed asymmetry).
  8. Import a file with no filter lines → the exact "No AutoEQ filter lines found in the file." error; table untouched.
  9. Save Profile "Test" → tray profile submenu now lists it with a checkmark.
  10. In Audio MIDI Setup, flip the output device between 44.1/48 kHz mid-playback → status strip shows the new rate and **EQ character is unchanged** (the forwarder re-designed and re-sent — the stage-3 carry-forward proven).
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cd desktop/ui && npx tsc -b && npm run build && npx vitest run && cd ../..
git add desktop/src-tauri desktop/ui
git commit -m "feat(desktop,ui): EQ tab at parity+ (band table, preamp, live-rate curve, AutoEQ import/export, save profile)"
```

---

### Task 15: UI — draggable band handles on the curve

**Files:**
- Modify: `desktop/ui/src/tabs/EqTab.tsx`, `desktop/ui/src/plot/FreqPlotRenderer.test.ts` (if any new pure math is added)

**Interfaces:**
- Handles: one per band, `PlotHandle { id: bandIndex, f: band.fc, db: band.gain_db, color: BAND_PALETTE[i % 8] }` — same palette as the band's curve. Active/hover handle draws larger (renderer's `activeId` param, already built in Task 12).
- Drag semantics (decision 13 — NEW UX, the prototype had none; this section is the reference):
  - `"start"`: snapshot the band list; enter drag mode (local `draggingBands` state — the ONE sanctioned piece of ephemeral forked state, per spec:135's "ephemeral drag interactions").
  - `"move"` (rAF-throttled by the wrapper): update `draggingBands[id].fc = clamp(f, 20, 20000)` (2 decimals) and `.gain_db = clamp(db, -20, 20)` (1 decimal — plot range bound; the table still accepts up to ±30 by typing); render table + handles from `draggingBands`; refresh the curve via `eqResponse` with the dragged bands (round-trip is microseconds-cheap — decision 12; only add a local approximation if this measurably lags); **throttle engine application**: at most one `eqSetBands(draggingBands)` per 100 ms during the drag (live audible sculpting without command flooding).
  - `"end"`: final `eqSetBands(draggingBands)`, clear drag state — AppState becomes the single truth again.
  - Wheel over a handle: `q *= Math.pow(1.05, -Math.sign(deltaY))`, clamped to [0.1, 20], one `eqSetBands` per wheel event (they're discrete).
- Notch bands: handle y follows `gain_db` like any other (parity: the gain cell stays live even though notch ignores it) — the curve simply won't move vertically; acceptable, documented here.
- Consumed by: Task 18 acceptance.

- [ ] **Step 1: Implement** (drag surface = manual smoke; any NEW pure helper — e.g. a `snapDb`/rounding fn — gets a vitest case).
- [ ] **Step 2: Typecheck + tests.** `cd desktop/ui && npx tsc -b && npm run build && npx vitest run`.
- [ ] **Step 3: Manual smoke** (music playing):
  1. Handles sit exactly on (fc, gain) of each band, colored like their curves.
  2. Drag band 1 up-right → table fc/gain update live, curve follows, audio sweeps audibly during the drag (throttled apply working).
  3. Release → table, curve, and audio agree; `get_app_state` (devtools) shows the final values.
  4. Drag past the plot edges → values pin at 20 Hz/20 kHz/±20 dB, no jumps.
  5. Wheel over a handle → Q column changes multiplicatively; curve narrows/widens; wheel elsewhere scrolls nothing (preventDefault only on hit).
  6. During a fast drag, no error toasts (validation never sees out-of-clamp values) and no UI freeze.
- [ ] **Step 4: Gates + commit.**

```bash
cd desktop/ui && npx tsc -b && npm run build && npx vitest run && cd ../..
git add desktop/ui
git commit -m "feat(ui): draggable band handles (fc/gain drag, wheel-Q, throttled live apply)"
```

---

### Task 16: UI — AutoEQ DB browse dialog

**Files:**
- Create: `desktop/ui/src/dialogs/AutoEqBrowser.tsx`
- Modify: `desktop/ui/src/tabs/EqTab.tsx` (enable the toolbar button), `desktop/ui/src/ipc/commands.ts`

**Interfaces:**
- shadcn Dialog opened by `Browse AutoEQ DB…`: search input (debounced ~200 ms → `autoeqSearch(query)` → `IndexEntry[]`), scrollable result list rendering the prototype's disambiguated row format `"{name}  —  {source} / {rig}"` (autoeq_browser.py:141 — the same model appears under multiple sources/rigs; bare names cannot disambiguate; cap 200 per Task 9), a `Sync index` button (→ `autoeqSyncIndex(true)`, shows entry count / error), and per-selection footer: `Apply`.
- First open with no cached index → auto-call `autoeqSyncIndex(false)`; network failure renders the error string inline with a Retry button (the client already fell back jsDelivr→raw before erroring).
- Apply flow: `autoeqFetchPreset(entry.path)` (the selected ENTRY, keyed by path — Task 9) → `ParsedPresetDto` → `eqSetBands(dto.bands)` AND `engineSetPreampDb(clamped(dto.preamp_db))` (clamp into −30..+10, note in UI if clamped — the SAME clamp policy as Task 14's file import) → close dialog → toast "Applied <name> (N bands, preamp X dB)". Zero-band presets never reach here (Task 9 errors them); the error toast shows the Rust message. This is the DB-browser preamp-apply path at parity (prototype: `preamp_changed` emit), now shared with file import.
- Consumed by: Task 18 acceptance.

- [ ] **Step 1: Implement** dialog + wrappers (no unit tests — dialog surface; the search/fetch/parse logic underneath is Task 9's tested client).
- [ ] **Step 2: Typecheck + build.** `cd desktop/ui && npx tsc -b && npm run build`.
- [ ] **Step 3: Manual smoke** (network required once; TCC + music for the audible part):
  1. First open syncs the index; entry count appears; `~/Library/Application Support/com.paraeq.desktop/autoeq/INDEX.md` exists.
  2. Search "HD 650" (or any known model) filters live; nonsense query → empty state, no error.
  3. Apply a preset → table fills, preamp updates, sound changes audibly; tray/profile state untouched.
  4. Re-open offline (Wi-Fi off) → cached index still browses; fetching an uncached preset errors gracefully with the client's message; cached preset applies fine.
  5. Kill the dialog mid-fetch → no crash, no stuck state.
- [ ] **Step 4: Gates + commit.**

```bash
cd desktop/ui && npx tsc -b && npm run build && npx vitest run && cd ../..
git add desktop/ui
git commit -m "feat(ui): AutoEQ DB browse dialog (search, cache-aware sync, apply with preamp)"
```

---

### Task 17: Setup wizard + deterministic chime probe (backend commands + UI)

**Files:**
- Create: `desktop/ui/src/wizard/SetupWizard.tsx`
- Modify: `desktop/src-tauri/src/commands.rs` (+ probe state in `state.rs` if needed), `desktop/src-tauri/src/lib.rs` (register the `setup_*` commands in `generate_handler!`), `desktop/ui/src/App.tsx` (wizard gate), `desktop/ui/src/ipc/commands.ts`

**Interfaces:**
- Rust commands (`setup_*` group):
  - `setup_probe_start()` — idempotent. Spawns a `std::thread` looping (max ~12 s or until stopped): `std::process::Command::new("afplay").arg("/System/Library/Sounds/Glass.aiff")` spawn + wait, small sleep between plays. Pattern: `crates/paraeq-coreaudio/tests/test_hardware.rs:19-51` — READ IT; child playback needs no TCC grant, and a helper process is REQUIRED because ParaEQ's own process is excluded from its tap by design. Keep an `Arc<AtomicBool>` stop flag + `Mutex<Option<Child>>` for kill-on-stop in `AppShared`.
  - `setup_probe_stop()` — set flag, kill any live child, join politely.
  - `setup_open_privacy_settings()` — `Command::new("open").arg(PRIVACY_URL)` where `PRIVACY_URL` targets Privacy & Security → Screen & System Audio Recording. **Verify the exact anchor at execution** by running locally: `open "x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture"` — if it doesn't land on the right pane on macOS 14/15, fall back to the plain Privacy & Security root URL and note it.
  - `setup_complete()` — `data.setup_complete = true`, publish (persists).
- UI: `App.tsx` renders `SetupWizard` INSTEAD of the tabs while `state.setup_complete === false` (closing the window mid-wizard just hides it — Task 10; the engine stays disabled; re-showing resumes the wizard). Four steps:
  1. **Welcome/explain** — what ParaEQ does; that clicking Enable will trigger macOS's one-time "System Audio Recording" permission prompt; that a granted permission requires an app relaunch to take effect (TCC reality).
  2. **Output device** — Select fed by `state.devices` (call `engineListOutputs()` on entry), current default preselected; changing it calls `engineSetDefaultOutput(uid)`. "Next" always enabled (parity with the prototype's tolerant wizard).
  3. **Enable + verify capture** — "Enable EQ" button → `engineEnable()` then `setupProbeStart()`. Watch `state.engine.status` live: `kind === "running"` → probe stop, success panel, auto-advance. If `no_input_detected` persists ~10 s (client-side timer; deliberately < the 15 s fail-open) OR `kind === "auto_disabled_no_input"` → guidance panel: the permission is likely missing; buttons `Open Privacy Settings` (→ `setupOpenPrivacySettings()`), and `Re-test` (→ `engineEnable()` — clears the fail-open latch, engine contract — then `setupProbeStart()` again); remind about the relaunch-after-grant requirement. `failed` → show `status.reason` + Re-test.
  4. **Done** — `setupComplete()`; tabs appear. Wizard never blocks: a "Skip for now" link on step 3 also calls `setupComplete()` (engine stays disabled; the tray/EQ tab can enable later — decision 1's flow still holds because enabled-state persistence is by explicit user action).
- Semantics that MUST hold: probe stop always kills the child (no orphaned afplay looping Glass.aiff forever — also stop it on wizard unmount and on `setup_complete`); `engine_enabled` persists as true after a successful step 3, so next launch auto-enables (decision 1).
- Consumed by: Task 18 acceptance.

- [ ] **Step 1: Rust probe commands** (unit-test the stop-flag/kill bookkeeping where pure; the spawn path is smoke-tested): `cargo test -p paraeq-desktop`.
- [ ] **Step 2: Implement the wizard UI.**
- [ ] **Step 3: Typecheck + build.** `cd desktop/ui && npx tsc -b && npm run build`.
- [ ] **Step 4: Manual smoke — the full first-run theater, twice:**
  1. Delete `~/Library/Application Support/com.paraeq.desktop/` and (System Settings) remove any existing System Audio Recording grant for the terminal/app if practical.
  2. Launch → wizard shows, tabs hidden; system audio completely untouched (engine disabled — no mute, no prompt yet).
  3. Step 2 lists real output devices with the current default preselected; switching devices audibly moves system audio.
  4. Step 3 "Enable EQ" → chime loops audibly → within ~10 s either `Running` + success, or (no grant) the guidance panel with working `Open Privacy Settings` deep link. Grant → relaunch app → wizard resumes (setup incomplete), Re-test → success.
  5. Step 4 → tabs appear; quit; relaunch → NO wizard, engine auto-enables (persisted), status reaches Running with music.
  6. Second pass: fresh state, step 3 → "Skip for now" → tabs appear with engine disabled; tray Enable works later.
  7. At no point does an afplay loop keep playing after the wizard step ends (check `pgrep afplay`).
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
cd desktop/ui && npx tsc -b && npm run build && npx vitest run && cd ../..
git add desktop/src-tauri desktop/ui
git commit -m "feat(desktop,ui): setup wizard with deterministic afplay chime probe, device pick, TCC guidance"
```

---

### Task 18: CI, docs, and the stage-4 acceptance run

**Files:**
- Modify: `.github/workflows/ci.yml` (frontend job gains `- run: npx vitest run` with `working-directory: desktop/ui`, after the build step), `docs/CONTEXT.md` (stage-4 status), `crates/paraeq-dsp/DIVERGENCES.md` (only if implementation surfaced new ones)

- [ ] **Step 1: CI.** Add the vitest step; confirm the rust job already covers src-tauri (workspace member — it does; no change needed).
- [ ] **Step 2: CONTEXT.md.** Replace the stage-4 "Next:" line and carry-forward block with a completion entry: stage 4 complete — Tauri shell (tray-resident, single-instance, wizard-gated launch flow), EQ tab end-to-end (validated bands → live-rate SOS → SetCorrection; rate-change re-send OWNED by the desktop forwarder — carry-forward closed), AutoEQ import/export + DB client, minimal profile store + tray switching, hand-rolled FrequencyPlot. Record: the deliberate divergences (preamp round-trip; live-rate curve; loud-reject band validation), **the single-processor contract for the stage-5 planner (decision 26: one correction slot, last-applied wins; stage 5's FIR preview takes the same slot via the same commands)**, the deferrals list verbatim from Architecture (spectrum Channel → stage 6; **mid-run TCC revocation prompt → post-parity, with the undetectability rationale**; etc.), and "Next: stage 5 — Target editor + profiles (tab UI, WAV impulses, target math wiring)".
- [ ] **Step 3: Final whole-workspace gates.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
.venv/bin/pytest prototype/tests -q                      # oracle untouched and green
cd desktop/ui && npx tsc -b && npm run build && npx vitest run && cd ../..
cargo test -p paraeq-coreaudio -- --ignored              # local hardware suite, incl. Task 4's device tests
```

- [ ] **Step 4: Stage-4 acceptance — "first audible corrected audio in the real app"** (owner/manual; run against a real `npx tauri dev` or a `tauri build` .app; record results in the task report). This list deliberately includes the outstanding stage-3 ears-on items (CONTEXT.md:114) — stage 4 is where they get exercised in-app:
  1. Fresh first launch → wizard → grant flow → `Running` (Task 17's theater condensed).
  2. Music + a +9 dB @ 80 Hz band: correction is UNMISTAKABLY audible. ✅ = the stage-4 acceptance bar (spec:196).
  3. Tray Bypass on/off: instant A/B, no click/pop (bypass-edge state reset working through the real stack).
  4. Hardware volume keys + on-screen HUD work while processing (CONTEXT.md:81 product requirement).
  5. Close window → audio keeps processing; tray Show Window restores.
  6. Tray Quit → system audio INSTANTLY normal (Disable → drop teardown). Relaunch → auto-enabled, correction AND preamp restored from settings (the Task 7 `SetGainDb`-at-setup path), saved profiles listed in the tray submenu (the Task 8 setup population).
  7. Ctrl-C the `tauri dev` process → system audio restored. NOTE: this is NOT the in-process teardown — no signal handler is installed, default SIGINT death runs no destructors; like kill -9, it relies on the OS dropping the tap with the process (decision 21; document observed behavior).
  8. `kill -9` the app process → system audio still OK (tap dies with the process — the same OS-level guarantee; document observed behavior).
  9. Sample-rate flip mid-playback (Audio MIDI Setup 44.1↔48) → EQ character unchanged (re-send proven end-to-end).
  10. Device switch mid-playback (e.g. to AirPods): engine rebuilds and follows; **mic-capable default output KNOWN LIMITATION check** (backend.rs:121-130): verify tap input isn't polluted by the mic (EQ applies to music, no feedback/garbling). Record the outcome — this is the owner-hardware test CONTEXT.md has been waiting on.
  11. Profile round-trip: save "Desk", tweak bands, tray-switch back to "Desk" → bands + preamp + audio revert.
  12. AutoEQ DB preset applied → audibly different; export → import round-trips preamp.
- [ ] **Step 5: Commit** (two commits — conventional-commit types don't join; the exemplar's final task is a plain `docs:`):

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run vitest in the frontend job"
git add docs/CONTEXT.md crates/paraeq-dsp/DIVERGENCES.md
git commit -m "docs: stage-4 status (acceptance results, divergences, deferrals)"
```

---

## Completion Checklist (whole plan)

- [ ] **The acceptance bar:** first audible corrected audio in the real app (Task 18 item 2), plus the full 12-point acceptance list executed and recorded.
- [ ] Stage-3 carry-forward CLOSED: rate-change coefficient re-send lives in the desktop forwarder (`eq::resend_decision`, unit-tested; proven end-to-end in Task 18 item 9).
- [ ] Launch flow: engine NEVER spawns enabled before the wizard; enabled-state persists and restores; fail-open sessions still restore as "on" (decision 1 semantics in Tasks 1/7/17).
- [ ] Never-leave-muted invariant: every orderly quit path (tray Quit, Cmd-Q) sends `Disable` and drops `EngineHandle` (joins) via `RunEvent::Exit`; Ctrl-C/SIGINT and `kill -9` deliberately install NO handler and rely on the OS dropping the tap with the process (stage-3 design — default signal death runs no destructors, decision 21); verified by Task 10 smoke + Task 18 items 6-8. No `ManuallyDrop`/static leak anywhere (grep the branch).
- [ ] Crate boundaries hold: `paraeq-engine` has zero Tauri deps; ALL new unsafe FFI (devices.rs) is in `paraeq-coreaudio` with SAFETY comments; `paraeq-dsp` gained only serde/regex (pure). No ndarray/scirs2/fundsp anywhere.
- [ ] Wire format pinned: `test_wire_format.rs` (engine) + dsp golden band JSON + `app_state_wire_format_is_pinned` (src-tauri, Task 5) are the enforcement points for the hand-written `ipc/types.ts` — every interface in it is covered by one of the three (header comment cross-references in place).
- [ ] Fixtures discipline: `fixtures/autoeq_parser.json` generated only by the extended `generate_fixtures.py`, committed together with the script; oracle pytest green throughout.
- [ ] Validation wall: no band/preamp value reaches `to_sos`/`SetGainDb` unvalidated — including the forwarder's rate-resend/first-stream path (re-validated at the NEW rate, `ClearCorrection` on failure), the persisted preamp (clamped at setup), and both AutoEQ apply paths (same clamp policy). Task 6 consts are the single source of the limits; UI clamps mirror them.
- [ ] Deferrals stated, not dropped: spectrum Channel + analyzer ring (stage 6), Profiles tab UI + WAV impulses + target editor (stage 5), Accessory mode / template tray icon / autostart / updater / signing / CSP hardening (post-parity or ship stage) — all recorded in CONTEXT.md by Task 18.
- [ ] CI green without hardware or network: vitest step added; AutoEQ client tests are seam-mocked; coreaudio hardware tests stay `#[ignore]` (run locally in Task 18 step 3).
- [ ] Merge per superpowers:finishing-a-development-branch; next plan = **stage 5 — Target editor + profiles** (anchors + deviation model + live min-phase FIR preview taking the single correction slot, Profiles tab UI + WAV impulse storage over Task 8's store format, AutoEQ target-side reuse of Task 9's client).
