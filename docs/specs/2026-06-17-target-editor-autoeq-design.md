# Target Curve Editor Interactivity + AutoEQ Database Browser — Design Spec

**Date:** 2026-06-17
**Status:** Approved
**Author:** Ronak Patel (with Claude)

## Overview

This spec covers the two remaining **functionality** items in ParaEQ's Phase 1 (the
installer/packaging items are explicitly out of scope — the Python app is a throwaway
prototype ahead of the Rust port, so we invest only in correct functionality and in
capturing port-relevant knowledge):

1. **Interactive target curve editor** — draggable control points, live audio preview while
   dragging, and an "auto-match closest preset" helper.
2. **AutoEQ database browser** — a searchable headphone-model picker that loads parametric
   EQ presets from the AutoEq project on demand.

Both features build on existing modules; no new third-party dependencies are introduced
(`urllib.request` from the stdlib handles networking).

### Existing code this builds on

- `app/editor/target_curve_editor.py` — `TargetCurveEditor(QWidget)`. Log-x pyqtgraph plot
  with three non-interactive traces (measured / target / correction-delta). Holds the active
  target in `self._target_curve` (a `TargetCurve`). Emits `correction_generated(dict)` only on
  the explicit Generate FIR / Generate PEQ buttons. Receives the measurement via
  `set_measurement(freqs, magnitude_db)`. **No mouse/drag interaction exists today.**
- `paraeq/correction/target_curves.py` — `TargetCurve` dataclass (`name`, `frequencies`,
  `gains_db`, optional `category`/`description`/`source`); `interpolate()` uses `CubicSpline`
  in log10-frequency space with constant boundary hold; `compute_correction()` =
  `target_db - measured_db`; `list_builtin_targets()` loads the `targets/*.csv` presets.
  Built-in presets are dense (695 points); `flat.csv` is 2 points.
- `app/eq/manual_eq_editor.py` — `ManualEQEditor`. Table-driven PEQ editor; module-level
  `_parse_autoeq(text)` regex parser (currently **drops the Preamp line** and only the
  `PK/LSC/HSC/NO` codes); toolbar with Import/Export AutoEQ buttons; emits `eq_changed(list)`
  on every change → `MainWindow._on_eq_changed` → `_update_iir_processor` → live audio engine.
- `paraeq/correction/fir_filter.py` — FIR design (linear + minimum phase). Min-phase requires
  squaring the magnitude before `scipy.signal.minimum_phase` (homomorphic method takes sqrt).
- `paraeq/correction/parametric_eq.py` — `EQBand`, `ParametricEQ`, `export_autoeq_format()`.
- `paraeq/engine/convolver.py` — overlap-add FIR convolver.
- `AudioEngine.set_processor(callable)` (`paraeq/audio/stream.py`) — the single live-audio
  processor hook, shared by all correction paths.
- `MeasurementWorker` (`app/wizard/`) — the established `QThread` pattern for off-GUI-thread work.

---

## Feature 1 — Interactive Target Curve Editor

### 1a. Draggable control points (deviation-layer model)

**Problem.** Built-in presets are 695-point curves containing fine detail (e.g. the Harman
ear-gain peak near 2.7 kHz). Representing the editable curve as a handful of anchors and
*replacing* the curve with a spline through them would destroy that detail — a correctness
bug, not just a cosmetic one.

**Model.** Anchors are a **deviation layer** added on top of the selected preset:

```
displayed_target(f) = base_preset(f) + deviation(f)
deviation(f)        = CubicSpline through {(anchor_freq_i, offset_db_i)},  offsets start at 0
```

At rest all offsets are 0, so the deviation is flat and the displayed target equals the
preset exactly; the draggable handles sit *on* the target line at their anchor frequencies.
Dragging a handle changes that anchor's offset; the preset bends smoothly around it while its
fine detail elsewhere is preserved. Selecting a different preset resets all offsets to 0 over
the new base.

**Anchors.** 16 **fixed, log-spaced** anchor frequencies spanning 20 Hz – 20 kHz
(ISO-ish: 20, 32, 50, 80, 125, 200, 315, 500, 800, 1250, 2000, 3150, 5000, 8000, 12500, 20000).
**Only the gain (vertical) axis is draggable; frequencies are fixed.** This is deliberate:

- It sidesteps `CubicSpline`'s strictly-increasing / distinct-frequency requirement — no
  sorting or duplicate-frequency bugs are possible.
- 16 anchors is enough vertical resolution for "tweak to taste" on a prototype.
- No add/remove of anchors (kept minimal; a possible future extension, not built now).

**Library (TDD) — `paraeq/correction/target_curves.py`:**

```python
def build_anchor_target(
    base: TargetCurve,
    anchor_freqs: np.ndarray,
    offsets_db: np.ndarray,
    *,
    name: str = "Custom",
) -> TargetCurve:
    """Return a densely-sampled TargetCurve = base + (cubic-spline deviation through
    the anchor offsets). anchor_freqs are fixed; offsets_db start at 0. The result is
    sampled on the base's own frequency grid so downstream interpolation is unchanged."""
```

