# Refined Target Curve Data — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the four hand-typed approximate target CSVs with high-resolution AutoEQ-sourced data, expand the built-in set to six curves, and extend the CSV header schema with optional metadata (`name`, `category`, `description`, `source`).

**Architecture:** Pure data + loader cycle. `TargetCurve` dataclass gains three optional fields. `load_target_csv()` parses `# key: value` lines from the comment header. All target CSVs in `targets/` are rewritten with metadata headers. No GUI changes, no DSP changes, no profile schema changes.

**Tech Stack:** Python 3, NumPy, SciPy (`CubicSpline`, already in use), `pytest` for tests, `curl` + shell for fetching AutoEQ data.

**Spec:** `docs/specs/2026-04-26-refined-target-curves-design.md`

---

## File Structure

**Modified files:**
- `paraeq/correction/target_curves.py` — extend `TargetCurve` dataclass + `load_target_csv()` to parse metadata headers
- `tests/test_target_curves.py` — add new tests; existing tests pass unchanged
- `docs/CONTEXT.md` — record AutoEQ commit pin and updated curve list

**Overwritten target files (filename stems preserved):**
- `targets/flat.csv`
- `targets/harman_ie_2019.csv`
- `targets/harman_oe_2018.csv`
- `targets/diffuse_field.csv`

**New target files:**
- `targets/harman_ie_2019_without_bass.csv`
- `targets/harman_oe_2018_without_bass.csv`

**AutoEQ commit pinned:** `7ae0f56d53074872b028649617a22bbb4232feb7` (short: `7ae0f56`)

---

### Task 1: Extend `TargetCurve` dataclass with optional metadata fields

**Files:**
- Modify: `paraeq/correction/target_curves.py:16-20` (the `@dataclass` block)
- Test: `tests/test_target_curves.py`

- [ ] **Step 1: Write the failing test**

Add to `tests/test_target_curves.py`:

```python
def test_target_curve_metadata_fields_default_to_none():
    curve = TargetCurve(
        name="x",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
    )
    assert curve.category is None
    assert curve.description is None
    assert curve.source is None


def test_target_curve_metadata_fields_can_be_set():
    curve = TargetCurve(
        name="x",
        frequencies=np.array([20.0, 20000.0]),
        gains_db=np.array([0.0, 0.0]),
        category="reference",
        description="A flat curve.",
        source="hand-authored",
    )
    assert curve.category == "reference"
    assert curve.description == "A flat curve."
    assert curve.source == "hand-authored"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `source .venv/bin/activate && pytest tests/test_target_curves.py::test_target_curve_metadata_fields_default_to_none tests/test_target_curves.py::test_target_curve_metadata_fields_can_be_set -v`
Expected: FAIL with `TypeError: TargetCurve.__init__() got an unexpected keyword argument 'category'`

- [ ] **Step 3: Add metadata fields to the dataclass**

Edit `paraeq/correction/target_curves.py` — the `TargetCurve` block. Replace:

```python
@dataclass
class TargetCurve:
    name: str
    frequencies: np.ndarray
    gains_db: np.ndarray

    def interpolate(self, query_freqs: np.ndarray) -> np.ndarray:
```

With:

```python
@dataclass
class TargetCurve:
    name: str
    frequencies: np.ndarray
    gains_db: np.ndarray
    category: str | None = None
    description: str | None = None
    source: str | None = None

    def interpolate(self, query_freqs: np.ndarray) -> np.ndarray:
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_target_curves.py -v`
Expected: all tests PASS, including the two new ones (existing tests are unaffected — new fields have defaults).

- [ ] **Step 5: Commit**

```bash
git add paraeq/correction/target_curves.py tests/test_target_curves.py
git commit -m "feat(target_curves): add optional metadata fields to TargetCurve dataclass"
```

---

### Task 2: Parse `# key: value` metadata headers in `load_target_csv` (happy path)

