# Rescope Implementation Plan — Cross-Spec Build Order

**Date:** 2026-07-16
**Specs covered:** the six `docs/specs/2026-07-15-*.md` rescope specs
(decision-engine, engine-hardening, measurement-safety, measurement-suite,
room-dsp, wizard).
**Status (updated 2026-09-16):** Stages 1–5 are implemented
(`feature/rescope-stage1` → `-stage2` → `-stage3` → `-stage4` → `-stage5`) and
the shell branch is merged on top of them. **The merge landed on
`feature/integration`, not on `main`** (merge commit `8334583`, parents
`c180658` + `2752c44`, forked from `main` at `179cd34`); `main` is untouched by
rescope work, and promoting `feature/integration` to it still waits on the
owner's ears-on 12-point acceptance run. Stage 2's merge-gated remainder
(engine-hardening R1-1's engine half, R1-8, R1-6) and wizard/1 are unblocked
and **in flight on `feature/integration`** as short-lived worktree branches;
item 21 (`TapStatus` over the live `TapSystem`) is unblocked and closed as a
question. Stage 5 closed cross-spec question 2 (the autofit shape). Still open
from Stage 2: the Tier-4 REW characterization corpus. Next: Stage 6.

This plan sequences the six specs into seven stages. It exists because the
specs cross-reference each other heavily (shared deliverables, ordering
constraints, one implementation serving three specs) and no single spec owns
the global order. Item ids below are `<spec>/<n>` work-item decompositions;
the authoritative requirements are always the spec text itself.

---

## Branch strategy

**Status (2026-09-16): the gating merge has landed — on
`feature/integration`, not on `main`.** `feature/rust-port-tauri-shell` was
merged into the rescope stack exactly as it stood (merge `8334583`, parents
`c180658` + `2752c44`, base `179cd34`): the measurement-suite spec's "do not
reopen" ruling held, and the 27-commit branch was never rebased onto new work.
The `CorrectionConfig`/`EngineState` shape freeze this section used to impose
is **struck** — not softened — and every item this plan deferred behind the
merge is unblocked.

- **Where the work happens now.** Short-lived worktree branches
  (`.worktrees/<name>`) forked from **`feature/integration`** and merged back
  into it. `main` stays at `179cd34` until promotion. The owner's ears-on
  12-point acceptance run now gates **promoting `feature/integration` to
  `main`** — it no longer gates doing the work. It is still the single cheapest
  unblock in the program, because nothing ships to `main` without it.
- **Shape changes are not frozen; they are serialized.** `CorrectionConfig` /
  `EngineState` changes land in this order, one branch at a time:
  (1) R1-6 — `CorrectionConfig::Peq { bands, design_rate }` + re-derive at the
  live rate + `EngineState.correction_rate_mismatch`;
  (2) R1-8's meters (`clipped_samples`, `input_peak_session`,
  `invalid_samples`, `output_peak`) together with R1-1's engine half
  (`auto_preamp_db`, `Correction.preamp_lin`);
  (3) wizard/1's `EngineState.self_excluded` + `MeasurementLease`.
  The reason is mechanical, not stylistic: every one of them edits
  `crates/paraeq-engine/src/controller.rs`, so concurrent worktrees collide in
  `EngineState`, `publish()`, `effectively_equal()` and `start_with()`.
  Merged `EngineState` field order, alphabetical per CLAUDE.md, on top of the
  shell's frozen shape: `auto_preamp_db, bypass, clipped_samples, correction,
  correction_rate_mismatch, enabled, frame_mismatch_blocks, gain_db,
  input_peak, input_peak_session, invalid_samples, latency_ms, output_peak,
  self_excluded, status, stream`.
- **All three hand-mirrored wire tripwires move in the same commit** as any
  `EngineState` change. There is no codegen, so a partial change is red CI in a
  file that reads as unrelated to the change:
  `crates/paraeq-engine/tests/test_wire_format.rs::engine_state_wire_format_is_pinned`,
  `desktop/src-tauri/src/state.rs::app_state_wire_format_is_pinned`, and the
  hand-written `desktop/ui/src/ipc/types.ts`.
- **Wizard / renderer / desktop work is unblocked**, along with every other
  item this plan parked behind the merge.
- **Two merged-shape properties must survive all of the above:** the shell's
  `#[serde(rename_all = "snake_case", tag = "kind")]` on `EngineStatus` (the TS
  union is keyed on `kind`) and `#[serde(rename_all = "snake_case")]` on
  `FilterType` — or every persisted `settings.json` profile breaks the moment
  `CorrectionConfig::Peq` puts `EQBand` on the wire.

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

- ~~measurement-suite/4~~ — **done 2026-09-16:** the shell branch is merged
  into `feature/integration`. The owner's ears-on acceptance run now gates
  promoting that branch to `main`, not the merge itself.
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
- ~~Deferred to post-merge~~ — **unblocked 2026-09-16 and in flight on
  `feature/integration`:** engine-hardening/2 (R1-8 meters) and wizard/1
  (`EngineState.self_excluded` + `MeasurementLease`). These are ordinary
  sequenced work now, not deferrals; the order they land in is in
  § Branch strategy.

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

