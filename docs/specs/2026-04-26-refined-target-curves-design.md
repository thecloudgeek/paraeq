# Refined Target Curve Data — Design Spec

**Date:** 2026-04-26
**Status:** Draft
**Author:** Ronak Patel (with Claude)
**Cycle:** 1 of 6 (post–Phase 1 polish)

## Overview

The four built-in target curves shipped with ParaEQ Phase 1 (`harman_ie_2019.csv`, `harman_oe_2018.csv`, `diffuse_field.csv`, `flat.csv`) are hand-typed approximations at ~18 round-number control points. The CSV headers explicitly mark them as "approximate." They produce visibly imprecise correction curves compared to authoritative published data.

This cycle replaces the approximate data with high-resolution, sourced curves drawn from the AutoEQ project (MIT licensed) and expands the built-in set from 4 to 6 curves. It also extends the CSV header schema with optional metadata fields (`name`, `category`, `description`, `source`) so downstream features — the AI advisor in cycle 1.5a/b and the AutoEQ DB picker in cycle 2 — can reason about presets with semantic context.

### Context: This is a data + schema cycle

- **No DSP algorithm changes.** Filter generation, measurement, and engine code are untouched.
- **No GUI restructuring.** The target curve editor reads curves through the existing API; description metadata may surface as a tooltip or label, but UI layout changes are deferred to cycle 1.5a where the editor is restructured anyway.
- **Loader gains optional metadata fields**, with full backward compatibility for files lacking them.

### Phased context (why this matters for the Rust port)

The data files and CSV schema are durable — they will be carried over verbatim when the DSP core ports to Rust. The loader gains a tiny widening (parse a few comment-prefixed metadata fields) that is trivial to reimplement in Rust. There is **no PyQt6-specific code** in this cycle, so there is no throwaway work.

## Goals

1. Replace the 4 existing approximate curves with high-resolution, sourced data.
2. Add 2 additional curves (`harman_ie_2019_v1`, `harman_ie_2019_v2_bass_shy`) for users who prefer alternatives to the modern Harman 2019 v2 standard.
3. Extend `TargetCurve` and the CSV loader to carry optional metadata: `name`, `category`, `description`, `source`.
4. Maintain backward compatibility with the existing CSV format (files without metadata still load).
5. Establish a reproducible pipeline for source attribution: each shipped CSV references the exact AutoEQ commit it was derived from.

## Non-Goals

- Adding every available target curve (Crinacle, Etzold, B&K, Sound Guys, etc.). Users can drop their own CSVs into `targets/`; the loader already supports it. Maintaining a large curated set is decision paralysis for new users and maintenance burden for us.
- Surfacing description metadata in the GUI. The fields are populated and exposed via the dataclass; UI presentation lands in cycle 1.5a.
- Programmatic fetching of AutoEQ curves at runtime. This cycle ships static, vendored data files. Live fetching is cycle 2's concern.
- Algorithmic generation of curves (e.g., parametric Harman 2019 generators). The built-ins are static reference data.
- Manual digitization from Olive/AES papers. AutoEQ is the canonical source.

## Design

### Built-in curve set (6 curves)

| Filename | Purpose |
|---|---|
| `flat.csv` | Reference (0 dB everywhere); no measurement correction beyond what the user explicitly draws |
| `harman_ie_2019_v1.csv` | Sean Olive's earlier 2019 in-ear preference revision; gentler treble attenuation than v2 |
| `harman_ie_2019_v2.csv` | Sean Olive's current modern in-ear preference target; the de facto IEM reference |
| `harman_ie_2019_v2_bass_shy.csv` | Harman IE 2019 v2 with a −3 dB shelf below 200 Hz; preferred by treble-sensitive listeners and the audiophile-neutral community |
| `harman_oe_2018.csv` | Sean Olive's current modern over-ear preference target |
| `diffuse_field.csv` | IEC 60268-7 diffuse field; academic neutral reference |