**Files:**
- Modify: `paraeq/correction/target_curves.py:50-82` (the `load_target_csv` function)
- Test: `tests/test_target_curves.py`
- Create: `tests/fixtures/target_curve_with_metadata.csv`

- [ ] **Step 1: Create the test fixture**

Create `tests/fixtures/target_curve_with_metadata.csv`:

```
# name: Sample Target
# category: reference
# description: A two-point sample target for testing metadata parsing.
# source: hand-authored
# frequency_hz,gain_db
20.0,0.0
20000.0,-3.0
```

(If `tests/fixtures/` does not exist, the directory will be auto-created by `git add`. Verify with `ls tests/fixtures/` before adding the file — fixtures dir already exists per the project structure.)

- [ ] **Step 2: Write the failing test**

Add to `tests/test_target_curves.py`:

```python
def test_load_target_csv_parses_metadata_header():
    fixtures = Path(__file__).parent / "fixtures"
    curve = load_target_csv(fixtures / "target_curve_with_metadata.csv")
    assert curve.name == "Sample Target"
    assert curve.category == "reference"
    assert curve.description == "A two-point sample target for testing metadata parsing."
    assert curve.source == "hand-authored"
    assert len(curve.frequencies) == 2
    assert curve.gains_db[1] == -3.0
```

- [ ] **Step 3: Run test to verify it fails**

Run: `pytest tests/test_target_curves.py::test_load_target_csv_parses_metadata_header -v`
Expected: FAIL — `curve.name` will be `"target_curve_with_metadata"` (file stem), not `"Sample Target"`. `category`, `description`, `source` will be `None`.

- [ ] **Step 4: Extend `load_target_csv` to parse metadata**

Replace the body of `load_target_csv` in `paraeq/correction/target_curves.py`. Replace:

```python
def load_target_csv(filepath: Path) -> TargetCurve:
    """Load a target curve from a CSV file.

    Lines beginning with '#' and blank lines are ignored. Each data line must
    be ``frequency_hz,gain_db``.

    Args:
        filepath: Path to the CSV file.

    Returns:
        A :class:`TargetCurve` with ``name`` set to the file stem.

    Raises:
        FileNotFoundError: If ``filepath`` does not exist.
        ValueError: If a data line cannot be parsed as two floats.
    """
    freqs: list[float] = []
    gains: list[float] = []
    with open(filepath, "r") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(",")
            freqs.append(float(parts[0]))
            gains.append(float(parts[1]))
    name = filepath.stem
    logger.info("Loaded target curve '%s' with %d points from %s", name, len(freqs), filepath)
    return TargetCurve(
        name=name,
        frequencies=np.array(freqs, dtype=np.float64),
        gains_db=np.array(gains, dtype=np.float64),
    )
```

With:

```python
def load_target_csv(filepath: Path) -> TargetCurve:
    """Load a target curve from a CSV file.

    Lines beginning with '#' and blank lines are ignored. Each non-comment
    line must be ``frequency_hz,gain_db``. Comment lines of the form
    ``# key: value`` populate optional metadata on the returned curve;
    recognized keys (case-insensitive) are ``name``, ``category``,
    ``description``, ``source``. Unknown keys are silently ignored.

    Args:
        filepath: Path to the CSV file.

    Returns:
        A :class:`TargetCurve`. ``name`` falls back to the file stem if no
        ``# name:`` header is present.

    Raises:
        FileNotFoundError: If ``filepath`` does not exist.
        ValueError: If a data line cannot be parsed as two floats.
    """
    freqs: list[float] = []
    gains: list[float] = []
    metadata: dict[str, str] = {}
    recognized_keys = {"name", "category", "description", "source"}
    with open(filepath, "r") as f:
        for line in f:
            stripped = line.strip()
            if not stripped:
                continue
            if stripped.startswith("#"):
                # Try to parse "# key: value"
                content = stripped.lstrip("#").strip()
                if ":" in content:
                    key, _, value = content.partition(":")
                    key_lower = key.strip().lower()
                    if key_lower in recognized_keys:
                        metadata[key_lower] = value.strip()
                continue
            parts = stripped.split(",")
            freqs.append(float(parts[0]))
            gains.append(float(parts[1]))
    name = metadata.get("name", filepath.stem)
    logger.info("Loaded target curve '%s' with %d points from %s", name, len(freqs), filepath)
    return TargetCurve(
        name=name,
        frequencies=np.array(freqs, dtype=np.float64),
        gains_db=np.array(gains, dtype=np.float64),
        category=metadata.get("category"),
        description=metadata.get("description"),
        source=metadata.get("source"),
    )
```

