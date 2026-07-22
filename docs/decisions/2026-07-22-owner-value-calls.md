# Owner Value Calls — Decisions

**Date:** 2026-07-22
**Status:** decided by the owner (except where a downstream dependency is noted).
Companion to `2026-07-21-decision-engine-open-questions.md`; this doc records
the remaining `OPEN [OWNER]` value/policy calls across the six specs plus the
`TransducerClass` naming that gated Stage 3.

## Summary

| Call | Decision | Note |
|------|----------|------|
| TransducerClass naming | Variants `{Bookshelf, Floorstander, InEar, OverEar}`; `Headphone`/`Iem` are display strings, not variants | Unblocks Stage 3 targets rework |
| Commercial posture | **Stay free & non-commercial** | Posture set; counsel still verifies the cited clause numbers |
| Room easy-mode default | **Moving Microphone Method (MMM)** | Discrete sweeps stay the precision option |
| Mode-preference location | **App-global for v1** | Per-profile is additive later |
| MS-2 sweep level | **Hard cap (refuse), not a higher ceiling** | Quiet is the safe direction |
| Preamp convention | AutoEQ-exact (−max_gain, no headroom, clamp ≤0) | Already landed Stage 1, DIVERGENCES #14 |
| Coupler smoothing default | `Fixed(6)` | Matches pinned oracle + REW's dB-averaging regime |
| `validate_bands` guard location | `paraeq-engine` (next to `validate_correction`) | Keeps `paraeq-dsp` free of policy |
| trybuild toolchain pin | Do not pin; regenerate snapshots on drift | Repo-wide, minor |

## TransducerClass naming

The enum keeps the variants `{Bookshelf, Floorstander, InEar, OverEar}` in
`paraeq-dsp::targets` (alphabetical; already scaffolded in Stage 2). These names
are precise about coupling — `OverEar`/`InEar` are the acoustically meaningful
distinction, where "Headphone" is ambiguous — and they parallel the two room
variants. `Headphone`/`Iem` are **retired as variant names** and become UI
**display strings** via a `display_name()` method: "Over-ear headphones",
"In-ear monitors", "Bookshelf speakers", "Floorstanding speakers". The wizard
renders those. The decision-engine `PathProfile` is the canonical struct; the
wizard's illustrative view defers to it (splits `sweep` into scalars; carries
`coupling`/`positions_default`/`positions_domain`/`reposition_noun`/
`flatness_target_db`; derives `target`/`refusal` as `Decision`s). This closes
the wizard §Spine `[DESIGN — blocks Stage 3]` item, plan cross-spec question 3,
and plan item 11's naming half.

## Commercial posture — stay free & non-commercial

ParaEQ stays free and supplied entirely outside commercial activity, retaining
the PLD 2024/2853 Art. 2(2) FOSS exemption (the only thing that keeps the "AS
IS" disclaimer effective — Art. 14 voids contractual exclusion otherwise). No
paid tier and no "pay with personal data," either of which forfeits the
exemption (recital 15). This is also why the conservative safety design (level
ladder, driver caps, hearing-safety refusals) is load-bearing, not just
polish. Resolves decision-engine §Q8, measurement-suite Q1, measurement-safety
S-3d, wizard Open Q6.

**Still owner/counsel, not closed by this decision:** the cited article and
clause numbers remain unverified — EN 50332 / IEC 62368-1 cl. 10.6 (safety
S-1) and PLD Art. 2(2)/6(1)(a)/14 + recital 15 (safety S-3a/b/c). Get a real
legal opinion before a published release; the posture above is the plan-around
shape regardless of how the numbers land.

## Room easy-mode default — MMM

The "just fix my sound" room path defaults to the Moving Microphone Method:
faster, more forgiving, and immune to the two-clock problem. It is magnitude-
only, so the correction is the simpler cut-focused one (no gating, no σ(f)
throttle, no group-delay authority). Discrete sweeps remain available as the
precision option for users who want the fuller correction. Resolves the plan's
MMM section open question "room easy-mode default." The MMM authority model and
its level-safety row (needs real pink-noise data) stay open — see the plan's
MMM section and measurement-safety.

## Mode-preference location — app-global for v1

The auto/guided mode preference is a single app-global setting for v1. A
per-profile override (a user wanting guided for their room and auto for their
headphones) is additive and can be added later without rework. Resolves wizard
Open Q7.

## MS-2 sweep level — hard cap

The per-class dBFS "sweep level" is a hard cap: `SweepLevel::new` refuses a
chain too insensitive to reach the SPL target at or below the safe level,
rather than driving it louder. Quiet is the safe direction on every axis
(hearing exposure, driver excursion), and MS-17's envelope check should refuse
such a chain anyway. No separate higher ceiling. Resolves measurement-safety's
MS-2 item (plan item 7).

## Recorded-with-recommendation (no owner input needed)

- **Preamp convention:** AutoEQ-exact — `−max_gain`, no headroom constant,
  clamped ≤ 0 — already landed in Stage 1 (`DIVERGENCES.md` #14). The
  inter-sample-peak headroom alternative is recorded but not taken. (wizard
  Open Q5)
- **Coupler smoothing default:** `Fixed(6)` — matches the pinned oracle
  (`measurement_wizard.py:481–484`) and REW's endorsed dB-averaging regime.
  The 1/12 + 6–8 kHz sigmoid-taper alternative is recorded. (wizard Open Q4)
- **`validate_bands` guard location:** `paraeq-engine`, next to
  `validate_correction` (`controller.rs:80`), so the daemon seam inherits it
  and `paraeq-dsp` stays pure designers. Finalize at implementation.
  (engine-hardening R1-3)
- **trybuild toolchain pin:** do not pin `rust-toolchain.toml`; regenerate the
  compile-fail snapshots with `TRYBUILD=overwrite` if a compiler release
  rewords the diagnostics. (plan item 12)

## Deliberately still open after this pass

Value/ears still needed: room-target defaults (tilt/shelf) and the autofit
constants (`BOOST_WEIGHT`, narrow-dip Q, `cut_limit`) — these need the owner's
ears on real measurements, so they stay data-gated. The autofit **shape** that
carries those constants remains the plan's cross-spec question 2 (a design
reconciliation, blocks Stage 5). Legal counsel on the clause numbers (above).
Everything data-gated (Q3/Q4/Q5 validation, two-clock magnitude, MS-3 DC-gate
restatement, MMM level-safety numbers, stored-window, latency) is unchanged.
