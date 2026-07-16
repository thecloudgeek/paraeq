# Rescope Implementation Plan — Cross-Spec Build Order

**Date:** 2026-07-16
**Specs covered:** the six `docs/specs/2026-07-15-*.md` rescope specs
(decision-engine, engine-hardening, measurement-safety, measurement-suite,
room-dsp, wizard).
**Status:** Stage 1 in progress (this branch).

This plan sequences the six specs into seven stages. It exists because the
specs cross-reference each other heavily (shared deliverables, ordering
constraints, one implementation serving three specs) and no single spec owns
the global order. Item ids below are `<spec>/<n>` work-item decompositions;
the authoritative requirements are always the spec text itself.

---

## Branch strategy

- **`feature/rust-port-tauri-shell` merges first, as-is** ("do not reopen" per
  the measurement-suite spec). It is CI-green and gated only on the owner's
  ears-on 12-point acceptance run — scheduling that run is the single
  cheapest unblock in the program. Do not rebase the 27-commit branch onto
  new work.
- Until that merge lands, work proceeds on `main` via short-lived worktree
  branches (`.worktrees/<name>`), restricted to `crates/` + `prototype/` +
  docs. **Any `CorrectionConfig` or `EngineState` shape change is frozen**
  until the merge (the branch pins the snapshot wire format and reworks
  `eq.rs`/`engine_bridge.rs`/`controller.rs`; engine-hardening R1-6 is
  explicitly sequenced post-merge for this reason).
- All wizard/renderer/desktop work is unconditionally post-merge.

## Fixture plan (the four-tier oracle convention)

Codified in Stage 1 before anything regenerates:

1. **Provenance pin:** `numpy==2.5.0` / `scipy==1.18.0` in a
   `prototype/pyproject.toml` `fixtures` extra; `generate_fixtures.py`
   asserts versions and fails loudly (`--allow-version-drift` stamps
   `"drift": true` into the manifest). The manifest is a record, not a pin.
2. **Tier 1 (frozen):** the ten existing module fixtures are never
   regenerated for rescope work. `test_fr.rs::averaging_is_in_db_domain` and
   `test_sweep.rs`'s 1e-12 sweep fixture stay green untouched. All room
   behavior arrives as additive functions (`Fixed(n)` smoothing bit-exact
   through the old boxcar, `apply_fade` additive, `deconvolve` kept as a
   `#[deprecated]` shim over `deconvolve_ir`).
3. **Tier 2 (scipy-direct):** new `gen_*()` cases in `generate_fixtures.py`
   that do **not** import `prototype/paraeq` (scipy windows, Alvarez-Mazorra
   vs `gaussian_filter1d` at 1e-3 dB, RMS averaging/σ, Schroeder
   reverse-cumsum, log-f resample vs `np.interp` at 1e-12, cal-file parse
   cases). Adding `gen_*()` cases is the sanctioned workflow; writing room
   DSP in Python first is rejected as fake provenance.
4. **Tier 3 (analytic physics, no fixtures):** in-crate Rust oracles
   (`apply_fdw_bruteforce`) and physics invariants for gating/FDW/splice/
   authority/PerChannel — the `spline.rs` precedent. The four named tests
   land in Stage 2 *before* their modules.
5. **Tier 4 (characterization):** one real measured IR (WAV) + REW's own
   gated/smoothed FR, asserted end-to-end at 0.1–0.5 dB, regression-only.
6. **`fixtures/decide/`** (new kind, outside `generate_fixtures.py`):
   `<case>/{bundle,expected}.json` characterization bundles, owner-reviewed
   once then frozen. No prototype decision engine is ever written.
7. **Exactly one sanctioned lockstep prototype edit:** the `compensation.py`
   parser fix lands in the same commit as the `compensation.rs` rewrite plus
   regenerated cal fixtures (the existing EARS fixture is numerically
   invariant under the fix).

## The seven stages

### Stage 1 — Merge, pins, and the independent safety floor *(this branch)*

Everything here is merge-independent except the merge itself and the two
`EngineState` shape changes (deferred to Stage 2):

- ~~measurement-suite/4~~ — **owner-gated:** ears-on acceptance → merge shell branch
- measurement-suite/1 — oracle pins + fail-loud generator + `fir.rs` comment
- measurement-suite/2 — the two verbatim CLAUDE.md edits (four-tier oracle)
- measurement-suite/3 — retire stale 2026-07-02 spec text in place
- engine-hardening/1 — `ParametricEQ::preamp_db()` (realized cascade, ≤0)
- engine-hardening/4 — R1-2 NaN sanitization (capture guard + output backstop
  + `Correction::reset()` self-heal)
- engine-hardening/5 — R1-3 `biquad::is_stable` (Jury) + identity
  substitution in `build_iir`
- engine-hardening/7 — R1-5 `kAudioSubDeviceInputChannelsKey: 0` on the tap
  aggregate (+ delete backend.rs KNOWN LIMITATION; owner fresh-TCC check)
- engine-hardening/9 — R1-7a `IIRProcessor::adopt_state_from`
- **Deferred to post-merge** (EngineState wire shape frozen):
  engine-hardening/2 (R1-8 meters) and wizard/1
  (`EngineState.self_excluded` + `MeasurementLease`).

### Stage 2 — Test-first infrastructure, hardening completion, crate scaffolds