- [ ] **Step 5: Run test to verify it passes**

Run: `pytest tests/test_target_curves.py -v`
Expected: all tests PASS, including `test_load_target_csv_parses_metadata_header`.

- [ ] **Step 6: Commit**

```bash
git add paraeq/correction/target_curves.py tests/test_target_curves.py tests/fixtures/target_curve_with_metadata.csv
git commit -m "feat(target_curves): parse '# key: value' metadata headers in load_target_csv"
```

---

### Task 3: Handle unknown header keys and case-insensitive matching

**Files:**
- Test: `tests/test_target_curves.py`
- Create: `tests/fixtures/target_curve_unknown_and_mixed_case.csv`

The implementation in Task 2 already handles these cases (unknown keys silently ignored, `key.strip().lower()` makes matching case-insensitive). Task 3 adds explicit regression tests.

- [ ] **Step 1: Create the test fixture**

Create `tests/fixtures/target_curve_unknown_and_mixed_case.csv`:

```
# Name: Mixed Case Sample
# CATEGORY: reference
# Description:    Surrounding whitespace is preserved-stripped.
# foo: this should be ignored
# version: 1.0
# frequency_hz,gain_db
20.0,0.0
20000.0,-3.0
```

- [ ] **Step 2: Write the failing test (it should already pass)**

Add to `tests/test_target_curves.py`:

```python
def test_load_target_csv_ignores_unknown_keys_and_is_case_insensitive():
    fixtures = Path(__file__).parent / "fixtures"
    curve = load_target_csv(fixtures / "target_curve_unknown_and_mixed_case.csv")
    assert curve.name == "Mixed Case Sample"
    assert curve.category == "reference"
    assert curve.description == "Surrounding whitespace is preserved-stripped."
    # source not in fixture → falls back to None
    assert curve.source is None
    # data still parses normally
    assert len(curve.frequencies) == 2
```

- [ ] **Step 3: Run test to verify it passes**

Run: `pytest tests/test_target_curves.py::test_load_target_csv_ignores_unknown_keys_and_is_case_insensitive -v`
Expected: PASS (the implementation already handles this — this test is a regression guard).

If it FAILS, debug the implementation before proceeding.

- [ ] **Step 4: Commit**

```bash
git add tests/test_target_curves.py tests/fixtures/target_curve_unknown_and_mixed_case.csv
git commit -m "test(target_curves): regression tests for unknown keys + case-insensitive metadata"
```

---

### Task 4: Backward-compatibility test for old-format CSVs

**Files:**
- Test: `tests/test_target_curves.py`
- Create: `tests/fixtures/target_curve_no_metadata.csv`

- [ ] **Step 1: Create the test fixture**

Create `tests/fixtures/target_curve_no_metadata.csv` (mimics the legacy header style):

```
# Some Old Curve (approximate)
# frequency_hz,gain_db
20.0,0.0
1000.0,-3.0
20000.0,-6.0
```

- [ ] **Step 2: Write the failing test**

Add to `tests/test_target_curves.py`:

