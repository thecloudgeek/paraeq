# CLAUDE.md

Instructions for Claude when working in this repo.

## Project

ParaEQ — open-source headphone measurement and correction EQ for macOS. Two-part project: a pip-installable Python DSP library (`paraeq/`) and a PyQt6 desktop app (`app/`) that wraps it.

For full context, read **`docs/CONTEXT.md`** before making changes. For the design rationale, see **`docs/specs/2026-04-22-paraeq-design.md`**. For the original implementation plan, see **`docs/plans/2026-04-22-paraeq-phase1.md`**.

## Commands

```bash
# Activate venv (always do this first)
source .venv/bin/activate

# Run full test suite
pytest tests/ -v

# Run a single test file
pytest tests/test_sweep.py -v

# Launch the GUI
python -m app.main

# Install / reinstall in editable mode with GUI deps
pip install -e ".[dev,gui]"
```

## Project Conventions

- **TDD**: Tests are written before implementation. Every new module in `paraeq/` has a corresponding `tests/test_*.py`. The GUI layer in `app/` is not unit-tested (requires a display).
- **Structured logging**: Modules use `logger.debug(..., extra={...})` for observability. Don't add print statements.
- **Alphabetical ordering**: Imports, dict keys, enum values, and lists should be alphabetized where order doesn't matter functionally.
- **Context7 for libraries**: Before using/changing 3rd-party SDK calls (sounddevice, PyQt6, pyqtgraph, scipy, hatchling), verify current usage with `npx ctx7@latest library <name> "<question>"` then `npx ctx7@latest docs <id> "<question>"`.
- **No requirements.txt**: Dependencies live in `pyproject.toml` only. To install: `pip install -e ".[dev,gui]"`.

## Architecture Boundaries

The DSP library (`paraeq/`) must remain GUI-free — it should be usable from scripts without PyQt6. Don't import anything from `app/` into `paraeq/`. The GUI imports the library, never the reverse.

```
app/  ────►  paraeq/  ────►  numpy, scipy, sounddevice, platformdirs
              (no GUI)
```

## Worktrees

Worktree directory: `.worktrees/` (already in `.gitignore`). Use this for feature branches when implementing multi-task plans:

```bash
git worktree add .worktrees/<feature-name> -b feature/<feature-name>
```

## File Organization

- `paraeq/measurement/` — sweep, deconvolution, compensation, FR computation
- `paraeq/correction/` — target curves, FIR/biquad/parametric filters, auto-fit
- `paraeq/engine/` — real-time convolver and IIR processor
- `paraeq/audio/` — device discovery, streams (sounddevice wrapper)
- `paraeq/profiles/` — per-headphone profile save/load
- `app/<area>/` — one subfolder per major GUI surface (wizard, editor, eq, analyzer, profiles, menu_bar, setup)
- `targets/` — built-in target curve CSVs (Harman IE/OE, Diffuse Field, Flat)
- `tests/fixtures/` — test data files

## When Making Changes

- Read existing code before modifying — patterns are already established
- Run the full test suite before committing (`pytest tests/`)
- Don't break the DSP library's standalone usability (no GUI imports)
- For new functionality, follow the TDD cycle: failing test → implementation → passing test → commit
