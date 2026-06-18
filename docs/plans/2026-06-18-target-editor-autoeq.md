# Target Editor Interactivity + AutoEQ Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add draggable control points + live FIR preview + auto-match to the target curve editor, and a searchable AutoEQ database browser to the manual EQ editor.

**Architecture:** All DSP/parse/fetch logic lives in `paraeq/` (GUI-free, TDD'd): a deviation-layer
`build_anchor_target`, a `match_closest_target` scorer, and a new `autoeq_db` module (index parser,
parametric-EQ parser, caching HTTP client over stdlib `urllib`). The GUI layer in `app/` (manual
smoke only — no display in CI) adds a pyqtgraph draggable-anchor overlay wired to a debounced live
FIR preview, a "Match closest" button, and an AutoEQ browser dialog that runs network I/O on a
`QThread` worker and feeds presets into the existing Manual-EQ band table.

**Tech Stack:** Python 3.11, NumPy, SciPy (`CubicSpline`, `minimum_phase`), PyQt6, pyqtgraph,
`urllib.request` (stdlib — no new deps), `platformdirs`. Spec:
`docs/specs/2026-06-17-target-editor-autoeq-design.md`.

**Conventions:** TDD for `paraeq/` (failing test → minimal impl → pass → commit). GUI is
manual-smoke. Alphabetize where order is irrelevant. Structured logging via `logger.debug(..., extra={...})`,
no `print`. Run the venv pytest: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest`. Each commit message
ends with the `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` trailer. Work happens on the
already-created branch `feature/target-editor-autoeq`.

---

## File Structure

**Create:**
- `paraeq/correction/autoeq_db.py` — `AutoEQEntry`, `ParsedPreset`, `parse_index`, `parse_parametric_eq`, `_http_get`, `AutoEQClient`.
- `app/editor/draggable_anchors.py` — `DraggableAnchors(pg.GraphItem)` vertical-drag overlay.
- `app/eq/autoeq_browser.py` — `AutoEQBrowserDialog(QDialog)` + `_AutoEQWorker(QThread)`.
- `tests/test_autoeq_db.py` — index/preset parsers + client cache/casing tests.
- `tests/fixtures/autoeq_index_sample.md` — small INDEX.md snippet.
- `tests/fixtures/autoeq_parametric_sample.txt` — sample ParametricEq.txt.

**Modify:**
- `paraeq/correction/target_curves.py` — add `ANCHOR_FREQS`, `build_anchor_target`, `match_closest_target`.
- `tests/test_target_curves.py` — tests for the two new functions.
- `app/editor/target_curve_editor.py` — draggable anchors, deviation editing, `target_preview` signal, "Match closest" button.
- `app/eq/manual_eq_editor.py` — replace local `_parse_autoeq` with `autoeq_db.parse_parametric_eq`; add "Browse AutoEQ DB…" button.
- `app/main_window.py` — wire `target_preview` → live FIR preview handler.
- `docs/CONTEXT.md` — mark items done; capture port knowledge.

---

## Task 1: `ANCHOR_FREQS` + `build_anchor_target` (library)

**Files:**
- Modify: `paraeq/correction/target_curves.py`
- Test: `tests/test_target_curves.py`

- [ ] **Step 1: Write the failing tests**

Append to `tests/test_target_curves.py`:

```python
from paraeq.correction.target_curves import ANCHOR_FREQS, build_anchor_target


def test_anchor_freqs_are_sorted_and_span_audio_band():
    assert ANCHOR_FREQS[0] == 20.0
    assert ANCHOR_FREQS[-1] == 20000.0
    assert np.all(np.diff(ANCHOR_FREQS) > 0)
    assert len(ANCHOR_FREQS) == 16


def test_build_anchor_target_zero_offsets_equals_base():
    base = TargetCurve(
        name="Harmanish",
        frequencies=np.array([20.0, 200.0, 1000.0, 3000.0, 20000.0]),
        gains_db=np.array([4.0, 0.0, -1.0, 6.0, -8.0]),
    )
    offsets = np.zeros(len(ANCHOR_FREQS))
    result = build_anchor_target(base, ANCHOR_FREQS, offsets)
    # Sampling at the base's own anchor frequencies must reproduce the base.
    np.testing.assert_allclose(
        result.interpolate(base.frequencies), base.gains_db, atol=1e-6
    )


def test_build_anchor_target_single_offset_bends_locally():
    base = TargetCurve(
        name="Flat",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
    )
    offsets = np.zeros(len(ANCHOR_FREQS))
    idx_1250 = int(np.argmin(np.abs(ANCHOR_FREQS - 1250.0)))
    offsets[idx_1250] = 6.0
    result = build_anchor_target(base, ANCHOR_FREQS, offsets)
    at = result.interpolate(np.array([1250.0, 20.0, 20000.0]))
    assert abs(at[0] - 6.0) < 0.5          # bent up at the dragged anchor
    assert abs(at[1] - 0.0) < 0.5          # boundary held near zero
    assert abs(at[2] - 0.0) < 0.5


def test_build_anchor_target_does_not_mutate_base():
    base = TargetCurve(
        name="Flat",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
    )
    original = base.gains_db.copy()
    offsets = np.zeros(len(ANCHOR_FREQS))
    offsets[5] = 3.0
    build_anchor_target(base, ANCHOR_FREQS, offsets)
    np.testing.assert_array_equal(base.gains_db, original)
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_target_curves.py -k anchor -v`
Expected: FAIL with `ImportError: cannot import name 'ANCHOR_FREQS'`.

- [ ] **Step 3: Implement**

In `paraeq/correction/target_curves.py`, after the `_METADATA_KEYS` line add the constant:

```python
# Fixed, log-spaced anchor frequencies for the interactive target editor.
# Only the gain axis is draggable, so frequencies stay sorted/distinct by
# construction (no CubicSpline degenerate-input risk).
ANCHOR_FREQS = np.array(
    [20, 32, 50, 80, 125, 200, 315, 500, 800, 1250, 2000, 3150, 5000, 8000, 12500, 20000],
    dtype=np.float64,
)
```

At the end of the file add:

```python
def build_anchor_target(
    base: TargetCurve,
    anchor_freqs: np.ndarray,
    offsets_db: np.ndarray,
    *,
    name: str = "Custom",
) -> TargetCurve:
    """Return a new TargetCurve = base + a smooth deviation through the anchors.

    The deviation is a cubic spline (in log-frequency space) through
    ``(anchor_freqs, offsets_db)``, held constant outside the anchor range.
    Sampling on the union of the base's own frequencies and the anchor
    frequencies preserves the base preset's fine detail while applying the
    user's anchor offsets. ``base`` is never mutated.

    Args:
        base: The preset being edited (offsets start at zero).
        anchor_freqs: Fixed anchor frequencies in Hz (sorted, distinct).
        offsets_db: One dB offset per anchor (same length as anchor_freqs).
        name: Name for the returned curve.

    Returns:
        A new :class:`TargetCurve`.
    """
    grid = np.unique(np.concatenate([base.frequencies, anchor_freqs]))
    base_db = base.interpolate(grid)

    cs = CubicSpline(np.log10(anchor_freqs), offsets_db, extrapolate=True)
    deviation = cs(np.log10(grid))
    deviation = np.where(grid < anchor_freqs[0], offsets_db[0], deviation)
    deviation = np.where(grid > anchor_freqs[-1], offsets_db[-1], deviation)

    logger.debug(
        "build_anchor_target",
        extra={
            "base": base.name,
            "n_grid": len(grid),
            "offset_max": float(np.max(np.abs(offsets_db))),
        },
    )
    return TargetCurve(
        name=name,
        frequencies=grid,
        gains_db=base_db + deviation,
        category=base.category,
        description=base.description,
        source=base.source,
    )
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_target_curves.py -k anchor -v`
Expected: 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/target_curves.py tests/test_target_curves.py
git commit -m "feat(correction): deviation-layer build_anchor_target for editable targets

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 2: `match_closest_target` (library)

**Files:**
- Modify: `paraeq/correction/target_curves.py`
- Test: `tests/test_target_curves.py`

- [ ] **Step 1: Write the failing tests**

Append to `tests/test_target_curves.py`:

```python
from paraeq.correction.target_curves import match_closest_target


def test_match_closest_target_picks_level_aligned_best():
    freqs = np.array([20.0, 200.0, 1000.0, 5000.0, 20000.0])
    measured = np.array([2.0, 0.0, -3.0, 4.0, -6.0])
    # near: same shape as measured but shifted +10 dB (level-aligned → ~0 residual)
    near = TargetCurve(name="near", frequencies=freqs, gains_db=measured + 10.0)
    # far: a different shape
    far = TargetCurve(name="far", frequencies=freqs, gains_db=np.array([-8.0, 0.0, 8.0, -8.0, 8.0]))
    chosen = match_closest_target(freqs, measured, [far, near])
    assert chosen.name == "near"


def test_match_closest_target_returns_a_member():
    freqs = np.array([20.0, 1000.0, 20000.0])
    measured = np.array([0.0, 0.0, 0.0])
    targets = list_builtin_targets()
    chosen = match_closest_target(freqs, measured, targets)
    assert chosen in targets
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_target_curves.py -k match_closest -v`
Expected: FAIL with `ImportError: cannot import name 'match_closest_target'`.

- [ ] **Step 3: Implement**

At the end of `paraeq/correction/target_curves.py` add:

```python
def match_closest_target(
    measured_freqs: np.ndarray,
    measured_db: np.ndarray,
    targets: list[TargetCurve],
) -> TargetCurve:
    """Return the target the measurement is already closest to.

    Scores each target by the level-aligned RMS of ``target - measured``
    (the mean is removed first because absolute level is arbitrary) and
    returns the smallest-scoring target.

    Args:
        measured_freqs: Measurement frequencies in Hz.
        measured_db: Measured response in dB (same length as measured_freqs).
        targets: Candidate target curves (must be non-empty).

    Returns:
        The closest :class:`TargetCurve`.
    """
    best: TargetCurve | None = None
    best_score = np.inf
    for t in targets:
        residual = t.interpolate(measured_freqs) - measured_db
        residual = residual - residual.mean()
        score = float(np.sqrt(np.mean(residual ** 2)))
        logger.debug("match_closest_target candidate", extra={"target": t.name, "rms": score})
        if score < best_score:
            best_score = score
            best = t
    logger.info("match_closest_target chose '%s' (rms=%.3f dB)", best.name if best else None, best_score)
    return best
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_target_curves.py -k match_closest -v`
Expected: 2 PASS.

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/target_curves.py tests/test_target_curves.py
git commit -m "feat(correction): match_closest_target auto-match scorer

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 3: AutoEQ index parser (library)

**Files:**
- Create: `paraeq/correction/autoeq_db.py`
- Create: `tests/fixtures/autoeq_index_sample.md`
- Create: `tests/test_autoeq_db.py`

- [ ] **Step 1: Create the fixture**

Create `tests/fixtures/autoeq_index_sample.md`:

```markdown
# Index

This is a list of all equalization profiles.

- [Sennheiser HD 600](./oratory1990/over-ear/Sennheiser HD 600) by oratory1990 on over-ear
- [Shure SE846](./crinacle/711 in-ear/Shure SE846) by crinacle on 711 in-ear
- [Sony WH-1000XM4](./rtings/over-ear/Sony WH-1000XM4) by rtings on over-ear
```

- [ ] **Step 2: Write the failing test**

Create `tests/test_autoeq_db.py`:

```python
from pathlib import Path

import pytest

from paraeq.correction.autoeq_db import AutoEQEntry, parse_index

FIXTURES = Path(__file__).parent / "fixtures"


def test_parse_index_extracts_entries():
    text = (FIXTURES / "autoeq_index_sample.md").read_text()
    entries = parse_index(text)
    assert len(entries) == 3
    hd600 = entries[0]
    assert isinstance(hd600, AutoEQEntry)
    assert hd600.name == "Sennheiser HD 600"
    assert hd600.source == "oratory1990"
    assert hd600.rig == "over-ear"
    assert hd600.rel_path == "oratory1990/over-ear/Sennheiser HD 600"


def test_parse_index_handles_multiword_rig():
    text = (FIXTURES / "autoeq_index_sample.md").read_text()
    entries = parse_index(text)
    se846 = entries[1]
    assert se846.name == "Shure SE846"
    assert se846.source == "crinacle"
    assert se846.rig == "711 in-ear"
    assert se846.rel_path == "crinacle/711 in-ear/Shure SE846"


def test_parse_index_ignores_non_entry_lines():
    entries = parse_index("# Heading\n\nsome prose\n")
    assert entries == []
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_autoeq_db.py -k parse_index -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'paraeq.correction.autoeq_db'`.

- [ ] **Step 4: Implement**

Create `paraeq/correction/autoeq_db.py`:

```python
"""AutoEq database access: index/preset parsing and a caching HTTP client.

GUI-free. Networking goes through the module-level ``_http_get`` so tests can
patch it. Data source: github.com/jaakkopasanen/AutoEq (raw.githubusercontent.com).
"""

import logging
import re
from dataclasses import dataclass

logger = logging.getLogger(__name__)

_INDEX_LINE = re.compile(
    r"^\s*-\s*\[(?P<name>.+?)\]\((?P<path>.+?)\)\s+by\s+(?P<source>.+?)\s+on\s+(?P<rig>.+?)\s*$"
)


@dataclass
class AutoEQEntry:
    name: str
    source: str
    rig: str
    rel_path: str


def parse_index(markdown: str) -> list[AutoEQEntry]:
    """Parse an AutoEq ``results/INDEX.md`` into a list of entries.

    Each recognised line looks like
    ``- [Model](./source/rig/Model) by source on rig``. Lines that do not
    match (headings, prose, blanks) are skipped. The leading ``./`` is
    stripped from the relative path.
    """
    entries: list[AutoEQEntry] = []
    for line in markdown.splitlines():
        m = _INDEX_LINE.match(line)
        if not m:
            continue
        path = m.group("path").strip()
        if path.startswith("./"):
            path = path[2:]
        entries.append(
            AutoEQEntry(
                name=m.group("name").strip(),
                source=m.group("source").strip(),
                rig=m.group("rig").strip(),
                rel_path=path,
            )
        )
    logger.info("Parsed %d AutoEq index entries", len(entries))
    return entries
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_autoeq_db.py -k parse_index -v`
Expected: 3 PASS.

- [ ] **Step 6: Commit**

```bash
git add paraeq/correction/autoeq_db.py tests/test_autoeq_db.py tests/fixtures/autoeq_index_sample.md
git commit -m "feat(correction): AutoEq INDEX.md parser

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 4: ParametricEq.txt parser (library)

**Files:**
- Modify: `paraeq/correction/autoeq_db.py`
- Create: `tests/fixtures/autoeq_parametric_sample.txt`
- Modify: `tests/test_autoeq_db.py`

- [ ] **Step 1: Create the fixture**

Create `tests/fixtures/autoeq_parametric_sample.txt`:

```text
Preamp: -6.2 dB
Filter 1: ON PK Fc 105 Hz Gain -1.7 dB Q 0.70
Filter 2: ON LSC Fc 105 Hz Gain 3.0 dB Q 0.70
Filter 3: ON HS Fc 10000 Hz Gain -2.0 dB Q 0.70
Filter 4: ON XYZ Fc 500 Hz Gain 1.0 dB Q 1.00
```

- [ ] **Step 2: Write the failing tests**

Append to `tests/test_autoeq_db.py`:

```python
from paraeq.correction.autoeq_db import ParsedPreset, parse_parametric_eq
from paraeq.correction.parametric_eq import EQBand


def test_parse_parametric_eq_captures_preamp_and_bands():
    text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    parsed = parse_parametric_eq(text)
    assert isinstance(parsed, ParsedPreset)
    assert parsed.preamp_db == pytest.approx(-6.2)
    assert len(parsed.bands) == 4
    assert all(isinstance(b, EQBand) for b in parsed.bands)


def test_parse_parametric_eq_maps_filter_types_incl_aliases():
    text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    bands = parse_parametric_eq(text).bands
    assert bands[0].filter_type == "peaking"     # PK
    assert bands[1].filter_type == "low_shelf"   # LSC
    assert bands[2].filter_type == "high_shelf"  # HS alias
    assert bands[3].filter_type == "peaking"     # XYZ unknown → peaking
    assert bands[0].fc == 105.0
    assert bands[0].gain_db == -1.7
    assert bands[0].q == 0.70


def test_parse_parametric_eq_defaults_preamp_to_zero_when_absent():
    parsed = parse_parametric_eq("Filter 1: ON PK Fc 100 Hz Gain 1.0 dB Q 1.0")
    assert parsed.preamp_db == 0.0
    assert len(parsed.bands) == 1
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_autoeq_db.py -k parametric -v`
Expected: FAIL with `ImportError: cannot import name 'parse_parametric_eq'`.

- [ ] **Step 4: Implement**

In `paraeq/correction/autoeq_db.py`, add after the `AutoEQEntry` dataclass:

```python
_PREAMP_RE = re.compile(r"Preamp\s*:\s*([-\d.]+)\s*dB", re.IGNORECASE)
_FILTER_RE = re.compile(
    r"Filter\s+\d+\s*:\s*ON\s+(\w+)\s+Fc\s+([\d.]+)\s+Hz\s+Gain\s+([-\d.]+)\s+dB\s+Q\s+([\d.]+)",
    re.IGNORECASE,
)
# AutoEq short codes → internal filter types. Accepts both EqualizerAPO shelf
# spellings (LSC/HSC) and the legacy LS/HS aliases.
_AUTOEQ_TO_INTERNAL = {
    "HS": "high_shelf",
    "HSC": "high_shelf",
    "LS": "low_shelf",
    "LSC": "low_shelf",
    "NO": "notch",
    "PK": "peaking",
}


@dataclass
class ParsedPreset:
    preamp_db: float
    bands: list  # list[EQBand]


def parse_parametric_eq(text: str) -> ParsedPreset:
    """Parse AutoEq ParametricEq.txt text into a :class:`ParsedPreset`.

    Captures the ``Preamp`` line (defaulting to 0.0 dB if absent) and every
    ``Filter N: ON <TYPE> Fc <hz> Hz Gain <db> dB Q <q>`` line. Unknown filter
    codes fall back to peaking.
    """
    from paraeq.correction.parametric_eq import EQBand

    preamp_db = 0.0
    bands: list[EQBand] = []
    for line in text.splitlines():
        pm = _PREAMP_RE.search(line)
        if pm:
            preamp_db = float(pm.group(1))
            continue
        fm = _FILTER_RE.search(line)
        if fm:
            short, fc, gain, q = fm.group(1), fm.group(2), fm.group(3), fm.group(4)
            bands.append(
                EQBand(
                    filter_type=_AUTOEQ_TO_INTERNAL.get(short.upper(), "peaking"),
                    fc=float(fc),
                    gain_db=float(gain),
                    q=float(q),
                )
            )
    logger.info("Parsed AutoEq preset: preamp=%.1f dB, %d bands", preamp_db, len(bands))
    return ParsedPreset(preamp_db=preamp_db, bands=bands)
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_autoeq_db.py -k parametric -v`
Expected: 3 PASS.

- [ ] **Step 6: Commit**

```bash
git add paraeq/correction/autoeq_db.py tests/test_autoeq_db.py tests/fixtures/autoeq_parametric_sample.txt
git commit -m "feat(correction): AutoEq ParametricEq.txt parser with preamp + shelf aliases

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 5: `AutoEQClient` with caching (library)

**Files:**
- Modify: `paraeq/correction/autoeq_db.py`
- Modify: `tests/test_autoeq_db.py`

- [ ] **Step 1: Write the failing tests**

Append to `tests/test_autoeq_db.py`:

```python
from unittest import mock

from paraeq.correction.autoeq_db import AutoEQClient


def test_fetch_index_cache_miss_downloads_and_writes(tmp_path):
    index_text = (FIXTURES / "autoeq_index_sample.md").read_text()
    client = AutoEQClient(cache_dir=tmp_path)
    with mock.patch(
        "paraeq.correction.autoeq_db._http_get", return_value=index_text
    ) as get:
        entries = client.fetch_index()
    assert len(entries) == 3
    get.assert_called_once()
    assert (tmp_path / "INDEX.md").read_text() == index_text


def test_fetch_index_cache_hit_does_not_download(tmp_path):
    index_text = (FIXTURES / "autoeq_index_sample.md").read_text()
    (tmp_path / "INDEX.md").write_text(index_text)
    client = AutoEQClient(cache_dir=tmp_path)
    with mock.patch(
        "paraeq.correction.autoeq_db._http_get", side_effect=AssertionError("must not fetch")
    ):
        entries = client.fetch_index()
    assert len(entries) == 3


def test_fetch_preset_tries_both_filename_casings(tmp_path):
    preset_text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    client = AutoEQClient(cache_dir=tmp_path)
    entry = AutoEQEntry(
        name="Sennheiser HD 600",
        source="oratory1990",
        rig="over-ear",
        rel_path="oratory1990/over-ear/Sennheiser HD 600",
    )

    def fake_get(url):
        if url.endswith("ParametricEq.txt"):  # lowercase q fails first
            raise OSError("404")
        return preset_text  # capital Q succeeds

    with mock.patch("paraeq.correction.autoeq_db._http_get", side_effect=fake_get):
        parsed = client.fetch_preset(entry)
    assert parsed.preamp_db == pytest.approx(-6.2)
    assert len(parsed.bands) == 4


def test_fetch_preset_uses_cache_on_second_call(tmp_path):
    preset_text = (FIXTURES / "autoeq_parametric_sample.txt").read_text()
    client = AutoEQClient(cache_dir=tmp_path)
    entry = AutoEQEntry(
        name="Shure SE846",
        source="crinacle",
        rig="711 in-ear",
        rel_path="crinacle/711 in-ear/Shure SE846",
    )
    with mock.patch(
        "paraeq.correction.autoeq_db._http_get", return_value=preset_text
    ) as get:
        client.fetch_preset(entry)
        client.fetch_preset(entry)
    get.assert_called_once()  # second call served from cache
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_autoeq_db.py -k "fetch" -v`
Expected: FAIL with `ImportError: cannot import name 'AutoEQClient'`.

- [ ] **Step 3: Implement**

In `paraeq/correction/autoeq_db.py`, update the imports at the top to add:

```python
import urllib.parse
import urllib.request
from pathlib import Path

from platformdirs import user_data_dir
```

Then add at the end of the file:

```python
_RAW_BASE = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master"
_INDEX_REL = "results/INDEX.md"
_FILENAME_CASINGS = ("ParametricEq.txt", "ParametricEQ.txt")


def _http_get(url: str) -> str:
    """GET a UTF-8 text resource. Patched in tests."""
    with urllib.request.urlopen(url, timeout=30) as resp:
        return resp.read().decode("utf-8")


def _default_cache_dir() -> Path:
    return Path(user_data_dir("ParaEQ")) / "autoeq_cache"


class AutoEQClient:
    """Cache-first access to AutoEq's index and per-model presets.

    The index (``results/INDEX.md``) is fetched once and cached; individual
    presets are fetched lazily on first access and cached thereafter. All
    network I/O is synchronous — callers in the GUI run it on a worker thread.
    """

    def __init__(self, cache_dir=None):
        self.cache_dir = Path(cache_dir) if cache_dir is not None else _default_cache_dir()

    def fetch_index(self, *, force: bool = False) -> list[AutoEQEntry]:
        cache_file = self.cache_dir / "INDEX.md"
        if cache_file.exists() and not force:
            text = cache_file.read_text(encoding="utf-8")
            logger.debug("AutoEq index served from cache: %s", cache_file)
        else:
            text = _http_get(f"{_RAW_BASE}/{urllib.parse.quote(_INDEX_REL)}")
            self.cache_dir.mkdir(parents=True, exist_ok=True)
            cache_file.write_text(text, encoding="utf-8")
            logger.info("AutoEq index downloaded and cached: %s", cache_file)
        return parse_index(text)

    def fetch_preset(self, entry: AutoEQEntry, *, force: bool = False) -> ParsedPreset:
        cache_file = self.cache_dir / (entry.rel_path.replace("/", "__") + ".txt")
        if cache_file.exists() and not force:
            logger.debug("AutoEq preset served from cache: %s", cache_file)
            return parse_parametric_eq(cache_file.read_text(encoding="utf-8"))
        text = self._download_preset(entry)
        self.cache_dir.mkdir(parents=True, exist_ok=True)
        cache_file.write_text(text, encoding="utf-8")
        logger.info("AutoEq preset downloaded and cached: %s", entry.name)
        return parse_parametric_eq(text)

    def _download_preset(self, entry: AutoEQEntry) -> str:
        model = entry.rel_path.rstrip("/").split("/")[-1]
        last_err: Exception | None = None
        for casing in _FILENAME_CASINGS:
            rel = f"results/{entry.rel_path}/{model} {casing}"
            url = f"{_RAW_BASE}/{urllib.parse.quote(rel)}"
            try:
                return _http_get(url)
            except Exception as exc:  # try the other casing before giving up
                last_err = exc
                logger.debug("AutoEq preset fetch failed for %s: %s", url, exc)
        raise last_err
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/test_autoeq_db.py -v`
Expected: all PASS (parse + fetch).

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/autoeq_db.py tests/test_autoeq_db.py
git commit -m "feat(correction): cache-first AutoEQClient over urllib

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 6: Reuse the canonical parser in Manual EQ (refactor)

**Files:**
- Modify: `app/eq/manual_eq_editor.py`

- [ ] **Step 1: Confirm nothing else imports the local helper**

Run: `grep -rn "_parse_autoeq" /Users/ronakpatel/code/paraeq/app /Users/ronakpatel/code/paraeq/tests`
Expected: only references inside `app/eq/manual_eq_editor.py`. If a test references it, keep a thin
delegating wrapper instead of deleting (see Step 3 note).

- [ ] **Step 2: Remove the duplicated parser + map**

In `app/eq/manual_eq_editor.py`, delete the module-level `_AUTOEQ_TO_INTERNAL` dict (lines ~30-35)
and the entire `_parse_autoeq` function (lines ~58-78). Keep `_INTERNAL_TYPES`.

- [ ] **Step 3: Rewrite `_import_autoeq` to use the canonical parser**

Replace the body of `_import_autoeq` (the `try:` block) with:

```python
        try:
            from paraeq.correction.autoeq_db import parse_parametric_eq

            parsed = parse_parametric_eq(Path(path).read_text())
            if not parsed.bands:
                QMessageBox.warning(
                    self, "Import", "No AutoEQ filter lines found in the file."
                )
                return
            self._table.blockSignals(True)
            self._table.setRowCount(0)
            for band in parsed.bands:
                self._append_row(band.filter_type, band.fc, band.gain_db, band.q)
            self._table.blockSignals(False)
            self._refresh_plot()
            logger.info(
                "Imported %d bands (preamp %.1f dB) from %s",
                len(parsed.bands), parsed.preamp_db, path,
            )
        except Exception as exc:
            logger.error("AutoEQ import failed: %s", exc)
            QMessageBox.critical(self, "Import Error", str(exc))
```

(`re` may now be an unused import — remove `import re` from the top of the file if no other
references remain; check with `grep -n "re\." app/eq/manual_eq_editor.py`.)

- [ ] **Step 4: Verify the suite still passes (no import breakage)**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/ -q`
Expected: all green (same count as before plus the new autoeq/target tests).

- [ ] **Step 5: Manual smoke (optional but recommended)**

Launch `/Users/ronakpatel/code/paraeq/.venv/bin/python -m app.main`, open the **EQ** tab, click
**Import AutoEQ…**, choose `tests/fixtures/autoeq_parametric_sample.txt`. Expected: 4 bands appear in
the table and the composite curve redraws.

- [ ] **Step 6: Commit**

```bash
git add app/eq/manual_eq_editor.py
git commit -m "refactor(eq): reuse autoeq_db.parse_parametric_eq, drop duplicated parser

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 7: Draggable anchors + deviation editing + preview signal (GUI smoke)

**Files:**
- Create: `app/editor/draggable_anchors.py`
- Modify: `app/editor/target_curve_editor.py`

- [ ] **Step 1: Verify the pyqtgraph drag API**

Use context7 to confirm the current pyqtgraph `GraphItem`/`ScatterPlotItem` drag API before coding:

```bash
npx ctx7@latest library "pyqtgraph" "draggable GraphItem nodes mouseDragEvent scatter pointsAt in log mode plot"
npx ctx7@latest docs <id> "GraphItem subclass draggable nodes mouseDragEvent ev.pos buttonDownPos scatter pointsAt index setLogMode coordinates"
```

Confirm: (a) items added to a `setLogMode(x=True)` ViewBox use **log10(Hz)** x-coordinates;
(b) the `mouseDragEvent`/`scatter.pointsAt(pos)` / `point.index()` signatures below. Adjust the code
in Step 2 if the API differs.

- [ ] **Step 2: Create the draggable overlay**

Create `app/editor/draggable_anchors.py`:

```python
"""Vertical-drag anchor overlay for the target curve editor.

A pyqtgraph GraphItem subclass that renders a row of control points at fixed
x-positions (log10(Hz)) and lets the user drag each one vertically. The plot
is in log-x mode, so positions are stored in log10-frequency space; only the
gain (y) coordinate changes on drag.
"""

import logging

import numpy as np
import pyqtgraph as pg
from pyqtgraph import QtCore

logger = logging.getLogger(__name__)


class DraggableAnchors(pg.GraphItem):
    """Fixed-frequency, vertically-draggable control points.

    Args:
        on_moved: callback ``(index: int, new_gain_db: float)`` invoked on each
            drag step with the anchor index and its new gain (y) value.
    """

    def __init__(self, on_moved):
        self._on_moved = on_moved
        self._log_freqs = np.zeros(0)
        self._gains = np.zeros(0)
        self._drag_index: int | None = None
        super().__init__()

    def set_anchors(self, freqs_hz: np.ndarray, gains_db: np.ndarray):
        """Position the anchors. ``freqs_hz`` are real Hz; stored as log10."""
        self._log_freqs = np.log10(np.asarray(freqs_hz, dtype=float))
        self._gains = np.asarray(gains_db, dtype=float).copy()
        self._redraw()

    def _redraw(self):
        if self._log_freqs.size == 0:
            self.setData()
            return
        pos = np.column_stack([self._log_freqs, self._gains])
        self.setData(
            pos=pos,
            size=12,
            symbol="o",
            symbolBrush=pg.mkBrush("#FFB74D"),
            symbolPen=pg.mkPen("#E65100", width=1.5),
            pxMode=True,
        )

    def mouseDragEvent(self, ev):
        if ev.button() != QtCore.Qt.MouseButton.LeftButton:
            ev.ignore()
            return
        if ev.isStart():
            pts = self.scatter.pointsAt(ev.buttonDownPos())
            if len(pts) == 0:
                self._drag_index = None
                ev.ignore()
                return
            self._drag_index = int(pts[0].index())
            ev.accept()
            return
        if self._drag_index is None:
            ev.ignore()
            return
        if ev.isFinish():
            self._drag_index = None
            ev.accept()
            return
        new_gain = float(ev.pos().y())
        self._gains[self._drag_index] = new_gain
        self._redraw()
        self._on_moved(self._drag_index, new_gain)
        ev.accept()
```

- [ ] **Step 3: Add deviation state + preview signal to the editor**

In `app/editor/target_curve_editor.py`:

(a) Extend the imports:

```python
from PyQt6.QtCore import pyqtSignal, QTimer
```

(b) Add a second signal under `correction_generated`:

```python
    target_preview = pyqtSignal(object, object)  # (measured_freqs, correction_db)
```

(c) In `__init__`, after `self._target_curve = None`, add:

```python
        from paraeq.correction.target_curves import ANCHOR_FREQS

        self._base_target = None                       # preset before deviation
        self._anchor_freqs = ANCHOR_FREQS
        self._anchor_offsets_db = np.zeros(len(ANCHOR_FREQS))
        self._preview_timer = QTimer(self)
        self._preview_timer.setSingleShot(True)
        self._preview_timer.setInterval(70)            # ms debounce
        self._preview_timer.timeout.connect(self._emit_preview)
```

(d) At the end of `_setup_ui`, after the three `plot(...)` traces, add the overlay:

```python
        from app.editor.draggable_anchors import DraggableAnchors

        self._anchors = DraggableAnchors(self._on_anchor_moved)
        self._plot_widget.addItem(self._anchors)
```

- [ ] **Step 4: Make preset changes drive the base + anchors**

In `target_curve_editor.py`, replace `_on_preset_changed` with:

```python
    def _on_preset_changed(self, index: int):
        target = self._preset_combo.itemData(index)
        if target is None:
            try:
                from paraeq.correction.target_curves import TargetCurve

                target = TargetCurve("Flat", np.array([20.0, 20_000.0]), np.array([0.0, 0.0]))
            except Exception:
                target = None
        self._base_target = target
        self._anchor_offsets_db = np.zeros(len(self._anchor_freqs))
        self._rebuild_target()
        logger.debug("Target preset changed to index=%d", index)
```

Add two helpers (e.g. just below `_on_preset_changed`):

```python
    def _rebuild_target(self):
        """Recompute the active target from base + anchor offsets, redraw."""
        if self._base_target is None:
            self._target_curve = None
        elif np.any(self._anchor_offsets_db):
            from paraeq.correction.target_curves import build_anchor_target

            self._target_curve = build_anchor_target(
                self._base_target, self._anchor_freqs, self._anchor_offsets_db
            )
        else:
            self._target_curve = self._base_target
        self._update_plot()
        self._update_anchor_positions()

    def _update_anchor_positions(self):
        if self._base_target is None:
            return
        base_gains = self._base_target.interpolate(self._anchor_freqs)
        self._anchors.set_anchors(self._anchor_freqs, base_gains + self._anchor_offsets_db)

    def _on_anchor_moved(self, index: int, new_gain_db: float):
        base_gain = float(self._base_target.interpolate(self._anchor_freqs[index : index + 1])[0])
        self._anchor_offsets_db[index] = new_gain_db - base_gain
        self._rebuild_target()
        if self._measured_freqs is not None:
            self._preview_timer.start()

    def _emit_preview(self):
        if self._measured_freqs is None or self._target_curve is None:
            return
        from paraeq.correction.target_curves import compute_correction

        target_db = self._target_curve.interpolate(self._measured_freqs)
        correction_db = compute_correction(self._measured_db, target_db)
        self.target_preview.emit(self._measured_freqs, correction_db)
        logger.debug("Emitted target_preview (%d freqs)", len(self._measured_freqs))
```

Note: `_on_anchor_moved` calls `_rebuild_target`, which itself calls `_update_anchor_positions` and
re-sets the scatter — that's intended (it keeps the dragged point consistent with the rebuilt curve).

(e) In `_import_csv`, after `self._target_curve = target`, also seed the base and reset offsets so the
imported curve is editable:

```python
            self._base_target = target
            self._anchor_offsets_db = np.zeros(len(self._anchor_freqs))
            self._rebuild_target()
```

(replace the existing `self._target_curve = target` / `self._update_plot()` pair in that method).

- [ ] **Step 5: Manual smoke test**

Launch `/Users/ronakpatel/code/paraeq/.venv/bin/python -m app.main`. On the **Target** tab:
- Orange anchor dots appear along the target line.
- Pick **Harman In-Ear 2019** — anchors snap onto that curve; its ear-gain peak is still visible
  (proof the deviation model preserves preset detail).
- Drag an anchor near 1.25 kHz up ~5 dB — the target bends smoothly there, the **Correction delta**
  trace updates, and the boundary anchors stay put.
- Switch presets — offsets reset to zero, anchors re-seat on the new curve.

- [ ] **Step 6: Verify the suite still imports cleanly**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/ -q`
Expected: all green (no GUI tests, but confirms no import-time breakage).

- [ ] **Step 7: Commit**

```bash
git add app/editor/draggable_anchors.py app/editor/target_curve_editor.py
git commit -m "feat(editor): draggable deviation-layer anchors with debounced preview signal

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 8: Live FIR preview wiring in MainWindow (GUI smoke)

**Files:**
- Modify: `app/main_window.py`

- [ ] **Step 1: Connect the preview signal**

In `app/main_window.py` `_connect_signals`, after the `correction_generated` block, add:

```python
        # Target editor → live FIR preview while dragging anchors
        if hasattr(self._target_tab, "target_preview"):
            self._target_tab.target_preview.connect(self._on_target_preview)
```

- [ ] **Step 2: Add the preview handler**

In the "Audio engine" section of `app/main_window.py` (e.g. just above `_update_iir_processor`), add:

```python
    def _on_target_preview(self, freqs, correction_db):
        """Audition a target-editor correction live by swapping in a FIR.

        Designs a minimum-phase FIR from the correction curve and installs it
        as the engine processor. Debounced upstream by the editor. The single
        processor slot is shared with the manual-EQ IIR path, so previewing a
        target temporarily takes over the active correction (by design).
        """
        if self._audio_engine is None:
            return
        try:
            from paraeq.correction.fir_filter import design_fir_correction
            from paraeq.engine.convolver import OverlapAddConvolver

            fir = design_fir_correction(correction_db, freqs)
            convolver = OverlapAddConvolver(fir)
            self._audio_engine.set_processor(convolver.process)
            logger.info("Live target preview applied (FIR taps=%d)", len(fir))
        except Exception as exc:
            logger.error("Target preview failed: %s", exc)
```

- [ ] **Step 3: Manual smoke test**

Requires audio set up (aggregate device running). Launch the app, start playback through ParaEQ,
go to **Target**, load a measurement (or a profile that has one), and drag an anchor. Expected: the
timbre changes as you drag and settles when you stop; logs show repeated
`Live target preview applied (FIR taps=4096)` lines, debounced (not one per pixel). With no audio
engine running, dragging still updates the plot and logs nothing from this handler.

- [ ] **Step 4: Verify the suite still imports cleanly**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/ -q`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add app/main_window.py
git commit -m "feat(app): live minimum-phase FIR preview from target editor drags

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 9: "Match closest" button (GUI smoke)

**Files:**
- Modify: `app/editor/target_curve_editor.py`

- [ ] **Step 1: Add the button**

In `_setup_ui`, in the preset row (after the Export CSV button, before `root.addWidget(preset_group)`):

```python
        self._match_btn = QPushButton("Match closest")
        self._match_btn.setToolTip("Select the built-in target your measurement is closest to")
        self._match_btn.clicked.connect(self._match_closest)
        preset_layout.addWidget(self._match_btn)
```

- [ ] **Step 2: Add the handler**

Add to `target_curve_editor.py` (e.g. after `_on_preset_changed`'s helpers):

```python
    def _match_closest(self):
        if self._measured_freqs is None or self._measured_db is None:
            QMessageBox.warning(self, "No Measurement", "Load a measurement first.")
            return
        if not self._builtin_targets:
            QMessageBox.warning(self, "No Targets", "No built-in targets available.")
            return
        from paraeq.correction.target_curves import match_closest_target

        best = match_closest_target(self._measured_freqs, self._measured_db, self._builtin_targets)
        for i in range(self._preset_combo.count()):
            t = self._preset_combo.itemData(i)
            if t is best:
                self._preset_combo.setCurrentIndex(i)  # fires _on_preset_changed
                break
        self._status_label.setText(f"Closest target: {best.name}")
        logger.info("Auto-matched closest target: %s", best.name)
```

- [ ] **Step 3: Manual smoke test**

Launch the app, load a measurement on the **Target** tab, click **Match closest**. Expected: the
preset dropdown jumps to a built-in target and the status label reads "Closest target: …".

- [ ] **Step 4: Commit**

```bash
git add app/editor/target_curve_editor.py
git commit -m "feat(editor): Match closest button using match_closest_target

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 10: AutoEQ browser dialog (GUI smoke)

**Files:**
- Create: `app/eq/autoeq_browser.py`

- [ ] **Step 1: Create the dialog + worker**

Create `app/eq/autoeq_browser.py`:

```python
"""Searchable AutoEq headphone-preset browser dialog.

Network I/O runs on a QThread worker (the AutoEQClient is synchronous). On
accept, the selected model's parsed bands + preamp are exposed via
``selected_bands`` / ``selected_preamp_db`` for the caller to apply.
"""

import logging

from PyQt6.QtCore import Qt, QThread, pyqtSignal
from PyQt6.QtWidgets import (
    QDialog,
    QDialogButtonBox,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QVBoxLayout,
)

logger = logging.getLogger(__name__)

_ATTRIBUTION = (
    "Presets from the AutoEq project (github.com/jaakkopasanen/AutoEq), "
    "measured by oratory1990, crinacle, Rtings, and others."
)


class _AutoEQWorker(QThread):
    """Runs one AutoEQClient call off the GUI thread."""

    index_ready = pyqtSignal(list)        # list[AutoEQEntry]
    preset_ready = pyqtSignal(object)     # ParsedPreset
    failed = pyqtSignal(str)

    def __init__(self, client, mode, entry=None):
        super().__init__()
        self._client = client
        self._mode = mode                 # "index" or "preset"
        self._entry = entry

    def run(self):
        try:
            if self._mode == "index":
                self.index_ready.emit(self._client.fetch_index())
            else:
                self.preset_ready.emit(self._client.fetch_preset(self._entry))
        except Exception as exc:  # noqa: BLE001 — surfaced to the UI
            logger.error("AutoEq worker (%s) failed: %s", self._mode, exc)
            self.failed.emit(str(exc))


class AutoEQBrowserDialog(QDialog):
    def __init__(self, parent=None, client=None):
        super().__init__(parent)
        self.setWindowTitle("Browse AutoEQ Database")
        self.resize(520, 560)

        if client is None:
            from paraeq.correction.autoeq_db import AutoEQClient

            client = AutoEQClient()
        self._client = client
        self._entries = []
        self._worker = None

        self.selected_bands = []
        self.selected_preamp_db = 0.0

        self._build_ui()
        self._load_index()

    def _build_ui(self):
        layout = QVBoxLayout(self)
        self._search = QLineEdit()
        self._search.setPlaceholderText("Search headphone model…")
        self._search.textChanged.connect(self._apply_filter)
        layout.addWidget(self._search)

        self._list = QListWidget()
        self._list.itemDoubleClicked.connect(lambda _i: self._accept_selection())
        layout.addWidget(self._list, 1)

        self._status = QLabel("Loading index…")
        layout.addWidget(self._status)

        attribution = QLabel(_ATTRIBUTION)
        attribution.setWordWrap(True)
        attribution.setStyleSheet("color: gray; font-size: 10px;")
        layout.addWidget(attribution)

        self._buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        self._buttons.accepted.connect(self._accept_selection)
        self._buttons.rejected.connect(self.reject)
        layout.addWidget(self._buttons)

    # --- index ---
    def _load_index(self):
        self._worker = _AutoEQWorker(self._client, "index")
        self._worker.index_ready.connect(self._on_index_ready)
        self._worker.failed.connect(self._on_failed)
        self._worker.start()

    def _on_index_ready(self, entries):
        self._entries = entries
        self._status.setText(f"{len(entries)} models. Double-click or select + OK.")
        self._apply_filter(self._search.text())

    def _apply_filter(self, text):
        needle = text.strip().lower()
        self._list.clear()
        for entry in self._entries:
            if needle and needle not in entry.name.lower():
                continue
            item = QListWidgetItem(f"{entry.name}  —  {entry.source} / {entry.rig}")
            item.setData(Qt.ItemDataRole.UserRole, entry)
            self._list.addItem(item)

    # --- preset ---
    def _accept_selection(self):
        item = self._list.currentItem()
        if item is None:
            self._status.setText("Select a model first.")
            return
        entry = item.data(Qt.ItemDataRole.UserRole)
        self._status.setText(f"Fetching {entry.name}…")
        self._buttons.setEnabled(False)
        self._worker = _AutoEQWorker(self._client, "preset", entry)
        self._worker.preset_ready.connect(self._on_preset_ready)
        self._worker.failed.connect(self._on_failed)
        self._worker.start()

    def _on_preset_ready(self, parsed):
        self.selected_bands = parsed.bands
        self.selected_preamp_db = parsed.preamp_db
        logger.info("AutoEq preset selected: %d bands, preamp %.1f dB", len(parsed.bands), parsed.preamp_db)
        self.accept()

    def _on_failed(self, message):
        self._buttons.setEnabled(True)
        self._status.setText(f"Error: {message}")
```

- [ ] **Step 2: Manual smoke test (uses real network)**

Add a tiny throwaway launcher or test from the EQ tab in Task 11; for now just confirm the module
imports: `/Users/ronakpatel/code/paraeq/.venv/bin/python -c "import app.eq.autoeq_browser"`.
Expected: no output, exit 0.

- [ ] **Step 3: Commit**

```bash
git add app/eq/autoeq_browser.py
git commit -m "feat(eq): AutoEQ database browser dialog with threaded fetch

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 11: Wire the browser into Manual EQ + preamp → master gain (GUI smoke)

**Files:**
- Modify: `app/eq/manual_eq_editor.py`
- Modify: `app/main_window.py`

- [ ] **Step 1: Add the button + signal to ManualEQEditor**

In `app/eq/manual_eq_editor.py`:

(a) Add a signal under `eq_changed`:

```python
    preamp_changed = pyqtSignal(float)  # AutoEq preamp in dB
```

(b) In `_setup_ui`, insert a button immediately before `self._import_btn`:

```python
        self._browse_btn = QPushButton("Browse AutoEQ DB…")
        self._browse_btn.clicked.connect(self._browse_autoeq_db)
        tb.addWidget(self._browse_btn)
```

(c) Add the handler (near `_import_autoeq`):

```python
    def _browse_autoeq_db(self):
        from app.eq.autoeq_browser import AutoEQBrowserDialog

        dlg = AutoEQBrowserDialog(self)
        if dlg.exec() != dlg.DialogCode.Accepted:
            return
        if not dlg.selected_bands:
            QMessageBox.warning(self, "AutoEQ", "That preset had no usable bands.")
            return
        self.load_bands(dlg.selected_bands)
        self.preamp_changed.emit(dlg.selected_preamp_db)
        logger.info(
            "Loaded AutoEq preset: %d bands, preamp %.1f dB",
            len(dlg.selected_bands), dlg.selected_preamp_db,
        )
```

- [ ] **Step 2: Map preamp to master gain in MainWindow**

In `app/main_window.py` `_connect_signals`, in the Manual-EQ block, add:

```python
        if hasattr(self._eq_tab, "preamp_changed"):
            self._eq_tab.preamp_changed.connect(self._on_preamp_changed)
```

Add the handler near `_on_eq_changed`:

```python
    def _on_preamp_changed(self, preamp_db: float):
        """Apply an AutoEq preset's preamp to the master volume.

        AutoEq presets include a negative preamp to leave headroom for their
        boosts. We fold it into the engine's master gain (BlackHole exposes no
        system volume), relative to the current slider position.
        """
        slider_gain = (self._volume_slider.value() / 100.0) ** 2
        gain = slider_gain * (10.0 ** (preamp_db / 20.0))
        if self._audio_engine is not None and hasattr(self._audio_engine, "set_gain"):
            self._audio_engine.set_gain(gain)
        self._status_bar.showMessage(f"AutoEq preamp applied: {preamp_db:+.1f} dB")
        logger.info("Applied AutoEq preamp %.1f dB (gain=%.3f)", preamp_db, gain)
```

- [ ] **Step 3: Manual smoke test (uses real network)**

Launch the app, **EQ** tab, click **Browse AutoEQ DB…**. Expected: dialog lists thousands of models;
typing "HD 600" filters; selecting one + OK fills the band table, redraws the composite, and (if audio
is running) the status bar shows the preamp applied. First open downloads `INDEX.md` to the cache dir;
second open is instant (cache hit).

- [ ] **Step 4: Verify the suite still imports cleanly**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/ -q`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add app/eq/manual_eq_editor.py app/main_window.py
git commit -m "feat(eq): Browse AutoEQ DB button + preamp-to-master-gain wiring

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Task 12: Docs + knowledge capture + final verification

**Files:**
- Modify: `docs/CONTEXT.md`

- [ ] **Step 1: Run the full suite**

Run: `/Users/ronakpatel/code/paraeq/.venv/bin/pytest tests/ -v`
Expected: all green; new tests present (anchor×4, match_closest×2, autoeq_db×~10). Note the total count.

- [ ] **Step 2: Update CONTEXT.md "What's Left"**

In `docs/CONTEXT.md`, mark these done (strike + ✅ like the existing aggregate entry) under Near-term:
- Interactive draggable control points on the target curve editor → **Done.**
- AutoEQ database integration → **Done.**

Bump "Last updated" and the test count in the Testing section.

- [ ] **Step 3: Capture port knowledge**

Add a short "Notable Implementation Decisions" bullet block to `docs/CONTEXT.md`:

```markdown
- **Target editor uses a deviation-layer model** (`build_anchor_target` in
  `paraeq/correction/target_curves.py`): 16 fixed log-spaced anchors carry dB
  *offsets* added on top of the selected preset, sampled on the union of the
  base's frequencies and the anchor frequencies. This preserves a dense preset's
  fine detail (e.g. the Harman ear-gain peak) while letting the user tweak it.
  Anchors are vertical-drag-only, so anchor frequencies stay sorted/distinct and
  CubicSpline never sees degenerate input. The pyqtgraph overlay works in
  **log10(Hz)** coordinates because the plot is `setLogMode(x=True)`.
- **Live preview swaps a minimum-phase FIR** into the single
  `AudioEngine.set_processor` slot, debounced ~70 ms; it shares that slot with
  the manual-EQ IIR path. The Rust port should keep the same single-processor
  contract.
- **AutoEq browser uses `results/INDEX.md` as the index** (not `webapp/data/*.json`),
  because the markdown gives each model's exact relative folder path — sidestepping
  the inconsistent folder layout. Preset filenames vary in casing
  (`ParametricEq.txt` vs `ParametricEQ.txt`); the client tries both and
  percent-encodes spaces. Index + presets are cached under the platformdirs data
  dir; only first access hits the network.
```

- [ ] **Step 4: Commit**

```bash
git add docs/CONTEXT.md
git commit -m "docs(context): mark target interactivity + AutoEQ browser done; capture port knowledge

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

- [ ] **Step 5: Finish the branch**

Use the superpowers:finishing-a-development-branch skill to decide merge vs PR for
`feature/target-editor-autoeq` into `main`.

---

## Self-Review Notes (for the implementer)

- **Spec coverage:** Feature 1a → Tasks 1, 7; 1b → Task 8 (+ signal in 7); 1c → Tasks 2, 9.
  Feature 2 library → Tasks 3, 4, 5; canonical-parser reuse → Task 6; dialog → Task 10; wiring +
  preamp → Task 11. Cross-cutting tests live in their feature tasks; docs/knowledge → Task 12.
- **Type consistency:** `AutoEQEntry(name, source, rig, rel_path)`, `ParsedPreset(preamp_db, bands)`,
  `build_anchor_target(base, anchor_freqs, offsets_db, *, name)`,
  `match_closest_target(measured_freqs, measured_db, targets)`,
  `DraggableAnchors(on_moved)` with `set_anchors(freqs_hz, gains_db)`,
  `target_preview(object, object)`, `preamp_changed(float)` — all referenced consistently across tasks.
- **GUI caveat:** Tasks 6–11 touch `app/` (no display in CI). Their "tests" are manual smoke checks;
  every such task still runs `pytest tests/` to catch import-time breakage. Verify the pyqtgraph drag
  API (Task 7 Step 1) before relying on the overlay code.