```python
def test_load_target_csv_no_metadata_falls_back_to_stem():
    fixtures = Path(__file__).parent / "fixtures"
    curve = load_target_csv(fixtures / "target_curve_no_metadata.csv")
    assert curve.name == "target_curve_no_metadata"  # file stem
    assert curve.category is None
    assert curve.description is None
    assert curve.source is None
    assert len(curve.frequencies) == 3
    assert curve.gains_db[2] == -6.0
```

- [ ] **Step 3: Run test to verify it passes**

Run: `pytest tests/test_target_curves.py::test_load_target_csv_no_metadata_falls_back_to_stem -v`
Expected: PASS (regression guard — existing behavior is preserved by the implementation).

If it FAILS, the implementation in Task 2 needs revision.

- [ ] **Step 4: Commit**

```bash
git add tests/test_target_curves.py tests/fixtures/target_curve_no_metadata.csv
git commit -m "test(target_curves): backward-compat test for legacy CSVs without metadata"
```

---

### Task 5: Replace `flat.csv` with metadata-headered version

**Files:**
- Modify (overwrite): `targets/flat.csv`

- [ ] **Step 1: Overwrite `flat.csv`**

Use Write tool to replace the contents of `targets/flat.csv` with:

```
# name: Flat
# category: reference
# description: Reference 0 dB target. Applies no correction beyond what the user explicitly draws. Useful baseline for manual EQ work.
# source: hand-authored (two-point flat reference)
# frequency_hz,gain_db
20.0,0.0
20000.0,0.0
```

- [ ] **Step 2: Verify it loads correctly**

Run:
```bash
source .venv/bin/activate && python -c "
from paraeq.correction.target_curves import load_target_csv
from pathlib import Path
c = load_target_csv(Path('targets/flat.csv'))
print(f'name={c.name!r}, category={c.category!r}, description={c.description[:40]!r}..., source={c.source!r}')
print(f'points: {len(c.frequencies)}, gains: {list(c.gains_db)}')
"
```
Expected output (similar to):
```
name='Flat', category='reference', description='Reference 0 dB target. Applies no correc'..., source='hand-authored (two-point flat reference)'
points: 2, gains: [0.0, 0.0]
```

- [ ] **Step 3: Run existing tests to confirm `test_load_target_csv` still passes**

Run: `pytest tests/test_target_curves.py::test_load_target_csv -v`
Expected: PASS. (The existing test asserts `curve.name == "flat"` — but the CSV now has `# name: Flat` (capitalized). This will FAIL.)

- [ ] **Step 4: Update `test_load_target_csv` to match the new metadata-driven name**

The existing test asserts `curve.name == "flat"`. With the metadata header, the name is now `"Flat"`. Update the assertion in `tests/test_target_curves.py`:

Replace:
```python
def test_load_target_csv():
    targets_dir = Path(__file__).parent.parent / "targets"
    curve = load_target_csv(targets_dir / "flat.csv")
    assert curve.name == "flat"
    assert len(curve.frequencies) == 2
    assert curve.frequencies[0] == 20.0
    assert curve.gains_db[0] == 0.0
```

With:
```python
def test_load_target_csv():
    targets_dir = Path(__file__).parent.parent / "targets"
    curve = load_target_csv(targets_dir / "flat.csv")
    assert curve.name == "Flat"  # populated from "# name:" header
    assert curve.category == "reference"
    assert len(curve.frequencies) == 2
    assert curve.frequencies[0] == 20.0
    assert curve.gains_db[0] == 0.0
```

- [ ] **Step 5: Run test again**

Run: `pytest tests/test_target_curves.py::test_load_target_csv -v`
Expected: PASS.

- [ ] **Step 6: Run the existing `test_list_builtin_targets` and verify it still passes**

Run: `pytest tests/test_target_curves.py::test_list_builtin_targets -v`
Expected: FAIL on `assert "flat" in names` — because the curve's `name` is now `"Flat"` (from the metadata header), not the file stem `"flat"`.

- [ ] **Step 7: Update `test_list_builtin_targets` to use display names**