Approximate curves currently shipped (`harman_ie_2019.csv`, `harman_oe_2018.csv`, `diffuse_field.csv`) are deleted and replaced. The replacements use the same or close filename stems where appropriate; profile compatibility is addressed under Migration.

### Data source: AutoEQ project

All curves except `flat.csv` and `harman_ie_2019_v2_bass_shy.csv` are sourced from [github.com/jaakkopasanen/AutoEq](https://github.com/jaakkopasanen/AutoEq), MIT licensed. A specific commit hash is pinned and recorded in each CSV header. This makes the source auditable and reproducible — anyone can verify the data by checking out the same commit.

`flat.csv` is trivially constructed (two points, both 0 dB).

`harman_ie_2019_v2_bass_shy.csv` is derived from `harman_ie_2019_v2.csv` by applying a low-shelf attenuation: `−3 dB` at and below 200 Hz, transitioning to `0 dB` by 400 Hz with a smooth log-frequency interpolation. The derivation is documented in the CSV header.

### CSV schema extension

Existing schema (preserved):
- Lines starting with `#` are comments; blank lines are ignored.
- Each non-comment line is `frequency_hz,gain_db`.

New schema additions (all optional, all in the comment header):

```
# name: <human-readable curve name>
# category: <in-ear | over-ear | reference | derived>
# source: <AutoEq @ commit <hash> / <upstream filename>>  OR  <free-text source>
# description: <one-sentence description suitable for tooltips and LLM context>
# frequency_hz,gain_db
20.0,-0.3
...
```

Field semantics:
- **`name`**: Display name for the GUI preset combo. Falls back to the file stem (current behavior) if absent.
- **`category`**: Coarse classification. Used for grouping in future GUI work and as input to the AI advisor's preset-selection reasoning. One of `in-ear`, `over-ear`, `reference`, `derived`. Falls back to `None`.
- **`source`**: Provenance string. For AutoEQ-sourced curves, format is `AutoEq @ commit <7-char hash> / <upstream filename>`. For derived or hand-authored curves, free-text describing how the curve was produced.
- **`description`**: One sentence (≤200 chars) describing the curve's character. Surfaced as a tooltip in the GUI (cycle 1.5a) and as context for the AI advisor (cycle 1.5b).

Parsing: header fields are matched as `# <key>: <value>` (case-insensitive key, leading/trailing whitespace stripped). Unknown header fields are ignored silently. The data section parsing is unchanged.

### Loader changes (`paraeq/correction/target_curves.py`)

`TargetCurve` dataclass gains three optional fields:

```python
@dataclass
class TargetCurve:
    name: str
    frequencies: np.ndarray
    gains_db: np.ndarray
    category: str | None = None
    description: str | None = None
    source: str | None = None
```

`load_target_csv()` is extended to:
1. Parse `# key: value` lines from the comment header (stops parsing metadata at the first non-comment line).
2. Populate `name`, `category`, `description`, `source` from matching keys.
3. Fall back to file stem for `name` if no `name:` header is present (current behavior preserved).

`list_builtin_targets()` and `compute_correction()` are unchanged.

### Resolution

AutoEQ target CSVs are typically log-spaced from 20 Hz to 20 kHz with ~140 control points per curve, vs. the existing ~18-point hand-typed curves. Higher resolution is a strict win for cubic-spline interpolation accuracy. No code path needs adjustment; the existing interpolator handles arbitrary point counts.

### Backward compatibility

- **Old CSV format (no metadata header):** still parses correctly. `name` falls back to file stem; `category`, `description`, `source` are `None`.
- **`TargetCurve` consumers:** the new fields are kwargs with defaults, so existing test code constructing `TargetCurve(name=..., frequencies=..., gains_db=...)` continues to work without modification.
- **Existing tests for `interpolate()` and `compute_correction()`:** logic is unchanged.

### Migration

The four files shipped today have these stems:
- `harman_ie_2019` → replaced by `harman_ie_2019_v2` (most users will have selected this expecting the modern reference)
- `harman_oe_2018` → replaced by file of the same stem with new high-resolution data
- `diffuse_field` → replaced by file of the same stem with new high-resolution data
- `flat` → replaced by file of the same stem (effectively unchanged, plus metadata)

**Profile compatibility risk:** any saved profile referencing `harman_ie_2019` by exact filename stem will break (file no longer exists). Mitigation:
- Profiles store target curve data as a snapshot, not by filename reference (verified in `paraeq/profiles/profile.py`). Re-check this assumption during implementation; if profiles store filename references, add a one-time alias map `{ "harman_ie_2019": "harman_ie_2019_v2" }` in the loader.

## Test Strategy

### New tests

| Test | Purpose |
|---|---|
| `test_target_csv_parses_metadata_header` | A CSV with `# name:`, `# category:`, `# description:`, `# source:` populates all four fields. |
| `test_target_csv_no_metadata_still_loads` | A CSV with only the existing comment style (no `key: value` headers) loads correctly with metadata fields as `None`, `name` falling back to file stem. |
| `test_target_csv_unknown_header_field_ignored` | A CSV with `# foo: bar` does not raise; `foo` is silently dropped. |
| `test_target_csv_metadata_case_insensitive_keys` | `# Name: ...`, `# NAME: ...`, `# name: ...` all populate the `name` field. |
| `test_list_builtin_targets_returns_six` | The 6 expected curves are loaded with their categories populated. |
| `test_builtin_curves_have_descriptions` | Every shipped builtin has a non-empty `description`. (Documentation contract test.) |

### Modified tests

Existing tests in `tests/test_target_curves.py` that assert specific filename presence (`harman_ie_2019` stem) update to assert `harman_ie_2019_v2`. Tests asserting interpolated values at specific frequencies are recalibrated against the new high-resolution reference data — values will shift slightly (sub-dB) due to higher resolution, not algorithmic change.

### Smoke verification

After implementation, manually launch the GUI (`python -m app.main`), select each of the 6 presets in the Target tab, and verify the plotted curve shape matches expectation against AutoEQ's plotted reference graphs.

## Implementation Sketch

Order of work:

1. Add metadata fields to `TargetCurve` dataclass.
2. Extend `load_target_csv()` to parse `# key: value` headers (TDD: write failing tests first).
3. Pin the AutoEQ commit hash. Document it in `docs/CONTEXT.md`.
4. Pull the 4 source curves from AutoEQ at the pinned commit; place into `targets/` with the new metadata headers.
5. Construct `flat.csv` (trivial) and `harman_ie_2019_v2_bass_shy.csv` (derive from v2 + shelf attenuation, document the derivation in the header).
6. Delete the old approximate CSVs.
7. Update existing tests in `tests/test_target_curves.py` for the new filenames and updated reference values.
8. Add the 6 new tests above.
9. Run full suite (`pytest tests/ -v`); confirm all 67+ tests pass.
10. Smoke-test the GUI.
11. Update `docs/CONTEXT.md` "What's Built" section to reflect the refined data and the metadata-aware loader.

## Future Work

- **Cycle 1.5a** uses the metadata fields (`description`, `category`) to populate tooltips and group presets in the restructured target curve editor.
- **Cycle 1.5b** passes `description` and `category` into the AI advisor's system prompt as preset context.
- **Cycle 2** (AutoEQ DB integration) will fetch headphone-specific presets from AutoEQ at runtime; that data layer is independent of these built-in target curves but reuses the same `TargetCurve` dataclass.

## Open Questions

None blocking. The design is complete as specified.

## Out of Scope

- AI-assisted curve recommendation (cycle 1.5a/b)
- Draggable in-plot control points (cycle 3)
- AutoEQ database picker (cycle 2)
- macOS Aggregate Device automation (cycle 4)
- Live fetching of curves over the network
- Generation of new curves via parametric formulas
- Refining the Phase 1 approximate curves *in place* (we replace, not refine)