### Stage 5 — Authority, room-safe autofit, level ladder, persistence *(done)*

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
   *re-derived* at the live rate on every rebuild. **Resolved** — adopt fix
   (1) (re-derive), with R1-6's refuse-and-fail-open as the last-resort guard
   for configs that cannot be re-derived; see
   `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q1.
   **Closed 2026-09-16.** No owner input is outstanding: the implementation is
   post-merge work in flight on `feature/integration` (R1-6, the first of the
   three shape commits). `design_rate` sits on the `Peq` variant rather than on
   a shared `CorrectionConfig` field — a shared field would force the same
   refusal compare on `Peq`, which §Q1 says must *not* refuse. The spec text
   that still prints the baked-SOS struct is the losing text; see
   `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md`.
2. **Authority-limited autofit shape (blocked Stage 5): RESOLVED
   (2026-07-25).** Three shapes were proposed — mutate `auto_fit_parametric_eq`
   in place (decision-engine), add a mono
   `auto_fit_parametric_eq_with_authority` (engine-hardening), add a
   per-channel `auto_fit_room` (room-dsp). **room-dsp's `auto_fit_room` wins**,
   on three non-preference grounds: in-place mutation breaks the frozen Tier-1
   fixture, so it is out on this plan's own terms; of the two additive shapes
   only the per-channel one matches the `PerChannel<T>` seam Stage 3 landed
   *specifically* so the policy code would not be written mono and rewritten;
   and a mono variant would need a per-channel wrapper anyway, i.e. two
   signatures owning one policy. Reasoning is in `autofit.rs`'s module header.
   Two documented deviations from room-dsp's literal signature: it takes
   `min_valid_freq_hz` (the spec's own item 4 requires it and the signature
   omits it) and returns `RoomFitReport` rather than a bare `Vec<EQBand>` (a
   bare return makes `clamp_band`'s mandated clamp reporting unreachable from
   its only caller). The **constants** are reconciled per-field in
   `authority::AuthorityPolicy` and remain OPEN \[OWNER\] as values — see
   items 15 and 16.
3. **`TransducerClass` ownership + naming (blocks Stage 3 targets rework):**
   **Resolved** — enum in `paraeq-dsp` with variants `{Bookshelf,
   Floorstander, InEar, OverEar}`; `Headphone`/`Iem` become `display_name()`
   strings, not variants; decision-engine `PathProfile` is canonical. See
   docs/decisions/2026-07-22-owner-value-calls.md.
4. **Two-clock capture problem (gates the room path):** "Farina tolerates
   skew" covers only ungated coupler magnitude; gating needs a trustworthy
   t=0. Run the experiment (measurement-suite/9) the moment the Stage-4
   aggregate exists. **Likely resolution (see the REW comparison below):**
   adopt REW's bracketed-timing-marker skew estimate + resample, default on,
   with `Warn(TwoClock)` as the fallback when no estimate can be formed — REW
   already ships this and reports the drift is only ~12 ppm, so this is a
   port, not open research.
5. **Verification gate definition (blocks Stage 6): quantity, band, location
   and packaging RULED 2026-09-16; the threshold is ESCALATED.**

   *First, a correction to this question as it was written.* It attributed the
   `> 2·flatness_target_db` gate to "(decision-engine, **measurement-safety**)".
   That attribution is wrong: `grep -n "flatness"
   docs/specs/2026-07-15-measurement-safety-design.md` returns **nothing** —
   measurement-safety states no verification gate at all. So the vote was
   1–1–abstain, not the 2-of-3 majority this plan uses as a tie-break (item 16).

   - **Quantity: `residual_vs_prediction`.** Rig error cancels in the
     difference, so this is the only claim that is about ParaEQ rather than
     about the rig (wizard:396, :400 — "Report it, plot it, never gate on it").
     The shipped type already decided it: `crates/paraeq-decide/src/bundle.rs`
     carries `Verification { ir, installed, position_index }` with `installed`
     present **only** so the prediction can be re-derived, and no target at all.
     `residual_vs_target` is computed and attached as evidence, never gated.
   - **Band:** `correction_range` ∩ `{ f : authority.at(f).max_boost_db > 0 ||
     max_cut_db > 0 }`, RMS taken on `Analysis::freqs_hz` so it is
     octave-weighted. Neither spec defines "the authority band" and both use
     it; without this sentence the gate is not computable.
   - **Location:** `paraeq_decide::decide()`. The code
     (`outcome.rs`'s `VerificationResidual`), the input slot and all three
     inputs (`flatness_target_db`, `correction_range`, `authority`) are already
     there.
   - **Positions:** 1 in auto (the primary seat), all-N offered in guided —
     wizard:529's own lean, and `Verification::position_index` is singular.
     N-position is additive later as a `Vec<Verification>`.
   - **Helper packaging:** a new workspace member `crates/paraeq-stimulus` with
     one `[[bin]]`, installed to `Contents/MacOS/paraeq-stimulus`. Decisive
     ground: Stage 6 must be testable headless before any wizard UI exists, and
     a `desktop/`-owned binary cannot be spawned from a headless
     `paraeq-measure` test. measurement-suite:162 ("in `desktop/`") is the
     losing text.
   - **Threshold: ESCALATED [OWNER + NEEDS DATA], not decided.** Ship the
     mechanism as `VERIFICATION_RESIDUAL_MULTIPLE = 2.0` read off
     `decisions.flatness_target_db.value`, and rule the number separately — it
     collides with the spec's own passing example. See escalation **E1**.

Also record before R7: the PLD 2024/2853 commercial-posture decision
(appears in three specs).

## Open questions raised by implementing Stages 1–2

These were found while building, not while planning. Each is recorded at the
type or in a test on the branch; none is invented policy.

6. **The MS-3 DC gate is infeasible for the sweep (spec defect).** The
   measurement-safety spec's post-fade assertion `|mean(x)| < 1e-4` cannot be
   met by a sweep at the policy fade (10 ms in / 50 ms out). Measured
   independently twice (two numpy ports agreeing to every digit): 1.59e-3
   un-faded → 1.24e-3 faded on a 5 s / 20 Hz–20 kHz / 48 kHz sweep. Post-scale
   it improves but still misses — 1.76e-4 at −20 dBFS (1.8×) and 4.42e-4 at
   −12 dBFS (4.4×). The sweep's DC is the LF stationary-phase residue, not an
   edge artefact, so no fade-out can touch it; only a ~100 ms fade-in gets
   under the gate (−7.6e-5) and the same spec section rejects a long fade-in
   for costing real LF energy. The pilot tone meets the gate easily
   (−1.5e-5), so this is sweep-specific. **Decision needed:** restate the
   threshold for sweeps, or DC-block in MS-4's stimulus assembly — but do not
   inflate the fade-in. `apply_fade` implements the spec's envelope exactly;
   `test_sweep.rs::fade_is_not_a_dc_blocker` documents the gap.
   **Status (2026-09-16): implemented in the sanctioned direction, OWNER
   RATIFICATION PENDING — not closed.** `crates/paraeq-measure/src/stimulus.rs`
   takes the second of the spec's two sanctioned resolutions: an
   envelope-shaped DC block (`x -= c·env` with `c = mean(x_faded)/mean(env)`,
   applied between fade and scale). Because the faded signal is `s·env`, the
   result factors as `env·(s − c)`, so the endpoints stay exactly 0.0 and the
   fade-out stays monotone while the mean goes to zero — all three MS-3
   properties from one identity, and the fade-in is not inflated.
   `test_stimulus.rs` pins the spec's measured worst case passing 1e-4 at both
   −20 and −12 dBFS. What is still owed: the owner's ratification, and the spec
   edit at `docs/specs/2026-07-15-measurement-safety-design.md:237`, which
   still reads OPEN \[OWNER + NEEDS DATA\]. This is **not** the same open item
   as the MMM level-safety gap in the same spec (escalation E4 below).
7. **MS-2's per-class dBFS column: cap, or starting point?** The spec calls
   the −20 dBFS coupler / −12 dBFS room "sweep level" column "a starting
   point for the solve, not the emitted level — the solve overrides it", yet
   MS-2 requires `SweepLevel::new` to refuse "above the class cap" and that
   column is the only per-class dBFS number in the table. Implemented as the
   cap (reading "the solve overrides it" as downward-only), so a chain too
   insensitive to reach the SPL target at ≤ −20 dBFS is refused. If a
   separate, higher ceiling is intended, only the table constant changes.
8. **The cal-parser spec is silent on encodings and line endings.** The
   spec's algorithm step 1 ("attempt to parse column 1 as a float; on
   failure, push the line to `ignored_lines`") literally licenses a UTF-8 BOM
   silently eating a headerless file's first data row — the exact bug class
   the rewrite exists to eliminate. Fixed in lockstep on the branch (BOM
   stripped, `\r`/`\r\n`/`\n` all end a line, number grammar pinned to the
   C-locale float), but the spec needs a sentence saying so.
9. **`q_cap`'s domain is a range over a scalar, not over the enum.** The spec
   gives `q_cap`'s domain as "Range 1.0..=20.0 on the ceiling" while typing
   the decision over `QCapPolicy`, so the room's own `LogLinear` value has no
   representable membership answer and the "every value is inside its domain"
   invariant is not mechanically checkable for it. Either the domain becomes
   a `Choice` over the two path policies, or `QCapPolicy` splits into a
   decided ceiling scalar plus a profile-owned shape.
   **Resolved 2026-09-16: the `Choice` option** —
   `Domain::Choice(vec![Ceiling(5.0), LogLinear { hi: (10000.0, 3.0), lo: (200.0, 10.0) }])`.
   It is the smaller of the two options the spec itself sanctions and the only
   one with no `Decisions`/`Overrides` shape churn: the drawer's control
   becomes a two-item select over the two path policies, which is what the
   value actually is. decision-engine:334 is the losing text. **Must land
   before the `fixtures/decide/` freeze** — domains are serialized into
   `expected.json`, so ruling this afterwards is a fixture-invalidating
   change. Rationale in
   `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md`.
10. **`authority`'s domain is unexpressible as typed.** Spec: "Choice:
    Standard, Conservative(×0.5), Custom(curve)" but `Decision<AuthorityCurve>`
    — so `Domain::Choice` can hold only concrete curves and the preset NAMES
    (what the drawer labels its control with, and what an override would
    round-trip) have nowhere to live.
    **Resolved 2026-09-16:** re-type to `Decision<AuthorityPreset>` with
    `{ Conservative, Custom(AuthorityCurve), Standard }`, and put the
    **resolved** curve in `Analysis`. This matches decision-engine:333's domain
    text verbatim, keeps `Decisions` at 21 fields so the exhaustiveness test
    stays green, keeps `AuthorityCurve` sealed, and makes an override
    round-trip a **name** rather than a 957-point curve. It is what the
    owner-directed must-expose list below ("authority preset + max-boost
    ceiling") needs in order to be presentable. **Must land before the
    `fixtures/decide/` freeze**, same reason as item 9. Rationale in
    `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md`.
11. **Type reconciliations due when Stage 3 lands** (all flagged in-source):
    `SmoothingMode` {Fixed, None, Variable} in paraeq-decide vs room-dsp's
    `Smoothing` incl. `Gaussian { fraction }`; `CorrectionKind`
    {MinPhaseFir, Peq} sharing a name with the engine's {Fir, Iir};
    `CorrectionPlan.bands: Vec<Vec<EQBand>>` becoming `PerChannel`;
    `AuthorityCurve` becoming a re-export of `authority.rs`'s; and the
    coupler `f_start` modelled `None` (per-DUT, safety spec) vs `20.0`
    (PathProfile).
12. **trybuild snapshots vs a floating toolchain.** MS-2's compile-fail proof
    pins rustc's E0308/E0423 wording while CI and `rust-toolchain.toml` track
    `stable`, so a compiler release can turn it red on an unrelated PR.
    **Decided (owner, 2026-07-22): do not pin** `rust-toolchain.toml`;
    regenerate with `TRYBUILD=overwrite` on drift. See
    docs/decisions/2026-07-22-owner-value-calls.md.
13. **Room-target shelf default: two specs, two numbers.** **Shape closed
    2026-09-16; the value is still OPEN [OWNER]** —
    surfaced by the Stage-3 review. room-dsp's Room target generator says
    `shelf_gain_db` default **+4.0** (implemented as
    `RoomTargetSpec::default()`); the decision-engine spec's decision table
    says `shelf_db ∈ 0.0..=6.0 (default +3.0)`. One number must win before
    Stage 6 wires `decide()`'s room-target default to `RoomTargetSpec` — an
    ears call, same bucket as the other room-target defaults (room-dsp Open
    Q5). When ruled, update the losing spec and add a cross-crate test
    asserting the `decide()` default equals `RoomTargetSpec::default()`
    field-for-field.
    **Shape resolved 2026-09-16; the value stays an ears call.** `decide()`'s
    room `Parametric` default **reads `RoomTargetSpec::default()`** instead of
    carrying a literal of its own, so there is exactly one source — which ships
    **+4.0** today and sits inside decision-engine's own `0.0..=6.0` domain, so
    nothing is invalidated by shipping it. decision-engine:371 is the losing
    text. The number itself remains **OPEN \[OWNER\]**: a ruling costs one line
    at `crates/paraeq-dsp/src/targets.rs:515`, and the cross-crate test above
    keeps `decide()` in step with it automatically.
14. **`align_spl` default band: two specs, two numbers.** **CLOSED
    2026-09-16** —
    surfaced by the Stage-4 review. `fr.rs`'s `DEFAULT_SPL_ALIGN_BAND` is
    **(200, 2000) Hz** per room-dsp; the decision-engine spec's `align_spl_band`
    decision default is **(500, 2000) Hz**. The constant has zero consumers
    today (`align_spl` takes the band as an argument), so nothing is wrong at
    runtime — but the two published defaults must be reconciled before Stage 6
    wires the `align_spl_band` decision. Same shape as item 13: pick one, fix
    the losing spec, and let the `decide()` default be the single source. Lower
    stakes than the shelf (both bands sit above the modal region and below
    directivity); a desk call, not an ears call.
    **Resolved 2026-09-16: (200, 2000) Hz** — the owning module's constant wins
    (`fr::DEFAULT_SPL_ALIGN_BAND`), the same layering rule decision-engine
    states for the σ endpoints. Extra support: the spec's own midband reference
    `M` — what `low_corner_hz` is measured against — is defined over
    200 Hz–2 kHz, so this makes the alignment band and the midband reference
    the same band. decision-engine:328 is the losing text. **Closed**; no owner
    input needed.

## Open questions raised by implementing Stage 5

15. **`boost_ratio` / `BOOST_WEIGHT`: resolved by LAYER, still open by VALUE.**
    room-dsp defaults it to 0.5, engine-hardening calls it `BOOST_WEIGHT ≈ 0.5`
    and marks it OPEN \[OWNER\], and decision-engine § Authority 5 says room
    **auto mode** ships cut-only (`boost_ceiling` scaled to 0 "unless the user
    raises it in the drawer"). Those stop conflicting once the layers separate:
    `paraeq-dsp` owns the *mechanism* and ships 0.5
    (`authority::DEFAULT_BOOST_RATIO`), and whether the room auto path passes
    `boost_ratio = 0.0` is `decide()`'s Stage-6 policy call — expressible as a
    value, not a second default. **Still OPEN \[OWNER\]:** the number itself
    needs ears on real measurements. One named constant; a ruling costs one
    line.
    **Layer split resolved 2026-09-16 into a concrete value:** `decide()`
    passes `boost_ratio = 0.0` on the room **auto** path and
    `authority::DEFAULT_BOOST_RATIO` (0.5) on the coupler path, both as one
    named `PathProfile` field — decision-engine § Authority 5 states the policy
    ("cuts are free, boosts cost headroom and can damage drivers") and
    `paraeq-dsp` keeps 0.5 as the mechanism default. The **number** is
    unchanged and still **OPEN \[OWNER\]**.
16. **Narrow-dip veto: 1/6 octave wins, on a 2-of-3 majority.** room-dsp types
    it as a width (`min_dip_width_oct`, default 1/6 oct) and decision-engine
    § Authority 4 words it identically; engine-hardening words the same veto as
    `Q > 3.0` and itself records that the two "must reconcile to one definition
    together". The width form wins because two specs state it and the API is
    typed that way. The difference is material, not rounding —
    `authority::width_oct_for_q` makes the conversion exact: Q = 3 is 0.479
    octave, 1/6 octave is Q = 8.65, so engine-hardening's is the **stricter**
    threshold. Shipped at 1/6, **OPEN \[OWNER\]** (an ears call), and bounded
    meanwhile: any dip surviving the veto is still gain-limited to
    `boost_ratio·e(f)·w(f)` — at most 5 dB below 150 Hz, 1 dB above 500 Hz.
    **Update the losing spec (engine-hardening) when this is ruled.**
17. **The MS-17 chain-sensitivity envelope has no numbers in any spec.**
    MS-17 requires a per-class envelope and refuses outside it, but states no
    values, so `TransducerCaps::sensitivity_envelope_spl_per_dbfs` ships
    engineering estimates (coupler 85–130, room 65–120 dB SPL per dBFS RMS).
    **OPEN \[NEEDS DATA\]** — settle by measuring `S` on the EARS rig and on a
    real room system, which Stage 5's headless run makes possible for the first
    time. One end is *not* a guess and is pinned by
    `test_ladder.rs::the_envelope_admits_every_chain_that_can_reach_target`:
    the envelope must contain `spl_target_db − sweep_level_dbfs_rms` (104
    coupler, 87 room), or this table would refuse chains the caps table calls
    legal. **Escalated 2026-09-16 — see E5 below:** the *shape* ships on a
    default (mirror the ranges onto `PathProfile` with a cross-crate equality
    test), but the *numbers* need the owner's rig.
18. **The input-gain SNR remedy cannot be expressed in dB.** MS-8's first
    remedy is "raise the input gain", but a CoreAudio gain scalar is a
    normalized 0..=1 register position, not dB, and the HAL exposes no way to
    ask a device what one step of it is worth acoustically. Implemented as
    `INPUT_GAIN_REMEDY_STEP = 0.1` of the register's travel followed by a
    re-measure — what a human does — bounded by the 2-attempt budget.
    **OPEN \[NEEDS DATA\].** Note also the MS-11 interaction the spec does not
    call out: moving the gain off the cal's reference makes `PinnedGain::matches`
    false and derates the emitted level by 6 dB, which helps only when the floor
    is *electrical*. `Remedy::RaiseInputGain`'s docs state it; the caller must
    either re-pin against a cal captured at the new gain or accept the derate.
19. **IR storage window: reconciled to the superset; both specs need updating.**
    decision-engine says `[peak−100 ms, peak+1100 ms]` with no taper; wizard
    says `[peak−min(64 ms, peak_index), peak+1500 ms]` with a Tukey α = 0.25
    taper. Shipped as **−100 / +1500 ms**: the larger pre-window satisfies both
    stated reasons for free, and the wizard's post figure binds a domain
    (`n_c ≤ 30`) the decision-engine's does not. **The taper is a spec defect,
    not a preference:** a Tukey α is a fraction of the *whole* window, so
    α = 0.25 over a 1.6 s store is a 200 ms ramp at each end — it would consume
    the entire pre-peak region and another 100 ms past the peak, destroying the
    direct arrival the window exists to centre on. Any α large enough to matter
    is large enough to eat the peak, so the taper is specified as a **duration**
    (`STORE_TAPER_MS = 10.0`), additionally clamped to half the realized
    pre-roll. Wizard's `+1500 ms` stays **OPEN \[NEEDS DATA\]** as it already
    was (re-derive against real room IR decay; if a bass-heavy untreated room
    needs more, the drawer's `n_c` ceiling moves with it).
20. **`serde_json` needed `float_roundtrip`, and this was a real defect.** The
    default parser is speed-optimized and best-effort to ~15 significant
    digits, so `f64 → JSON → f64` can land one ULP away — measured on this
    workspace, a 957-point `AuthorityCurve` came back different in *every*
    component vector. Two things in the design require exact reload: the
    decision engine's idempotence property ("identical in every field except
    that decision's `source`"), compared across the `fixtures/decide/` bundles,
    and profile persistence, where a reloaded profile must re-`decide()` to the
    same plan or Reanalyze is not deterministic. Enabled workspace-wide; costs
    ~2× on float parsing, nowhere near a hot path. Nothing further needed —
    recorded because it would have surfaced in Stage 6 as flaky fixtures.
21. **`TapStatus` over the live `TapSystem` — CLOSED as a question
    (2026-09-16).** MS-6 requires `TapSystem` to expose `self_excluded: bool`,
    and `MeasurementSession::begin` refuses without that witness — so no real
    headless run is possible regardless of what else exists
    (`crates/paraeq-measure/src/session.rs` already calls
    `tap.self_excluded()`; only the engine side is missing). It is an
    `EngineState` shape change, which the branch strategy above froze until
    `feature/rust-port-tauri-shell` merged (the same deferral as wizard/1).
    That merge landed on 2026-09-16, so the freeze is struck and this is
    ordinary sequenced work: it ships with wizard/1's `self_excluded`, the
    third of the three shape commits. **Nothing here needs an owner** — the
    ears-on acceptance run gates promoting `feature/integration` to `main`, not
    this item.

## Open questions raised by planning the post-merge work and Stage 6

Surfaced 2026-09-16 while sequencing the post-merge items and Stage 6 against
the spec text. Each one below ships on a stated default; the rulings and their
grounds are in `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md`.

22. **An override that lands outside its `Domain` has no defined behaviour.**
    **OPEN \[DESIGN\]** — genuinely new policy; no precedent exists anywhere in
    the specs. `decide()` has no `Result` (its own Decisions Log: "no `Result`;
    a refusal must still carry the decisions and evidence that produced it"),
    so there are exactly two candidates: clamp-and-warn, or ignore-and-keep-Auto.
    **Default shipped: clamp into the domain, set `source: UserOverride` with
    the clamped value, and emit `DiagnosticCode::OverrideOutOfDomain` at
    `Severity::Warn`, naming the decision and both numbers.** Silently ignoring
    the user's intent would recreate exactly the "second, lying source of truth
    about what the app did" that the `Decision<T>` design exists to prevent.
23. **`positions_n` is `Recapture` "increase only" — what tier is a decrease?**
    **OPEN \[DESIGN\]**. **Default: keep `Recapture` in both directions.**
    `Invalidation` is a single value on a `Decision`, so it cannot be
    direction-dependent, and the conservative reading never under-invalidates.
    A decrease is arguably a subset selection the spec does not model. Record
    the UX cost in the doc comment; the clean fix later is a separate
    "positions used" selection that is not a `Decision` at all.
24. **`LowSnrSoft` says "Warn + de-weight" but nothing in `fr.rs` accepts
    weights.** **Default: additive** —
    `fr::average_measurements_rms_weighted(&AlignedSet, &[f64])` and
    `fr::sigma_db_weighted`, leaving the unweighted, fixture-frozen functions
    untouched (this plan's standing rule that new room behaviour arrives as
    additive functions). Interim if it slips: emit the Warn and do **not**
    de-weight — **and say so in the rationale string**. A documented no-op
    beats an undocumented one.
25. **`DiagnosticCode` has no numbering contract while `MeasurementDiagnostic`
    has one.** `paraeq-measure`'s `MeasurementDiagnostic` carries MS-20's
    stable numbered, append-only contract (measurement-safety:347; the
    implementation's own header states numbers are "never reused and never
    reordered" and that "additions are cheap — that is the point of the
    contract"). MS-21, the adjacent row at :348, is the separate capture
    peak/clip metering requirement and is already implemented in
    `crates/paraeq-measure/src/capture.rs`. `paraeq-decide`'s `DiagnosticCode`
    has no such contract, yet `fixtures/decide/expected.json` and every session
    log will key on it. **Default: adopt the same contract** — explicit
    discriminants, a `code()` method, the same append-only header comment —
    plus a doc-comment mapping between the two vocabularies. One commit now; a
    breaking change to recorded sessions later. The mapping comment also guards
    against a future "cleanup" that merges the two enums and drags
    `paraeq-measure` into `paraeq-decide`, which would break the crate boundary
    and daemon-readiness.
26. **MMM is the room easy-mode default, but it is in no stage and its
    level-safety row does not exist.** **ESCALATED — see E3 and E4 below.** The
    owner decided on 2026-07-22 that MMM is the room easy-mode default with
    discrete sweeps as the precision option; three specs still list MMM as Out
    of Scope and were never updated (wizard:557, room-dsp:1054,
    measurement-suite:598); none of the seven stages contains MMM work; and
    measurement-safety:145 explicitly forbids reusing the sweep caps for it.
    Two consequences bite immediately: `CaptureMethod` changes `bundle.json`,
    so **`fixtures/decide/` cannot be frozen until the scope call is made**,
    and the level-safety gap sits on the *default* room journey rather than on
    a corner case.

## Escalations for the owner (2026-09-16)

Seven items that no precedent in this plan can settle. Everything else the
post-merge / Stage-6 planning pass surfaced ships on a documented default (the
numbered items above, plus
`docs/decisions/2026-09-16-post-merge-and-stage6-calls.md`). Each item below is
**OPEN \[OWNER\]**, states the recommended option, and is written so it can be
ruled from this file.

**E1. The verification residual threshold — the `2×` multiplier.**
**OPEN \[OWNER + NEEDS DATA\].** Blocks the constant behind cross-spec question
5, and the credibility of what wizard:297 calls "the single most important
refusal in the product" — but it does **not** block building the gate. The
mechanism ships either way. `2 · flatness_target_db` evaluates to **2.0 dB** on
the coupler (`flatness_target_db = 1.0`) and **6.0 dB** in a room (3.0). Set
against that, wizard:337's own results-screen example of a *passing*
verification is "to within **1.8 dB RMS** across 20–240 Hz". So the coupler gate
refuses the spec's own success story by 0.2 dB, and the room gate is more than
3× looser than the spec's own expectation of a good result. The number was
almost certainly chosen with the *target* residual in mind, where rig error
dominates; it does not transfer to the *prediction* residual unexamined.
Options: (a) ship 2.0 as `VERIFICATION_RESIDUAL_MULTIPLE` and retune on the
first EARS run; (b) ship an absolute dB gate instead of a multiple, since the
prediction residual is a property of the DSP chain rather than of the path's
flatness ambition; (c) per-path multiples. **Recommended: (a)** — one named
constant read off `decisions.flatness_target_db.value` so an override moves the
gate with it, with the retune signal stated (*the first EARS run where a
correction that A/Bs correctly still refuses*). But the owner should see the
2.0-vs-1.8 collision **before** the first ears-on verification run, not after.

**E2. How a genuine TCC silent failure is detected *during* a measurement
session.** **OPEN \[OWNER\].** Blocks Stage 6's verification wiring and the
wizard's refusal table; does not block the post-merge lease work, which ships
either way. wizard:416 requires that "a genuine TCC silent failure during a
measurement session is still a `Refuse`". But the same section explains why the
tap legitimately sees zeros throughout — `Direct` captures are tap-excluded **by
design** — and `EngineStatus::NoInputDetected` means exactly "no nonzero sample
since start". Read literally, that sentence refuses 100% of wizard runs.
**Recommended split:** the measurement lease suspends only the auto-disable;
`NoInputDetected` keeps being reported and logged but is not blocking during a
`Direct` capture; and the TCC witness moves to the **verify** gate, where it is
falsifiable — the helper process is tap-*included* and is playing, so the tap
**must** see nonzero blocks within about a second (the hardware tests record 0
callbacks/s idle against ~94/s during playback). No nonzero blocks while the
helper plays ⇒ TCC silent failure ⇒ `Refuse`. Escalated because it
reinterprets a safety sentence in the spec, not because the engineering is
unclear.

**E3. MMM scope and sequencing.** **OPEN \[OWNER\].** Blocks the
`MeasurementBundle` shape, and therefore the `fixtures/decide/` freeze. MMM is
the owner-decided room easy-mode default (2026-07-22), yet three specs still
call it Out of Scope, no stage contains any MMM work, and pink noise is
explicitly absent from the stimulus module ("a later stage and deliberately
absent here"). There is no safe default: a `decide()` written without
`CaptureMethod` is throwaway if MMM ships, and one written with it is
speculative if MMM slips. Options: (a) MMM in v1 — add `CaptureMethod` now and
branch `decide()`'s authority model on it; (b) MMM deferred past v1 — say so
here and correct the three Out-of-Scope sections so they say the right thing
for the right reason; (c) ship `CaptureMethod { DiscreteSweep }` as a
one-variant enum now, so adding `MovingMic` later is additive and
`bundle.json`'s shape is stable. **Recommended: (c)** — the cheapest hedge; it
costs one enum and unblocks the freeze without committing the schedule.
Natural companion call: measurement-suite:571's schedule contingency ("correct
below the transition only if the date slips"), which is also OPEN \[OWNER\].

**E4. The MMM level-safety row.** **OPEN \[OWNER + NEEDS DATA\].** Blocks any
MMM capture actually running; contingent on E3. measurement-safety:145,
verbatim: the per-class caps table "has no row for the **Moving Microphone
Method (MMM)** … MMM plays continuous pink noise — continuous energy with a
different crest factor than a sweep — so hearing-exposure and driver-heating
limits differ; the sweep caps above must **not** be reused unexamined. MMM
needs its own level-safety row, derived from real pink-noise measurements." The
spec therefore forbids the only available default, and MMM is now the *default*
room journey — so this is a safety gap on the path most novices will take.
**Recommended: refuse the MMM path entirely until the row exists** (quiet is
the safe direction), and make the gap visible as an explicit
OPEN \[NEEDS DATA\] MMM row *inside* the per-class caps table rather than only
in the prose beneath it. This is the one item where proceeding on a default is
a safety regression rather than rework. Distinct from item 6's MS-3 DC-gate
item, which is a different open item in the same spec.

**E5. MS-17's chain-sensitivity envelope.** **OPEN \[OWNER + NEEDS DATA\]** —
this is item 17 above, escalated. Blocks the class cross-check and the
`WrongTransducer` refusal, and in practice every real headless run on unusual
hardware: a chain outside the envelope is refused **before any sweep is
emitted**. No spec states any values; `crates/paraeq-measure/src/level.rs`
ships engineering estimates (coupler 85–130, room 65–120 dB SPL per dBFS RMS).
One end is pinned and is not a guess —
`test_ladder.rs::the_envelope_admits_every_chain_that_can_reach_target` forces
the envelope to contain `spl_target_db − sweep_level_dbfs_rms` (104 coupler, 87
room); the other end is a guess. **Recommended:** ship the *shape* on a default
— mirror the same ranges onto `PathProfile` (because `paraeq-decide` may not
depend on `paraeq-measure`) with a cross-crate test asserting the two tables
agree field-for-field — and have the owner settle the **numbers** by measuring
`S` on the EARS rig and on a real room system. Wrong values surface to users as
"ParaEQ won't measure my headphones", which is a user-visible failure
attributable to a guessed constant.

**E6. Hardware spike: can the tap aggregate and the measurement aggregate
coexist?** **OPEN \[OWNER — rig action, not a decision\].** Blocks the
verification loop's capture-side design. Nobody has ever run both at once:
`tap.rs` composes the default output as a tap-aggregate sub-device and
`measure_aggregate.rs` composes it again ("mirrors `tap.rs::create_aggregate`
key-for-key, minus the tap list"). Verification is the first operation that
needs both live, and the Stage-5 hardware tests deliberately run without a
session or engine. No spec addresses coexistence. **Recommended:** schedule the
spike **before** the verification capture path is designed around an
assumption. Fallback if they cannot coexist: capture the verification mic
through a mic-only aggregate and lean entirely on the two-clock marker path for
t=0, accepting the two-clock warning — which redesigns the capture side, so
finding out late is expensive.

**E7. The `fixtures/decide/` freeze sign-off.** **OPEN \[OWNER\].** Blocks the
freeze by definition: decision-engine:574 says the expected values "are
reviewed by the owner once and then frozen", which is an owner gate an agent
cannot default. What the owner is being asked to sign: eight `notes.md` files
(one paragraph each — what the case exercises, the expected verdict, the
diagnostics it should raise), written **before** the bless, plus the eight
`expected.json` the bless produces. Without the notes, "reviewed once" means
reading several hundred KB of `Analysis` curves. **Recommended: do not schedule
this until** items 9, 10, 13, 14 and 15, item 11's type reconciliations,
cross-spec question 5 and E3's `CaptureMethod` call are all settled. A freeze
that re-blesses twice in its first week trains everyone to rubber-stamp the
diff, which is exactly what the mechanism exists to stop.

**Adjacent owner *actions* (not decisions, listed so they are not lost).**
The **Tier-4 REW characterization corpus** — no `fixtures/rew/` exists; it was a
Stage-2 gate and has slipped past Stage 5, so the whole room pipeline ships
validated only against its own analytic invariants and never against the
field-standard tool. The **two-clock magnitude run** — the harness is written
and `#[ignore]`d in `crates/paraeq-coreaudio/tests/test_measure_hardware.rs`;
it unfreezes the provisional 5.5 s sweep-length cap, sets the clock-adjust
reject bound, and closes the IR-perturbation question, and measurement-safety
gates the gated room path's *ship* on it. **Counsel** on the EN 50332 /
IEC 62368-1 cl. 10.6 and PLD 2024/2853 article numbers, which the safety spec
records as "corpus-sourced and unverified" — the posture is already decided
(stay FOSS/non-commercial); only the published claim waits, so this blocks R7
rather than code, and the 85 dB warn / 100 dB refuse thresholds stand as
engineering thresholds regardless. Pair the REW capture, the two-clock run and
E6's spike into **one rig session** so all three land together.