The set membership check currently expects file stems. With metadata-driven names, the set will contain display names. Update `tests/test_target_curves.py`:

Replace:
```python
def test_list_builtin_targets():
    targets = list_builtin_targets()
    names = {t.name for t in targets}
    assert "diffuse_field" in names
    assert "flat" in names
    assert "harman_ie_2019" in names
    assert "harman_oe_2018" in names
```

With:
```python
def test_list_builtin_targets():
    targets = list_builtin_targets()
    names = {t.name for t in targets}
    # Names come from "# name:" headers (display names), not file stems.
    assert "Flat" in names
```

(The Harman / diffuse-field assertions will be re-added in Task 11 once their files are written. Removing them here keeps this task self-contained — the test only asserts what currently exists in `targets/`.)

- [ ] **Step 8: Run all target_curves tests**

Run: `pytest tests/test_target_curves.py -v`
Expected: all PASS.

- [ ] **Step 9: Commit**

```bash
git add targets/flat.csv tests/test_target_curves.py
git commit -m "feat(targets): rewrite flat.csv with metadata header; update tests"
```

---

### Task 6: Fetch the 5 AutoEQ source CSVs and write them with metadata headers

**Files:**
- Overwrite: `targets/harman_ie_2019.csv`, `targets/harman_oe_2018.csv`, `targets/diffuse_field.csv`
- Create: `targets/harman_ie_2019_without_bass.csv`, `targets/harman_oe_2018_without_bass.csv`

Each new file consists of: 5 metadata header lines + AutoEQ data points (with the upstream `frequency,raw` first-line header stripped via `tail -n +2`).

- [ ] **Step 1: Verify the AutoEQ commit hash and source files are reachable**

Run (sanity check):
```bash
COMMIT=7ae0f56d53074872b028649617a22bbb4232feb7
for FNAME in "Harman in-ear 2019" "Harman in-ear 2019 without bass" "Harman over-ear 2018" "Harman over-ear 2018 without bass" "Diffuse field 5128"; do
  ENC=$(python3 -c "from urllib.parse import quote; print(quote('$FNAME.csv'))")
  URL="https://raw.githubusercontent.com/jaakkopasanen/AutoEq/$COMMIT/targets/$ENC"
  echo "Checking $URL"
  curl -sI "$URL" | head -1
done
```
Expected: each request prints `HTTP/2 200`.

If any returns 404, stop and investigate before proceeding (the AutoEQ repo layout may have changed; consult `https://github.com/jaakkopasanen/AutoEq/tree/$COMMIT/targets`).

- [ ] **Step 2: Write `targets/harman_ie_2019.csv`**

Run:
```bash
COMMIT=7ae0f56d53074872b028649617a22bbb4232feb7
URL="https://raw.githubusercontent.com/jaakkopasanen/AutoEq/$COMMIT/targets/Harman%20in-ear%202019.csv"
{
  echo "# name: Harman In-Ear 2019"
  echo "# category: in-ear"
  echo "# description: Sean Olive's 2019 in-ear preference target. Bass-emphasized low end (+7 dB shelf below 200 Hz), gentle 3 kHz presence peak, attenuated treble above 8 kHz. Best fit for IEMs."
  echo "# source: AutoEq @ commit 7ae0f56 / Harman in-ear 2019.csv"
  echo "# frequency_hz,gain_db"
  curl -sL "$URL" | tail -n +2
} > targets/harman_ie_2019.csv
```

Verify:
```bash
head -10 targets/harman_ie_2019.csv && echo "---" && wc -l targets/harman_ie_2019.csv
```
Expected: header lines visible, then `20.00,7.61` etc., total ~700 lines.

- [ ] **Step 3: Write `targets/harman_ie_2019_without_bass.csv`**

