# CLAUDE.md

Instructions for Claude when working in this repo.

## Project

ParaEQ — open-source headphone measurement and correction EQ for macOS.
**The product is the Rust port**: a Cargo workspace (`crates/`) + Tauri 2/React
app (`desktop/`), built on Core Audio process taps. The old Python/PyQt6
prototype lives in `prototype/` as the **numerical oracle** for the ten DSP
modules ported from it — it generates the golden fixtures in `fixtures/` that
those modules must match, and that parity is frozen. Don't add features to the
prototype (this retires the PyQt6 *product* under `prototype/app/`; adding a
`gen_*()` case to `prototype/tools/generate_fixtures.py` is the prescribed
fixture workflow, not a feature). **DSP written for the measurement suite has
no prototype oracle; see the four-tier strategy in
`docs/specs/2026-07-15-measurement-suite-design.md`.**

Read **`docs/CONTEXT.md`** before making changes. Current design spec:
**`docs/specs/2026-07-15-measurement-suite-design.md`** (and its companion
specs); **`docs/specs/2026-07-02-rust-port-design.md`** remains authoritative
for the engine, the tap architecture and packaging. Current plan:
**`docs/plans/2026-07-02-rust-port-foundation.md`**.

## Commands

```bash
# Rust: build + test everything
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Desktop app (dev) — the CLI finds tauri.conf.json in desktop/src-tauri, so it
# must run from desktop/ (a subfolder search from desktop/ui would miss its sibling).
# The `tauri` npm script anchors cwd at desktop/ for you.
cd desktop/ui && npm run tauri dev

# Frontend only
cd desktop/ui && npx tsc -b && npm run build

# Tap spike (standalone, not a workspace member)
cargo run --release --manifest-path spikes/tap-spike/Cargo.toml -- run

# Python oracle (only for fixtures / verifying prototype behavior)
source .venv/bin/activate
# First-time setup: python3 -m venv .venv && .venv/bin/pip install -e "./prototype[dev,gui]"
pytest prototype/tests -v
python prototype/tools/generate_fixtures.py   # regenerates fixtures/ — commit the diff deliberately
```

## Project Conventions

- **TDD**: Rust DSP modules are written against failing tests first
  (`crates/paraeq-dsp/tests/`). The oracle depends on the tier — golden
  fixtures for prototype-ported modules, scipy-direct fixtures for new
  primitives with a library delegate, analytic-physics invariants where there is
  none, REW characterization at 0.1–0.5 dB for the end-to-end pipeline. Pick the
  tier before writing the module and state it in the module header comment, as
  `spline.rs:1-5` does. Engine code gets synthetic-block unit
  tests. GUI (`desktop/ui`) is typecheck + manual smoke.
- **Fixtures are sacred**: `fixtures/` is generated ONLY by
  `prototype/tools/generate_fixtures.py` (deterministic, seeded). Never edit
  fixtures by hand; regenerate and commit script + output together.
- **Crate boundaries** (spec constraints):
  - `paraeq-dsp`: pure math, zero platform deps — no CoreAudio, no Tauri.
  - `paraeq-coreaudio`: the ONLY crate with unsafe CoreAudio FFI.
  - `paraeq-engine`: no Tauri deps (daemon-ready). No locks/allocation on the
    realtime path. Every exit path runs the full teardown sequence ending in
    tap destruction — the system must never be left muted.
  - Forbidden deps: ndarray, scirs2-anything, fundsp.
- **Alphabetical ordering**: imports, dict keys, dep lists where order doesn't
  matter functionally.
- **Context7 for libraries**: verify 3rd-party API usage with
  `npx ctx7@latest library <name> "<question>"` before using/changing it.

## Worktrees

Worktree directory: `.worktrees/` (gitignored). Use for feature branches:
`git worktree add .worktrees/<name> -b feature/<name>`
