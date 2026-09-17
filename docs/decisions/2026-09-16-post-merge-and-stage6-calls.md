# Post-Merge and Stage-6 Calls — Decisions

**Date:** 2026-09-16
**Status:** ruled. Everything in *Phase A* and *Phase B* below is a
precedent-following default that ships now; a later owner ruling on any of them
costs one line or one named constant. §"Escalated to the owner" is **not** a set
of decisions — those seven items are questions that no precedent in this repo
can answer safely, and they are recorded here so they do not dissolve.

**Source.** The merged post-merge + Stage-6 build plan written 2026-09-16 over
the `feature/integration` merge (rescope `crates/` + shell `desktop/`), itself
built from seven area surveys plus direct re-reads of the cited spec lines.
Companion to `docs/decisions/2026-07-21-decision-engine-open-questions.md`
(the eight decision-engine Open Questions) and
`docs/decisions/2026-07-22-owner-value-calls.md` (the owner's value calls).
This record picks up everything those two left open plus everything the merge
itself created.

## Naming: which "Open Q5" this record closes

"Open Q5" is overloaded three ways across this repo, and two of the three are
already settled. Every reference below is explicit about which one it means:

| Name used here | What it is | Status |
|---|---|---|
| **plan cross-spec Open Question 5** | The verification-gate reconciliation — which residual gates, at what threshold, and where the stimulus helper lives (`docs/plans/2026-07-16-rescope-implementation.md`, cross-spec question 5) | **Quantity, band, location and position-count decided here (D-G, D-H, D-I). Threshold escalated (E1).** |
| decision-engine §Q5 | The σ authority endpoints `σ_full = 1.0` / `σ_none = 6.0` dB | Already decided — `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q5 |
| room-dsp Open Q5 | Room-target defaults (tilt / shelf) | Closed here as D-A |

## How to read the tables

Each row is **Decision / reasoning / Confidence / Kind**, the shape the
2026-07-21 and 2026-07-22 records use. *Kind* is one of:

- **Pin now** — a shape or policy call; nothing downstream is waiting on data.
- **Ship starting value** — a number that only real measurements can finalize;
  it ships as one named constant with the retune signal stated (precedent P3).
- **Ship + validate** — shipped behaviour that the first real run may demote.
- **Already decided** — recorded here only so a future session does not reopen
  it as unbuilt.

## Precedents cited by id

| id | Precedent | Established at |
|---|---|---|
| **P1** | 2-of-3 majority across specs wins; a second, independent ground is "the shipped API is typed that way" | `docs/plans/2026-07-16-rescope-implementation.md` item 16 (narrow-dip veto) |
| **P2** | The owning module's constant is canonical on a numeric divergence | `docs/specs/2026-07-15-decision-engine-design.md` §Open Questions 5 — "reconciled to the owning module" |
| **P3** | Ship the value, flag `OPEN [OWNER]`, one named constant, state the retune signal | plan item 15; `docs/decisions/2026-07-21-decision-engine-open-questions.md` header |
| **P4** | Conservative / quiet is the safe direction | `docs/decisions/2026-07-22-owner-value-calls.md` §"MS-2 sweep level"; engine-hardening §Autofit limits (cut limit) |
| **P5** | Reconcile to the superset when one number satisfies both stated reasons | plan item 19 (IR store window) |
| **P6** | Never mutate a frozen Tier-1 fixture in place; new behaviour arrives additively | plan §Fixture plan; plan cross-spec question 2 |
| **P7** | Every automated decision is also a drawer control | plan §REW-parity drawer (owner-directed 2026-07-19) |
| **P8** | Update the losing spec when a divergence is ruled | plan items 13/14/16 |
| **P9** | Let the `decide()` default *be* the owning module's constant, plus a cross-crate equality test | plan item 13 |

---

## Phase A — post-merge

| id | Question | Decision | Precedent + ground | Confidence | Kind |
|---|---|---|---|---|---|
| **D-1** | R1-1 preamp carrier: `Correction.preamp_lin` or `EngineCommand::SetGainDb`? | **`preamp_lin` inside `Correction`, applied on the corrected path only.** Expanded below | **P8**. engine-hardening's own Decisions Log already rules this row, and its R1-1 §4 gives the atomicity argument and the corrected-path-only requirement | High | Pin now |
| **D-2** | Where is the engine-side preamp number computed, given the pre-R1-6 `CorrectionConfig::Iir` holds baked SOS and `build_correction` takes no rate? | Sequence R1-1's engine half **after** R1-6, then run `ParametricEQ { bands, sample_rate: live }.preamp_db()` verbatim inside `build_correction` | engine-hardening R1-1 §5 ("the controller computes `preamp_db` from the config at `build_correction` time") stays literally true once Q1's `Peq { bands, design_rate }` lands. One implementation, exact agreement with the exported number, and the band-`fc` grid union that makes high-Q peaks exact stays intact — a grid-only recompute would disagree with the export | High | Pin now |
| **D-3** | `effectively_equal`: exact counters (engine-hardening R1-8) against the shell's shipped `> 0`? | `clipped_samples` and `invalid_samples` compare **exactly**; `frame_mismatch_blocks` keeps the shell's `> 0`. Document the asymmetry in the doc comment | engine-hardening R1-8 §Publish — "the **counters compare exactly** — a clip must publish". In a healthy session the two new counters are pinned at 0, so there is no chatter to prevent; when they are non-zero the chatter *is* the signal. `frame_mismatch_blocks` is shell-branch behaviour the merge took "as-is / do not reopen" | High | Pin now |
| **D-4** | `output_peak` pre- or post-clamp? | **Pre-clamp**, per the fix snippet in engine-hardening R1-8 (`let a = v.abs();` is taken *before* `*o = v.clamp(-1.0, 1.0)`); restate the `+6 dB` test row as `output_peak ≈ 2.0` | **P5** / plan item 19's template — ship the reconciliation, say plainly it is a spec-text defect, fix the losing text. A post-clamp peak carries no information the clip counter does not (it saturates at exactly 1.0 precisely when `clipped_samples > 0`); pre-clamp is what proves the preamp is right, which R1-8 gives as the drawer's job; `clipped` and `peak_out` then derive from one quantity and cannot disagree. Cost of being wrong: a UI label | High | Pin now |
| **D-5** | Does `output_peak` decay, or is it a session max? | **Decay it**, with the same `decay_per_block` coefficient as `input_peak`. No output session max | The spec's own logic applied to its own new field: R1-8's defect statement is precisely that a non-decaying peak is "a session statistic wearing a meter's clothes", and it calls output peak a drawer **meter**. "Did it ever go over" is already answered by `clipped_samples` | High | Pin now |
| **D-6** | How do the manual and the computed preamp compose? | They **compose** — multiply in linear, add in dB. The computed number rides `Correction.preamp_lin` (corrected path); the user's manual trim stays on `gain_bits` (both paths, unchanged `[-30, +10]` range, unchanged persistence) | The only structural answer anywhere is engine-hardening R1-1 §4's carrier snippet. Folding them into one number would stop the user moving their trim without removing the headroom the boosts need, and engine-hardening R1-1 §6 requires the app to be able to explain the number it applied | High | Pin now |
| **D-7** | R1-8's decay test cannot reach its own assertion | Keep the assertion (it pins the broadcast release rate, which is the meaningful invariant) and pump `ceil(1.7 · rate / block) + 2` silent blocks, written **as a formula** so it survives a geometry change | Arithmetic, verified: at 512 frames / 48 kHz the 0.1 crossing is at **159.4** blocks, and the spec's "100 silent" blocks only decay to `0.98565^100 = 0.236`. Recorded as a spec-text arithmetic defect at the test, the shape plan items 6 and 19 already use | High | Pin now |
| **D-8** | Does a 4 Hz decaying meter need mitigation? | **Accept and flag.** No throttling, no slower meter | It does not hit disk (`desktop/src-tauri/src/state.rs`: engine-only ticks "never touch disk, not even to read"), and engine-hardening R1-8 states the decaying meter is explicitly **not** release-blocking. If the re-render cost proves real, the honest fix is a separate lighter meters event in Stage 7, not a slower meter | Medium | Ship + validate |
| **D-9** | `design_rate` on a shared `CorrectionConfig` field or per variant? | **Per variant** | engine-hardening R1-6's own DECIDED block defers to the later 2026-07-21 record, which shows `Peq { bands, design_rate }`. A shared field forces the same refusal compare on `Peq`, which §Q1 says must **not** refuse; `paraeq-decide`'s `CorrectionPlan.design_rate` is already documented as "Provenance only — never the rate the engine designs at"; and the name `CorrectionKind` is already taken by `paraeq_decide::decisions` | High | Pin now |
| **D-10** | A band illegal at the **new** rate: drop it, or refuse the whole set? | **Drop the offending bands and count them** in `BuildReport.bands_dropped` (logged at `warn`; publishing the count in `EngineState` is a Stage-7 follow-up, not built). Refuse the whole config only when nothing survives. Expanded below | engine-hardening R1-3's DECIDED rationale is directly on point: "the auto front-end must never be bricked by one bad band… An error would mean *no correction at all* from one bad row." **User-visible behaviour change** from the shell's whole-set `ClearCorrection`; recorded in `crates/paraeq-dsp/DIVERGENCES.md` #18 and flagged `OPEN [OWNER]` per **P3** | High (shape); Medium (product) | Ship + validate, `OPEN [OWNER]` |
| **D-11** | Build a re-derivable FIR variant now? | **Defer** to the Stage-6 FIR room path; ship `Fir { design_rate, firs }` under R1-6's refusal net | `fir::design_fir_correction(correction_db, n_taps, phase)` is **positional** — no Hz grid, no rate — so re-derivation is new DSP; `CorrectionPlan` carries no FIR shape at all; engine-hardening R1-6 says "stage 4 has no FIR path" and assigns the FIR room correction to stage 6. Precedent: R1-4's risk row — "If it slips, ship the wrapper + the existing fixture and hold the new fn." Record it in R1-6's RESOLVED block so a future session does not re-derive the question | High | Pin now |
| **D-12** | Delete `eq::resend_decision`, or keep it? | **Keep it, re-pointed at `EngineState.correction_rate_mismatch`**, demoted from primary mechanism to fallback | engine-hardening R1-6's test table and its risks table both say the tests are "retained and re-pointed, not deleted" and "**Whoever merges the branch must not delete these.**" It still has two live jobs the engine cannot do: the first-ever stream (a persisted, hand-editable `settings.json` band set needs validating and re-stamping at a real rate) and supplying fresh design intent for a config the engine could not re-derive | High | Pin now |
| **D-13** | Where does `validate_bands` live? | `paraeq-engine`, next to `validate_correction`; a thin desktop wrapper only for per-band UI strings. Split the rate-independent checks (command time) from the Nyquist check (build time) | **Already owner-decided**, not open: `docs/decisions/2026-07-22-owner-value-calls.md` — "`validate_bands` guard location \| `paraeq-engine` (next to `validate_correction`) \| Keeps `paraeq-dsp` free of policy"; engine-hardening R1-3 adds "Finalize at implementation." Coordinate with R1-3's remainder so the code moves exactly once | High | Already decided |
| **D-14** | Is the microsecond un-corrected window from moving the correction build after `backend.start()` acceptable? | **Yes** | The live rate is not knowable before `start` returns (`crates/paraeq-engine/src/backend.rs`); the window is microseconds rather than a 250 ms tick; it fails in the un-EQ'd direction engine-hardening R1-6 explicitly prefers ("Audibly un-EQ'd is always better than audibly wrong-EQ'd"); and the renegotiation loop already produces the same class of transient. Keep `backend_event_triggers_stop_start_rebuild` green as the pin | High | Pin now |
| **D-15** | Lease release while the fail-open watchdog is still `NoInputDetected`: fire immediately, or restart the window? | **Restart** — re-baseline `since_ms = now_ms` on release | No spec says. During a wizard session the tap seeing zeros is the **designed** topology (`Direct` captures are tap-excluded), so time accrued under the lease is not evidence of a TCC failure; firing on release would auto-disable the engine right after every successful measurement. **P3**. Pinned by `lease_release_rearms_fail_open_from_zero` | High | Pin now |
| **D-16** | `self_excluded` when no tap is live? | **`false`** | The field is a witness that the invariant is **currently in force**, and `try_start` can bring a tap up on the next 250 ms tick, so `true` for "there is no tap right now" would be a claim about a topology that can change under the session. "No tap" stays distinguishable on the wire because `EngineState.stream` is already `Option<StreamInfo>`. Keeps the spec's literal `bool` (wizard §fail-open hazard); `Option<bool>` considered and rejected as an unnecessary deviation | High | Pin now |
| **D-17** | Should a stopped/disabled engine refuse measurement, and under which diagnostic? | Wizard `Probe` checks engine status **before** the MS-6 self-exclusion witness and refuses with a new append-only `MeasurementDiagnostic::EngineNotRunning = 24`, plus `AbortReason::EngineStopped` for a mid-run `Disable` | With D-16, a stopped engine otherwise produces `SelfExclusionUnavailable`, whose remedy is "Restart ParaEQ" — the wrong remedy for "the EQ is switched off". **MS-20** is the numbered append-only `MeasurementDiagnostic` contract (measurement-safety §Test plan), and `crates/paraeq-measure/src/diagnostic.rs`'s header says additions are cheap — "that is the point of the contract". Nothing is renumbered | High | Pin now |
| **D-18** | Carrier for `self_excluded` into the controller? | New trait method `AudioBackend::self_excluded(&self) -> bool`, **no default impl** | `StreamInfo` is documented as "the effective stream **geometry**" and is already serialized as `EngineState.stream`, so a field there would put one fact at two wire paths (`engine.stream.self_excluded` and `engine.self_excluded`). Cost is confined to `MockBackend` + `TapBackend`; no out-of-tree backends exist. A default impl would be a silent lie on a safety witness | High | Pin now |
| **D-19** | Lease mechanism: atomic + RAII, or a request/reply `EngineCommand`? | **`Arc<AtomicBool>` CAS + RAII token** | `EngineCommand` is fire-and-forget everywhere (`send()` returns nothing) and there is no reply plumbing; adding one introduces a blocking round-trip on the UI thread and a "controller busy" failure mode for no gain. The atomic also survives the `EngineHandle` being dropped and works during unwinding. Matches the crate's style (ArcSwap for state, atomics for telemetry) | High | Pin now |
| **D-20** | Maximum lease duration / self-heal? | **No break in v1**; `WARN` on acquire and on release with the held duration. Mitigate with scope-local RAII | The default is the less surprising behaviour and a break is additive later. The shell has a precedent for a hard cap (`desktop/src-tauri/src/setup.rs`'s probe-loop cap), so `MEASUREMENT_LEASE_MAX_MS` (e.g. 10 min) is a one-line follow-up. Flagged `OPEN [OWNER]` per **P3** because it re-arms a safety net: a leaked token disables fail-open forever, and a TCC denial then holds the user's system muted with no auto-recovery. That is the highest-severity failure mode in this item | Medium | Ship + validate, `OPEN [OWNER]` |
| **D-21** | Publish lease state on the wire (`measurement_active`)? | **No** | The wizard spec asks for exactly one new `EngineState` field. The lease's effect is observable behaviourally (the tap is not destroyed) and in the controller log. Additive later if the Advanced drawer wants it | High | Pin now |
| **D-22** | Does the lease block `Disable` / `Shutdown`? | **No** | User intent and app exit outrank a lease; blocking `Shutdown` would hang quit (`EngineHandle::drop` sends `Shutdown` and joins). The wizard spec names **one** mechanism ("suspends the fail-open watchdog"), not all teardown paths. The wizard aborts instead, via D-17's `AbortReason::EngineStopped` | High | Pin now |
| **D-23** | Where in the measurement spine is the lease acquired? | **Acquire at `Probe`, release at Result/Save, cancel, or any error exit**, as a scope-local RAII token declared **before** the sessions it covers, so reverse-declaration drop releases it last | The wizard spec's spine lists "engine lease" in the `Probe` box, which already precedes every session. Rust drops LOCALS in reverse declaration order (only struct FIELDS drop in declaration order), so "declared after the sessions" — as this row read until 2026-09-17 — releases the lease FIRST, re-arming fail-open while MS-14's restore sequence is still running. Not parked in `AppShared` — a token in long-lived state is not released by a panic | High | Pin now |
| **D-24** | An imported/loaded file's `Preamp:` line now composes with R1-1's auto-preamp — is it a user trim or a headroom number? | **Not decided this phase. Behaviour unchanged: it keeps riding `gain_bits` as the user's trim, so an AutoEq preset attenuates roughly twice.** Recorded rather than fixed | D-6 rules how the two preambles compose, but its premise throughout is a number the **user typed**; no record contemplates one arriving from a file. AutoEq's `ParametricEq.txt` preamp is exactly `−max_gain` of its own bands (engine-hardening `:81`), i.e. the quantity R1-1 now derives, so the two stack: measured `−6.8` (file, both paths) + `−6.78` (engine, corrected path) = `−13.58 dB`. The Phase-A plan freezes this deliberately — "**No `AppData`/`EqState` change.** `EqState.preamp_db` keeps meaning 'the user's trim' ... unchanged persistence" — and the import path is named nowhere in R1-1's file list, so changing it is a product call with a persistence migration behind it, not a review fix. It fails quiet, never loud, and the number is shown on import and editable in one field. **P3**: ship the value, flag it, state the retune signal — the signal is the owner reporting that an imported preset sounds too quiet. Recorded in `crates/paraeq-dsp/DIVERGENCES.md` #19 | High (that it composes); Low (that composing is right) | `OPEN [OWNER]` |

### D-1, expanded — the preamp carrier

**Decision.** The computed preamp travels as a `preamp_lin: f32` field **inside**
`Correction`, and it is applied **only on the corrected path**. It is not sent as
a second `EngineCommand::SetGainDb`.

**Reasoning.** This is not a fresh call; it is a reconciliation of two specs that
already disagree, and engine-hardening has the fuller record:

1. **engine-hardening's Decisions Log already rules it**, verbatim: "Preamp
   carrier | A `preamp_lin` field **inside** `Correction`, applied only on the
   corrected path | *(rejected)* A second atomic alongside `gain_bits`
   (correction and preamp would swap non-atomically → a window of un-preamped
   boost → clipping)".
2. **Its R1-1 §4 gives the atomicity argument in full**: "The preamp must swap
   **atomically with the correction it protects**. Routing it through
   `RtShared.gain_bits` would not: the correction arrives via the `rtrb` swap
   ring and the gain via a relaxed atomic store, and there is no ordering
   between them. If the correction lands first, there is a window in which a
   +12 dB boost runs un-preamped — exactly the case the preamp exists to
   prevent."
3. **R1-1 §4 also states the corrected-path-only requirement outright**:
   "Critically, **the preamp applies only on the corrected path** — `chain.rs`'s
   pass-through (bypass, `frame_mismatch`, no correction) must not attenuate,
   because there is no boost to compensate."
4. **A/B parity is the consequence, and it is the decisive one.** `gain_bits` is
   read once and applied on **both** chain paths — `crates/paraeq-engine/src/chain.rs:217`
   (corrected) and `:252` (pass-through). So under a `SetGainDb` carrier,
   pressing Bypass leaves the auto-attenuation in place and the bypassed side is
   quieter than the corrected side by the whole preamp — up to ~10 dB of silent
   bias in the product's headline A/B control, which is also the wizard's own
   answer to "it removed my bass". That is exactly what (3) forbids. What no
   spec does is **reconcile** (3) with decision-engine's `SetGainDb` sentence;
   engine-hardening states the requirement, decision-engine states an
   incompatible carrier, and neither cites the other.
5. **`gain_bits` is already occupied.** `desktop/src-tauri/src/commands.rs`'s
   `engine_set_preamp_db` → `eq::validate_preamp` (`[-30, +10]`) → `SetGainDb` →
   persisted in `settings.json` → rendered in the EQ tab. One atomic, last
   writer wins, in both directions. See D-6 for how the two compose instead.
6. **Blast radius.** Once R1-6 lands, the carrier route needs **zero** desktop
   changes for the IIR arm; `SetGainDb` needs a second command remembered at
   three call sites forever (`apply_bands`, `profiles_activate`, the forwarder's
   rate-change re-send).

**The FIR arm's preamp is live code, not a placeholder.** `build_fir` is
dispatched from `build_correction` alongside `build_iir`, so the FIR arm reaches
a user the same way the IIR arm does. engine-hardening R1-1 §5 specifies it
verbatim: "For the FIR arm, 'the realized cascade' is the FIR's own magnitude
response — `preamp_lin = 1.0 / max(1.0, max|H(f)|)` over the same grid, computed
with one FFT of the tap vector on the control plane." That is a **pure helper**
— in `paraeq-dsp`, or on `paraeq-engine`'s control plane, never on the realtime
lane — and it ships with R1-1's engine half, pinned by a test in the shape of
R1-8's: a FIR designed for +N dB, full-scale input at its peak frequency,
`clipped_samples == 0`.

**Losing text (P8):** decision-engine §Preamp's "(b) the Tauri backend sends
`EngineCommand::SetGainDb(preamp_db)` alongside `SetCorrection`"; the copy of
that wording in `crates/paraeq-dsp/src/peq.rs`'s
`export_autoeq_format_with_preamp` doc comment; `paraeq-decide`'s
`CorrectionPlan.preamp_db` doc ("Applied to the engine's gain stage"); and the
wizard's verification-sequence step 1 ("the computed preamp on the gain stage").
All four are corrected in the same commit as this record.

**The mechanical guard.** `preamp_does_not_attenuate_the_bypassed_path` is the
test that fails under a `SetGainDb` carrier. It is the reason this ruling cannot
quietly regress: decision-engine and a `peq.rs` doc comment both told an
implementer to do the other thing for two months.

**Confidence:** High. **Kind:** Pin now.

### D-10, expanded — dropping a band that is illegal at the new rate

**Decision.** When `build_correction` re-derives at a live rate that makes some
bands illegal (a centre frequency at or above the new Nyquist, most concretely),
it **drops the offending bands and counts them** in
`BuildReport.bands_dropped`. It refuses the whole configuration only when
**nothing** survives.

**What "counts them" means today, precisely.** The count is returned on
`BuildReport` and logged at `warn` by `build_correction`. It is **not**
published: `EngineState` carries no drop or substitution count, and neither do
the three hand-mirrored wire tripwires. R1-3's rationale below (and
engine-hardening `:216`) says "the published count makes it visible in the
Advanced drawer" — that drawer does not exist (Stage 7), and putting the
count on the wire is a follow-up to be taken with it. Until then a dropped band
is visible only in the controller log, which is the weaker half of this
ruling's acceptability argument and part of why it stays `OPEN [OWNER]`.

**Reasoning.** engine-hardening R1-3 already answered the identical question for
Jury-unstable sections and gave the rationale in general terms: "the auto
front-end must never be bricked by one bad band. Dropping a band degrades
audibly only as 'that band did nothing,' and the published count makes it
visible in the Advanced drawer. An error would mean *no correction at all* from
one bad row." R1-6 is the same funnel one rate-change later. (R1-3's own
`sections_substituted` count has the same gap: it too is logged and not
published.)

**This is a user-visible behaviour change**, and it is stated as one. On the
shell branch the desktop forwarder (`eq::resend_decision`) re-validates the whole
band set at the new rate and issues `ClearCorrection` if any single band fails:
AirPods handing off 48 → 44.1 kHz with one band above 22.05 kHz removes the
user's **entire** EQ. Under this ruling only that band goes. Recorded in
`crates/paraeq-dsp/DIVERGENCES.md` #18, and it belongs in the PR body.

**Confidence:** High on the shape (it is R1-3's own rule), Medium on the product
call. Flagged `OPEN [OWNER]` per **P3** — a reversal is one branch in one
function.

---

## Phase B — Stage 6

| id | Question | Decision | Precedent + ground | Confidence | Kind |
|---|---|---|---|---|---|
| **D-A** | Room target shelf: +3.0 or +4.0 dB? | `decide()`'s room `Parametric` default **reads `RoomTargetSpec::default()`** rather than carrying a literal — which ships **+4.0** today | **P2** + **P9**. The plan itself nominates `RoomTargetSpec` as the single source and asks for "a cross-crate test asserting the `decide()` default equals `RoomTargetSpec::default()` field-for-field". +4.0 sits inside decision-engine's own `0.0..=6.0` domain, so nothing is invalidated. An ears ruling then costs **one line** in `crates/paraeq-dsp/src/targets.rs` | High (shape); the value stays the owner's ears call | Ship + validate |
| **D-B** | `align_spl_band`: (500, 2000) or (200, 2000)? | **(200, 2000)** — `fr::DEFAULT_SPL_ALIGN_BAND` | **P2**, the same layering rule decision-engine states for the σ endpoints. Extra support: the spec's own midband reference `M`, which `low_corner_hz` is measured against, is defined over **200 Hz–2 kHz**, so (200, 2000) makes the alignment band and the midband reference the same band. The constant has zero consumers today, so the change is documentation-only | High | Pin now |
| **D-C** | `q_cap`'s domain is unfalsifiable as typed (decision-engine Open Question 9) | `Domain::Choice(vec![Ceiling(5.0), LogLinear { hi: (10000.0, 3.0), lo: (200.0, 10.0) }])` | The smaller of the **two options decision-engine's Open Question 9 itself sanctions**, and the only one with no `Decisions`/`Overrides` shape churn — the drawer's control becomes a two-item select over the two path policies, which is what the value actually is. The alternative (a decided ceiling scalar + a profile-owned shape) changes both shapes and is the bigger, later move. **Must land before the fixture freeze** — domains are serialized into `expected.json` | High | Pin now |
| **D-D** | `authority`'s preset names have nowhere to live (decision-engine Open Question 10) | Re-type to `Decision<AuthorityPreset>` with `{ Conservative, Custom(AuthorityCurve), Standard }`; put the **resolved** curve in `Analysis` | Required by an owner **direction** already given (**P7**): the plan's REW-parity section says "(Resolving open question 10 above — the preset *names* need a home — is what makes this override presentable.)" and the drawer's must-expose list names "authority preset + max-boost ceiling". It matches decision-engine's domain text verbatim, keeps `Decisions` at 21 fields so the exhaustiveness test stays green, keeps `AuthorityCurve` sealed, and makes an override round-trip a **name** rather than a 957-point curve | High | Pin now |
| **D-E** | `boost_ratio` on the room auto path | `decide()` passes **0.0** on room auto and `DEFAULT_BOOST_RATIO` (0.5) on coupler, both as one named `PathProfile` field | The layers already separate: `paraeq-dsp` owns the mechanism at 0.5 (`authority.rs`), and whether the room auto path passes 0.0 is `decide()`'s Stage-6 policy call. decision-engine states the policy: "Room auto mode ships **cut-only by default**… cuts are free, boosts cost headroom and can damage drivers." **P4** + **P3** | High | Pin now |
| **D-F** | Narrow-dip veto: 1/6 octave or `Q > 3.0`? And `cut_limit`? | **Nothing to decide** — 1/6 octave already shipped (`crates/paraeq-dsp/src/authority.rs`); `decide()` just uses `AuthorityCurve::min_dip_width_oct()`. `cut_limit` already shipped **symmetric** (`let cut = e * w;`, no ×2) | **P1** — two specs state the width and the shipped API is typed as a width. engine-hardening's `Q > 3.0` is the losing text, and its RESOLVED block already carries the reversal recipe (`DEFAULT_MIN_DIP_WIDTH_OCT = width_oct_for_q(3.0)`). Symmetric `cut_limit` is the conservative direction engine-hardening itself asks for (**P4**). Recorded so nobody reopens either as unbuilt | High | Already decided |
| **D-G** | **plan cross-spec Open Question 5** — verification gate quantity, band and location | **`residual_vs_prediction`**, RMS over `correction_range ∩ { f : authority allows any boost or cut at f }` evaluated on `Analysis::freqs_hz` (so it is octave-weighted), computed inside `paraeq_decide::decide()`. `residual_vs_target` is computed and attached as `Evidence`, **never** gated. Expanded below | **P1's second arm** — the shipped type says so in as many words: `crates/paraeq-decide/src/bundle.rs` — "`decide()` re-derives the prediction from it — the gate is residual vs prediction, which is rig-independent", and `Verification { ir, installed, position_index }` carries `installed` **only** so a prediction can be re-derived, and carries no target at all. Plus: a `vs target` gate on a coupler with `flatness_target_db = 1.0` would refuse a *correct* correction whenever the rig's own error exceeds 2 dB RMS, which EARS routinely does — decision-engine records a real 2.1 dB L/R capsule offset baked into the cal | High | Pin now |
| **D-H** | Verification at 1 position or N? | **1 in auto, all-N offered in guided** | The wizard's own lean, and the shipped type agrees: `Verification::position_index` is **singular**. N-position is additive later (`Vec<Verification>`), so defaulting to 1 costs nothing. Same shipped-type tie-break as D-G | High | Pin now |
| **D-I** | Helper binary source location | New workspace member **`crates/paraeq-stimulus`**, one `[[bin]]`; installed to `Contents/MacOS/paraeq-stimulus`. May depend on `paraeq-coreaudio`; must **not** depend on `paraeq-measure`, `paraeq-decide` or Tauri | Decisive ground: the plan requires Stage 6 testable end-to-end on the EARS rig **headless**, before any wizard UI exists, and a `desktop/`-owned binary cannot be spawned from a headless `paraeq-measure` test. Satisfies the wizard's install path and measurement-suite's R5 work item; measurement-suite §Scope's "in `desktop/`" is the losing text. The dependency rule follows measurement-safety's own reasoning — the less policy inside a separate process, the better | High | Pin now |
| **D-J** | Left-window Farina clamp: `0.5·dt₂` (spec) or the full `dt₂` (code)? | `decide()` computes `requested_left_ms = min(profile_request, 0.5·dt₂)` and hands it to `apply_gate`, whose own `min(·, t_peak, dt₂)` then never binds | **P5** / plan item 19's "the number that satisfies both stated reasons wins" — take the stricter. Neither decision-engine's table nor `crates/paraeq-dsp/src/gating.rs` changes, and `decide()`'s is strictly the safer | High | Pin now |
| **D-K** | `fdw_post_cycles`' ceiling of 61.0 is unattainable | Domain becomes `Range 3.0..=30.0`, **derived at runtime** as `STORE_POST_MS/1000 · grid.f_min()` and additionally floored by the grid's `ppo/1.849` limit, with the derivation written into the domain's doc comment | The binding constraint wins. `fdw::apply_fdw` **errors** when `sigma_bins < 1.0`, i.e. `n_c > ppo/1.849 = 51.9` on the 96-ppo standard grid; and plan item 19's shipped +1500 ms store window binds `n_c ≤ 30`. This is a **narrowing to what the implementation can honour**, not a policy change | High | Pin now |
| **D-L** | `transition_hz` no-volume clamp: `[80, 400]` or `room::transition_range`'s `(100, 200, 400)`? | **Keep both.** `decide()` clamps the σ crossing to `[80, 400]`; `room::transition_range` is used only for the Schroeder cross-check and the `SchroederRange` evidence | They answer different questions: `room::transition_range` is display-only and returns a range around a Schroeder estimate; `[80, 400]` is the **clamp on the scan**. No spec change. Note the Schroeder branch is effectively **dead** — room volume is never asked for inside the two-plus-one question budget | High | Pin now |
| **D-M** | Three thresholds differ between decision-engine and the `paraeq-measure` capture layer | **Not in conflict — different gates in different layers.** `decide()` carries decision-engine's numbers as its own named constants in `refusal.rs`, with a doc comment stating the layering, exactly as `paraeq-measure` documents its own | `capture::CLIP_THRESHOLD = 1.0` ("Full scale exactly, not a hair under") and `ladder::NOISE_FLOOR_MAX_DBFS = -60.0` fire **during** capture and refuse to emit; decision-engine's fire on the assembled bundle **afterwards**. They must not be "reconciled" into one number. `compensation::validate_cal_with_threshold` already takes the threshold as an argument, so passing 1.5 needs no change to `DEFAULT_OUTLIER_DB = 1.0` | High | Pin now |
| **D-N** | What happens when an override lands outside its `Domain`? | **Clamp into the domain, set `source: UserOverride` with the clamped value, and emit `DiagnosticCode::OverrideOutOfDomain` at `Severity::Warn`**, naming the decision and both numbers | Genuinely new policy — no precedent exists, so it is flagged `OPEN [DESIGN]`. `decide()` has no `Result` (its Decisions Log: "no `Result`; a refusal must still carry the decisions and evidence that produced it"), so the only two candidates are clamp-and-warn or ignore-and-keep-Auto. Silently ignoring the user's intent recreates exactly the "second, lying source of truth about what the app did" this design exists to prevent | Medium | Ship + validate, `OPEN [DESIGN]` |
| **D-O** | `positions_n` is Recapture "increase only" — what tier is a *decrease*? | **Keep `Recapture`** | The conservative reading never under-invalidates, and `Invalidation` is a single value on a `Decision` so it cannot be direction-dependent. A decrease is arguably a subset selection the spec does not model. Note the UX cost in the doc comment; the clean fix later is a separate "positions used" selection that is not a `Decision` at all. Flagged `OPEN [DESIGN]` | Medium | Ship + validate, `OPEN [DESIGN]` |
| **D-P** | `LowSnrSoft` says "Warn + de-weight" but nothing in `fr.rs` accepts weights | **Additive**: `fr::average_measurements_rms_weighted(&AlignedSet, &[f64])` and `fr::sigma_db_weighted`, leaving the unweighted functions untouched and fixture-frozen | **P6** — the standing "new room behaviour arrives as additive functions". Interim if it slips: emit the Warn and do **not** de-weight, **and say so in the rationale** — a documented no-op beats an undocumented one | High | Pin now |
| **D-Q** | `TwoClock` is an unconditional Warn, but the 2026-07-21 record §Q6 resolved the problem by resampling with a skew estimate, default on | Add `CapturePlan.clock_skew_ppm: Option<f64>`; fire `TwoClock`/Warn only when it is `None`, carrying the ppm as `Diagnostic.value` when it is `Some` | The decision record post-dates the spec: §Q6 adopts "REW's bracketed-timing-marker skew estimate + resample, default on, with `Warn(TwoClock)` as the fallback **when no estimate can be formed**". The spec's unconditional Warn stays the behaviour until the resample lands, so this is safe either way | High | Pin now |
| **D-R** | `DiagnosticCode` has no numbering contract while `MeasurementDiagnostic` has one (**MS-20**) | **Adopt the same contract** — explicit discriminants, a `code()` method, the same append-only header comment, plus a doc-comment mapping between the two vocabularies | `fixtures/decide/expected.json` and any session log will key on these; it costs one commit now and is a breaking change to recorded sessions later. The mapping comment also guards against a future "cleanup" that tries to merge them and drags `paraeq-measure` into `paraeq-decide`, breaking the crate boundary and daemon-readiness | High | Pin now |
| **D-S** | How do per-position IR samples get into a committable `bundle.json`? | **Sidecar `.f64` arrays in the repo's existing convention**, referenced as `{file, len, shape}`; a `tests/common/golden.rs` hydrator fills `ImpulseResponse.samples` | Byte-exact, zero new dependencies, and the reader is a near-copy of `crates/paraeq-dsp/tests/common/mod.rs`. The alternative (f32 WAVs at `store.rs`'s exact naming, making "the fixture **is** a saved profile" literally true) is the better long-term shape but costs a `hound` dev-dependency — flagged, not taken. **Reject** a procedural recipe: it breaks decision-engine's premise that the fixture is a recorded artefact | High | Pin now |
| **D-T** | Which 8 `fixtures/decide/` cases, named how? | The eight named in the plan's B9 — four per-class cases doubling as four of the seven scenarios | decision-engine lists 8 bullets that expand to 11 scenarios while the plan says 8 cases; the mapping is derived, not quoted, so **name them explicitly in the README so it is checkable**, and state there that the remaining ~18 `DiagnosticCode` variants live in in-crate unit tests over perturbed synthetic bundles, **not** in `fixtures/decide/` — or the "owner reviews them once" promise dies quietly | High | Pin now |
| **D-U** | What does "frozen under owner review" mean operationally? | Four parts: byte-for-byte characterization, canonicalization, a per-file-digest `manifest.json`, and an env-var bless (`PARAEQ_BLESS_DECIDE=1`). Digest = **FNV-1a 64 inline** (~10 lines) | Nothing in any spec, plan or decision record defines it. The env-var form follows the owner's own trybuild ruling. FNV rather than `sha2` because the workspace has **no hashing crate** and CLAUDE.md carries a forbidden-deps list, and this is a drift detector against accidents, not a security boundary. `sha2` flagged as the alternative | Medium | Ship + validate |
| **D-V** | Verification and MS-6 (self-exclusion) | Verification does **not** re-check `self_excluded`; it relies on the **baseline** having passed MS-6 upstream | `crates/paraeq-decide/src/bundle.rs`: "`false` on a measurement (**not verification**) capture is a Refuse." For verification the tap **must** see the helper, which is never excluded either way. **Write this inversion down explicitly at the verify gate**, or someone copies MS-6 into the wrong place. If `self_excluded == false`, the baseline that verification subtracts against was itself contaminated and the whole bundle is already refused | High | Pin now |
| **D-W** | Abort across the helper-process boundary | **stdin line protocol**: `abort\n` ⇒ ramp ≤5 ms ⇒ exit 0; EOF means the same (the parent-crashed case); `kill()` only after `ramp_ms + one_block + slack`, and a kill records a **warning** diagnostic | measurement-safety MS-14 and its "**A hard stop is itself a full-scale click**". No spec addresses the process boundary at all, so it is recorded as a decision. Bookkeeping precedent: `desktop/src-tauri/src/setup.rs` (stop-flag + kill-and-reap + a `MAX_PROBE` self-deadline + idempotent stop) | High | Pin now |
| **D-X** | Level and grid alignment of the baseline and verification captures | Compensate the level difference **exactly** by `L_measure − L_verify` (= `−preamp_db`, per MS-19) rather than re-running `align_spl`, and assert the residual mean is near zero; resample both onto `Analysis::freqs_hz` with the **identical** smoothing the analysis path used | Unstated in every spec — a gap, not a conflict. Getting it wrong produces a constant residual equal to the level difference, which on a +6 dB-boost correction is an automatic refuse on **every** boosted correction. Record it in the module header per CLAUDE.md's tier-declaration convention | High | Pin now |
| **D-Y** | Tauri 2 sidecar mechanics | Follow the precedent that already ships: plain `std::process::Command` on a path resolved inside the app bundle (`desktop/src-tauri/src/setup.rs`), with the bundler config used **only** to copy the binary into `Contents/MacOS/` — keeping `tauri-plugin-shell` and its permission surface out of the app | The repo answers none of the mechanics (no `externalBin` key, no shell plugin, no shell capability). **Verify against current Tauri 2 documentation before writing the config.** It blocks release packaging (R7), not development | Medium | Ship + validate |
| **D-Z** | MMM authority model, **if** MMM is in v1 | Confirm cut-only with a fixed conservative boost ceiling and heavy smoothing, and make the IR-derived fields **structurally absent** (`Option` under `MovingMic`) rather than zero-filled | **P4**; the plan's MMM section — "falls back to the safe universal rule — cut peaks, do not fill dips, fixed conservative boost ceiling, heavy smoothing" — and "cleanly absent rather than faked". **Contingent on E3**, and its level-safety row is **E4**, which is a safety gap, not a default | Medium | Contingent — see E3/E4 |

### D-G, expanded — plan cross-spec Open Question 5's gate

**Decision.** The verification gate is `residual_vs_prediction`, RMS over
`correction_range ∩ { f : authority.at(f).max_boost_db > 0 || max_cut_db > 0 }`,
taken on `Analysis::freqs_hz` so it is octave-weighted, computed inside
`paraeq_decide::decide()`. `residual_vs_target` is computed and attached as
`Evidence` and is **never** the gate. **The threshold itself is escalated — see
E1.**

**Reasoning.**

1. **The vote is 1–1–abstain, not 2-of-3, so P1's majority arm does not
   apply.** decision-engine's refusal table says "residual RMS vs **target** over
   the authority band | `> 2·flatness_target_db` | Refuse". The wizard's
   Decisions Log says "`residual_vs_prediction` (rig-independent).
   `residual_vs_target` is *reported*, never the gate", and its body says "Report
   it, plot it, **never gate on it**". `grep -n "flatness" docs/specs/*.md`
   returns **zero** hits in `2026-07-15-measurement-safety-design.md`, so the
   plan's attribution of this question to "(decision-engine,
   **measurement-safety**)" is factually wrong; measurement-safety abstains.
2. **P1's second arm decides it — the shipped type.** `bundle.rs` says in as
   many words that "`decide()` re-derives the prediction from it — the gate is
   residual vs prediction, which is rig-independent", and `Verification` carries
   `installed: CorrectionPlan` **only** so a prediction can be re-derived. It
   carries no target at all. This is the same tie-break that decided the
   narrow-dip veto (plan item 16) and D-H.
3. **The band needed a sentence.** Neither spec defines "the authority band";
   both use the phrase. Without the intersection above the gate is not
   computable, and RMS on the octave-spaced analysis grid is what stops the
   treble dominating a bass correction's score.
4. **Location.** The code (`VerificationResidual` in
   `crates/paraeq-decide/src/outcome.rs`), the input slot (`bundle.rs`) and all
   three inputs (`flatness_target_db`, `correction_range`, `authority`) are
   already in `paraeq-decide`.

**Confidence:** High on the quantity, band, and location. The **threshold** is
not decided here — see E1.

---

## Losing-spec text corrected alongside this record (P8)

Every edit below lands in the same commit as this file. Each one is a spec (or
doc-comment) sentence that asserted a shape this record supersedes.

| Where | What it said | What it says now |
|---|---|---|
| decision-engine § Preamp | "(b) the Tauri backend sends `EngineCommand::SetGainDb(preamp_db)` alongside `SetCorrection`" | the `Correction.preamp_lin` carrier (D-1) |
| `crates/paraeq-dsp/src/peq.rs` (`export_autoeq_format_with_preamp` doc) | the same `SetGainDb` wording, copied into a Stage-1 doc comment | the same correction (D-1) |
| `crates/paraeq-decide/src/outcome.rs` (`CorrectionPlan.preamp_db` doc) | "Applied to the engine's gain stage, not just the export text." | the `Correction.preamp_lin` carrier, corrected path only (D-1) |
| wizard § Verification sequence, step 1 | "the computed preamp on the gain stage" | "the computed preamp carried on the installed `Correction`" (D-1) |
| wizard § Verification sequence, step 2 | "the same sweep WAV used for the baseline at that position" | same sweep, **re-levelled** to MS-19's `L_verify = L_measure − max(0, peak_correction_gain_db)` |
| measurement-suite § helper child process | "Same sweep, same deconvolution, two processes" | same sweep and deconvolution at **different levels**, per MS-19 |
| decision-engine § Refusal table | "residual RMS vs **target** over the authority band" | `residual_vs_prediction` RMS over the authority band, with the band defined (D-G); threshold flagged `OPEN [OWNER + NEEDS DATA]` (E1) |
| decision-engine § Decision table, `align_spl_band` | default `(500.0, 2000.0)` | `(200.0, 2000.0)` (D-B) |
| decision-engine § Targets, room `Parametric` | `shelf_db` default `+3.0` | reads `RoomTargetSpec::default()`, which is `+4.0` today (D-A) |
| decision-engine § Decision table, `fdw_post_cycles` | domain `Range 3.0..=61.0` | `Range 3.0..=30.0`, derived (D-K) |
| decision-engine § Decision table, `q_cap` | "`Range 1.0..=20.0` on the ceiling" | `Choice` over the two path policies (D-C) |
| decision-engine § Decision table, `authority` | `Choice: Standard, Conservative(×0.5), Custom(curve)` against a `Decision<AuthorityCurve>` | `Decision<AuthorityPreset>` + the resolved curve in `Analysis` (D-D) |
| decision-engine Open Questions 9 and 10 | "OPEN [DESIGN — blocks Stage 3]" | closed in place, in the same DECIDED formatting the other eight use (D-C, D-D) |
| engine-hardening § Autofit limits | narrow-dip veto "Proposed threshold `Q > 3.0`" | points at the shipped 1/6-octave width (D-F); the RESOLVED block below it already carried the reversal recipe |
| engine-hardening R1-6 § Fix | the baked-SOS `CorrectionConfig { design_rate, kind }` struct | a `> **RESOLVED**` block, the shape R1-4 already uses (D-9, D-11) |
| engine-hardening R1-8 § Tests | `output_peak == 1.0` at +6 dB | `output_peak ≈ 2.0` — pre-clamp (D-4) |
| engine-hardening R1-8 § Tests | "10 full-scale blocks then **100 silent**" | `ceil(1.7 · rate / block) + 2` silent blocks, as a formula (D-7) |
| measurement-suite § Scope | the `paraeq-stimulus` helper binary "in `desktop/`" | `crates/paraeq-stimulus`, installed to `Contents/MacOS/` (D-I) |
| wizard § Refusal table and § When verification fails | bare "threshold" | `2 · flatness_target_db`, explicitly flagged `OPEN [OWNER + NEEDS DATA]` (E1) |
| wizard Open Question 2 | "Leaning: 1-position" | closed: 1 in auto, all-N offered in guided (D-H) |
| measurement-safety § Fade and DC (MS-3) | "OPEN [OWNER + NEEDS DATA]" | **implemented in one of the two sanctioned directions** (the envelope-shaped DC block in `crates/paraeq-measure/src/stimulus.rs`), **owner ratification pending** — see below |
| measurement-safety § Targets and caps table | MMM mentioned only in a note under the table | an explicit `OPEN [NEEDS DATA]` **MMM row inside the table** (E4) |

**Two corrections to how these were previously characterized**, both load-bearing:

- **MS-3 is not "closed".** The spec offered two sanctioned resolutions
  ("restate the threshold for sweeps, or DC-block in the MS-4 stimulus
  assembly — but **do not inflate the fade-in**"), and
  `crates/paraeq-measure/src/stimulus.rs` took the second: an envelope-shaped
  offset `x -= c·env` with `c = mean(x_faded)/mean(env)`, which zeroes the mean
  identically while keeping the endpoints at exactly 0.0 and the fade-out
  monotone. That is an implementation inside the sanctioned set, not an owner
  ruling. The spec item is restated as **implemented in the sanctioned
  direction, OWNER RATIFICATION PENDING**.
- **The MMM level-safety gap is the `OPEN [NEEDS DATA]` note in the per-class
  caps table**, and the new explicit MMM row belongs in **that** table. It is a
  different item from MS-3, and unlike MS-3 it has no sanctioned default at all
  — see E4.

---

## Escalated to the owner

Seven items. **None of these is a decision.** Everything above ships on a
precedent; these cannot, and each says why.

### E1. The verification residual threshold — the `2×` multiplier

**Blocks:** the constant inside the Stage-6 verification gate, and the
credibility of the product's most important refusal. **Does not block** building
the gate — ship the mechanism, escalate the number.

**Summary.** D-G settles *what* is measured. It does not settle *how much is too
much*, and the only number on the table does not survive contact with the
wizard's own worked example:

- `2 · flatness_target_db` is **2.0 dB** on the coupler (`flatness_target_db =
  1.0`) and **6.0 dB** on the room (`3.0`).
- The wizard's results screen renders its own example of a **passing**
  verification as "It's doing what we designed, to within **1.8 dB RMS** across
  20–240 Hz."

So the coupler gate refuses the spec's own success story by 0.2 dB, and the room
gate is more than 3× looser than the spec's own stated expectation. The number
was almost certainly chosen with the **target** residual in mind, where rig error
dominates; it does not transfer to the **prediction** residual unexamined.

**Why no default is safe.** Both directions are wrong in a way that is invisible
until real hardware runs: too tight and auto mode becomes a false-refuse machine
that sends correct corrections to the guided path; too loose and the refusal the
wizard calls "the single most important refusal in the product" never fires, and
auto mode's silence stops being earned. An agent cannot pick between those from
the documents — the collision is *inside* one spec.

**Options.** (a) Ship 2.0 as `VERIFICATION_RESIDUAL_MULTIPLE`, read off
`decisions.flatness_target_db.value` so an override moves the gate with it, and
retune on the first EARS run. (b) Ship an **absolute** dB gate instead of a
multiple, on the ground that the prediction residual is a property of the DSP
chain and not of the path's flatness ambition. (c) Per-path multiples.

**Recommendation: (a)**, per **P3** — one named constant, one line to change,
flagged `OPEN [OWNER + NEEDS DATA]` with the retune signal stated: *the first
EARS run where a correction that A/Bs correctly still refuses.* But the owner
should see the 2.0-vs-1.8 collision **before** the first ears-on verification
run, not after. Recorded here as **ESCALATED [OWNER + NEEDS DATA]**, not decided.

### E2. How a genuine TCC silent failure is detected **during** a measurement session

**Blocks:** Stage 6's verification-loop wiring and the wizard's refusal table.
**Does not block** Phase A — the measurement lease (D-19/D-23) ships either way.

**Summary.** The wizard requires: "a genuine TCC silent failure during a
measurement session is still a `Refuse`". But the same section explains why the
tap **legitimately** sees zeros throughout — `Direct` captures are tap-excluded
**by design** — and `EngineStatus::NoInputDetected` means exactly "no nonzero
sample since start". **Implemented literally, that sentence refuses 100% of
wizard runs.**

**Why no default is safe.** The engineering is clear; what is not clear is
whether reinterpreting a **safety sentence** in the spec is the agent's call. It
is not.

**Options.** (a) Implement it literally and accept that every wizard run
refuses — not viable. (b) Drop the requirement — loses a real safety property.
(c) **The recommended split**: the lease suspends only the auto-disable;
`NoInputDetected` keeps being reported and logged but is **not blocking** during
`Direct` capture; and the TCC witness moves to the **Verify** gate, where it is
falsifiable — the helper has its own PID and is *not* tap-excluded, so while it
plays the tap **must** see nonzero blocks within ~1 s. The measured fact behind
that is already in the repo: `crates/paraeq-coreaudio/tests/test_hardware.rs` and
`crates/paraeq-coreaudio/src/status.rs` record **0 callbacks/s idle vs ~94/s
during playback**. No nonzero blocks while the helper plays ⇒ TCC silent failure
⇒ `Refuse`.

**Recommendation: (c)** — it replaces a state the session itself causes with a
precise, testable one. Escalated because it **reinterprets a safety sentence**,
not because the engineering is unclear.

### E3. MMM scope and sequencing

**Blocks:** the `MeasurementBundle` shape, and therefore the `fixtures/decide/`
freeze.

**Summary.** The owner already decided (2026-07-22) that **MMM is the room
easy-mode default** and discrete sweeps are the precision option. But:

- **Three specs still list MMM as Out of Scope and were never updated**: the
  wizard ("Incompatible with per-position σ(f), which is the differentiator"),
  room-dsp, and measurement-suite. Out-of-Scope sections exist so future sessions
  do not re-litigate; stale ones invert that protection.
- **None of the seven stages contains MMM work.** The plan's MMM section lists
  four architectural implications, all "new work, sequenced no earlier than
  Stage 4's room path", and one of them — `CaptureMethod { DiscreteSweep,
  MovingMic }` on `MeasurementBundle`, with `decide()` branching its authority
  model on it — is a **Stage 6** change.
- `crates/paraeq-measure/src/stimulus.rs` says pink noise "is a later stage and
  is deliberately absent here."

**Why no default is safe.** A `decide()` written without `CaptureMethod` is
throwaway if MMM ships; one written with it is speculative if MMM slips. And
because `CaptureMethod` changes `bundle.json`, **`fixtures/decide/` cannot be
frozen until this is ruled** — freezing first turns the question into a
fixture-invalidating change that must be argued in a PR.

**Options.** (a) MMM in v1 — add `CaptureMethod` and branch `decide()`. (b) MMM
deferred past v1 — say so in the plan and correct the three Out-of-Scope sections
to match (they currently say the right thing for the wrong reason). (c) Ship
`CaptureMethod` as a **one-variant enum now**, so adding `MovingMic` later is
additive and the bundle shape is stable.

**Recommendation: (c)** — the cheapest hedge, and it costs one enum. Natural
companion: measurement-suite's schedule-contingency call ("correct below the
transition only if the date slips"), which is also `OPEN [OWNER]`.

### E4. The MMM level-safety row — a safety gap on the **default** room path

**Blocks:** any MMM capture actually running. **Contingent on E3.**

**Summary.** measurement-safety, verbatim: "this table has no row for the
**Moving Microphone Method (MMM)** … MMM plays continuous pink noise —
continuous energy with a different crest factor than a sweep — so
hearing-exposure and driver-heating limits differ; the sweep caps above must
**not** be reused unexamined. MMM needs its own level-safety row, derived from
real pink-noise measurements." Tagged `OPEN [NEEDS DATA]`. This record makes that
gap a visible **row inside the caps table** rather than a note under it.

**Why no default is safe.** The spec **explicitly forbids the only available
default** (reusing the sweep caps), and MMM is now the **default** room journey —
so this is a safety gap on the path most novices will take, not a corner case.
This is the one item in the whole program where proceeding on a default is a
safety regression rather than rework.

**Options.** (a) **Refuse the MMM path entirely until the row exists** (P4 —
quiet is the safe direction). (b) Ship MMM behind an explicit "experimental,
levels unverified" acknowledgement. (c) Derive an interim row from pink noise's
crest factor relative to the sweep's and mark it provisional.

**Recommendation: (a)** as the conservative interim — but it is a product
decision only the owner can make, because it removes the default room journey.

### E5. MS-17's chain-sensitivity envelope — guessed numbers **refuse real hardware**

**Blocks:** the DT-01 class cross-check and the `WrongTransducer` refusal, and in
practice every real headless run on unusual hardware — a chain outside the
envelope is refused **before any sweep is emitted**.

**Summary.** No spec states any values. `crates/paraeq-measure/src/level.rs`
ships engineering estimates: coupler **85–130**, room **65–120** dB SPL per dBFS
RMS. One end is pinned and is not a guess —
`crates/paraeq-measure/tests/test_ladder.rs`'s
`the_envelope_admits_every_chain_that_can_reach_target` forces the envelope to
contain `spl_target_db − sweep_level_dbfs_rms`, i.e. 104 coupler / 87 room. The
other end is a guess.

**Why no default is safe.** The **shape** is a default and is taken: mirror the
same ranges onto `PathProfile`, because `paraeq-decide` may not depend on
`paraeq-measure` — the same reason `paraeq-measure` re-exports `TransducerClass`
from `paraeq-dsp` — with a cross-crate test asserting the two tables agree
field-for-field (the **P9** shape). **The numbers are not.** Wrong values surface
to a user as "ParaEQ won't measure my headphones": a user-visible failure
attributable entirely to a guessed constant.

**Options.** (a) Ship the estimates and retune on the first rig run. (b) Widen
the envelope until it is effectively a sanity check, and rely on the level ladder
and MS-2's hard cap for real protection. (c) Hold the `WrongTransducer` refusal
until measured values exist.

**Recommendation:** settle them with measurement — the owner measuring `S` on the
EARS rig and on a real room system, which the Phase-A hardware tests make
possible for the first time. Until then (a), flagged. Escalated because the
consequence is refusing a user's real hardware.

### E6. Hardware spike — can the tap aggregate and the measurement aggregate coexist?

**Blocks:** the Stage-6 capture-side design. This is an owner/rig **action**, not
a decision — but it needs scheduling, and finding out late is expensive.

**Summary.** Nobody has ever run both at once.
`crates/paraeq-coreaudio/src/tap.rs` composes the default output as a
tap-aggregate sub-device; `crates/paraeq-coreaudio/src/measure_aggregate.rs`
composes it **again** ("Mirrors `tap.rs::create_aggregate` key-for-key, minus the
tap list"). Verification is the first operation that needs both live, and the
Stage-5 hardware tests explicitly run **without** a session/engine. No spec
addresses it and `measure_aggregate.rs` contains no note about coexistence.

**Why no default is safe.** It is an empirical question about CoreAudio, not a
policy question. A guess either way silently designs the capture side wrong.

**Options / fallback.** Run the spike before the verification capture path is
designed around it. **If it fails:** capture the verification mic through a
mic-only aggregate and lean entirely on the two-clock marker path for `t = 0`,
accepting the two-clock warning — which redesigns the capture side.

### E7. The `fixtures/decide/` freeze sign-off

**Blocks:** the freeze, by definition.

**Summary.** decision-engine: the expected values "are **reviewed by the owner
once and then frozen**". That is an owner gate, not something an agent can
default.

**What the owner is being asked to sign:** eight `notes.md` files (one paragraph
each — what the case exercises, the expected verdict, the diagnostics it should
raise), written **before** the bless, plus the eight `expected.json` the bless
produces. Without the notes, "reviewed once" means reading several hundred KB of
`Analysis` curves.

**Why no default is safe.** A freeze that re-blesses twice in its first week
trains everyone to rubber-stamp the diff, which is exactly what the mechanism
exists to stop.

**Recommendation:** do **not** schedule this until decision-engine Open Questions
9 and 10, plan items 13, 14 and 15, the item-11 type reconciliations, **plan
cross-spec Open Question 5**, E3's `CaptureMethod` call, and the merged wire form
(snake_case `FilterType`) are all settled.

**Adjacent owner *actions*** (not decisions; listed so they are not lost):

- The **Tier-4 REW characterization corpus**. No `fixtures/rew/` exists; it was a
  Stage-2 gate and has slipped past Stage 5, so the whole room pipeline ships
  validated only against its own analytic invariants and never against the
  field-standard tool. Pair the capture session with E6 and the two-clock run so
  one rig session produces all three.
- The **two-clock magnitude run** — the harness is written and `#[ignore]`d at
  `crates/paraeq-coreaudio/tests/test_measure_hardware.rs`. It unfreezes the
  provisional 5.5 s sweep-length cap, sets the clock-adjust reject bound, and
  closes the IR-perturbation question. measurement-safety gates the **gated room
  path's ship** on it.
- **Counsel** on the EN 50332 / IEC 62368-1 cl. 10.6 and PLD 2024/2853
  Art. 2(2) / 6(1)(a) / 14 clause numbers — measurement-safety says plainly that
  "the article numbers are corpus-sourced and unverified". The **posture** is
  already decided (stay FOSS / non-commercial); only the published claim waits.
  Blocks R7, not code. The 85 dB warn / 100 dB refuse thresholds stand as
  engineering thresholds regardless.