## REW comparison and the automate-with-an-override principle

How REW (Room EQ Wizard, the field-standard measurement tool) handles the
five room-specific decision-engine open questions (Q3–Q7), researched against
its official help and John Mulcahy's forum posts. The organizing finding:
**REW gives the expert user the data and the controls and leaves the judgment
to them; ParaEQ automates the judgment as a safe default.** That is the
"just fix my sound" premise — but it is only safe if the automation is never
a cage.

**The principle (owner-directed, 2026-07-19).** Every automated decision must
also be a *manual control for power users*. This is already the decision-engine
spec's architecture, not new work: each `Decision<T>` carries a legal `Domain`,
a `Source` that flips `Auto → UserOverride`, and the Advanced drawer renders
each as a control (see "The One-Engine-Two-Front-Ends Seam" and "Override
Semantics"). So there is a third disclosure level above auto and guided: the
drawer, where a power user has REW-equivalent reach. The REW comparison's job
here is to pin **which specific levers must be exposed** so the drawer is a
genuine superset of REW, not a subset — an auto user never touches them, a REW
refugee finds all of them.

Per question (REW behavior → ParaEQ default + the power-user override the
drawer must expose):

- **Q3 — windowing (FDW).** REW uses a *single symmetric* window ("cycles" or
  octave-fraction; 15 cycles = 150 ms @ 100 Hz, 15 ms @ 1 kHz, 1.5 ms @ 10 kHz)
  with no pre/post-peak asymmetry — only Acourate exposes that, and REW users
  have asked Mulcahy for it. REW's "variable smoothing" (1/48 oct <100 Hz →
  1/3 oct >10 kHz) is what it "recommends for responses to be equalised", which
  matches ParaEQ's room smoothing curve. **Default:** auto variable window; the
  pre/post asymmetry (`n_c_pre`/`n_c_post`) stays an optional refinement, decided
  from real rooms — shipping it puts ParaEQ ahead of REW, in Acourate territory.
  **Drawer override:** window cycles / octave-fraction and smoothing mode, at
  minimum matching REW's single-value control.
  ([impulseresponse.html](https://www.roomeqwizard.com/help/help_en-GB/html/impulseresponse.html),
  [analysis.html](https://www.roomeqwizard.com/help/help_en-GB/html/analysis.html))
- **Q4 — where to boost (peaks vs dips).** REW's auto-EQ leaves the
  boost-vs-fill-a-null judgment to the user via a **global max-boost limit**
  plus standing advice to cut not boost; its minimum-phase / excess-group-delay
  displays are diagnostics the user reads, they do **not** feed the auto-EQ.
  ParaEQ's Q4 (per-region boost authority derived automatically from group
  delay + σ(f)) is doing by machine what REW expects expertise to do — the
  biggest philosophical gap, and the reason to err conservative on the auto
  path. **Drawer override:** the `authority` Decision (Standard / Conservative /
  Custom) plus a max-boost ceiling, i.e. REW's actual knob, on top of a safe
  default. (Resolving open question 10 above — the preset *names* need a home —
  is what makes this override presentable.)
  ([Match Response to Target / Filter Tasks, eq.html])
- **Q5 — seat disagreement.** REW gives the spatial average (RMS/power average
  for different positions; vector/phase average only for the same position or
  after alignment) but computes **no** per-frequency variance and does **not**
  throttle EQ by it — "correct less where the seats disagree" is entirely the
  user's call. ParaEQ's σ(f) authority throttle is a genuine value-add over
  REW. **Default:** auto-throttle by σ(f); **surface** the σ(f) band on the
  results screen (the confidence information REW never shows). **Drawer
  override:** the authority scaling and smoothing, so a power user can push
  correction further up-band against ParaEQ's advice — with the σ curve visible
  so they see the risk.
  ([graph_allspl.html](https://www.roomeqwizard.com/help/help_en-GB/html/graph_allspl.html))
- **Q6 — two clocks. REW already solved this; adopt its method.** REW handles
  the UMIK-1-vs-separate-output case with an **acoustic timing reference**, and
  its opt-in "Adjust clock with acoustic ref" plays a timing marker at the
  **start and end** of the sweep, counts elapsed samples vs expected, computes
  the clock-rate difference, and **resamples** the capture — exactly the
  "estimate-skew + resample" option. Mulcahy, verbatim: *"REW knows how many
  samples there should be between the timing signals and if there are more or
  fewer it knows the clocks were different and can resample the data
  accordingly"*; typical magnitude *"Only 12 ppm… when output and input are on
  different devices."* This **de-risks the spec's "largest unpriced item"**:
  it is a known technique with a known ~12 ppm magnitude, not open research.
  **Recommendation:** replace the plan's cross-spec open question 4 mitigation
  ("`Warn(TwoClock)`, resolution unscheduled") with **adopt REW's
  bracketed-timing-marker skew estimate + resample, default on**, keep
  `Warn(TwoClock)` only as the fallback when the estimate cannot be formed, and
  still run measurement-suite/9 to confirm the magnitude on our own rig before
  hardening. **Drawer override:** a clock-adjust on/off toggle with the
  estimated ppm shown — REW exposes it as an opt-in preference; ParaEQ should
  default it on and let a power user see and disable it.
  ([analysis.html](https://www.roomeqwizard.com/help/help_en-GB/html/analysis.html),
  [Mulcahy, AVNirvana](https://www.avnirvana.com/threads/propper-use-of-clock-adjustment.7839/))
- **Q7 — number of positions.** REW prescribes no count and effectively no cap
  (a configurable "Maximum measurements" pool defaults to 30, up to 250, but
  never binds spatial averaging), and offers the Moving Microphone Method as a
  continuous alternative to discrete points. Dirac prescribes fixed 9/13/17.
  ParaEQ's ceiling of 15 is a sensible middle — far below REW's pool limit, so
  it never constrains a REW-style workflow. **Default:** ~9 (where the known
  benefit plateaus). **Drawer override:** raise to the 15 ceiling or lower;
  consider offering MMM as a scope item, since REW has it and ParaEQ (discrete
  only) would not.
  ([graph_allspl.html](https://www.roomeqwizard.com/help/help_en-GB/html/graph_allspl.html),
  [spectrum.html](https://www.roomeqwizard.com/help/help_en-GB/html/spectrum.html))

**Net for the drawer's must-expose list:** window cycles + smoothing mode (Q3),
authority preset + max-boost ceiling (Q4), authority scaling with the σ(f) band
shown (Q5), clock-adjust toggle + ppm readout (Q6), position count up to the
ceiling (Q7). Every one of these is REW's actual control; together they make
the Advanced drawer a superset of REW rather than a walled garden — which is
the point of automating the judgment without taking the wheel away.

## Moving Microphone Method (MMM) — a second room capture modality

**Decision (owner-directed, 2026-07-21):** offer MMM alongside discrete-position
sweep capture for the room path. REW has it and it is popular for exactly the
reasons below.

**What it is.** Continuous pink noise plays while the user slowly moves the mic
through the listening volume, and a real-time analyzer power-averages over the
whole traversal (REW: RTA + "Forever" averaging). The output is **one
spatially-averaged magnitude curve** — no impulse response, no phase, no
per-position data. The moving mic + noise averages the position-dependent phase
away, so the power average falls out directly. It is fast, forgiving, samples
the whole area rather than a few points, and is **inherently immune to the
two-clock problem (Q6)** — with no gating and no t=0 to recover, clock drift is
irrelevant (this is the case "Farina tolerates skew" actually covered).

**Why it is not just another averaging mode.** MMM produces a *different kind of
bundle*. Because there is no impulse response it cannot feed most of the
sweep-path machinery:

- no time gating / FDW (Q3) — nothing to window;
- no per-position σ(f) (Q5) — the spatial average is baked in at capture;
- no excess-group-delay authority (Q4) — needs phase;
- no Schroeder decay / T60.

So an MMM correction falls back to the safe universal rule — **cut peaks, do not
fill dips, fixed conservative boost ceiling, heavy smoothing** (variable /
psychoacoustic) — which is exactly what a REW MMM user does by hand. A good,
robust, but deliberately *less sophisticated* correction than the full sweep
path. The cal file still applies (it is per-frequency magnitude compensation).

**Architectural implications (all new work, sequenced no earlier than Stage 4's
room path):**

1. **Capture method on the bundle.** `MeasurementBundle` assumes per-position
   `ImpulseResponse`s; MMM has none. Add `CaptureMethod { DiscreteSweep,
   MovingMic }` and branch `decide()`'s authority model on it — full σ(f) +
   group-delay for sweeps, conservative magnitude-only for MMM. The IR-derived
   bundle fields become absent/optional under `MovingMic`.
2. **New stimulus + capture runtime.** Pink-noise stimulus and continuous RTA
   power-averaging, distinct from sweep-and-deconvolve. A new `StimulusSink`
   stimulus kind and a capture loop in `paraeq-measure` / the wizard.
3. **New level-safety policy (OPEN).** The measurement-safety level ladder is
   entirely sweep-specific; pink noise is continuous energy with a different
   crest factor, so hearing-exposure (a longer continuous capture) and driver
   heating need their own MMM numbers. This wants its own row in the safety
   spec — do not reuse the sweep caps unexamined.
4. **Thinner persistence.** The decision-engine spec mandates raw per-position
   IR storage so Reanalyze is ~200 ms; MMM has no IRs, so it stores the averaged
   magnitude curve and Reanalyze can only re-smooth / re-target, not re-gate.

**Open questions MMM raises:**

- **Room easy-mode default: MMM or discrete sweep? DECIDED (owner,
  2026-07-22): MMM** is the "just fix my sound" room default (faster, more
  forgiving, two-clock-immune), with discrete sweeps as the precision option —
  accepting the weaker magnitude-only correction on the easy path in exchange
  for robustness. See docs/decisions/2026-07-22-owner-value-calls.md. (The MMM
  authority model and its level-safety row, below, remain open.)
- **MMM authority model.** Confirm the conservative magnitude-only policy
  (cut-focused, fixed boost ceiling, heavy smoothing) and how a bundle with no
  IRs is represented so the σ(f)/EGD fields are cleanly absent rather than
  faked.
- **A middle-ground modality?** Some tools do "moving mic with periodic
  sweeps," which recovers some IR/gating while still sampling the area. Noted;
  likely out of scope for the first release, but record the choice rather than
  omit it silently.

Consistent with the power-user principle: offer both modalities, expose the
choice in the mode chooser, and keep MMM's simpler authority overridable within
safe bounds in the drawer.
