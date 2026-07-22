# Unified Measurement Wizard — Design Spec

**Date:** 2026-07-15
**Status:** Draft — for owner review
**Amends:** `docs/specs/2026-07-02-rust-port-design.md` — voids the "Measure tab" parity row (EARS-centric framing; "the Farina method tolerates their small clock skew" does not extend to gated measurement) and expands stage 6 from "EARS sweep flow" into the flow specified here.
**Companion specs:** `docs/specs/2026-07-15-decision-engine-design.md` (the `decide()` function, `DecisionSet`, `PathProfile` field semantics, authority derivation). This spec owns the **flow, the screens, and the contracts between the wizard and the engine**; it does not re-derive decision rules — it cites them.

## Summary

One wizard, one entry point, one code path. The user picks what they are correcting — headphones, IEMs, bookshelf speakers, floorstanders — and everything downstream is parameterized by a `PathProfile`, not branched. Headphone measurement is *structurally* the same operation as room measurement: N captures at different positions, a variance gate, an average, an authority-limited fit. Re-seating a headphone is the coupler's version of moving a mic. Two wizards would fork within one release; a `PathProfile` cannot.

Two front-ends render one engine: **"Just fix my sound"** (auto — the app decides, and every decision is visible and overridable in an Advanced drawer) and **"Walk me through it"** (guided — the same decisions, one screen at a time, with rationale). The mode is chosen at first run and switchable in Settings at any time, including mid-session. Both are pure renderers over the `DecisionSet` returned by `decide(&MeasurementBundle)` (the `PathProfile` is derived inside from `bundle.class`, not passed as a second argument — see the decision-engine spec). No default, threshold, or policy lives in TypeScript.

Three things make this wizard defensible rather than merely friendly. **σ(f), the inter-position spread, becomes a user-visible confidence band** and gates EQ authority — it is one extra pass over data we already hold, and no competitor automates it. **Closed-loop verification** re-measures the corrected output through a helper child process, producing a residual that is *rig-independent* and therefore the only honest numeric claim the product can make. And **raw per-position impulse responses are persisted**, which is what turns the Advanced drawer from a settings page into a live instrument (Redesign ≈ 50 ms, Reanalyze ≈ 200 ms; only mic, output device, sample rate, or an N increase force a recapture).

## Decisions Log