Run:
```bash
COMMIT=7ae0f56d53074872b028649617a22bbb4232feb7
URL="https://raw.githubusercontent.com/jaakkopasanen/AutoEq/$COMMIT/targets/Harman%20in-ear%202019%20without%20bass.csv"
{
  echo "# name: Harman In-Ear 2019 (No Bass Shelf)"
  echo "# category: in-ear"
  echo "# description: Harman 2019 in-ear with the +7 dB sub-bass shelf removed. Preferred by treble-sensitive listeners and as a 'neutral IEM' reference target."
  echo "# source: AutoEq @ commit 7ae0f56 / Harman in-ear 2019 without bass.csv"
  echo "# frequency_hz,gain_db"
  curl -sL "$URL" | tail -n +2
} > targets/harman_ie_2019_without_bass.csv
```

Verify with `head -10 targets/harman_ie_2019_without_bass.csv`.

- [ ] **Step 4: Write `targets/harman_oe_2018.csv`**

Run:
```bash
COMMIT=7ae0f56d53074872b028649617a22bbb4232feb7
URL="https://raw.githubusercontent.com/jaakkopasanen/AutoEq/$COMMIT/targets/Harman%20over-ear%202018.csv"
{
  echo "# name: Harman Over-Ear 2018"
  echo "# category: over-ear"
  echo "# description: Sean Olive's 2018 over-ear preference target. Mild bass shelf, pronounced 3 kHz ear-gain peak, gentle treble roll-off. Best fit for closed-back and open-back over-ear headphones."
  echo "# source: AutoEq @ commit 7ae0f56 / Harman over-ear 2018.csv"
  echo "# frequency_hz,gain_db"
  curl -sL "$URL" | tail -n +2
} > targets/harman_oe_2018.csv
```

- [ ] **Step 5: Write `targets/harman_oe_2018_without_bass.csv`**

Run:
```bash
COMMIT=7ae0f56d53074872b028649617a22bbb4232feb7
URL="https://raw.githubusercontent.com/jaakkopasanen/AutoEq/$COMMIT/targets/Harman%20over-ear%202018%20without%20bass.csv"
{
  echo "# name: Harman Over-Ear 2018 (No Bass Shelf)"
  echo "# category: over-ear"
  echo "# description: Harman 2018 over-ear with the bass shelf removed. Preferred for over-ear listening at lower volumes or by treble-sensitive ears."
  echo "# source: AutoEq @ commit 7ae0f56 / Harman over-ear 2018 without bass.csv"
  echo "# frequency_hz,gain_db"
  curl -sL "$URL" | tail -n +2
} > targets/harman_oe_2018_without_bass.csv
```

- [ ] **Step 6: Write `targets/diffuse_field.csv`**

Run:
```bash
COMMIT=7ae0f56d53074872b028649617a22bbb4232feb7
URL="https://raw.githubusercontent.com/jaakkopasanen/AutoEq/$COMMIT/targets/Diffuse%20field%205128.csv"
{
  echo "# name: Diffuse Field"
  echo "# category: reference"
  echo "# description: Diffuse field reference based on B&K Type 5128 head-and-torso simulator. Academic neutral reference; flatter than Harman targets."
  echo "# source: AutoEq @ commit 7ae0f56 / Diffuse field 5128.csv"
  echo "# frequency_hz,gain_db"
  curl -sL "$URL" | tail -n +2
} > targets/diffuse_field.csv
```

- [ ] **Step 7: Sanity check all six built-in CSVs load**

Run:
```bash
source .venv/bin/activate && python -c "
from paraeq.correction.target_curves import list_builtin_targets
for c in list_builtin_targets():
    print(f'{c.name:50s} category={c.category!r:12s} points={len(c.frequencies):4d}')
"
```
Expected output (order may vary by alphabetical filename):
```
Diffuse Field                                       category='reference'  points= 695
Flat                                                category='reference'  points=   2
Harman In-Ear 2019                                  category='in-ear'     points= 695
Harman In-Ear 2019 (No Bass Shelf)                  category='in-ear'     points= 695
Harman Over-Ear 2018                                category='over-ear'   points= 695
Harman Over-Ear 2018 (No Bass Shelf)                category='over-ear'   points= 695
```