Tier-2 fixture cases; the four Tier-3 tests ahead of their modules; Tier-4
REW corpus; post-merge hardening (R1-1 engine half + R1-8 falsifier, R1-6
after Open Q1 is decided); scaffold `crates/paraeq-decide` and
`crates/paraeq-measure` (type contracts + stubs, `#![forbid(unsafe_code)]`,
`SweepLevel` newtype before any playback path); CoreAudio output-volume
get/set; the compensation.rs/py lockstep rewrite.

### Stage 3 — Room DSP substrate + stimulus/diagnostics

`PerChannel<T>` first; `logf.rs` first among DSP (gates FDW, smoothing,
splice, targets); `window.rs` + `gating.rs` against pre-written Tier-3 tests;
targets rework (`TransducerClass` arg, `# classes:` header, `bk_1974.csv`,
same-commit 6→7 fixture churn); MS-4 stimulus assembly; MS-9/20
`MeasurementDiagnostic` + refusal framework.

### Stage 4 — Room analysis core, mic capture, session runtime, two-clock experiment

`deconvolve_ir` (typed IR, deprecated shim); `fdw.rs` (bruteforce oracle
first, then O(N log N); the asymmetry test decides `splice.rs`'s fate);
`fr.rs` additive rework (align_spl, RMS averaging, σ(f), Alvarez-Mazorra);
`room.rs` (Schroeder/T60, display-only); MS-6 self-excluded refusal; MS-9/10/11
cal safety validation; MS-22 measurement aggregate (output+mic,
drift-compensated); MS-14/18/23 `MeasurementSession` (abort ramp, RAII
restore, ack gate); **the two-clock skew experiment the moment the aggregate
exists** — every spec flags it as the biggest unpriced risk.

### Stage 5 — Authority, room-safe autofit, level ladder, persistence

`authority.rs` (σ_full=1.0/σ_none=6.0 canonical; Trinnov excursion;
Q_max=0.227·f0/A) + `auto_fit_room` (legacy `auto_fit_parametric_eq` stays
byte-identical); realized-cascade preamp on the decide()/export path; the
closed-loop level ladder in strict order; profile storage (raw per-position
IRs as f32 WAV — what makes Reanalyze 200 ms instead of Recapture); capture
runtime. End state: a complete coupler measurement is testable end-to-end on
the EARS rig, headless, before any wizard UI exists.

### Stage 6 — Decision engine complete + closed-loop verification

`decide()` 21-row decision table + ~20-check refusal table; golden
`fixtures/decide/` bundles (8 named cases) frozen under owner review;
override/invalidation semantics + idempotence property; `paraeq-stimulus`
helper binary (own PID, tap-included) + verification loop (residual gate per
Open Q5); deferred convolver bench stays `#[ignore]`d.

### Stage 7 — Wizard, two front-ends, telemetry surfacing, ship

Tauri wiring over `paraeq-decide`/`paraeq-measure`; the 10-state wizard spine
parameterized by `PathProfile` (after Open Q5 reconciliation); cal
acquisition UX; sweep-in-progress screen; auto mode + Advanced drawer
generated from `Decision<T>.domain`; guided front-end over the same
`DecisionSet`; results screen + Redesign/Reanalyze/Recapture; hardening
telemetry surfacing; `splice.rs` only if Stage 4's FDW asymmetry test failed;
R7 ship (signed DMG, notarization, PLD 2024/2853 posture decision).

## Open questions needing owner decisions

1. **Rate-independence shape (blocks Stage 2's R1-6):** engine-hardening
   R1-6 says `build_correction` *refuses* on `design_rate != stream_rate`
   (fail open to flat pass-through); decision-engine Open Q1's recommended
   fix says `CorrectionConfig::Peq { bands, design_rate }` with SOS
   *re-derived* at the live rate on every rebuild. Materially different
   behavior on an AirPods 44.1↔48 handoff (correction drops vs. survives).
   Plausible reconciliation: land R1-6's refusal as the engine invariant,
   then layer band-carrying re-derivation on top — but no spec says that.
2. **Authority-limited autofit shape (blocks Stage 5):** three different
   shapes across specs (mutate `auto_fit_parametric_eq` / add
   `auto_fit_parametric_eq_with_authority` / per-channel `auto_fit_room`).
   Only the additive shape is compatible with frozen Tier-1 fixtures; names,
   signatures, per-channel-ness, and which spec's constants win
   (BOOST_WEIGHT, narrow-dip Q threshold, cut_limit) need one reconciled
   definition.
3. **`TransducerClass` ownership + naming (blocks Stage 3 targets rework):**
   the enum must physically live in `paraeq-dsp` (the only crate all
   consumers may depend on) with decision-engine owning its semantics;
   variant naming (Headphone/Iem vs OverEar/InEar) must be settled.
4. **Two-clock capture problem (gates the room path):** "Farina tolerates
   skew" covers only ungated coupler magnitude; gating needs a trustworthy
   t=0. Run the experiment (measurement-suite/9) the moment the Stage-4
   aggregate exists; day-one mitigation is `Warn(TwoClock)` on gated paths.
5. **Verification gate definition (blocks Stage 6):** residual RMS >
   2×flatness_target over the authority band (decision-engine,
   measurement-safety) vs. residual_vs_prediction with threshold unstated
   (wizard); helper packaging location also differs. One gate, one
   threshold, one location.

Also record before R7: the PLD 2024/2853 commercial-posture decision
(appears in three specs).