| Decision | Choice | Alternatives rejected |
|---|---|---|
| Wizard shape | One spine, `PathProfile`-parameterized; four transducer classes | Two wizards (coupler/room) over shared DSP — guarantees divergence within a release; four flows |
| Bookshelf vs floorstander | **Not a branch.** Class sets a conservative *sweep-band prior* only; the correction low corner is data-derived (lowest frequency within ~10 dB of midband) | A marketing distinction; separate profiles with separate correction rules |
| IEM vs over-ear | A `PathProfile` (SPL target, target family, 7–11 kHz refusal band), not a path | A separate coupler flow |
| Mode seam | Typed `DecisionSet` from Rust; both modes and the drawer are renderers | Rationale strings from Rust with policy in TS (untestable, unoverridable, forks); two wizards |
| Auto-mode question budget | **Two questions + one conditional**: transducer class, mic (only if ambiguous), UMIK serial (only if no cal is cached) | Dirac's arrangement picker (9/13/17); Sonarworks' 37-position protocol — both are decisions, not questions |
| Mic localization | **Rejected.** Show a diagram, say "move about 30 cm", trust the user | Sonarworks-style acoustic localization: needs known geometry, adds a class of unrecoverable "cannot locate microphone" errors, buys nothing for a gated, averaged *magnitude* correction |
| N (captures) | Room: 9 (Dirac's anchor), minimum 3 accepted. Coupler: 5 re-seats, minimum 3 accepted | Sonarworks' 37 (20–30 min); a single capture |
| Per-capture failure | Reject **the capture**, never the session; per-position clear + re-measure (Dirac's precedent) | Full restart on one bad capture |
| Verification stimulus | **Helper child process**, not in the tap's exclusion list, so the sweep is captured, corrected, and re-measured through the real runtime path | Pre-convolving the sweep with the active `CorrectionConfig` (defeats self-exclusion's purpose and measures the filter's *math*, not the engine); no verification at all (Dirac's and Sonarworks' choice) |
| Verification gate | `residual_vs_prediction` (rig-independent). `residual_vs_target` is *reported*, never the gate | Gating on target residual — inherits every rig limitation and cannot be claimed |
| Persistence unit | Raw per-position IRs (windowed, bounded), not derived curves | The prototype's derived-curve store (`measurement_wizard.py:483`), which discards every measurement on a cal change (`:389-391`) — a storage artifact masquerading as physics |
| Progress source | Capture callback frame counter over the existing ~30 fps binary Tauri Channel; render sweep instantaneous frequency on a log-f axis | The prototype's emit(10) → block 5 s in `sd.playrec` → emit(50) (`measurement_wizard.py:66,74,81`) — freezes at 10% for the entire siren |
| Limits register | Scoped competence on the frequency axis ("corrected 20–240 Hz; above that, tone only"), in-product at the moment of decision | Apologetic prose; a whitepaper (Dirac's placement); silence |

## The Spine

One state machine. Every state exists in every path; the `PathProfile` changes what a state *does*, never whether it runs.

```
                  ┌──────────────┐
   first run ───► │ ModeChoose   │  auto | guided      (Settings can re-enter)
                  └──────┬───────┘
                         ▼
                  ┌──────────────┐
                  │ Probe        │  devices, rates, TCC, self-exclusion,
                  └──────┬───────┘  cached cals, engine lease
                         ▼
                  ┌──────────────┐
                  │ Class        │  Headphone | Iem | Bookshelf | Floorstander
                  └──────┬───────┘  ── the ONLY question that is not inferable
                         ▼
                  ┌──────────────┐
                  │ Rig          │  output device, mic, cal file, input gain
                  └──────┬───────┘
                         ▼
                  ┌──────────────┐
                  │ Level        │  noise floor → pilot → solve → escalate
                  └──────┬───────┘  (refuse, never chase SNR with output level)
                         ▼
                  ┌──────────────┐
          ┌──────►│ Capture[i]   │  sweep, deconvolve, validate, accept/reject
          │       └──────┬───────┘
          │              │ i < N
          └──── reposition (PathProfile::repositioning)
                         │ i == N, accepted >= 3
                         ▼
                  ┌──────────────┐
                  │ Analyze      │  align SPL → gate/FDW → smooth → average
                  └──────┬───────┘  → σ(f) → transition → authority mask
                         ▼
                  ┌──────────────┐
                  │ Design       │  target select → correction → autofit
                  └──────┬───────┘  → limits → preamp
                         ▼
                  ┌──────────────┐
                  │ Verify       │  helper-process closed loop  (see below)
                  └──────┬───────┘
                         ▼
                  ┌──────────────┐
                  │ Result       │  A/B, numbered events, authority ribbon
                  └──────┬───────┘  Advanced drawer ⇄ Redesign/Reanalyze
                         ▼
                     Save profile
```

**Shared by every path, byte for byte:** the state machine; `Probe`; the level protocol (noise floor → pilot → solve → escalate in ≤6 dB steps → sweep); sweep generation and Farina deconvolution; per-capture validation structure; align-SPL; σ(f) computation; the authority mask machinery; autofit; the boost-Q cap; the preamp computation; verification; persistence; every screen's layout.

**Parameterized by `PathProfile`** (fields alphabetical, per CLAUDE.md):

```rust
pub struct PathProfile {
    pub authority: AuthorityEnvelope,   // frequency-indexed boost/cut ceiling
    pub averaging: Averaging,           // DbDomain | AlignSplThenPower
    pub capture_count: usize,           // N
    pub class: TransducerClass,         // Headphone | Iem | Bookshelf | Floorstander
    pub gating: Gating,                 // None | Fdw { pre_cycles, post_cycles }
    pub refusal_bands: Vec<RangeInclusive<f64>>, // never auto-fit narrow features here
    pub repositioning: Repositioning,   // Reseat | Move { nominal_cm: f64 }
    pub smoothing: Smoothing,           // Fixed(u32) | RewVariable
    pub sweep: SweepProfile,            // f_start, level, spl_target, spl_cap, duration
    pub targets: TargetFamily,          // gated by TransducerClass — see below
}
```

**`PathProfile` is owned by the decision-engine spec — this is an illustrative view, not the canonical definition.** `decision-engine-design.md` defines the authoritative struct, and its decomposition differs deliberately: it splits `sweep` into scalar fields (`sweep_f_start_hz`, etc.), carries `coupling`/`positions_default`/`positions_domain`/`reposition_noun`/`flatness_target_db`, and derives `target`/`refusal` as `Decision`s rather than profile fields. Where the two field lists diverge, the decision-engine definition wins; the fields named in this spec's screens must resolve against it. Likewise the canonical `TransducerClass` variants are `{Bookshelf, Floorstander, InEar, OverEar}` (decision-engine / room-dsp) — this spec's `Headphone`/`Iem` labels are UI display names for `OverEar`/`InEar`, not distinct variants. **OPEN [DESIGN — blocks Stage 3]:** the enum's home is decided (`paraeq-dsp`, variants `{Bookshelf, Floorstander, InEar, OverEar}`), but the variant naming vs the wizard's `Headphone`/`Iem` display names, and the `PathProfile` field-set reconciliation, remain an owner call — see docs/plans/2026-07-16-rescope-implementation.md cross-spec question 3 and item 11.

**Bookshelf vs floorstander is one data-derived rule, not a branch.** The class label does exactly one thing before measurement: it sets a conservative `sweep.f_start` prior (30 Hz vs 20 Hz), because you cannot ask a woofer where its tuning is until you have measured it, and driving a 50 Hz-tuned bookshelf at 20 Hz is 6.2× its at-tuning excursion. (REW's half-start convention — a 20 Hz request sweeps from 10 Hz — is rejected outright for this reason: 25×, unloaded.) After measurement the class label is *discarded* for correction purposes: the low corner is the lowest frequency within ~10 dB of midband, derived from the same code for both, and below it the correction freezes to 0 dB and never boosts. The two classes yield ~50–60 Hz and ~30–35 Hz from one rule. Do not invent a difference the physics does not give.

**IEM vs over-ear is a profile.** Same coupler path, **same 84 dB SPL target** (the measurement-safety spec owns the SPL table and sets both coupler paths to 84 dB — it deliberately rejected the corpus's 94 dB IEM figure as running 10 dB hot for no SNR benefit), different target family (`harman_ie_2019` vs `harman_oe_2018`), and one extra `refusal_bands` entry: 7–11 kHz, where the "8 kHz peak" is a nozzle-to-membrane λ/2 artifact whose frequency tracks insertion depth and will sit somewhere else in the user's ear.

## What Differs Per Transducer Class

| | Headphone (over-ear) | IEM | Bookshelf | Floorstander |
|---|---|---|---|---|
| **Rig / setup** | Coupler jig (EARS, 711-class) or mic-in-coupler; headphone seated on the jig | IEM sealed into the coupler with the supplied tip | Mic on a stand at the listening position, tweeter height | Same |
| **Sweep f_start** | 20 Hz | 20 Hz | **30 Hz** (excursion prior) | **20 Hz** (excursion prior) |
| **Sweep level / SPL target** | −20 dBFS RMS / 84 dB | −20 dBFS RMS / 84 dB | −12 dBFS RMS / 75 dB | −12 dBFS RMS / 75 dB |
| **SPL warn / hard refuse** | >85 dB / **>100 dB** | >85 dB / **>100 dB** | >85 dB / **>90 dB** | >85 dB / **>90 dB** |
| **Sweep duration** | 5.5 s | 5.5 s | 5.5 s | 5.5 s |
| **Gating applicable?** | **No** — a coupler has no reflection problem to gate away | **No** | **Yes** — FDW, `n_c = 15` post | **Yes** — FDW, `n_c = 15` post |
| **Repeat practice** | **Re-seat variance**, N=5. You cannot spatially average a coupler; re-seating is the analogous perturbation and produces the same statistic | Re-seat (re-insert), N=5 | **Spatial averaging**, N=9 positions, ~30 cm apart | Spatial averaging, N=9 |
| **Compensation** | Mic/jig cal, mandatory. EARS: averaged over-ear compensation. **Never normalized** — EARS encodes a 2.1 dB L/R capsule offset *in the curve* | Mic/jig cal, mandatory | Mic cal (UMIK-1 serial-addressed, 0°) | Mic cal (0°; 90° file if the mic points at the ceiling) |
| **Target family** | `harman_oe_2018` (+ bass tilt −2/0/+4 dB) | `harman_ie_2019` (+ bass tilt) | Room family: RBJ low shelf (fc = 105 Hz, Q = 0.71) + linear tilt (default −0.9 dB/oct, range −0.0…−1.5) | Same room family |
| **Averaging** | dB domain, after 1/6-oct smoothing (the pinned-oracle path; REW-endorsed regime) | dB domain, after 1/6-oct smoothing | **Align SPL → power/RMS** | Align SPL → power/RMS |
| **Smoothing** | `Fixed(6)` | `Fixed(6)` | **REW variable**: 1/48 below 100 Hz → 1/6 at 1 kHz → 1/3 above 10 kHz | REW variable |
| **Correction limits** | Trinnov excursion envelope; **zero authority above 10 kHz**; boost-Q cap `Q_max = 0.227·f0/A`, `A = 10^(G/40)`; path Q ceiling 5.0 | As headphone, plus **no auto-fit of narrow features 7–11 kHz** | Trinnov: ±10 dB ≤150 Hz → taper → ±2 dB by 500 Hz → ±2 dB above; **cut freely, boost sparingly** (+6 dB individual, 0 dB net); path Q ceiling 10.0 @200 Hz → 3.0 @10 kHz; 0 dB below the derived low corner | Same |
| **Dominant failure mode** | **Lost seal** — an LF outlier across re-seats is a seal failure, not the headphone. Scrap and re-seat. HF scatter above ~6 kHz is expected and must be *averaged*, not rejected | **Insertion depth** — moves the 7–11 kHz artifact and the sub-bass seal; plus the highest acute SPL risk in the product | **Modal dips read as correctable.** They are destructive interference and are not. Plus: boosting below box tuning | **SBIR / floor bounce common to every position** — see the k = N trap below |

**The k = N trap, stated once.** Power averaging is null-*resistant*, not null-*immune*. A null at k of N positions asymptotes to `10·log10((N−k)/N)` only as depth → −∞ (within 0.01 dB by about −20 dB depth); at −6 dB depth with N=5, k=1 the value is −0.705 dB, not the −0.969 dB floor. And when **k = N** — a floor bounce or SBIR notch present at *every* position — the power average passes the null through **exactly**. σ(f) does not catch it either: σ is *low* there, because the feature really is present everywhere. So neither of the two headline guards protects against the single most common room null. The third guard is what does: **asymmetric picking — cut peaks freely, never fill a narrow dip** — backed by the per-region excess-group-delay mask, which is flat only where the region is minimum-phase. Ship all three; each covers a hole the others leave.

## The Two Front-Ends

### First run — the mode chooser

Shown once, on first launch, before anything else. Both buttons lead to the same engine.

```
┌─ Welcome to ParaEQ ──────────────────────────────────────────┐
│                                                              │
│  ParaEQ measures what you actually own — with the mic you    │
│  actually have — and corrects everything your Mac plays.     │
│                                                              │
│  How would you like to do this?                              │
│                                                              │
│  ┌────────────────────────────┐  ┌────────────────────────┐  │
│  │  ⚡ Just fix my sound       │  │  🎓 Walk me through it │  │
│  │                            │  │                        │  │
│  │  Two questions, then we    │  │  Same measurement,     │  │
│  │  decide the rest. Every    │  │  same decisions — but  │  │
│  │  choice stays visible and  │  │  we explain each one   │  │
│  │  changeable afterwards.    │  │  before we make it.    │  │
│  │                            │  │                        │  │
│  │  ~3 minutes                │  │  ~8 minutes            │  │
│  └────────────────────────────┘  └────────────────────────┘  │
│                                                              │
│  Either way you get the same result. You can switch any      │
│  time in Settings → Mode, including mid-measurement.         │
└──────────────────────────────────────────────────────────────┘
```

The last line is a load-bearing promise, not copy: because both modes render one `DecisionSet`, switching mode mid-session is a re-render, not a restart. Settings → Mode is a two-item radio; changing it re-renders the current wizard state in the other mode with the same `MeasurementBundle` and `DecisionSet` intact. Guided → auto mid-session is allowed and common (users get bored). Auto → guided mid-session is what the wizard *itself* offers when verification fails.

### "Just fix my sound" — auto

Screen 1 is the whole interview. Transducer class is the only thing not inferable. The mic is auto-selected when exactly one plausible input exists. The UMIK serial is asked only when no cal is cached — and then ParaEQ fetches the file from miniDSP's serial-addressed endpoint itself, so the user never sees a file picker.

```
┌─ ParaEQ ─────────────────────────────────────────────────────┐
│  What are you correcting?                                    │
│                                                              │
│    ( ) Headphones           (•) Bookshelf speakers           │
│    ( ) IEMs                 ( ) Floorstanders                │
│                                                              │
│  ──────────────────────────────────────────────────────────  │
│  Playing to   MacBook Pro Speakers → Audioengine A5+  ✓      │
│  Measuring with   Umik-1 Gain: 18dB   (48 kHz)        ✓      │
│  Serial   [ 7009115        ]   ✓ calibration downloaded      │
│                                                              │
│                        [ Fix my sound ▸ ]      Advanced ▾    │
└──────────────────────────────────────────────────────────────┘
```

Expanding **Advanced ▾** on this screen renders the same `DecisionSet` the run will use, before it runs:

```
┌─ Advanced ───────────────────────────────────────────────────┐
│  Every value below is what ParaEQ decided. Change any of      │
│  them; we'll tell you what it costs.                          │
│                                                               │
│  Positions            [ 9  ]  ▾   auto    ⟳ recapture         │
│  Sweep                20 Hz–20 kHz, 5.5 s, −12 dBFS RMS       │
│                       → 75 dB SPL at the mic     auto   ⟳     │
│  Windowing            FDW, 15 cycles (≈1/12 oct)  auto  ↻ 200ms│
│  Smoothing            REW variable (1/48 → 1/3)   auto  ↻     │
│  Averaging            Align SPL, then power       auto  ↻     │
│  Transition           measured from σ(f)          auto  ↻     │
│  Target               Room tilt −0.9 dB/oct + 105 Hz shelf    │
│                                                   auto  ↻     │
│  Max boost            +6 dB per filter, 0 dB net  auto  ⚡50ms │
│  Filters              as many as it takes to 3 dB auto  ⚡     │
│                                                               │
│  ⚡ redesign ~50 ms   ↻ reanalyze ~200 ms   ⟳ needs re-measure │
└───────────────────────────────────────────────────────────────┘
```

Three ideas are doing the work here. Each row shows `Decision<T>.value` and `Decision<T>.source` (`auto` / `path default` / **`you`** in bold once overridden). Each row's control is generated from `Decision<T>.domain`, so an out-of-range override is unrepresentable rather than validated. And each row carries its `invalidates` tier as a glyph, so the cost of a change is legible *before* the click — which is only truthful because raw IRs are cached.

Auto mode shows **no `rationale_key` strings**. The `(?)` affordances on the result screen pull them on demand.

### "Walk me through it" — guided

Same `DecisionSet`, one decision per screen, `rationale_key` rendered. Guided mode never introduces a decision auto mode does not make, and never makes a different one.

```
┌─ Step 4 of 9 — How loud? ────────────────────────────────────┐
│                                                              │
│  We played a quiet 300 Hz tone and listened through your      │
│  mic. Your system produces 75 dB at the mic when we send      │
│  −12 dBFS. That's our sweep level.                            │
│                                                              │
│      quiet ├──────────────────█──────────┤ too loud           │
│                        75 dB                                  │
│                                            cap: 90 dB         │
│                                                              │
│  Why not louder? A louder sweep buys signal-to-noise you      │
│  can get for free by measuring for longer instead — and it     │
│  is how tweeters die. If your room is noisy, we'd rather      │
│  you wait for the noise than turn this up.  (REW: "if input   │
│  levels are low DO NOT KEEP MAKING THE TEST SIGNAL LOUDER.")  │
│                                                              │
│  Noise floor  −41 dBFS  ✓ (we need better than −24)           │
│                                                              │
│                  [ ◂ Back ]   [ Continue ▸ ]   Change ▾       │
└──────────────────────────────────────────────────────────────┘
```

The `Change ▾` on a guided screen is the same control as the drawer row: it writes `source: UserOverride` into the same `Decision<T>`.

## Sweep-in-Progress UX

A sweep is loud, it is slow, and it sounds like a fault. This screen exists to convert "an alarm is going off" into "the machine is 60% done", and it is the screen the prototype gets most wrong: it emits progress 10, blocks for the full 5.0 s inside `sd.playrec`, then emits 50 (`measurement_wizard.py:66,74,81`) — the bar freezes at 10% for the entire siren, which is exactly when a novice reaches for the volume key.

Progress is driven from the capture callback's frame counter, pushed over the binary Tauri Channel already specified for the analyzer (~30 fps). The marker is the sweep's instantaneous frequency, closed-form and free:

```
f(t) = f_start · (f_end / f_start)^(t / T)
```

rendered on the same log-f axis as every other plot in the app, so the picture tracks what the ear hears.

```
┌─ Position 4 of 9 ────────────────────────────────────────────┐
│                                                              │
│  This is supposed to be loud.                                │
│  Don't touch the volume — it would invalidate the run.       │
│                                                              │
│   20      50     100      500      1k      5k       20k      │
│   ├────────────────────────▼─────────────────────────┤       │
│                                                              │
│   ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░░░░░   3.1 s left          │
│                                                              │
│   In  −18 dBFS  ▁▃▅▆▅▃▁      SNR 41 dB  ✓                    │
│   SPL 74 dB     ├───█──┤ 90                                  │
│                                                              │
│                                          [ Stop ]  (Esc)     │
└──────────────────────────────────────────────────────────────┘
```

- **"Don't touch the volume" is literally true for ParaEQ**, and that is worth knowing: the physical device stays the system default, so the volume keys act *downstream* of the tap and would silently break level calibration across positions. Every other product with a virtual device can only say this as a nicety.
- **The SPL readout is a meter with a target band, not a number** (Dirac's precedent: users hit a target, they do not read a figure). The number is present but secondary.
- **Abort** is `Esc`, `Space`, or `Stop`, and must take effect **within one callback block**, ramping the stimulus to zero over ~5 ms. A hard stop is itself a full-scale click, at ~20 kHz, into a sealed IEM — the abort path must not be the loudest thing in the session. The same ~5 ms ramp is the tail of every normal sweep (fade in ~10 ms / out ~50 ms).
- **Cancel is not failure.** Aborting drops the in-flight capture and returns to `Capture[i]` with `i` unchanged and all previously accepted captures intact.

## Error and Failure UX

Every failure returns a typed `Diagnostic { code, evidence, remedy_key, severity }`. `severity` maps 1:1 onto the decision engine's refusal classes. **Refuse** stops and offers a remedy; it is never overridable. **Warn** accepts, annotates the capture, and de-weights or excludes it from the fit. The wizard never fails the *session* for a per-capture problem — reject the capture, keep the cohort (Dirac's per-position clear + re-measure).

| Condition | Detection | Class | Screen |
|---|---|---|---|
| Mic not found / no plausible input | Probe finds zero inputs, or the selected input vanished | **Refuse** | "We can't find a microphone. ParaEQ needs one — it measures, it doesn't guess." + device list + "measure later, EQ by ear now" escape |
| No cal file, or cal unparseable | `parse_compensation` error, or no cached cal for the serial | **Refuse** | Never fall back to an uncapped sweep: no cal ⇒ no sensitivity ⇒ no SPL solve ⇒ no safety. Offer serial fetch, file picker, "flat cal (no SPL claim, capped at −30 dBFS)" |
| Cal outlier | Neighbour-outlier > 1.5–2 dB, non-monotonic frequency, non-finite value | **Refuse** (cal-level) | A *shipping* vendor file (`7005770_90deg.txt`) has a bogus `0.0000` at 19.611 Hz between neighbours of −3.13 and −3.11 — a 3.1 dB error inside the full-authority band. Offer "drop the outlier and interpolate" |
| Self-exclusion not in force | `EngineState.self_excluded == false` (see below) | **Refuse** | The uncorrected baseline would be silently corrected, and feedback is live. "Restart ParaEQ before measuring." |
| No signal | Recorded block RMS below the noise floor by <3 dB after the pilot | **Refuse** | "We played a tone and heard nothing. Is the mic pointed at the speaker? Is the speaker on?" Names the *device* it played to |
| Too quiet (noise floor) | Silence capture floor above −24 dBFS | **Refuse** | "Your room is too noisy right now." Explicitly refuses to solve it with output level; offers a longer sweep and "come back later" |
| Too quiet (SNR) | SNR < ~25 dB after **at most two** automatic ≤6 dB level rungs | **Refuse** | Never chase SNR with output level past two rungs |
| Clipping | >30% of samples in an input block clipped (REW's rule), or any sample ≥ −0.3 dBFS | **Refuse** capture | Auto-suggests −6 dB input gain; re-measures this position only |
| Projected SPL over cap | Solved `L = SPL_target − S` projects above the path cap **before a sample is emitted** | **Refuse** | Cap is non-overridable. Named to EN 50332 / IEC 62368-1 cl. 10.6 (85 dBA warn / 100 dBA max) |
| SPL deviates from projection | Measured SPL differs from projection by >6 dB mid-ladder | **Refuse** | Wrong device, broken cal, or the DUT is not on the coupler. Refuse, never escalate — escalating into an empty jig is precisely how someone gets hurt when they then put the IEM in |
| Wrong transducer / coupling failure | Solved chain sensitivity outside the path's expected envelope | **Refuse** | Catches an empty jig, headphones on a head, the sweep going to the laptop speakers |
| Excessive variance — coupler, LF | Per-position σ outlier below ~200 Hz | **Refuse** capture | "Bass is 9 dB below the other four. That's a seal problem, not the headphone." → re-seat |
| Excessive variance — coupler, HF | σ outlier above ~1 kHz | **Warn** | Expected placement scatter. Average it, do not reject it. Widens the confidence band, which is correct |
| Excessive variance — room | σ(f) high across the band, or a position σ-outlier broadband | **Warn** → **Refuse** at threshold | Broadband outlier = the mic moved during the sweep, or a door opened. Reject the capture. Persistently high σ ⇒ authority shrinks by itself; the wizard says so rather than failing |
| Fewer than 3 accepted positions | End of capture loop | **Refuse** | Cannot compute σ(f) from 2 points and mean anything by it |
| Harmonic distortion | Energy integrated in windows at `peak − T·ln(N)/ln(f_end/f_start)`, N = 2,3 | **Warn** | Integrate *energy*; never compare peaks. Annotates the result; at extreme levels suggests a lower sweep level |
| Device / rate changed mid-session | Property listener fires | **Refuse** session | `Recapture` tier. The bundle's `DeviceIdentity` no longer matches. Offer "start over with the new device" |
| Cancelled mid-sweep | User abort | **Neither** | Not an error. Drop the in-flight capture, keep the cohort, return to `Capture[i]` |
| Verification residual over threshold | `residual_vs_prediction` RMS > threshold over the authority band | **Refuse to ship silently** | The single most important refusal in the product — see below |

## Results Presentation

The one reaction to pre-empt is *"it removed my bass"*. It is the most common response to correct room correction, it is subjective, and the only honest way to answer a subjective claim is to let the user test it. **The A/B toggle is the headline control**, not a buried option.

Report **numbered events**, not a curve. The curve is available; the events are the summary.

```
┌─ Done ───────────────────────────────────────────────────────┐
│                                                              │
│              [   ON   ] ⇄ [  off  ]     ← try both           │
│                                                              │
│  What changed                                                │
│   • Tamed a +9.1 dB room mode at 47 Hz                       │
│   • Tamed a +6.4 dB mode at 112 Hz                           │
│   • Tamed a +4.0 dB mode at 156 Hz                           │
│   • Gentle tone shaping above 240 Hz                         │
│   • Turned everything down 6.2 dB to make room for the       │
│     boosts — that's normal, and it's why it may sound        │
│     quieter at the same volume setting.               (?)    │
│                                                              │
│  What we left alone                                          │
│   • The dip at 68 Hz. That's sound cancelling itself out,    │
│     not a missing note. EQ can't fix it — it would just      │
│     make the cancellation louder.                     (?)    │
│   • Everything above 240 Hz, except broad tone.       (?)    │
│                                                              │
│   dB                                                         │
│  +12│      ╭─╮                                               │
│    0│─╮ ╭──╯ ╰──╮   ░░▒▒▓▓▓▒▒░░░▒▒░░░▒▒▒░░░░░░░░             │
│  −12│ ╰─╯        ╰────────────────────────────────           │
│     └───┬────────┬───────┬────────┬────────┬─────            │
│        20       50      240      1k       10k    20k         │
│      ├─── full authority ───┼────── tone only ──────┤        │
│      │                      ▒ = how much your 9 mic  │        │
│      │                        positions disagreed    │        │
│                                                              │
│  ✓ Verified: we re-measured your speakers with the           │
│    correction running. It's doing what we designed,          │
│    to within 1.8 dB RMS across 20–240 Hz.            (?)      │
│                                                              │
│                          [ Save ]        Advanced ▾          │
└──────────────────────────────────────────────────────────────┘
```

**σ(f) is the shaded band, and it is the differentiator.** It costs one extra pass over data already held. It separates features that are correctable-everywhere (σ ≈ 0.6–0.8 dB) from position-dependent junk (σ ≈ 10 dB), and it rises naturally toward the diffuse-field asymptote of 5.57 dB above the Schroeder frequency — which means **confidence-derived authority reproduces the ~200 Hz rule without hardcoding it, and adapts to a treated room automatically**. A user with bass traps gets more authority than a user with a bare box, and neither is told a number someone else picked. No competitor automates this; REW's author describes it in prose.

The band is labelled in the user's language ("how much your 9 mic positions disagreed"), not as σ. Hovering a frequency reads out "at 68 Hz your positions disagreed by 11 dB — this is a place in the room, not a property of the speaker."

**The authority split is annotated spatially, on the frequency axis** — a ribbon under the plot, not a paragraph. This is the entire register decision: *"corrected 20–240 Hz; above that, tone only"* reads as engineering. The identical fact as prose reads as apology. Adopt Sonarworks' register (complementary division of labour — "treatment does this, we refine what remains") with REW's placement (in the product, at the moment of decision). Dirac's caveats live in a whitepaper nobody opens; that gap is architectural and it is ours to take.

Every `(?)` renders the corresponding `rationale_key` — the same strings guided mode shows inline. One `DecisionSet`, two renderings, one vocabulary.

**The preamp disclosure is mandatory**, not decorative. `peq.rs:112` currently emits a hardcoded `"Preamp: 0.0 dB"` literal; the moment modal boosts exist that is a clipping bug. The computed preamp must drive the engine gain stage, not only the export text, and the user must be told the number and why — because "it got quieter" is the second reaction after "it lost bass", and it has a one-line true answer.

## Closed-Loop Verification

Neither Dirac nor Sonarworks re-measures. Verification is what *earns* auto mode the right to hide everything, and it is nearly free: sweep, deconvolution, and comparison all exist.

### The hard part

The tap excludes ParaEQ's own process (`tap.rs:26-48`, `initStereoGlobalTapButExcludeProcesses`; `tap.rs:153-154`, `excluded = vec![own]`) **on purpose** — design spec line 147: *"correction state cannot contaminate the measurement."* That is correct and stays. But it means a naive re-measure plays the sweep on the direct stream, which the tap never sees, so it measures the **uncorrected** response. To measure the corrected output, the stimulus must come from a process that is **not** excluded.

**Do not pre-convolve the sweep with the active `CorrectionConfig`.** It defeats the design, and worse, it verifies the wrong thing: it measures the filter's *math* (which the golden fixtures already cover) while skipping the engine — the coefficient install, the preamp gain stage, the ±1.0 clamp, the DF2T state, the FIR overlap tail, the sample-rate binding. Those are exactly the things verification exists to catch.

### The design: two stimulus paths

```rust
pub enum MeasurementStimulus {
    /// In-process direct output stream. Tap-EXCLUDED, therefore UNPROCESSED.
    /// Measures the transducer + room. This is the baseline capture.
    Direct,
    /// Bundled helper child process (Contents/MacOS/paraeq-stimulus).
    /// A different PID => not in the exclusion list => tap-CAPTURED =>
    /// RealtimeChain-processed => played to the device. Measures the
    /// corrected end-to-end chain. This is the verification capture.
    Helper,
}
```

The repo precedent is `crates/paraeq-coreaudio/tests/test_hardware.rs:19-51`, which spawns `afplay` in a child process precisely because the tap aggregate's IOProc delivers **zero callbacks on an idle system** (measured: 0 cb/s idle, ~94 cb/s during playback) — a test that counts callbacks must drive playback from a process the tap can see. Verification has the identical requirement, for the identical reason. The helper is a small bundled executable rather than `afplay` itself so that we control the exact WAV, the fade, the abort, and the frame-accurate start marker.

Verification sequence, per position (auto mode runs it at **one** position — the primary listening seat / the last coupler seating):

1. Engine is `Running`, `bypass = false`, the designed correction installed, the computed preamp on the gain stage.
2. Spawn the helper with the same sweep WAV used for the baseline at that position.
3. Helper plays → tap captures → `RealtimeChain` corrects → engine output stream → device → mic.
4. Deconvolve against the same inverse sweep. Compare.

### The two residuals — and which one is honest

```
residual_vs_prediction(f) = measured_corrected(f)
                          − [ measured_uncorrected(f) + designed_correction(f) ]

residual_vs_target(f)     = measured_corrected(f) − target(f)
```

**`residual_vs_prediction` is rig-independent, and it is therefore the honest claim and the pass/fail gate.** Any static rig error — mic cal error, EARS's per-headphone frequency-response error, coupler load mismatch, mic placement — appears in `measured_corrected` and `measured_uncorrected` identically and **cancels in the difference**. What survives is an assertion about ParaEQ: *the filter we designed is the filter your system is actually applying, to within X dB.* That claim is true on an EARS jig where target claims are not, and it is a claim no competitor makes at all.

It catches, concretely: wrong coefficients installed; preamp applied to the export text but not the gain stage; clipping against the ±1.0 clamp; **stale coefficients after a rate change** (`build_correction(config, channels, block_size)` at `controller.rs:105-116` takes no sample rate, so a rate change reinstalls coefficients designed for the old one); the FIR overlap tail discarded on swap (`chain.rs:89` `mem::replace` starts the new processor from zeroed state, dropping up to 4095 samples); channel swap; per-channel coefficients collapsed to mono.

**`residual_vs_target` is rig-dependent.** Report it, plot it, never gate on it, and never let it become a target-compliance claim on a rig that cannot support one.

### When verification fails

`residual_vs_prediction` RMS over the authority band exceeding threshold means **auto mode refuses to ship silently**. It surfaces the failure and offers the guided path, with the residual curve as evidence. This is the specific mechanism by which "the app decides everything" stops being a promise about our confidence and starts being a claim we test.

### The fail-open hazard — a hard interlock

`tap.rs:154-159` documents a fail-open: if `translate_pid` returns 0 after a 200 ms retry, the exclusion list is **empty**, ParaEQ's own audio **is** tapped, and the code logs `"own process not in HAL registry after retry — no self-exclusion (watch for feedback)"`. In normal operation that is a defensible warning. **For the wizard it is fatal**, and silently so: a `Direct` baseline capture would be tapped, corrected, and re-played — so the "uncorrected" reference is silently corrected, every subsequent inference is garbage, and feedback is live into a coupler that may be on someone's head.

**Requirement:** `EngineState` gains `self_excluded: bool` (it currently carries `bypass, correction, gain_db, input_peak, latency_ms, status, stream` — `controller.rs:133-143`). The wizard **refuses** to begin any `Direct` capture when `self_excluded == false`. Remedy: restart ParaEQ.

### The fail-open watchdog — a second interlock

`EngineConfig::fail_open_after_ms` defaults to `Some(15_000)`: `NoInputDetected` persisting for 15 s auto-disables the engine (backend stopped, tap destroyed, device unmuted). The guard is narrow — it fires only *before the first nonzero input since start*, and post-`Running` states never fail open, so pausing music never disables the EQ. But the wizard hits the exact uncovered case: a user launches ParaEQ, plays nothing, and goes straight to Measure. `Direct` captures are tap-excluded by design, so the tap sees zeros; 15 s in, the controller tears the tap down **mid-session**, and the subsequent `Helper` verification capture has no engine to run through.

**Requirement:** the wizard acquires a `MeasurementLease` from the controller for the duration of a session. The lease suspends the fail-open watchdog and is released on **every** exit path including panic — the same invariant, and the same discipline, as tap teardown. The lease must not suppress the watchdog's *reporting*: a genuine TCC silent failure during a measurement session is still a `Refuse`, it just must not race the wizard to the teardown.

## Persistence

**Store raw per-position impulse responses, not derived curves.** This is the prerequisite for everything the Advanced drawer promises.

The prototype does the opposite and is worth naming precisely, because the defect looks like physics and is not. `measurement_wizard.py:483` appends `mag_db_smooth` — post-compensation, post-normalization, post-smoothing — to `_measurements_db`. `:446` overwrites `_current_ir` on every capture, and `:522` persists `{"impulse_responses": [self._current_ir]}` — exactly **one** IR regardless of N. So when the cal file changes, `:389-391` clears the entire measurement set. That discard is *not* required by the signal chain: compensation is applied at `:467`, **after** the FFT and before smoothing, and could be re-applied for free. The prototype throws away measurements because it did not keep the thing it needed. The Rust wizard must not inherit this.

```rust
pub struct MeasurementBundle {
    pub cal: Option<CalFile>,        // raw text + parsed curve + provenance
    pub captures: Vec<Capture>,      // ALL of them, accepted and rejected
    pub created: OffsetDateTime,
    pub device: DeviceIdentity,      // output UID, nominal rate, buffer frames
    pub mic: MicIdentity,            // input UID, model, serial, pinned input gain
    pub path: PathProfile,
    pub schema_version: u32,
}

pub struct Capture {
    pub accepted: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub index: usize,
    pub ir: Vec<f32>,                // per channel, WAV via hound
    pub peak_index: usize,           // t = 0. deconvolve() must return this
    pub sample_rate: u32,
    pub stored_window: StoredWindow, // { pre_ms, post_ms, taper }
    pub sweep: SweepParams,
}
```

**`deconvolve` must change shape.** It is `deconvolve(recorded, sweep, _sample_rate) -> Vec<f64>` today (`deconvolution.rs:6`) — it takes the sample rate and discards it, so there is no time axis at all. Every gating operation needs t = 0. It must return `ImpulseResponse { peak_index, sample_rate, samples }`.

**Storage bound.** The full deconvolved IR is `next_power_of_two(recorded.len() + sweep.len())` — 524288 samples for a 5.5 s sweep at 48 kHz, i.e. ~2 MB/channel, ~37 MB for a 9-position stereo bundle. Store a bounded window around the peak instead: `[peak − min(64 ms, peak_index), peak + 1500 ms]` with a Tukey α = 0.25 taper on the stored edges, recorded in `stored_window`. That is ~200 KB/channel, ~3.6 MB per bundle. The post-window figure is *derived from the drawer's domain, not picked*: the largest post-peak extent any in-scope decision can request is `n_c / f_min`, so a 1.5 s store bounds the FDW cycles domain at `n_c ≤ 20 × 1.5 = 30` — which comfortably contains the 15-cycle default and REW's usable range. The pre-window is not a choice: `deconvolve` puts the IR peak at only ~46–64 ms (tap latency + propagation), so there is nothing else to keep, and REW's 125 ms left window is physically impossible here. **OPEN [NEEDS DATA]:** the 1500 ms figure should be re-derived against real room IR decay before implementation; if a bass-heavy untreated room needs more, the drawer's `n_c` ceiling moves with it.

**Invalidation tiers** — this is the contract the drawer's glyphs render:

| Tier | Cost | Triggers | Work |
|---|---|---|---|
| `Redesign` | ~50 ms | max boost, net boost, filter count, flatness target, target tilt, target anchors, preamp policy, Q ceiling | Re-run autofit + design from the cached analyzed curve |
| `Reanalyze` | ~200 ms | cal file, FDW cycles, smoothing mode, averaging mode, transition frequency, authority envelope, align-SPL band, target family | Re-run gating → smoothing → averaging → σ(f) → authority from the cached raw IRs |
| `Recapture` | minutes | mic, output device, sample rate, sweep band, sweep level, N increase | Re-measure. Nothing else does |

Rejected captures are persisted too, with their `Diagnostic`s. They cost nothing, they make "why did you throw position 4 away?" answerable, and they let a user override a rejection after the fact (`Reanalyze`, not `Recapture`).

Format: one JSON manifest plus one WAV per capture per channel, in the app-data dir, per the existing profile-store decision (serde JSON + `hound`). `schema_version` is present from v1 — a bundle is the most expensive artifact a user owns, and the one thing we must never ask them to regenerate.

## Honest Limits Per Transducer Class

The mechanism that makes this honest with zero apologetic copy: **the product measures its own uncertainty and silently withholds authority where its data disagrees with itself.** σ(f) and the excess-group-delay mask do the work; the copy just reports what they did.

| Path | What we may claim | What we must never claim |
|---|---|---|
| **Bookshelf / floorstander** | Corrected modal peaks over the derived authority band, with reduced ringing **at the measured positions**; broad tone trend above. The filter we shipped is the filter running, to within the verified residual | Flat / accurate / reference above the transition. Removal of reflections, change in RT60, seat-to-seat bass fixes, directivity correction, correction of narrow dips, "works anywhere in the room", time or phase correction |
| **Headphone (711-class)** | Matched to the chosen target on *your* unit, below 10 kHz | Anything above 10 kHz. Any implied per-unit precision in 2–6 kHz beyond the measured re-seat spread |
| **Headphone (EARS)** | Matched to the chosen target with a **stated per-headphone uncertainty**, using an averaged over-ear compensation | Target compliance to a precision the rig cannot support; anything above 10 kHz |
| **IEM** | As headphone, plus per-unit seal characterisation from re-insertion spread | Any narrow feature 7–11 kHz. Sub-bass precision beyond the measured seal scatter |

### The four facts this table rests on, stated correctly

**1. Modal peaks are correctable; modal dips are not — and the reason is not a frequency.** An individual room mode is a pole pair and *is* minimum-phase, so a matched parametric filter corrects amplitude and phase together and genuinely reduces ringing at the measurement position. But the **measured in-room response is the sum of many modes plus direct sound**, and summing minimum-phase systems does not preserve minimum phase — real in-room responses are **mixed-phase throughout the modal region**. REW's own help is explicit: *"we cannot simply say a response is minimum phase below some specific cutoff"*, and it documents non-minimum-phase regions at **44–56 Hz — below the transition** — alongside minimum-phase regions at 300–500 Hz, above it. Toole (2015, JAES) attributes minimum-phase behaviour to loudspeaker **transducers**, never to rooms. So **"full authority below 200 Hz" is unsafe as a blanket rule** and must not appear in the product or the code. Authority is established **per region, by excess group delay** (flat ⇒ minimum phase ⇒ invertible), and the ~200 Hz figure appears only as a *fallback constant* when the measurement cannot support a derived one. Modal dips are destructive interference: non-minimum-phase, uncorrectable by EQ, and filling them burns headroom on a cancellation that stays cancelled.

**2. An omni mic approximates the pressure at the ear only below about 300 Hz — not 500 Hz.** At 500 Hz, ka ≈ 0.82 and the ipsilateral ear is already **+2.3 dB above free field**. The honest thresholds: 0.5 dB or better needs f below ~270 Hz; 1 dB or better needs f below ~340 Hz. Use **~300 Hz**. And the reason ILD is small at low frequency is *not* that the head is acoustically transparent — the head **diffracts strongly** at low frequency; that is precisely *why* the interaural level difference is small. Never write "the head does not diffract below 500 Hz" in copy or comments; it is false and it is the kind of false that gets quoted back.

The result screen's honest sentence is therefore: *"We corrected your bass. Above about 300 Hz we only followed the trend, because a microphone at your seat stops standing in for your ears, and reflections can't be EQ'd anyway."* Two independent reasons, both true, one sentence.

**3. The EARS critique is a linear error, and it does not forbid targets.** oratory1990's objection to the miniDSP EARS is a **linear, per-headphone-variable frequency-response error** — plus mic-capsule limits above 10 kHz and seal-related scatter in the sub-bass. It is **not** non-linear distortion, and he does **not** say target comparison is impossible: an averaged over-ear compensation gets approximately there. So the EARS row above claims target matching *with a stated uncertainty* rather than refusing targets outright. The uncertainty is the honest part; refusing would be over-correction in the other direction.

**4. Two independent limits land on 10 kHz for couplers.** The 711 coupler is specified ~100 Hz–10 kHz with a half-wave resonance at 13.5 kHz and does not simulate ear-canal geometry; independently, re-seat variance runs 10–15 dB above 10 kHz across studies, because above ~8 kHz the quarter-wavelength is shorter than the ear canal and standing waves make level placement-dependent. Coupler validity and re-seat repeatability agree. Correcting above 10 kHz is fitting noise on any rig, and `authority` is zero there for both coupler profiles.

### Surfacing limits without destroying confidence

- **Scoped competence, on the axis.** The ribbon, not the paragraph.
- **Division of labour, not concession** (Sonarworks' register): *"Room treatment fixes reflections. We fix what treatment can't reach — the modes."* Never *"unfortunately we cannot..."*.
- **At the moment of decision** (REW's placement), not in a whitepaper (Dirac's).
- **Limits that the data set, not that we assert.** "Above 240 Hz, tone only" lands very differently when 240 Hz is *visibly* where the user's own nine positions started disagreeing, and the band on the plot shows it. This is why σ(f) is on the headline screen and not in the drawer.
- **One knob, not one apology.** The bass tilt (−2 / 0 / +4 dB) turns "a target you may not prefer" from a caveat into a control. (The corpus cites Olive's clustering — roughly 64% preferring the Harman target, with most of the disagreement being bass level; the clustering figure is corpus-sourced and not independently re-verified here, so it stays out of user-facing copy. The knob does not depend on the number.)

## Prerequisites Outside This Spec

The wizard cannot be built on the current tree. These are owned elsewhere but are hard dependencies, listed so a plan can sequence them:

| Prerequisite | Current state (verified) | Owner |
|---|---|---|
| Cal parser rewrite to REW's rule (*"only lines which begin with a number are loaded, others are ignored"*) | `compensation.rs:11` dispatches on a leading `"`; `:60` hardcodes `.skip(2)`. UMIK-1 0° files have **one** header line, 90° files have two — so the parser silently drops the first data row on single-header files and is brittle to real-world header variation. The oracle (`prototype/paraeq/measurement/compensation.py`, `np.loadtxt(skiprows=2)`) **shares the flaw**; both must change together or you manufacture a divergence | room-dsp / defect sprint |
| `average_measurements_rms` + `align_spl` + σ(f) | `fr.rs:61` arithmetic-means dB. It has **zero non-test callers in Rust** — this is a *latent design decision for new room code*, not a live bug. dB averaging **stays** on the coupler path: the prototype smooths 1/6-oct *before* averaging (`measurement_wizard.py:482-483`), which is REW's endorsed regime (*"dB averaging may be useful when averaging smoothed traces to derive an EQ target; with unsmoothed data the dips would have a disproportionate effect"*) | room-dsp |
| FDW, gating, log-f axis, room targets | None exist. `paraeq-dsp` is exactly: `autofit, biquad, compensation, deconvolution, fir, fr, peq, spline, sweep, targets`. Note `fir.rs:85 windowed_ir()` **does** Hann-window a *designed* IR on both arms of `design_fir_correction` — what is missing is time-gating a **measured** IR | room-dsp |
| `TransducerClass` gating on targets | `match_closest_target` (`targets.rs:193-211`) iterates **every** curve with no category filter, despite `category: Option<String>` existing and being parsed at `:76`. All six bundled curves are coupler targets. `harman_oe_2018` is +8.3 dB at 3 kHz; a B&K room curve is −3.0 dB — an **11.3 dB** gap at the ear's most sensitive frequency, because that peak is ear gain a headphone bypasses and a loudspeaker delivers acoustically via the listener's own ear. Make the class a required argument, not a UI default | room-dsp |
| Boost ceiling + gain-dependent Q cap in autofit | `autofit.rs` picks bands by symmetric `residual[i].abs()`, clamps Q globally to `0.5..=20.0` (`:71`), has **no gain limit at all**, and hardcodes a `20.0..=20000.0` mask. `Q_max = 0.227·f0/A`, `A = 10^(G/40)` doubles as the runtime stability guard | defect sprint |
| Computed preamp on the **gain stage** | `peq.rs:112` emits the literal `"Preamp: 0.0 dB"`. Note AutoEQ's actual parametric preamp is exactly `−compound.max_gain` with **no** headroom (`frequency_response.py:211`); its `PREAMP_HEADROOM = 0.2` applies only to the GraphicEQ string and the FIR impulse responses, and the README's parametric example uses a hardcoded 0.1. Pick our convention deliberately and write down which | defect sprint |
| Sweep output-level policy | **Not a regression.** `SWEEP_AMPLITUDE = 0.5` lives at `measurement_wizard.py:39` — the *playback* layer, which is not ported (there is no sweep playback path in Rust; `generate_sweep`'s only caller is its own test). `sweep.rs` faithfully matches its oracle `prototype/paraeq/measurement/sweep.py`, which is also unscaled, and the peak-1.0 convention is deliberate (`noise.py`: *"normalized to a peak of 1.0 so the caller can apply any output amplitude"*; CLAUDE.md scopes `paraeq-dsp` to pure math). The prototype's actual sweep is −9 dBFS RMS: 3 dB **above** REW's −12 dBFS default, 6 dB **below** its −3 dBFS maximum. **The real issue is forward-looking**: the −6 dBFS output-level policy has no home in the Rust tree, and this wizard is where it must land — in the measurement runtime, RMS-referenced, with the fade and the abort ramp | this wizard / stage 6 |
| NaN sanitization at the capture boundary | `(f32::NAN).clamp(-1.0, 1.0)` is NaN — documented and intentional; infinity *is* clamped correctly. Do **not** "fix" the clamp. One NaN poisons DF2T feedback state permanently (`iir.rs:56-60`); the FIR path flushes after the tail. Lift CamillaDSP's `src/utils/conversions.rs:89-106` pattern — `if !value.is_finite() { invalid_values += 1; *value = 0.0; }` fused into the existing peak-detection pass (one branch per sample) plus a `warn!` with the count. A NaN-triggered `Correction::reset()` (`iir.rs:66`, realtime-safe) is reasonable belt-and-braces; input sanitization is the primary fix | defect sprint |
| `kAudioSubDeviceInputChannelsKey: 0` on the aggregate sub-device | `tap.rs:57-61` is a bare `{kAudioSubDeviceUIDKey}`. OnlyEQ's one-line fix, with the rationale in their comment: *"otherwise running our IOProc counts as microphone access and macOS shows a mic permission prompt."* This dissolves the known limitation at `backend.rs:121-130` **and** removes a spurious mic prompt — which matters disproportionately now, because a spurious prompt is indistinguishable from ParaEQ's *legitimate* measurement-mic prompt and would poison trust in exactly this wizard | defect sprint |
| Pin scipy in the prototype | `fixtures/manifest.json` is a **generated provenance record** — `generate_fixtures.py:235-239` writes `scipy.__version__` at runtime. The only dependency *declaration* is `prototype/pyproject.toml:16`, `scipy>=1.10`, a floor. Nothing enforces 1.18.0; regenerating under a newer scipy silently rewrites the manifest and no test notices. **This is a real latent defect** — pin it | defect sprint |

**On the oracle question, for the record:** the tiered strategy (frozen prototype fixtures / scipy-direct / analytic physics / REW characterisation) is right, but it is a **clarification, not a resolution of a contradiction**. CLAUDE.md's rules are not jointly unsatisfiable for room DSP, and the counterexample is already merged: `crates/paraeq-dsp/src/spline.rs` (commit `309d71d`) is a `paraeq-dsp` module with no fixtures directory, zero mentions in `generate_fixtures.py`, and no prototype addition — verified instead by analytic invariants (`passes_through_knots`, `n2_is_linear_and_n3_is_parabola`) plus inline pinned scipy values. "Fixtures are sacred" is a **provenance** rule whose own text sanctions editing the generator (*"regenerate and commit script + output together"*). Frame the tiers as documenting what the repo already does. Note also that `fir.rs:1-4` says *"Oracles:"* plural and lists `prototype/paraeq/correction/fir_filter.py` **first** — scipy is a transitive dep of the prototype there. The honest precedent is `spline.rs:1-5` (scipy's `CubicSpline` cited alone), and even that is not an independent third-party oracle, because the prototype uses `CubicSpline` too. The rule the repo actually follows is: **cite the library the prototype itself delegates to.**

## Open Questions

1. **Two-clock topology.** **DECIDED (method) [NEEDS DATA]:** adopt REW's bracketed-timing-marker skew estimate + resample, default on, with `Warn(TwoClock)` as the fallback when no estimate can be formed — see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q6. A UMIK-1 runs on its own fixed clock against the output device's, and the existing spec's *"the Farina method tolerates their small clock skew (prototype proved it)"* was proven only for an **ungated coupler magnitude** measurement; gating needs a trustworthy t = 0 and an undistorted IR shape, so that claim does not transfer. This was called the largest unscheduled cost in the rescope — it is now a port of a known technique with a known ~12 ppm magnitude, not open research. A shared clock stays rejected (it kills USB mics, our wedge against Sonarworks). The **[NEEDS DATA]** part: run measurement-suite/9 the moment the Stage-4 aggregate exists to confirm the magnitude on the owner's own EARS/UMIK rig and set the clock-adjust reject bound from it.
2. **Verification at N positions or 1?** **OPEN [DESIGN — blocks Stage 6]:** this is the verification-gate reconciliation (docs/plans/2026-07-16-rescope-implementation.md cross-spec question 5) — the gate definition (`residual_vs_target` vs `residual_vs_prediction`) and the helper packaging location must be settled as one. Auto mode runs one position (the primary seat); is a single-position verification a sufficient gate for a 9-position spatial average? `residual_vs_prediction` is rig-independent and largely position-independent (it tests the DSP chain, not the room), which argues for 1. But a position-dependent failure — e.g. a channel swap that only shows off-axis — would hide. Leaning: 1-position `residual_vs_prediction` in auto, all-N offered in guided.
3. **Stored window (1500 ms post).** **OPEN [NEEDS DATA]:** re-derive against real room-IR decay before implementation; it sets the drawer's `n_c` ceiling (§Persistence).
4. **Coupler smoothing default: `Fixed(6)` or `Fixed(12)`?** **DECIDED `Fixed(6)` [OWNER may revisit]:** `Fixed(6)` matches the pinned oracle and is what makes the coupler path's dB averaging sit inside REW's endorsed dB-averaging regime. The corpus's 1/12 + 6–8 kHz sigmoid-taper alternative is recorded, and lives on as a drawer domain value. Changing the default would need the averaging argument re-made.
5. **Preamp convention.** **DECIDED (2026-07-21):** AutoEQ-exact — `−max_gain`, no headroom constant, clamped ≤ 0 — per DIVERGENCES.md #14, landed in Stage 1 (the realized-cascade `ParametricEQ::preamp_db()`). AutoEQ parametric is `−max_gain` with no headroom; the 0.2 dB constant is GraphicEQ/FIR-only. The inter-sample-peak-safety alternative (carrying ~0.2–0.5 dB against the ±1.0 clamp) is recorded but not taken.
6. **Commercial posture.** **OPEN [OWNER]:** stay FOSS / non-commercial to retain the PLD Art. 2(2) exemption — a business call, recommended and unpacked in docs/decisions/2026-07-21-decision-engine-open-questions.md §Q8, needed before ship. PLD 2024/2853 (transposition 9 Dec 2026, inside this release's life) makes software a product; Art. 6(1)(a) covers personal injury; **Art. 14 voids contractual exclusions**, so the MIT "AS IS" disclaimer is inert against a personal-injury claim. The Art. 2(2) FOSS carve-out survives only while ParaEQ is supplied entirely outside commercial activity (recital 15 kills it for software supplied "in exchange for a price, or for personal data"). It is a wizard requirement because any paid tier attaches strict liability to the sweep level policy that no disclaimer can shed.
7. **Where the mode preference lives.** **OPEN [OWNER]:** app-global (Settings) vs per-profile; leaning app-global for v1, per-profile is additive. A user with a measured room and measured headphones may reasonably want guided for one and auto for the other.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| The room DSP the wizard renders does not exist (FDW, gating, σ(f), room targets, authority mask are all greenfield) | The wizard is spec'd against `decide()`'s output, not against DSP internals. Sequence: defect sprint → oracle tiers → room DSP → decision engine → wizard. The wizard is last and it is the cheapest of the five |
| Two-clock skew breaks gating (Open Q1) | Schedule the investigation **before** gating implementation. Fallback: coupler path ships gate-free (it needs no gate) and the room path degrades to ungated + spatially averaged with authority derived from σ(f) alone — a weaker but honest product, not a broken one |
| σ(f)-derived transition proves unstable on real rooms | It is the one novel piece with no shipping precedent. Fall back to the 200 Hz constant and keep the σ(f) curve as *evidence only* — the confidence band remains user-visible and the differentiator survives even if the derivation does not |
| `decide()` logic leaks into TypeScript once the drawer needs interactivity | The central promise dies the moment one default lives in TS. Enforce structurally: the drawer's controls are **generated from `Decision<T>.domain`**, so a TS-side default has nowhere to live. A golden-fixture test feeds a stored `MeasurementBundle` and asserts the whole `DecisionSet` |
| Self-exclusion fail-open silently corrupts the baseline | Hard interlock: `EngineState.self_excluded` + refuse. Specified above; requires an `EngineState` field addition |
| Fail-open watchdog destroys the tap mid-session | `MeasurementLease` suspending `fail_open_after_ms`, released on every exit path including panic |
| Spurious mic prompt indistinguishable from the legitimate one | `kAudioSubDeviceInputChannelsKey: 0` — one key, OnlyEQ's shipped fix. Ships before the wizard, not with it |
| Users read correct correction as "it removed my bass" | A/B as the **headline** control; the numbered-events list; the "you didn't lose bass, you lost the room's boom" line with a `(?)`; the preamp disclosure |
| Auto mode ships a bad correction silently | Closed-loop verification with `residual_vs_prediction` as a **refusal**, not a report. This is the mechanism that earns auto mode its silence |
| Competitive window | Dirac shipped Mac ART on 2026-06-30 ($499/$899, HAL driver + root LaunchDaemon, **no macOS uninstaller**). Sonarworks SoundID Reference (€249) measures and corrects system-wide on Mac but **refuses USB microphones** — killing UMIK-1 and EARS — demanding XLR + phantom + interface. OnlyEQ (Unlicense, 2026-07-03), iQualize (MIT, 2026-03) and eqtune ship ParaEQ's exact tap architecture; **none measure**. The tap is table stakes, not a moat; the fixture-tested measurement core and this wizard are the moat. Window before one bolts on a sweep: ~12–18 months. Do **not** ship coupler-only as an interim — it re-anchors us as "the EARS app" at the moment we are escaping that, and burns the window on the weakest claim we own |

## Out of Scope

Explicitly, and in writing, so a future session does not re-litigate:

- **Time alignment, subwoofer integration, crossovers.** Clean seams only: `PathProfile` is the extension point, `Capture.peak_index` gives a future time-alignment pass its t = 0, and the room path's per-channel API makes per-driver a data change rather than a rewrite.
- **Anything the Mac does not play.** The Mac is always the source. This is settled and it is what makes the whole tap architecture correct.
- **MMM (moving microphone measurement).** Incompatible with per-position σ(f), which is the differentiator.
- **Absolute SPL claims / a device-sensitivity database.** We solve chain sensitivity per session; we do not claim to know your headphone's dB/mW.
- **Vector (coherent) averaging.** Rejected *and guarded*: it collapses toward the incoherent floor `−10·log10(N)` once position spread approaches a wavelength (−10.94 dB at 1.5 kHz for ±40 cm). Multi-position data reaching a coherent averaging routine must return an error, not a number.
- **Mixed-phase / FIR phase correction.** The tap floor is ~41 ms measured; there is no latency budget for a Dirac-class modelling delay, and the phase claim would not survive §"Honest Limits" fact 1.
- **The X-curve.** Scoped to rooms >125 m³; its −3 dB/oct is 3× the domestic consensus.
- **Acoustic mic localization** (Sonarworks' approach). See Decisions Log.
- **Sonarworks' 37-position class of protocol.** N = 9 (Dirac's anchor).
- **Excess-group-delay masking may slip to v1.1.** It is the sharpest differentiator, but three cheaper guards cover the same failure (power averaging refuses nulls structurally, σ(f) flags position-dependent features, asymmetric picking never fills a dip). If it slips, the 200 Hz **fallback constant** is what ships — and it must be labelled a fallback in the code, the UI, and the log, never a rule.
- **CamillaDSP YAML export.** Worth days, makes us a drop-in REW replacement for the existing CamillaDSP base, and REW ≥5.20.14 already exports it natively — but it is post-room-path, and it is an escape hatch, never the position.

## Research Basis

The corpus is `scratchpad/rescope/{verdict,dsp-plan,auto-engine,wizard,safety,honest-limits,roadmap,build_items,corrections}.md` and `scratchpad/report/*`. Several corpus claims were adversarially refuted and this spec follows the corrections, not the corpus. The refutations that changed this document, so a future session does not "fix" it back:

| Corpus said | Correct |
|---|---|
| "Room resonances below the transition are minimum-phase, so correcting amplitude corrects phase" (verdict.md, honest-limits.md) | False as a blanket. A single mode is a pole pair and is minimum-phase; the **measured** response is a sum of modes plus direct sound, and summing minimum-phase systems does not preserve minimum phase. In-room responses are **mixed-phase throughout the modal region**. Authority is per-region via excess group delay, never from a frequency threshold |
| "An omni mic samples ≈ the pressure the ear receives below ~500 Hz"; "the head does not diffract below 500 Hz" | ~**300 Hz** (0.5 dB needs f < ~270; 1 dB needs f < ~340). At 500 Hz, ka ≈ 0.82 and the ipsilateral ear is +2.3 dB above free field. The head **diffracts strongly** at LF — that is *why* ILD is small |
| "dB and RMS averaging are identical for any feature present at all positions" (dsp-plan.md) | False. They agree iff magnitude is **identical** at every position (zero variance). Gap ≈ `0.1151·σ_dB²`, and RMS ≥ dB always (power-mean inequality) |
| "Power averaging is null-immune, returning exactly `10·log10((N−k)/N)`" (dsp-plan.md, roadmap.md) | That is the **asymptotic floor** as depth → −∞ (within 0.01 dB by ~−20 dB depth). At −6 dB depth (N=5, k=1) it is −0.705 dB, not −0.969. And for **k = N** the power average passes the null through **exactly**. Null-**resistant**, not null-immune |
| "−30 dB null at one of five → dB-avg −5.0 dB, power-avg −0.79 dB" | **−6.0 dB / −0.97 dB.** (−5.0/−0.79 is the six-position result.) A −40 dB null at one of three gives −13.3 vs −1.76 |
| "REW says dB averaging is inappropriate for unsmoothed data" | Overstated. REW: *"dB averaging may be useful when averaging smoothed traces to derive an EQ target, with unsmoothed data the dips would have a disproportionate effect."* ParaEQ smooths 1/6-oct **before** averaging — REW's endorsed regime. And `fr.rs::average_measurements` has **zero non-test callers**: the issue is **latent**, a design decision for new room code, not a live bug |
| "CLAUDE.md's rules are jointly unsatisfiable for room DSP" (roadmap.md, dsp-plan.md) | False. `spline.rs` (commit `309d71d`) is already a fixture-free `paraeq-dsp` module verified by analytic invariants + inline pinned scipy values. "Fixtures are sacred" is a **provenance** rule whose own text sanctions editing the generator. The tiers are a **clarification** |
| "`fir.rs` cites scipy directly as an oracle" | False — `fir.rs:1-4` says *"Oracles:"* plural, listing the prototype **first**. The honest precedent is `spline.rs:1-5`, and even that is not independent (the prototype uses `CubicSpline` too). The rule is: cite the library the prototype delegates to |
| "`fixtures/manifest.json` pins scipy 1.18.0" | It is a **generated provenance record** (`generate_fixtures.py:235-239` writes `scipy.__version__` at runtime). The only declaration is `pyproject.toml:16`, `scipy>=1.10` — a floor. **Real latent defect: pin it** |
| "`paraeq-dsp` contains no IR windowing" | `fir.rs:85 windowed_ir()` Hann-windows a **designed** IR, live on both arms of `design_fir_correction`. What is missing is time-gating a **measured** IR |
| "The Rust port dropped `SWEEP_AMPLITUDE = 0.5` / the sweep runs at REW's maximum" (safety.md, roadmap.md) | **Not a regression.** The 0.5 is at `measurement_wizard.py:39` — the **playback** layer, not ported. `sweep.rs` matches its oracle, which is also unscaled; peak-1.0 is deliberate. The prototype's actual sweep is −9 dBFS RMS = 3 dB **above** REW's default, 6 dB **below** its max. **The real issue is forward-looking**: the output-level policy has no home in the Rust tree, and this spec gives it one |
| "The 33.33% tweeter-band figure is REW's" | The one-third figure is exact (`ln10/ln1000 = 1/3`) but is **not** REW's: REW widens a requested range to half-start/twice-end capped at Nyquist, so its sweeps do not show this ratio. REW does warn that long sweeps risk tweeter overheating and are not recommended for loudspeakers |
| "AutoEQ preamp is `−max` with `PREAMP_HEADROOM = 0.2`" | Parametric preamp is exactly `−compound.max_gain`, **no headroom** (`frequency_response.py:211`). The 0.2 applies only to GraphicEQ and the FIR IRs; the README's parametric example hardcodes 0.1 |
| "ParaEQ cannot parse a real UMIK-1 file / 80% hard-error" (dsp-plan.md, honest-limits.md, roadmap.md) | **Contested** — verifiers disagreed on whether real UMIK-1 files ship quoted or bare, and the 80% figure is **not established**. Do not state a failure rate. What is solid: `compensation.rs:11` dispatches on a leading `"`, `:60` hardcodes `.skip(2)`, 0° files have one header line vs 90°'s two, so the parser is brittle to real header variation and silently drops the first data row on single-header files. Fix to REW's documented rule; the oracle shares the flaw and must change in lockstep |
| "Pre-convolve the verification sweep with the active correction" (wizard.md, auto-engine.md) | Defeats self-exclusion's purpose and verifies the filter's math, not the engine. Use a **helper child process** (`test_hardware.rs:19-51` afplay precedent) |
| "Fix the NaN at the clamp" | `clamp` returning NaN for NaN is **documented and intentional**; infinity is clamped correctly. Sanitize at the **capture boundary**, CamillaDSP `conversions.rs:89-106` pattern. (Note CamillaDSP's own live device paths pass `check_for_nan: false`, so it shares the exposure — but the pattern is in-tree, tested, and directly liftable) |
| "ParaEQ adds 41 ms" | **Over-claim.** Measured 46–62 ms with a ~41 ms fixed tap floor (fits `40.96 ms + 2.0·buffer_period`; 1966-sample floor invariant across a 4× buffer sweep). But `backend.rs:195` computes `out_sample_time − in_sample_time`, which **includes** the output device's own safety offset and DAC latency that exist with or without ParaEQ. The added latency is not established. Competitors' "~10 ms" claims are arithmetic (OnlyEQ's `estimatedLatency = ioBufferFrames*2/sampleRate`), not measurement; iQualize **removed** its "Low Latency" toggle because ring capacity "did not meaningfully reduce latency" |

Verified independently against the tree for this spec: `compensation.rs:11,60`; `fr.rs:61`; `autofit.rs:26,71`; `peq.rs:112`; `targets.rs:76,193-211`; `deconvolution.rs:6`; `sweep.rs:6-16`; `tap.rs:26-48,57-61,153-159`; `chain.rs:89`; `controller.rs:105-116,133-143,161,174-180`; `test_hardware.rs:19-51,145-175`; `measurement_wizard.py:39,59-94,389-391,446,467,482-483,522`.