- Deviation spline is `CubicSpline` in log10-frequency space through `(anchor_freqs, offsets_db)`,
  evaluated on `base.frequencies`, added to `base.gains_db`. Boundary behaviour: clamp to the
  first/last offset (matches `TargetCurve.interpolate`'s constant-hold convention).
- Pure function, no GUI. Does **not** mutate `base` (avoids the cached-preset-mutation risk the
  review flagged — built-in `TargetCurve` instances are shared via combo `userData`).

**GUI (smoke) — `app/editor/target_curve_editor.py`:**

- A draggable-node overlay using pyqtgraph's canonical `GraphItem`-subclass + `mouseDragEvent`
  pattern (a `ScatterPlotItem` of the anchors; the dragged node's Y follows the mouse, X is
  pinned). The plot is log-x (`setLogMode(x=True)`), so anchor X-coordinates live in
  **log10(Hz)** space — the exact coordinate convention for items added to a log-mode ViewBox
  will be verified against current pyqtgraph docs (via context7) during implementation, as the
  research flagged this as a genuine gotcha.
- State: `self._anchor_offsets_db` (length-16 array, reset to zeros on preset/measurement
  change). On drag, update the dragged offset, call `build_anchor_target(...)` to get the new
  target, store it as `self._target_curve`, redraw, and emit the live-preview signal (below).

### 1b. Live preview

Correction recomputes and re-applies to live audio *while dragging*, debounced.

- New signal on `TargetCurveEditor`: `target_preview = pyqtSignal(object, object)`
  (`measured_freqs`, `correction_db`), emitted on a **~60–80 ms single-shot QTimer debounce**
  restarted on each drag tick.
- `MainWindow` handler (new `_on_target_preview`):
  - Guards on `self._audio_engine is None` (same guard as `_update_iir_processor`); if no engine,
    the drag still updates the plot but produces no audio.
  - Designs a **minimum-phase FIR** from `correction_db` via `fir_filter.py` (remembering the
    square-the-magnitude rule), loads coefficients into a **persistent preview convolver**, and
    calls `audio_engine.set_processor(convolver.process)`.
- **Accepted trade-offs (documented, not bugs):**
  - The single `set_processor` slot is shared with the Manual-EQ IIR path. Previewing a target
    temporarily takes over the active correction — expected when auditioning a target.
  - Swapping FIR coefficients mid-stream can cause minor clicks; acceptable for a *preview*.
  - **Generate FIR / Generate PEQ remain the commit actions** (save to profile / hand to the EQ
    tab) exactly as today. Live preview does not change the commit semantics.

### 1c. Auto-match closest preset

**Library (TDD) — `paraeq/correction/target_curves.py`:**

```python
def match_closest_target(
    measured_freqs: np.ndarray,
    measured_db: np.ndarray,
    targets: list[TargetCurve],
) -> TargetCurve:
    """Return the target the measurement is already closest to, by smallest
    level-aligned RMS residual of (target - measured)."""
```

- For each target: interpolate to `measured_freqs`, compute `residual = target_db - measured_db`,
  subtract `residual.mean()` (absolute level is arbitrary), score by `sqrt(mean(residual**2))`.
  Return the target with the smallest score. Pure, deterministic, easily tested (a target that
  equals the measurement up to a constant offset scores ~0 and wins).

**GUI:** a "Match closest" button that runs `match_closest_target` over `list_builtin_targets()`
and selects the winner in the preset dropdown.

---

## Feature 2 — AutoEQ Database Browser

### Data source

The AutoEq project (`github.com/jaakkopasanen/AutoEq`, MIT-licensed tooling; measurements
credited to oratory1990, crinacle, Rtings, et al.), fetched from `raw.githubusercontent.com`:

- **Index:** `results/INDEX.md` — a documented, stable markdown list. Each entry carries the
  model name **and its exact relative folder path**, e.g.
  `- [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser HD 600) by oratory1990 on over-ear`.
  Parsing the real links avoids the inconsistent-folder-layout problem (paths are read, never
  constructed). This is preferred over `webapp/data/*.json` because that schema could not be
  verified and the markdown format is documented.
- **Preset:** `results/<rel_path>/<Model> ParametricEq.txt`, e.g.
  `Preamp: -6.2 dB` / `Filter 1: ON PK Fc 105 Hz Gain -1.7 dB Q 0.70`. Files are a few hundred
  bytes each.

**Fetch strategy: index sync + lazy preset fetch, cache-first.** Sync `INDEX.md` once (cached);
search/browse the full list offline; fetch each selected model's `ParametricEq.txt` on demand
(cached after first use). Network is needed only for an index refresh and the first load of any
given preset. This mirrors what the Rust port will do.

**Gotchas (handled):**
- Filename casing is inconsistent across the ecosystem (`ParametricEq.txt` vs `ParametricEQ.txt`)
  — the client tries both.
- Model-name paths contain spaces/specials — percent-encode when building raw URLs.
- `raw.githubusercontent.com` is a CDN for individual files, not bulk crawling — we fetch the
  one index file + individual tiny presets, never crawl thousands in parallel.

### Library (TDD) — `paraeq/correction/autoeq_db.py` (GUI-free)

```python
@dataclass
class AutoEQEntry:
    name: str       # "Sennheiser HD 600"
    source: str     # "oratory1990"
    rig: str        # "over-ear"
    rel_path: str   # "oratory1990/over-ear/Sennheiser HD 600"

@dataclass
class ParsedPreset:
    preamp_db: float
    bands: list[EQBand]

def parse_index(markdown: str) -> list[AutoEQEntry]: ...
def parse_parametric_eq(text: str) -> ParsedPreset: ...

def _http_get(url: str) -> str:               # module-level, patched in tests
    """GET a text resource via urllib.request."""

class AutoEQClient:
    def __init__(self, cache_dir: Path | None = None): ...   # defaults to platformdirs data dir
    def fetch_index(self, *, force: bool = False) -> list[AutoEQEntry]: ...   # cache-first
    def fetch_preset(self, entry: AutoEQEntry, *, force: bool = False) -> ParsedPreset: ...
```

- `parse_parametric_eq` is the **canonical** PEQ parser: captures the `Preamp` line (today's
  `_parse_autoeq` drops it), accepts `LS`/`LSC` and `HS`/`HSC` shelf aliases plus `PK`/`NO`,
  unknown codes default to peaking. `ManualEQEditor` is refactored to reuse it (removing the
  duplicated type-maps the review flagged).
- `AutoEQClient` uses **`urllib.request`** (stdlib — no new dependency) behind the module-level
  `_http_get`, so tests patch `_http_get` to return fixtures (the same module-level-symbol
  patching the codebase already uses for `sd`). Caching writes `INDEX.md` and per-model `.txt`
  files under a `platformdirs` data subdir (`autoeq_cache/`); `fetch_*` check the cache first.
  `fetch_preset` tries both filename casings and percent-encodes the path.

### GUI (smoke) — `app/eq/autoeq_browser.py`

- An `AutoEQBrowserDialog(QDialog)`: a search `QLineEdit` filtering a list of models
  (name / source / rig), plus an AutoEq + measurement-source **attribution label**.
- Networking runs **off the GUI thread** via a `QThread` worker (mirrors `MeasurementWorker`),
  with a status/spinner. On open it loads the index (cache or fetch).
- On **Load**: fetches + parses the selected model in the worker, then populates the Manual-EQ
  band table through its existing band-population path (block signals → `setRowCount(0)` →
  append rows → `_refresh_plot`), which fires `eq_changed` → live audio. So AutoEQ presets are
  applied live for free.
- The parsed `Preamp` is surfaced in the dialog and mapped to the engine's **master gain**
  (`set_gain`) — the Manual-EQ editor has no preamp field.

### Wiring

A "Browse AutoEQ DB…" `QPushButton` in the Manual-EQ toolbar, immediately left of the existing
Import AutoEQ button, opens the dialog. NOTICES/attribution updated for AutoEq + sources.

---

## Cross-cutting

### Architecture boundary
All new DSP / parse / fetch logic lives in `paraeq/` (GUI-free; `numpy`/`scipy`/`urllib` only).
`app/` imports from `paraeq/`, never the reverse. **New third-party dependencies: none.**

### Testing
- `tests/test_target_curves.py` (extended): `build_anchor_target` (zero offsets → equals base;
  a single non-zero offset bends locally and preserves boundaries; does not mutate `base`);
  `match_closest_target` (exact-up-to-offset target wins; flat measurement → flat target wins).
- `tests/test_autoeq_db.py` (new): `parse_index` on a fixture INDEX.md snippet; `parse_parametric_eq`
  incl. preamp capture + `LS`/`LSC`/`HS`/`HSC` aliases + unknown→peaking; `AutoEQClient` cache miss
  (calls `_http_get`, writes cache) and cache hit (no `_http_get`), both via patched `_http_get`
  + `tmp_path`; both filename casings.
- GUI (`app/`) stays manual-smoke (no display in CI), per repo convention.
- Goal: full suite green, runtime stays ~2 s (network fully mocked).

### Knowledge capture for the Rust port
Document in `docs/CONTEXT.md`: the log-x drag coordinate handling; the deviation-layer target
model (why anchors are offsets, not the curve); and the AutoEq `INDEX.md`-as-index strategy with
the casing/layout/percent-encoding gotchas. These transfer directly to the Rust rebuild.

---

## Out of scope
Installer/DMG/pkg, PyInstaller bundling, Homebrew cask, launch-at-login (packaging — deferred
per the throwaway-prototype decision); anchor add/remove; per-channel (L/R) independent target
editing (the editor and live correction are mono-symmetric today); preamp as a dedicated EQ
field (mapped to master gain instead).
