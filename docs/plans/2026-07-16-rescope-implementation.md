# Rescope Implementation Plan — Cross-Spec Build Order

**Date:** 2026-07-16
**Specs covered:** the six `docs/specs/2026-07-15-*.md` rescope specs
(decision-engine, engine-hardening, measurement-safety, measurement-suite,
room-dsp, wizard).
**Status:** Stages 1 and 2 implemented (`feature/rescope-stage1` →
`feature/rescope-stage2`, stacked, unmerged). Stage 2's merge-gated items
(engine-hardening R1-1 engine half, R1-8, R1-6) and the Tier-4 REW corpus
remain open — see the gates below.

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
   *re-derived* at the live rate on every rebuild. **Resolved** — adopt fix
   (1) (re-derive), with R1-6's refuse-and-fail-open as the last-resort guard
   for configs that cannot be re-derived; see
   `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q1.
2. **Authority-limited autofit shape (blocks Stage 5):** three different
   shapes across specs (mutate `auto_fit_parametric_eq` / add
   `auto_fit_parametric_eq_with_authority` / per-channel `auto_fit_room`).
   Only the additive shape is compatible with frozen Tier-1 fixtures; names,
   signatures, per-channel-ness, and which spec's constants win
   (BOOST_WEIGHT, narrow-dip Q threshold, cut_limit) need one reconciled
   definition.
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
5. **Verification gate definition (blocks Stage 6):** residual RMS >
   2×flatness_target over the authority band (decision-engine,
   measurement-safety) vs. residual_vs_prediction with threshold unstated
   (wizard); helper packaging location also differs. One gate, one
   threshold, one location.

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
10. **`authority`'s domain is unexpressible as typed.** Spec: "Choice:
    Standard, Conservative(×0.5), Custom(curve)" but `Decision<AuthorityCurve>`
    — so `Domain::Choice` can hold only concrete curves and the preset NAMES
    (what the drawer labels its control with, and what an override would
    round-trip) have nowhere to live.
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
13. **Room-target shelf default: two specs, two numbers.** **OPEN [OWNER]** —
    surfaced by the Stage-3 review. room-dsp's Room target generator says
    `shelf_gain_db` default **+4.0** (implemented as
    `RoomTargetSpec::default()`); the decision-engine spec's decision table
    says `shelf_db ∈ 0.0..=6.0 (default +3.0)`. One number must win before
    Stage 6 wires `decide()`'s room-target default to `RoomTargetSpec` — an
    ears call, same bucket as the other room-target defaults (room-dsp Open
    Q5). When ruled, update the losing spec and add a cross-crate test
    asserting the `decide()` default equals `RoomTargetSpec::default()`
    field-for-field.
14. **`align_spl` default band: two specs, two numbers.** **OPEN [OWNER]** —
    surfaced by the Stage-4 review. `fr.rs`'s `DEFAULT_SPL_ALIGN_BAND` is
    **(200, 2000) Hz** per room-dsp; the decision-engine spec's `align_spl_band`
    decision default is **(500, 2000) Hz**. The constant has zero consumers
    today (`align_spl` takes the band as an argument), so nothing is wrong at
    runtime — but the two published defaults must be reconciled before Stage 6
    wires the `align_spl_band` decision. Same shape as item 13: pick one, fix
    the losing spec, and let the `decide()` default be the single source. Lower
    stakes than the shelf (both bands sit above the modal region and below
    directivity); a desk call, not an ears call.

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