(Exact `points=` count may differ slightly depending on AutoEQ's source data; should be in the 600–800 range.)

- [ ] **Step 8: Run the test suite — `test_list_builtin_targets` will fail until updated**

Run: `pytest tests/test_target_curves.py -v`
Expected: `test_list_builtin_targets` PASSES (the assertion only checks for `"Flat"` after Task 5; the new files add more curves but don't remove `"Flat"`).

The other tests should also PASS — including `test_target_curve_interpolate` and `test_compute_correction` which don't touch files, and `test_load_target_csv` which only loads `flat.csv`.

- [ ] **Step 9: Commit**

```bash
git add targets/
git commit -m "feat(targets): add 5 AutoEQ-sourced builtin target curves with metadata headers

Sources pinned to AutoEQ commit 7ae0f56:
- Harman in-ear 2019 (with and without bass)
- Harman over-ear 2018 (with and without bass)
- Diffuse field (B&K 5128)

All six builtin curves now carry name/category/description/source metadata."
```

---

### Task 7: Add new tests for builtin curve set and descriptions

**Files:**
- Test: `tests/test_target_curves.py`

- [ ] **Step 1: Add the failing tests**

Add to `tests/test_target_curves.py`:

```python
def test_list_builtin_targets_returns_six():
    targets = list_builtin_targets()
    assert len(targets) == 6
    names = {t.name for t in targets}
    assert names == {
        "Diffuse Field",
        "Flat",
        "Harman In-Ear 2019",
        "Harman In-Ear 2019 (No Bass Shelf)",
        "Harman Over-Ear 2018",
        "Harman Over-Ear 2018 (No Bass Shelf)",
    }


def test_builtin_curves_have_descriptions_and_categories():
    targets = list_builtin_targets()
    for t in targets:
        assert t.description, f"{t.name!r} missing description"
        assert t.category in ("in-ear", "over-ear", "reference"), (
            f"{t.name!r} has unexpected category {t.category!r}"
        )
        assert t.source, f"{t.name!r} missing source"
```

- [ ] **Step 2: Run the new tests to verify they pass**

Run:
```bash
pytest tests/test_target_curves.py::test_list_builtin_targets_returns_six tests/test_target_curves.py::test_builtin_curves_have_descriptions_and_categories -v
```
Expected: both PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/test_target_curves.py
git commit -m "test(target_curves): assert builtin set is exactly six curves with metadata"
```

---

### Task 8: Run full test suite and smoke-test the GUI

- [ ] **Step 1: Run the entire test suite**

Run: `source .venv/bin/activate && pytest tests/ -v`
Expected: ALL tests PASS (the original 67 plus the 6 new tests added in this plan = 73). Runtime ~1.3s.

If any unrelated test fails, investigate — there should be no breakage outside `test_target_curves.py`.

- [ ] **Step 2: Smoke-test the GUI**

Launch:
```bash
source .venv/bin/activate && python -m app.main
```

Manual checks (the engineer performs these visually; record results):
1. Application launches without exceptions in the terminal.
2. Navigate to the Target tab.
3. Open the preset dropdown — verify all six entries are visible: `Diffuse Field`, `Flat`, `Harman In-Ear 2019`, `Harman In-Ear 2019 (No Bass Shelf)`, `Harman Over-Ear 2018`, `Harman Over-Ear 2018 (No Bass Shelf)`.
4. Select each preset in turn — verify the target curve plots without error. The Harman curves should show characteristic shapes (bass shelf, 3 kHz ear-gain peak, treble roll-off). The "no bass" variants should be visibly flatter below 200 Hz than their full-bass counterparts.
5. Close the app.

- [ ] **Step 3: Record smoke-test results**

If all checks pass, proceed to Task 9.

If any check fails, do not proceed — diagnose and fix. Common likely issues:
- **GUI lists curve by file stem instead of display name**: open `app/editor/target_curve_editor.py` and check how the combo box is populated. The combo currently uses `t.name` from the dataclass — which is now the display name from metadata. This should "just work."
- **Plot looks wrong or empty**: verify the AutoEQ data downloaded correctly (`head targets/<file>.csv`).

- [ ] **Step 4: No commit needed — manual verification only**

---

### Task 9: Update `docs/CONTEXT.md`

**Files:**
- Modify: `docs/CONTEXT.md`

- [ ] **Step 1: Update the "Built-in Target Curves" reference**

Find this line in `docs/CONTEXT.md`:

```markdown
**Built-in Target Curves (`targets/`):** harman_ie_2019.csv, harman_oe_2018.csv, diffuse_field.csv, flat.csv. Approximate reference points — refinement against authoritative sources is a future enhancement.
```

Replace with:

```markdown
**Built-in Target Curves (`targets/`):** flat.csv, harman_ie_2019.csv, harman_ie_2019_without_bass.csv, harman_oe_2018.csv, harman_oe_2018_without_bass.csv, diffuse_field.csv. High-resolution data sourced from AutoEQ (MIT licensed) at commit `7ae0f56` (full SHA `7ae0f56d53074872b028649617a22bbb4232feb7`). Each CSV carries `# name:`, `# category:`, `# description:`, `# source:` metadata headers; the loader (`paraeq/correction/target_curves.py`) populates these on the `TargetCurve` dataclass.
```

- [ ] **Step 2: Update the "Last updated" date and the "What's Left" section**

Find:
```markdown
**Last updated:** 2026-04-25
```

Replace with:
```markdown
**Last updated:** 2026-04-26
```

Find this bullet in "Near-term (still Phase 1 polish)":

```markdown
- **Refine target curve data**: the built-in Harman/Diffuse Field curves are approximate reference points. Replace with authoritative published data.
```

Delete it (the work is done). Replace with a brief note above the list:

Find the section heading:
```markdown
### Near-term (still Phase 1 polish)
```

After the next paragraph, the bullets begin. Find and remove the **Refine target curve data** bullet.

- [ ] **Step 3: Add a "Recently completed" subsection note**

Insert (right after the "Notable Implementation Decisions" section, before "What's Left"):

```markdown
### Recently Completed Cycles

- **2026-04-26 — Cycle 1: Refined target curve data.** Replaced the four hand-typed approximate target CSVs with high-resolution data sourced from AutoEQ at commit `7ae0f56`. Expanded built-in set to six curves (added `harman_ie_2019_without_bass`, `harman_oe_2018_without_bass`). Extended `TargetCurve` and the CSV loader with optional metadata fields (`category`, `description`, `source`); old-style CSVs without metadata still load. See `docs/specs/2026-04-26-refined-target-curves-design.md` and `docs/plans/2026-04-26-refined-target-curves.md`.
```

- [ ] **Step 4: Commit**

```bash
git add docs/CONTEXT.md
git commit -m "docs: record cycle 1 (refined target curves) completion in CONTEXT.md"
```

---

## Self-Review Checklist (for the implementing engineer)

Before declaring this cycle complete, confirm:

- [ ] All 9 tasks above are checked off.
- [ ] `pytest tests/ -v` reports all tests passing.
- [ ] All six target CSVs in `targets/` carry the four metadata header lines (`# name:`, `# category:`, `# description:`, `# source:`).
- [ ] The GUI launches and all six presets are selectable (Task 8 step 2).
- [ ] `docs/CONTEXT.md` reflects the new state.
- [ ] No changes were made to the DSP library outside of `paraeq/correction/target_curves.py`.
- [ ] No GUI code (`app/`) was modified.
- [ ] The AutoEQ commit hash `7ae0f56` is recorded both in each CSV header and in `docs/CONTEXT.md`.
