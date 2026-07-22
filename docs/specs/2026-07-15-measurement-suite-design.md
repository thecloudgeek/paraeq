# ParaEQ Measurement Suite — Design Spec

**Date:** 2026-07-15
**Status:** Draft — rescope *direction* approved by the project owner (2026-07-15); this document is pending owner review
**Amends:** `docs/specs/2026-07-02-rust-port-design.md` — the crate map, the tap
architecture, the realtime invariants and the packaging plan all survive intact;
the *milestone* ("full functionality parity with the prototype") does not. See
**Spec Delta** for the clause-by-clause list of what is void, what is stale, and
what is unchanged.
**Companions:** this is the master spec and the entry point. Five companion specs
carry the depth; each is summarized in one line under **Companion Specs** and
referenced by filename throughout. Do not duplicate their content here.

## Summary

ParaEQ stops being "a headphone correction EQ built around a miniDSP EARS jig"
and becomes a **general measurement and correction suite for macOS**: you measure
what you actually own — headphones, IEMs, bookshelf speakers, floorstanders —
with the microphone you actually have (UMIK-1, EARS, or any mic with a
calibration file), and ParaEQ corrects everything your Mac plays, without
installing a driver, without surrendering your default output device, and without
hiding a single decision it made. The Core Audio process-tap architecture is
**unchanged and correct**: the Mac is always the source, the physical device stays
the system default, and nothing the Mac does not play is in scope. Room support
ships at a deliberately bounded depth — **frequency-dependent windowing plus
spatially-averaged magnitude correction, with no time alignment, no subwoofer
integration and no crossovers** — with clean seams left for all three. Two
front-ends sit over one engine and are chosen at first run: *"just fix my sound"*
(auto; the app decides everything, and every decision is visible and overridable
in an Advanced drawer) and *"walk me through it"* (guided; the same decisions, one
screen at a time, with the reason attached). The two front-ends are renderers over
a single pure Rust function; they cannot fork because there is no second set of
defaults to fork into.

The work is real. `paraeq-dsp` is ten modules totalling ~1,145 lines of pure math
(1,172 including the crate root `lib.rs`) with no gating, no FDW, no variance, no
RT60/Schroeder, and no room targets; the room
half is greenfield, and its oracle is the thing that gates months of it. That
question is settled here, in **The Oracle and Testing Methodology**, before any
room module is written.

## Companion Specs

| Spec | Covers |
|---|---|
| `docs/specs/2026-07-15-room-dsp-design.md` | The `paraeq-dsp` room core: log-f grid, windowing, gating, FDW, splice, RMS/power averaging + σ(f), Schroeder/RT60, variable smoothing, `TransducerClass` targets, the authority envelope, and the compensation-parser rewrite. **Owns `authority.rs` — the σ(f) thresholds and Trinnov excursion curve are canonical there.** |
| `docs/specs/2026-07-15-decision-engine-design.md` | `decide(&MeasurementBundle) -> DecisionSet` in a **new crate `paraeq-decide`** (depends on `paraeq-dsp` only): the typed decision table, `Decision<T>` with value/domain/source/evidence/invalidation tier, and the refusal checks that earn auto mode the right to stay silent. (The `PathProfile` is derived inside from `bundle.class`, not passed as a second argument.) |
| `docs/specs/2026-07-15-wizard-design.md` | The one wizard spine parameterized by `PathProfile`, the two front-ends as renderers, the question budget, sweep-in-progress UX, per-position failure states, the honest result screen, **and the per-rig / per-band claims-and-limits (what each path may claim and must never claim, σ(f)-as-confidence, scoped competence rather than apology)** — the claims material lives in that spec's *Honest Limits Per Transducer Class* section and in this master's *Positioning*, not in a separate file. |
| `docs/specs/2026-07-15-measurement-safety-design.md` | The output-level policy, in a **new crate `paraeq-measure`**: noise floor → pilot → closed-loop level solve → escalate → sweep; per-transducer SPL caps and targets (referenced to EN 50332 / IEC 62368-1 — clause numbers flagged unverified there); refusal conditions; fade; abort; the NaN guard; and the commercial-posture decision that PLD 2024/2853 forces. **Owns the SPL targets/caps table.** |
| `docs/specs/2026-07-15-engine-hardening-design.md` | The R1 safety floor: ten verified defects in existing engine/DSP code (auto-preamp, NaN sanitization, runtime Jury stability guard, autofit boost caps, `kAudioSubDeviceInputChannelsKey: 0`, sample-rate coefficient staleness, filter-state-preserving swap, meters/clip counter, and the deferred denormals + convolver-partitioning items). |

## Decisions Log

Settled with the project owner during the rescope on 2026-07-15. Rows marked
**(amends 2026-07-02)** replace a row in the prior spec's Decisions Log.

| Decision | Choice | Alternatives rejected |
|---|---|---|
| Product scope **(amends 2026-07-02, "Parity scope")** | A general measurement + correction suite: any transducer, any calibrated mic. Parity with the prototype is no longer the milestone. | Ship prototype parity first, rescope after (re-anchors ParaEQ as "the EARS app" at the exact moment it is escaping that framing, and burns the competitive window on its weakest claim); rescope to rooms only |
| Measurement device | **Device-agnostic**: any mic with a calibration file. EARS is one supported jig; UMIK-1 is the most common and must work on day one. | EARS-only (the current framing — and the one path with no scientific defence: an impedance-mismatched jig cannot be compared to a fixed target); UMIK-1-only; a vendor whitelist |
| Transducer coverage | **All four in one release** — headphones, IEMs, bookshelf, floorstanding — via one wizard spine parameterized by a `PathProfile` that branches to a **coupler path** or a **room path**. | Coupler-only interim release (see above); rooms as a v2; four separate wizards (guarantees divergence within one release) |
| Room depth | **Gated (FDW) + spatially averaged magnitude correction.** NO time alignment, NO subwoofer integration, NO crossovers — with clean seams left for each. | Full Dirac-class room correction (mixed-phase, time alignment, sub integration) — months of work and a latency budget ParaEQ does not have against a ~41 ms tap floor; magnitude-only ungated (blind to the reflection problem it exists to solve) |
| Gating mechanism | **Frequency-dependent window (FDW)**, width in cycles, on the complex spectrum. One parameter delivers gating, variable resolution and Schroeder-awareness together. | A single fixed gate — self-contradictory: a 3–6 ms gate is blind below 167–333 Hz, i.e. blind to the modal region the gate exists to protect; a two-window pipeline + splice (kept as the documented fallback, since FDW subsumes it) |
| Spatial averaging operator | **Align SPL first (mandatory), then RMS/power averaging**, for the room path only. σ(f) emitted alongside the mean. | dB-domain averaging for rooms (retained for the coupler path, fixture-pinned — see Spec Delta on line 104); vector/coherent averaging (rejected *and guarded*: it collapses toward the incoherent floor −10·log₁₀(N) once position spread approaches a wavelength — −10.94 dB at 1.5 kHz for ±40 cm) |
| EQ authority | Established **per region** from measured evidence: excess group delay (flat ⇒ locally minimum-phase ⇒ invertible) and inter-position σ(f). EGD masking ships in v1.1; v1 uses σ(f) + RMS averaging + asymmetric cut/boost as three cheaper guards covering the same failure. | "Full authority below ~200 Hz" as a blanket rule — **unsafe**; REW documents non-minimum-phase regions at 44–56 Hz (below transition) and minimum-phase regions at 300–500 Hz (above it) |
| Front-ends | **Two front-ends over one engine**, chosen at first run and switchable later. Both are pure renderers over one `DecisionSet`. | One "pro" front-end with an easy preset (the preset is a lie the moment a default lives only in the pro path); policy in TypeScript (strings are neither overridable nor testable, and the drawer becomes a second, lying source of truth) |
| Decision engine home | **Rust, a new crate `paraeq-decide`** depending on `paraeq-dsp` only: one pure `decide(&MeasurementBundle) -> DecisionSet` returning typed decisions. Fixture-testable, daemon-ready, and it keeps `paraeq-dsp` pure math. Resolved in `docs/specs/2026-07-15-decision-engine-design.md`. | A `decide.rs` module inside `paraeq-dsp` (policy is not pure math; drags target/cal domain types into the math crate); TypeScript policy with Rust emitting rationale strings; policy split across both |
| Signal path | **Unchanged**: Mac is always the source; muted global process tap on a private aggregate; physical device stays system default. | Cross-device redirect; capture of non-Mac sources; a HAL driver (rejected 2026-07-02 and still rejected — see Positioning) |
| Closed-loop verification | **Yes, and it is what earns auto mode its silence.** The stimulus plays from a **helper child process** so it is not covered by the tap's self-exclusion. | Pre-convolving the sweep with the active correction — **inverts the design**: self-exclusion exists *so that* measurement sweeps play unprocessed (2026-07-02 spec line 147: "correction state cannot contaminate the measurement"); shipping without verification (Dirac and Sonarworks both terminate at "here is a correction, trust us") |
| Oracle methodology | **Four tiers**, documented before any room code: frozen prototype fixtures / scipy-direct fixtures / analytic-physics invariants / REW characterization at 0.1–0.5 dB regression-only. Framed as a **clarification** of CLAUDE.md, not an amendment resolving a contradiction. | "Write the room DSP in Python first" (fake provenance: new Python written the same afternoon for the same purpose is not an independent oracle — it would launder a design bug into a golden fixture); skip fixtures for room modules (leaves the highest-risk new code untested) |
| Export to CamillaDSP | **Deferred** until after the room path ships. Emit YAML; vendor nothing; reimplement `is_stable`, Tilt and the graphic-EQ layout from their published formulas. | Ship it now; adopt CamillaDSP as the execution engine ("CamillaDSP executes, ParaEQ measures" surrenders the only real advantage — and on macOS CamillaDSP requires BlackHole as system default, breaking the volume keys the tap architecture exists to preserve, and hard-stops on sample-rate change); never export |

## Positioning

**The claim we ship:**

> ParaEQ measures the speakers or headphones you actually own, with the mic you
> actually have, and corrects everything your Mac plays — without installing a
> driver, surrendering your default output device, or hiding a single decision it
> made.

**The subordinate line, and the truer one:** *ParaEQ is the correction engine REW
has always told you to go find.* Not "the only free one" — the only one that is a
single app.

Every clause is defensible against the incumbents' own documentation. Dirac
Live Processor requires a HAL plugin plus a root LaunchDaemon and ships **no macOS
uninstaller**. Sonarworks' driver "will always revert itself as the default output
device," and it **structurally refuses USB microphones** — demanding XLR, phantom
power and an interface — which kills both the UMIK-1 and the EARS. Rogue Amoeba
independently validated the tap direction by deprecating their own ACE driver
engine for taps.

### What we may claim

| Path | Honest claim | Mandatory disclaimer |
|---|---|---|
| Bookshelf / floorstanding | Corrected modal region (typically 20 Hz–Schroeder) with reduced ringing at the measured seat; broad tone trend above | Cannot remove reflections, change RT60, fix seat-to-seat bass variation, or correct directivity. Above transition: tone only. Valid near the measured positions. |
| Headphone (711-class rig) | Matched to a target ~64% of listeners prefer, on *your* unit | No claim above 10 kHz. Re-seat moves 2–6 kHz by several dB, >10 kHz by 10–15 dB. |
| Headphone (EARS) | **Relative correction only** | No target-compliance claim at all — the impedance mismatch invalidates fixed targets. |
| IEM | As headphone, plus per-unit seal characterisation | Never auto-correct narrow features 7–11 kHz: the "8 kHz peak" is a nozzle-to-membrane λ/2 artifact whose frequency tracks insertion depth. |

Bookshelf vs. floorstanding is **not** a claims distinction. It is one
data-derived low corner (the lowest frequency within ~10 dB of midband, yielding
~50–60 Hz and ~30–35 Hz from the same code), below which we freeze to 0 dB and
never boost.

### What we must never claim

Flat / accurate / reference sound above the transition frequency. Any target
compliance on EARS. Anything above 10 kHz for couplers (two independent limits —
711 coupler validity and re-seat repeatability — land on the same number).
Reflection, reverberation, directivity, or seat-to-seat correction. Correction of
narrow dips. "Works anywhere in the room." Time or phase correction. And two
specific overclaims that the research corpus itself got wrong and that must not
resurface in ParaEQ's copy:

- **Never** write "room resonances below the transition frequency are
  minimum-phase, so correcting amplitude corrects phase." An individual mode is a
  pole pair and *is* minimum-phase, but the measured in-room response is the sum
  of many modes plus direct sound, and summing minimum-phase systems does not
  preserve minimum phase. Real in-room responses are **mixed-phase throughout the
  modal region**. REW's own help: "we cannot simply say a response is minimum
  phase below some specific cutoff." Toole's 2015 JAES paper attributes
  minimum-phase behaviour to loudspeaker **transducers**, never to rooms. What
  survives, and what we may say: *modal peaks are poles, locally minimum-phase,
  and a matched parametric filter corrects amplitude and phase together and
  reduces ringing at the measurement position.* Modal dips are destructive
  interference, non-minimum-phase, and uncorrectable by EQ.
- **Never** write "an omni mic at the listening position samples approximately the
  pressure the ear receives below ~500 Hz." At 500 Hz *ka* ≈ 0.82 and the
  ipsilateral ear sits **+2.3 dB above free field**. The honest thresholds are
  ≤0.5 dB below ~270 Hz and ≤1 dB below ~340 Hz — so the number is **~300 Hz, not
  500 Hz**. And do not write "the head does not diffract below 500 Hz": the head
  diffracts *strongly* at low frequency, and that is precisely *why* the
  interaural level difference is small.

### The competitive clock

The tap is **not a moat**. OnlyEQ (Unlicense, 2026-07-03), iQualize (MIT, March
2026) and eqtune all ship ParaEQ's exact realtime architecture — muted global tap,
private aggregate, single IOProc, atomic coefficient swap. Two independent
developers reached it in four months. **None of them measures.** Sonarworks
SoundID Reference (€249) measures and corrects system-wide on Mac but refuses USB
mics. Dirac shipped Mac ART on **2026-06-30** at $499/$899, two weeks before this
decision.

So the empty cell is exactly: **measure + correct + system-wide + no driver + any
mic**. The only durable differentiator is the fixture-tested measurement and
design core plus the unified wizard. The window before one of the OSS tap EQs
bolts on a sweep is roughly **12–18 months**. That number is the schedule
constraint behind the staging below, and it is why "ship coupler-only as an
interim" is rejected in the Decisions Log rather than merely disfavoured.

## Architecture

The rescope adds four things, all new workspace members or additive modules: a
**room DSP layer** in `paraeq-dsp` (new modules), a **decision engine** in a new
crate **`paraeq-decide`** (depends on `paraeq-dsp` only — see the decision-engine
spec), a **measurement runtime** in a new crate **`paraeq-measure`** (level solve,
capture orchestration; no Tauri, no CoreAudio FFI — see the measurement-safety
spec), and the thin **Tauri/UI wiring** plus the **`paraeq-stimulus`** helper
binary in `desktop/`. It changes nothing below the `AudioSource` seam.

```
Every app on the Mac
      │  plays audio normally to the system default output
      ▼
Core Audio process tap  (macOS 14.4+, MutedWhenTapped, private)
      │  excludes ParaEQ's own process  ──────────────┐
      ▼                                               │  (see Closed-loop
┌─ ParaEQ.app — one Tauri process, Cargo workspace ─┐ │   verification below)
│                                                   │ │
│  crates/paraeq-coreaudio    UNCHANGED             │ │
│    tap lifecycle, private aggregate, ONE IOProc,  │ │
│    device/format listeners, teardown ordering.    │ │
│    The ONLY crate with unsafe CoreAudio FFI.      │ │
│    NEW: kAudioSubDeviceInputChannelsKey: 0 on the │ │
│         sub-device dict; NaN sanitization at the  │ │
│         capture boundary; input-device open for   │ │
│         the measurement mic.                      │ │
│                                                   │ │
│  crates/paraeq-engine       UNCHANGED shape       │ │
│    RtProcessor / RealtimeChain: bypass → correc-  │ │
│    tion → gain → clamp(±1.0). rtrb SPSC rings     │ │
│    inbound, ArcSwap EngineState outbound.         │ │
│    No Tauri deps. No locks/alloc on the RT lane.  │ │
│    NEW: sample-rate-aware build_correction;       │ │
│         runtime Jury stability guard; state-      │ │
│         preserving coefficient swap.              │ │
│                                                   │ │
│  crates/paraeq-dsp          pure math, no plat-   │ │
│    form deps. TEN MODULES TODAY:                  │ │
│      autofit biquad compensation deconvolution    │ │
│      fir fr peq spline sweep targets              │ │
│    ── NEW (room core) ────────────────────────    │ │
│      logf     log-f grid; gates everything below  │ │
│      window   Rect/Hann/Tukey/Blackman-Harris     │ │
│      gating   peak detect, GateSpec, min_valid_f  │ │
│      fdw      constant-Q Gaussian on COMPLEX spec │ │
│      splice   Klippel AN39 (FDW fallback only)    │ │
│      room     T60 (Schroeder), f_s, transition    │ │
│      authority  boost/cut ceiling + Q cap, σ(f)   │ │
│    (paraeq-dsp stays PURE MATH — no policy here)  │ │
│                                                   │ │
│  crates/paraeq-decide       NEW crate (policy)    │ │
│    depends on paraeq-dsp only; no Tauri/CoreAudio.│ │
│      decide   pure fn: MeasurementBundle          │ │
│                      -> DecisionSet               │ │
│                                                   │ │
│  crates/paraeq-measure      NEW crate (runtime)   │ │
│    level solve, capture orchestration, per-pos IR │ │
│    store; no Tauri, no CoreAudio FFI (trait sinks)│ │
│                                                   │ │
│  desktop/src-tauri          NEW: Tauri wiring —    │ │
│    profile store, cal fetch, AutoEQ client, and   │ │
│    the IPC over paraeq-decide + paraeq-measure.    │ │
│    Spawns:                                         │ │
│      paraeq-stimulus (helper child binary) ───────┼─┘  NOT excluded by
│                                                   │    the tap → its audio
│  desktop/ui                 two front-ends, one   │    IS captured and
│    DecisionSet, one FrequencyPlot                 │    processed
└───────────────────────────────────────────────────┘
      ▼
Physical output device — stays system default (volume keys keep working)
      ▼
Measurement mic (UMIK-1 / EARS / any calibrated mic) — SECOND clock domain
```

### The tap architecture is unchanged, and why

Nothing in the rescope touches it. The Mac is always the source; ParaEQ is a
system-mix EQ, not a router. The tap captures the system default output, mutes it
at the device, and ParaEQ plays the processed mix back to that same device — one
clock domain on the playback side, no drift, no resampler. Every reason the taps
were chosen on 2026-07-02 still holds and none of the rescope's new requirements
argues against them. The realtime invariants — no locks, no allocation, and every
exit path running the full teardown sequence ending in tap destruction — are
unchanged spec constraints.

Two `paraeq-coreaudio` changes are additive and both are worth calling out here
because they interact with the rescope:

- **`kAudioSubDeviceInputChannelsKey: 0`** on the aggregate's sub-device
  dictionary. ParaEQ's sub-device dict (`tap.rs:57-61`) is today a bare
  `{kAudioSubDeviceUIDKey}`. OnlyEQ sets this key with the rationale in a comment:
  "otherwise running our IOProc counts as microphone access and macOS shows a mic
  permission prompt." One key dissolves the KNOWN LIMITATION documented at
  `backend.rs:121-130` (a mic-capable default output could contribute buffers
  ahead of the tap stream) **and** removes a spurious mic prompt — which now
  matters far more than it did, because a spurious prompt is indistinguishable
  from ParaEQ's *legitimate* measurement-mic prompt and would poison trust in the
  wizard at its most fragile moment.
- **NaN sanitization at the capture boundary.** Do **not** try to fix this at the
  clamp: `f32::clamp` returning NaN for NaN is documented and intentional
  (verified: `(f32::NAN).clamp(-1.0, 1.0).is_nan() == true`; infinity *is* clamped
  correctly). One NaN poisons DF2T feedback state (`iir.rs:56-60`) permanently;
  the FIR path flushes after the tail instead. The fix is CamillaDSP's
  `src/utils/conversions.rs:89-106` pattern, lifted into `paraeq-coreaudio`'s
  capture path: `if !value.is_finite() { invalid_values += 1; *value = 0.0; }`,
  fused into the existing peak-detection pass (one branch per sample on a loop
  already being walked), plus a `warn!` with the count. CamillaDSP gates this
  per-format because integer formats cannot encode NaN; taps deliver float, so
  ParaEQ is in the exposed class. Note honestly that CamillaDSP's own live device
  paths pass `check_for_nan: false` and therefore share the exposure — the pattern
  is worth copying because it is present, tested in-tree and directly liftable,
  not because CamillaDSP is protected. A NaN-triggered `Correction::reset()` is
  reasonable belt-and-braces (`iir.rs:66` exists and is realtime-safe), but input
  sanitization is the primary fix.

### The closed-loop verification problem, and its resolution

`tap.rs:26-48` calls `CATapDescription::initStereoGlobalTapButExcludeProcesses`
and `tap.rs:153-154` builds the exclusion list as `vec![own]`. This is
**deliberate and correct**: it exists so that a measurement sweep played by ParaEQ
reaches the output device unprocessed and unmuted, which is what makes the
measurement an uncontaminated measurement of the raw transducer (2026-07-02 spec
line 41: "prevents feedback, and lets measurement sweeps play cleanly"; line 147:
"correction state cannot contaminate the measurement").

But it creates a genuine problem for the feature the rescope depends on. To
measure the *corrected* output you need the stimulus to go **through** the engine —
which is exactly what self-exclusion prevents. Two resolutions were considered:

- **Rejected: pre-convolve the sweep with the active `CorrectionConfig` and play
  it on the direct stream.** This defeats the design. It also does not measure the
  thing you want measured — it measures a simulation of the filter, not the filter
  the realtime chain actually runs, so it cannot catch the class of bug (stale
  coefficients on rate change, filter-count truncation, a swap that dropped an
  overlap tail) that verification exists to catch.
- **Adopted: play the stimulus from a helper child process.** A separate
  short-lived binary (`paraeq-stimulus`) has its own pid and its own HAL process
  object, so it is not in the exclusion list; its audio is tapped, processed by
  `RealtimeChain`, and emitted through the real correction. The repo precedent is
  `crates/paraeq-coreaudio/tests/test_hardware.rs`, which already drives the tap
  with an `afplay` child process for exactly this reason. The *measurement* sweep
  keeps playing from ParaEQ's own process (excluded → uncorrected); the
  *verification* sweep plays from the helper (not excluded → corrected). Same
  sweep, same deconvolution, two processes, and the difference between them is the
  measured correction.

One caveat must be carried into the implementation plan: self-exclusion has a
**documented fail-open fallback**. If `translate_pid` returns 0 after a 200 ms
retry the exclusion list is empty and ParaEQ's own audio *is* tapped
(`tap.rs:154-159`, logging "watch for feedback"). On that degraded path the
measurement sweep would be processed. The measurement runtime must read the
exclusion state from the engine and **refuse to measure** rather than silently
produce a correction-contaminated capture.

### The second clock

The measurement mic is a second clock domain: a UMIK-1 runs at its own fixed rate
against the output device's rate. This is the biggest unscheduled cost in the
rescope and it is tracked as such in **Risks**. The 2026-07-02 spec's claim that
"the Farina method tolerates their small clock skew (prototype proved it)" was
proven for an **ungated coupler magnitude** measurement. Gating needs a
trustworthy *t = 0* and an undistorted IR shape. **That claim does not transfer.**

## The Oracle and Testing Methodology

This section gates months of work. It is the first thing to land (stage R0) and
nothing in the room core may be written before it is written down.

### The problem, stated accurately

The Python prototype contains **zero room DSP**. `generate_fixtures.py` imports
eleven of the seventeen prototype modules (never: `measurement/noise.py`,
`correction/autoeq_db.py`, `audio/*`, `profiles/profile.py`), and none of the
eleven gates, windows a measured IR, integrates a Schroeder curve, or computes
inter-position variance. There is nothing to generate room fixtures from.

Two tempting answers are both wrong. **Writing the room DSP in Python first** is
fake provenance: the prototype earned oracle status by being a shipped,
ears-validated product; new Python written the same afternoon by the same person
for the same purpose is not an independent oracle, and testing the Rust against it
would launder a design bug into a golden fixture. **Skipping fixtures entirely**
leaves the highest-risk new code in the project untested.

### This is a clarification, not a contradiction

The claim that CLAUDE.md's rules are *jointly unsatisfiable* for room DSP is
false, and the spec should not be written as if resolving a paradox — because the
"impossible" maneuver is **already merged**.

`crates/paraeq-dsp/src/spline.rs` (commit `309d71d`, "feat(dsp): not-a-knot cubic
spline") is a `paraeq-dsp` module with **no `fixtures/spline` directory**, **zero
mentions of `spline` in `generate_fixtures.py`** (verified: `grep -c` returns 0),
and **no prototype addition**. It is verified by analytic invariants
(`passes_through_knots`; `n2_is_linear_and_n3_is_parabola`) plus inline pinned
scipy values (`test_spline.rs:14-21`: `expected: [f64; 6] = [1.125, 0.375, 0.375,
1.125, -3.125, -3.125]`, commented "pinned via scipy CubicSpline oracle"). A
`paraeq-dsp` module was added without touching the oracle, without fixtures, and
without amending CLAUDE.md.

The apparent contradiction is an equivocation on "feature." **"Fixtures are
sacred" is a provenance rule**, and its own text sanctions editing the generator:
"Never edit fixtures by hand; **regenerate and commit script + output together**."
Adding a `gen_*()` case is the workflow the rule prescribes, not a violation of
it. And "Don't add features to the prototype" is the last sentence of the
paragraph that assigns the prototype its role as the numerical oracle — it retires
the PyQt6 **product** (`prototype/app/`), not the oracle math.

Be honest about the strength of the precedent. `fir.rs:1-4` says "**Oracles:**"
*plural* and lists `prototype/paraeq/correction/fir_filter.py` **first**; scipy is
a *transitive* dependency of the prototype (`fir_filter.py:4` is `from
scipy.signal import minimum_phase`). The real precedent is `spline.rs:1-5`, which
cites `scipy.interpolate.CubicSpline` alone — and even that is not an independent
third-party oracle, because the prototype uses `CubicSpline` too
(`target_curves.py:8`). The honest statement of the precedent is: **"cite the
library the prototype itself delegates to."** Tier 3 exists precisely because that
precedent does not stretch to cover gating, FDW or splice, for which no library
delegate exists at all.

### The four tiers

| Tier | Applies to | Method | Tolerance | Status |
|---|---|---|---|---|
| **1 — Frozen prototype fixtures** | The ten existing modules | `generate_fixtures.py` imports `prototype/paraeq`, dumps input→output pairs; Rust TDD'd against them | 1e-12 (closed-form coeffs) · ~1e-9 (single-FFT) · ~1e-6 taps + 0.01 dB magnitude (min-phase chain) | **Frozen.** Not regenerated for room work. Room behaviour arrives as **new functions, never modified ones.** `test_fr.rs::averaging_is_in_db_domain` must keep passing untouched — that test is the contract that stops someone "unifying" the two averaging paths. |
| **2 — scipy-direct fixtures** | New primitives with a scipy reference: windows (`scipy.signal.windows.tukey/hann/blackmanharris`), Alvarez–Mazorra Gaussian, RMS/power averaging, Schroeder backward integration, log-f resampling | `generate_fixtures.py` gains `gen_*()` cases that **do not import `prototype/paraeq`** | 1e-9 relative | "Fixtures are sacred" survives fully: the generator remains the sole writer and the manifest still records versions. |
| **3 — Analytic physics** | Gating, FDW, splice, authority — where no library delegate exists | Assert closed-form physics, not a Python transcription. **Cannot inherit an oracle's bugs**, which makes it a strict upgrade over parity for exactly the code with no oracle. | per-test, stated inline | Four tests available today — see below. |
| **4 — REW characterization** | End-to-end room pipeline | Commit one real IR as WAV plus REW's own gated/smoothed FR as text; assert agreement | **0.1–0.5 dB, regression-detection only** | **Never assert bit parity.** REW is closed-source with undocumented window defaults and ships betas weekly. |

**The Tier-3 tests that exist today**, and which the room-DSP plan must write
first:

1. **Gate kills a reflection.** A two-path IR `h = δ(t₀) + g·δ(t₀+τ)` has ungated
   `|H| = |1 + g·e^{−jωτ}|` — a comb with peaks `1+g`, nulls `1−g`, spacing `1/τ`.
   A right window shorter than τ must return it **exactly flat**.
2. **FDW fast path equals brute force.** The O(N log N) log-f Gaussian convolution
   must match an O(F·N) brute-force per-frequency window. Write the brute force as
   the test oracle and ship the fast path. Verified agreement across
   n_c ∈ {3…61} to **< 2e-15 dB**.
3. **Splice is continuous after level matching.** Two curves offset by a known
   constant must show zero discontinuity after mean-dB level matching over the
   overlap.
4. **Power averaging floors correctly** — stated as the *exact* formula, not the
   refuted "null-immune" one:
   `10·log₁₀( ((N−k) + k·10^(d/10)) / N )`. Assert the value at finite depths
   (N=5, k=1, d=−6 dB → **−0.705 dB**), assert monotone approach to the floor
   `10·log₁₀((N−k)/N)` (**−0.969 dB** for N=5, k=1), assert the floor is reached
   within 0.01 dB by ≈ −20 dB depth, and assert the k=N case **passes the null
   through exactly** (a null common to every position — floor bounce, SBIR — is
   not attenuated at all). Power averaging is null-**resistant**, not null-immune.

### The manifest-is-not-a-pin defect, and its fix

`fixtures/manifest.json` is a **generated provenance record, not a pin**. Verified:
`generate_fixtures.py:235-239` writes `scipy.__version__`, `numpy.__version__` and
`platform.python_version()` at runtime, recording whatever happened to be
installed. The only dependency *declaration* is `prototype/pyproject.toml:16`,
`"scipy>=1.10"` — a **floor**. Nothing enforces 1.18.0; regenerating under a newer
scipy silently rewrites the manifest, and `fir.rs`'s transcribed
`scipy.signal.minimum_phase` algorithm and `spline.rs`'s `CubicSpline` port are
both pinned to *behaviour*, not to a declared version. This is a real latent
defect: a fixture regeneration under a scipy that changed either algorithm would
produce a green suite against silently different goldens.

**The fix (R0, hours):**

1. Pin exactly in `prototype/pyproject.toml`, in a new `[project.optional-dependencies] fixtures` extra:
   `numpy==2.5.0`, `scipy==1.18.0`. Keep the loose runtime floors for the
   prototype's own use; the fixture generator gets exact pins.
2. Make `generate_fixtures.py` **fail loudly** on mismatch: assert
   `scipy.__version__` and `numpy.__version__` equal the pinned values before
   writing anything, with an override flag (`--allow-version-drift`) that also
   stamps a `"drift": true` field into the manifest.
3. Keep the manifest as a provenance record — it is useful — but stop treating it
   as the pin, and correct `fir.rs`'s header comment, which currently says "scipy
   1.18.0 (the version that generated fixtures/, see fixtures/manifest.json)" as
   though the manifest were normative.

### The CLAUDE.md text to change

Two edits, precisely scoped. The "Fixtures are sacred" bullet stays as written —
it is a provenance rule and it survives intact.

**Edit 1 — the Project paragraph.** Replace:

> The old Python/PyQt6 prototype lives in `prototype/` as the **numerical
> oracle** — it generates the golden fixtures in `fixtures/` that the Rust DSP
> core must match. Don't add features to the prototype.

with:

> The old Python/PyQt6 prototype lives in `prototype/` as the **numerical
> oracle** for the ten DSP modules ported from it — it generates the golden
> fixtures in `fixtures/` that those modules must match, and that parity is
> frozen. Don't add features to the prototype (this retires the PyQt6 *product*
> under `prototype/app/`; adding a `gen_*()` case to
> `prototype/tools/generate_fixtures.py` is the prescribed fixture workflow, not
> a feature). **DSP written for the measurement suite has no prototype oracle;
> see the four-tier strategy in
> `docs/specs/2026-07-15-measurement-suite-design.md`.**

**Edit 2 — the TDD bullet.** Replace:

> - **TDD**: Rust DSP modules are written against failing golden-fixture tests
>   first (`crates/paraeq-dsp/tests/`).

with:

> - **TDD**: Rust DSP modules are written against failing tests first
>   (`crates/paraeq-dsp/tests/`). The oracle depends on the tier — golden
>   fixtures for prototype-ported modules, scipy-direct fixtures for new
>   primitives with a library delegate, analytic-physics invariants where there is
>   none, REW characterization at 0.1–0.5 dB for the end-to-end pipeline. Pick the
>   tier before writing the module and state it in the module header comment, as
>   `spline.rs:1-5` does.

Also update the Project paragraph's pointers to name this spec as the current
design spec, keeping `2026-07-02-rust-port-design.md` as the still-authoritative
source for the engine, the tap architecture and packaging.

### Testing policy for everything else

Unchanged from 2026-07-02, and restated because the rescope adds two new test
subjects:

- **`decide()` is fixture-testable and must be fixture-tested.** Feed a stored
  `MeasurementBundle` + `PathProfile`, assert the whole `DecisionSet`. This is
  what makes "every decision visible and overridable" a type-system property
  rather than a discipline, and it is the mechanism that stops the two front-ends
  diverging.
- **The measurement runtime gets synthetic-capture tests** — the level solver, the
  refusal checks and the abort path are pure logic over recorded numbers and must
  not need hardware.
- **Hardware-dependent integration tests stay behind `#[ignore]`** and run locally
  (CI has no audio devices). The helper-child-process verification loop joins them.

## Spec Delta

What in `docs/specs/2026-07-02-rust-port-design.md` no longer holds. Line numbers
are from that file as of this writing.

### Void — actively wrong, must be retired

| Where | Text | Why void |
|---|---|---|
| **Line 24** (Decisions Log, "Parity scope") | "Everything the prototype does + volume keys + output picker" | The milestone is no longer parity. Superseded by the Product-scope row in this spec's Decisions Log. |
| **Line 104** (port map, `dsp::fr`) | "averaging stays in dB domain" | Void **for the room path**. `average_measurements` stays exactly as it is — dB-domain, oracle-documented, fixture-pinned — and remains correct for the coupler path, where repeated reseats on one jig produce smooth variance and no interference nulls. The room path gets a **new** `average_measurements_rms` plus mandatory Align SPL. Note the two operators are **not** interchangeable-except-on-nulls: they agree **iff magnitude is identical at every position** (zero variance), with `RMS_dB − dB_avg ≈ 0.1151·σ_dB²` and RMS ≥ dB always by the power-mean inequality. A feature present everywhere but with spread still diverges. |
| **Line 106** (port map, `dsp::targets`) | "same six bundled CSVs" | Void. All six (`diffuse_field`, `flat`, `harman_ie_2019`, `harman_ie_2019_without_bass`, `harman_oe_2018`, `harman_oe_2018_without_bass`) are coupler targets — categories `reference`, `in-ear`, `over-ear`. **No room target ships.** Worse, `match_closest_target` (`targets.rs:193-211`) iterates every curve with **no category filter**, despite `category: Option<String>` existing on `TargetCurve` and being parsed at `targets.rs:76`. Add a room target to that directory today and a speaker measurement can match `harman_oe_2018`, double-applying ear gain the loudspeaker already delivers acoustically via the listener's own ear. The gap at 3 kHz is **11.3 dB** (`harman_oe_2018` is +8.3 dB with a +0.22 dB/oct tilt; the B&K room curve is −3.0 dB with −0.84 dB/oct). Promote `category` to a `TransducerClass` enum and make it a **required argument** to `match_closest_target` — a type-level constraint, not a UI default. |
| **Line 147** (parity surfaces, Measure tab) | The whole row | Void twice. (a) "EARS input picker" is the EARS-centric framing the rescope exists to escape. (b) "the Farina method tolerates their small clock skew (prototype proved it)" — proven for an **ungated coupler magnitude** measurement, which does not extend to gating, where *t = 0* must be trustworthy and the IR shape undistorted. That claim moves to the risk register (see Risks). What **survives and is correct**: the tap-era simplification that self-exclusion lets the sweep play cleanly so "correction state cannot contaminate the measurement." |
| **Line 219** (Out of Scope) | "Parameterized targets (baseline + tilt/bass/treble preference adjustments) … post-parity backlog" | Void. A room target **is** a parameterized target: an RBJ low shelf (fc ≈ 105 Hz, Q ≈ 0.71) plus a linear tilt. The shelf is already in `biquad.rs`. Excluding parameterized targets excludes the room path. (The related exclusion of newer 5128-era curves — JM-1, IEF 2025 — **stays** out of scope: they are further from EARS, not closer.) |
| **Line 206** (Risks, "Measurement play/record clock skew") | Mitigation: "Farina sweep deconvolution tolerates small skew — empirically proven by the prototype with the same topology" | Void as a mitigation. The evidence does not cover gating. Replaced by the two-clock risk row in this spec. |

### Stale — actively misleading, must be corrected

| Where | Text | What shipped |
|---|---|---|
| **Lines 82–85** (Realtime Audio Pipeline, Threads and data flow) | "**Tap IOProc** … pushes into a lock-free SPSC ring buffer. **Output IOProc** … pulls a block from the ring" | Stage 3 shipped **ONE IOProc on a private aggregate** containing both the tap and the output device. There is no tap→output ring and no second IOProc. |
| **Line 85** (same section, Control plane) | "hands ready-to-run processors to an atomic pointer swap (`arc-swap`)" | Inverted. **`rtrb` SPSC rings carry commands and processor swaps inbound** (control → rt) and retired configs back (rt → control, so deallocation always happens off the realtime thread); **`ArcSwap` publishes `EngineState` outbound** (rt telemetry → UI). See `crates/paraeq-engine/src/shared.rs`. |
| **Line 28** (Why taps, not a custom driver) | The volume-keys rationale, and "eqMac's issue tracker documents years of driver-exposed-volume fragility" | The *conclusion* (taps, not a driver) is right and unchanged. The *rationale* overstates: drivers **can** expose native volume controls — `equaliser` sets `kEnableVolumeControl`, `radioform` uses `AddStreamWithControlsAsync`. The real cost of the driver path is **host-side forwarding plumbing** (the driver's volume control must be wired to the physical device's), not broken volume keys. Correct the text so the decision rests on what actually justifies it: no driver install, no admin prompt, no reboot, no root LaunchDaemon, single notarized .app, one TCC prompt, and no uninstaller problem. |
| **Line 90** (Latency budget) | "62.3 ms added at 512-frame buffers / 51.6 ms at 256 / 46.3 ms at 128 … over a ~41 ms fixed tap-path floor" | The **measurements** are right (46–62 ms, fitting `40.96 ms + 2.0 × buffer_period` with a 1966-sample floor invariant across a 4× buffer sweep). The word **"added" is an over-claim** and must go. `backend.rs:195` computes `out_sample_time − in_sample_time`, which **includes the output device's own safety offset and DAC latency — latency that exists with or without ParaEQ**. "ParaEQ adds 41 ms" is not established. Report it as `EngineState.latency_ms` = measured end-to-end tap-to-output delta, and say so. (For contrast, competitors' "~10 ms" claims are arithmetic, not measurement: OnlyEQ's `estimatedLatency = ioBufferFrames * 2 / sampleRate`. iQualize **removed** its "Low Latency" toggle because ring capacity "did not meaningfully reduce latency.") |

### Unchanged and still authoritative

The crate map and its constraints; the "why taps" **decision**; one clock domain
on the playback side; two bypass levels; rebuild-on-change; **fail-safe teardown
ordering**; f32 samples with f64 design math and f64 biquad state; the dependency
list and the forbidden-deps list; the Tauri IPC contract (typed commands, one
serialized state snapshot on change, a binary Channel for spectrum and
measurement progress); packaging (Developer ID + notarization, DMG, macOS 14.4
minimum); and the golden-fixture pipeline **for the ten ported modules**.

### Known defects this spec inherits and schedules

Verified in the current tree, listed here **as a scheduling index only** —
because the Spec Delta is where a future session will look for them. Each is
scheduled in Staging, and the **authoritative fix detail lives in the companion
that owns it**, not here: the R1 engine/DSP defects (auto-preamp, NaN, Jury guard,
autofit boost caps, `kAudioSubDeviceInputChannelsKey`, rate staleness, swap-state,
meters) in `engine-hardening-design.md`; the cal parser, `TransducerClass` targets
and `deconvolve` reshape in `room-dsp-design.md`; the sweep output-level policy in
`measurement-safety-design.md`. Do not treat the one-line notes below as the
contract — follow the companion.

| Defect | Location | Note |
|---|---|---|
| No boost cap in autofit | `autofit.rs` — picks bands by symmetric `residual[i].abs()`, global Q clamp `0.5..=20.0` at `:71`, **no gain limit**, hardcoded `20.0..=20000.0` mask at `:13` | The genuinely actionable neighbour of the averaging question. The dB-averaging issue is **latent** (`fr.rs::average_measurements` has **zero non-test callers in Rust** — verified; the prototype call site at `measurement_wizard.py:481-484` smooths 1/6-octave *before* averaging, which is REW's *endorsed* regime for dB averaging). The missing boost cap is not latent. |
| Hardcoded preamp | `peq.rs:112` emits the literal `"Preamp: 0.0 dB"` | Must become computed, and must drive **the engine gain stage**, not just the export text. Get AutoEQ's convention right: `ParametricEQ.txt` preamp is exactly `−compound.max_gain` with **no headroom** (`frequency_response.py:211`). `PREAMP_HEADROOM = 0.2` applies **only** to the GraphicEQ string and the min/linear-phase FIR impulse responses, and subtracts from the normalized equalization *curve*. The README-quoted parametric preamp uses a **hardcoded 0.1**, not the constant. |
| No runtime stability guard | `biquad.rs` designers return `[f64; 6]` infallibly — no Q clamp, no Nyquist/fc validation, no `Result` | The Jury test `|a2| < 1 && |a1| < a2 + 1` exists **only** as build-time proptests (`tests/test_props.rs:7-42`, 4 tests × 64 cases). It must also exist at runtime, in library code, the moment autofit places high-Q filters near DC in the modal region. REW's boost-Q cap `Q_max = 0.227·f₀/A` with `A = 10^(G/40)` (from a 500 ms T60 rule, verified against ParaEQ's own `biquad.rs` coefficients) doubles as this guard. |
| Brittle cal parser | `compensation.rs:11` dispatches on a leading double-quote; `:60` hardcodes `.skip(2)` | UMIK-1 0-degree files have **one** header line; 90-degree files have **two**. On a single-header file `skip(2)` silently drops the first data row and `linear_interp_edge_hold` then edge-holds from the wrong point. Do **not** state a failure rate — verifiers disagreed on whether real files ship quoted or bare, and the "80%" statistic is not established. The robust fix is REW's own documented rule: **"Only lines which begin with a number are loaded, others are ignored."** The oracle (`prototype/paraeq/measurement/compensation.py`, `np.loadtxt(skiprows=2)`) shares the flaw — **both must change together** or you manufacture a divergence, and the Tier-1 fixture regenerates in lockstep. |
| Stale coefficients on rate change | `controller.rs:105` `build_correction(config, channels, block_size)` takes no sample rate | A 48 → 44.1 kHz switch (AirPods, routine) silently shifts every filter ~8.8%. A product that certifies its own residual cannot have this. |
| No time axis on `deconvolve` | `deconvolution.rs:6` `deconvolve(recorded, sweep, _sample_rate) -> Vec<f64>` — takes the rate and **discards** it | Every gating operation needs *t = 0*. Must return `ImpulseResponse { samples, peak, sample_rate }`. |
| Coefficient swap discards state | `chain.rs:89` `set_correction` is a `mem::replace` | The new processor starts from **zeroed** state, discarding up to 4095 samples of FIR overlap tail. `equaliser`'s `BiquadFilter.setCoefficients(_:setup:resetState:)` with `resetState: false` on incremental edits preserves delay state — which is how it avoids clicks with no crossfade at all. Note the engine has **no** smoothing/ramping/crossfade (grep-verified), no denormal handling, no clip counter, and `input_peak` is a monotonic session max with no decay. |
| No IR windowing of a *measured* IR | — | State this precisely. `fir.rs:85` `windowed_ir()` **does** apply a Hann window to a **designed** impulse response, live on both arms of `design_fir_correction`. What is true is that **nothing time-gates a measured IR**. |
| Sweep output level has no home | — | Not a regression — **a forward-looking requirement**. The `SWEEP_AMPLITUDE = 0.5` lives at `prototype/app/wizard/measurement_wizard.py:39`, the **playback layer**, which is not ported (there is no sweep playback path anywhere in Rust; `generate_sweep`'s only caller is its own test). `sweep.rs` faithfully matches its oracle `prototype/paraeq/measurement/sweep.py`, which is *also* unscaled, and the peak-1.0 convention is deliberate — `noise.py`'s docstring: "normalized to a peak of 1.0 so the caller can apply any output amplitude," and CLAUDE.md scopes `paraeq-dsp` to pure math. For reference, the prototype's actual sweep is **−9 dBFS RMS = 3 dB above REW's −12 dBFS default and 6 dB below REW's −3 dBFS maximum**. The −6 dBFS output-level policy has **no home in the Rust tree**, and R5 must create one. |

## Staging

Finish stage 4 (`feature/rust-port-tauri-shell`, CI-green) on the owner's ears-on
12-point run and merge it. **Do not reopen it.** Stages 5–6 of the 2026-07-02 port
order are replaced by R0–R7.

| Stage | Deliverable | Duration | Gates |
|---|---|---|---|
| **R0 — Methodology + spec delta** | The four-tier oracle written down; the two CLAUDE.md edits; the scipy/numpy pin + generator version assert; the void/stale spec text retired in place | days | **Gates everything.** Skipping this produces untested room DSP that looks tested, and every new module reads as a convention violation. |
| **R1 — Safety and correctness floor** (detailed in `engine-hardening-design.md`) | Cal parser rewritten to REW's leading-numeric rule **in both the oracle and Rust**, with the Tier-1 fixture regenerated in lockstep + the never-normalize regression test + the outlier validator; NaN sanitization at the capture boundary; runtime Jury guard + gain-dependent Q cap; computed auto-preamp driving the engine gain stage; boost ceiling in autofit; `kAudioSubDeviceInputChannelsKey: 0`; sample-rate-aware `build_correction` | days | None of this is room work; all of it is unshippable-without. R1's stability guard gates R4. |
| **R2 — Oracle + fixtures** | Tier-2 `gen_*()` cases (windows, Gaussian, RMS averaging, Schroeder integration, log-f resample) + the four Tier-3 analytic tests + the Tier-4 REW characterization corpus | weeks | **Gates R3.** No Rust gating code lands before its test exists. |
| **R3 — `paraeq-dsp` room core** | **Per-channel API first** — every design fn takes one mono curve and `ParametricEQ` has no channel field, while the engine is already per-channel capable (`sos_per_channel`, per-channel `zi`); writing autofit policy against a mono API and retrofitting means touching everything twice. Then: `deconvolve` → `ImpulseResponse`; `logf` (gates FDW, smoothing, splice, targets — it must land first even though it looks like plumbing); `window`; `gating`; `fdw`; `fr` rework (`average_measurements_rms`, `align_spl`, σ(f), variable smoothing, Alvarez–Mazorra); `room`; `targets` rework (`TransducerClass`, room target); `authority`; `splice` (fallback only) | weeks | Gates R4, R5. |
| **R4 — Decision engine** | New crate `paraeq-decide`: pure `decide(&MeasurementBundle) -> DecisionSet` returning typed `Decision<T>` with value, domain, source, evidence and invalidation tier (`Redesign` ~50 ms / `Reanalyze` ~200 ms / `Recapture`) | weeks | Gates R6. Put it in Rust or the two front-ends fork within a release. |
| **R5 — Measurement runtime + safety** | New crate `paraeq-measure`: level-ladder solver; abort guards; windowed `input_peak`; the sweep level parameter + fade; the two-clock handling; the `paraeq-stimulus` helper binary; the closed-loop verification pass | weeks | Gates R6. **Sequencing caveat: do not buy safety margin with sweep length until the acoustic timing reference exists** — a 12 dB level cut costs 12 dB SNR and needs 16× the length to recover, and the "Farina tolerates the skew" evidence covers 5 s only. |
| **R6 — Wizard (both front-ends)** | The one spine parameterized by `PathProfile`; **persist raw per-position IRs** — the prototype's discard-on-cal-change is a storage artifact masquerading as physics (compensation is applied *after* the FFT and could be re-applied for free), and IR retention is what makes the Advanced drawer feel live rather than punitive | weeks | Gates R7. |
| **R7 — Ship** | Signed DMG, docs, release | weeks | — |

## Risks and Mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| **Two-clock topology + gating.** The mic runs at its own fixed rate against the output device. "Farina tolerates their small clock skew (prototype proved it)" was proven for an **ungated coupler magnitude** measurement; gating needs a trustworthy *t = 0* and an undistorted IR shape, and that claim does not transfer. **Biggest unscheduled cost in the rescope — now de-risked to a known-magnitude port (see Mitigation).** | High | **DECIDED (method) [NEEDS DATA]:** adopt REW's bracketed-timing-marker skew estimate + resample (default on; ~12 ppm typical), and confirm the magnitude on the owner's rig via measurement-suite/9 before hardening — see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q6 and the plan doc's REW-comparison section; option (2) below is now the chosen path, not one of four coequals. Schedule it explicitly in R5, not as a footnote. Measure the actual drift before designing around it: capture a known stimulus on both clocks and quantify the *t = 0* jitter across a 5 s sweep. Escalation path, cheapest first: (1) accept it if measured jitter is small against the FDW's LF window; **(2) resample the capture to the playback clock via a measured drift estimate — the adopted method**; (3) drift-compensate in the aggregate as the tap path already does (`kAudioSubTapDriftCompensationKey`); (4) require the mic and output on one aggregate. Cross-cutting constraint: the Farina harmonic bound `dt₂ = T·ln2/ln(f₂/f₁)` puts H2 only **502 ms** before the peak for a 5 s 20 Hz–20 kHz sweep, so a long left window folds distortion into the "linear" response — and `deconvolve()` puts the IR peak at only **~46–64 ms** (tap latency + propagation), so REW's 125 ms left window is **physically impossible**: clamp left to `min(requested, peak_index)`. |
| **Scope doubling.** R2–R6 replace two stages with five. Four to six months at current cadence. | High | **OPEN [OWNER]:** schedule contingency — correct below the transition only if the date slips (owner's ship-scope call). Cut **room ambition, not room presence**: if the date is at risk, correct below the transition only. Dirac sells exactly that as a $499 Limited-Bandwidth tier, so the precedent is commercial, not apologetic. The transducer count is **not** the risk — four types is one spine parameterized by a `PathProfile`, and bookshelf vs. floorstanding is one data-derived low-corner rule, not a branch. |
| **The competitive window.** ~12–18 months before an OSS tap EQ bolts on a sweep. Dirac shipped Mac ART 2026-06-30. | High | Do not ship coupler-only as an interim — it re-anchors ParaEQ as "the EARS app" and burns the window on the weakest claim it owns. Ship the two differentiators nobody has and that are nearly free once R3 lands: **σ(f) as measured confidence** gating EQ authority (REW's author describes it in prose; no product automates it) and **closed-loop verification** (neither Dirac nor Sonarworks re-measures). |
| **Oracle-less room DSP.** ~two-thirds of the room core has no oracle of any kind. | High | The four tiers, decided in R0 before code. Tier 3 is the honest answer and is a **strict upgrade** over parity for this code: an analytic-physics test cannot inherit an oracle's bugs. Be candid that Tier 3 covers fewer behaviours than a fixture would — pair each Tier-3 module with Tier-4 REW characterization at 0.1–0.5 dB so a regression is caught even where an invariant does not pin the value. |
| **Smoothing is O(N²) and room IRs are 10–100× longer than headphone IRs.** `fractional_octave_smooth` (`fr.rs:34-59`) is a rectangular boxcar whose inner loop scans **every** bin for **every** bin — measured **2586 ms for 65536 bins**. It never bit because headphone IRs are short. It will visibly hang the wizard. | Medium | Alvarez–Mazorra recursive Gaussian (λ = q²/(2K), ν = (1+2λ−√(1+4λ))/(2λ), K ≈ 4, with Getreuer's q-correction) gives O(N·K) with published boundary handling. **The kernel changes from boxcar to Gaussian, so exact fixture parity is impossible** — keep `Fixed(n)` bit-exact on the old code path (Tier 1 is frozen) and make the new modes **additive**. Likewise implement FDW as an O(N log N) convolution on a log-f axis, never the O(F·N) per-frequency loop — and note the FDW **must** operate on the *complex* spectrum; magnitude smoothing is a different, wrong operation. |
| **Target category error.** `match_closest_target` has no category filter. Adding a room target to `targets/` without fixing this lets a speaker measurement match `harman_oe_2018` and double-apply ~11 dB of ear gain at 3 kHz. | Medium | `TransducerClass` as a **required argument**, landed in the same commit as the first room target. Compiler-enforced, not a UI default. This is the one defect whose exploit is *created by* the rescope: it is inert today only because all six bundled curves are coupler targets. |
| **The FDW at low frequency needs data we do not have.** A symmetric 15-cycle FDW at 50 Hz wants **300 ms** of pre-peak data; ParaEQ has ~46–64 ms. | Medium | **OPEN [NEEDS DATA]:** the pre-peak data gap cannot close until real EARS/UMIK captures exist. Asymmetric pre/post cycle counts, with the left side clamped to `min(requested, peak_index)`. Publish `min_valid_freq = 1/T_right` and render it — grey out what the measurement cannot resolve rather than plotting noise. The resolution limit is not folklore: a window of length T resolves nothing below ~1/T, with the stricter criterion `f = (1/T)/(2^(1/2N) − 2^(−1/2N))`. |
| **σ(f)-derived transition frequency has no shipping precedent.** It is the one genuinely novel piece with no competitor to check against. | Medium | It is also the best differentiator in the set: σ separates correctable-everywhere features (σ ≈ 0.6–0.8 dB) from position-dependent junk (σ ≈ 10 dB), and rises toward the diffuse-field asymptote of **5.57 dB** above Schroeder — so confidence-derived authority **reproduces the ~200 Hz rule without hardcoding it** and adapts to a treated room. Fallback if it proves unstable on real rooms: the 200 Hz constant, with the variance curve retained as **evidence only**. Cross-check against `f_s = 2000·√(T60/V)` where volume is given, and return a **range** (0.5·f_s … 2·f_s) rather than a point. |
| **Align SPL is mandatory and easy to forget.** REW: "it is usually best to first use Align SPL to remove overall level differences due to different source distances." | Low | Make it structural: `average_measurements_rms` takes already-aligned curves and the room pipeline calls `normalize_to_reference_band` (the existing primitive) first. Without it, near positions dominate the power average **and** inflate σ(f), corrupting the confidence metric that everything downstream depends on. |
| **Tap goes all-zero in steady state.** Apple forum reports describe taps going silent after minutes of correct operation, not just at engage. | Low | The watchdog must cover steady-state, not just the ~5 s engage window. The engine's fail-open-on-undetected-input behaviour (commit `179cd34`) is the right shape; extend its coverage and test it. |

## Out of Scope

Explicitly, in writing, so a future session does not relitigate:

- **Time alignment** between drivers or channels. Clean seam: `deconvolve` returns
  `ImpulseResponse { samples, peak, sample_rate }`, so *t = 0* exists for a later
  release to use.
- **Subwoofer integration** and **crossovers**. Clean seam: `authority.rs` is
  frequency-indexed and the room path already computes a transition range.
- **Excess-group-delay authority masking** — **v1.1**, not v1. It is the sharpest
  differentiator, and it is still deferred: three cheaper guards cover the same
  failure in v1 (RMS averaging refuses nulls structurally, σ(f) flags them as
  position-dependent, asymmetric cut/boost never fills them). Clean seam:
  `fir.rs`'s `minimum_phase_homomorphic` already provides the reference.
- **Mixed-phase / phase correction of any kind.** No latency budget exists for a
  Dirac-style ~7 ms modeling delay against a ~41 ms tap floor, and see the
  never-claim list.
- **MMM (moving microphone method)**, absolute SPL / device-sensitivity databases,
  vector averaging (rejected *and guarded*), the X-curve (scoped to rooms > 125 m³;
  its −3 dB/oct is 3× the domestic consensus), Sonarworks' 37-position class, and
  acoustic mic localisation (needs known geometry, adds a class of unrecoverable
  "cannot locate microphone" errors, and buys nothing for a gated, averaged
  magnitude correction). Anchor N at Dirac's 9.
- **CamillaDSP export** — deferred until after the room path ships. Then: emit
  YAML, vendor nothing, reimplement from published formulas.
- **Per-app EQ**, **cross-device redirect**, **output groups** (SoundSource-class).
- **Windows / Linux.** Clean seam: `paraeq-coreaudio` remains the single platform
  crate.
- **Publishing a measurement database.** The only defensible variant —
  unit-variation data on one rig — is a research paper, not a product.
- **Newer 5128-era target curves (JM-1, IEF 2025).** They deepen the rig-mismatch
  problem rather than relieving it. Parameterized targets (tilt/bass/treble) come
  **in** scope with the room path; these stay out.
- **Dosimetry, exposure tracking, listening-time budgets.** At correct levels they
  would never fire — a full 24-sweep session at the EARS 84 dB SPL reference burns
  0.23% of the WHO/ITU H.870 weekly allowance. The entire risk is acute
  misconfiguration, and the correct response is hard interlocks plus a closed-loop
  level solve. See `docs/specs/2026-07-15-measurement-safety-design.md`.

## Open Questions

Retagged by kind on 2026-07-21 as the settled decisions were folded in.
**DECIDED** items carry a pointer to where they were resolved; **OPEN [OWNER]**
items are value / business / ears calls that block ship, not the next code;
**OPEN [NEEDS DATA]** items cannot close until real EARS/UMIK measurements exist;
**RESOLVED** items were settled in a companion spec. Nothing here is papered over.

1. **Commercial posture. OPEN [OWNER]:** owner's business call; the recommendation
   is to stay FOSS / non-commercial to retain the PLD Art. 2(2) exemption — see
   docs/decisions/2026-07-21-decision-engine-open-questions.md §Q8. EU PLD 2024/2853
   (transposition due 9 Dec 2026 — inside
   this release's life) makes software a product; Art. 6(1)(a) covers personal
   injury; **Art. 14 voids any contractual exclusion**, so the MIT "AS IS"
   disclaimer is inert against a personal-injury claim. The real protection is
   Art. 2(2)'s FOSS carve-out, which survives **only** while ParaEQ is supplied
   entirely outside commercial activity — recital 15 kills it for software supplied
   "in exchange for a price, or for personal data." **The owner must decide
   explicitly and it must be recorded in a spec.** Any paid tier forfeits the
   exemption and attaches strict liability that cannot be disclaimed.
2. **Is the two-clock skew tolerable for gating, or does the mic have to join the
   aggregate? DECIDED (method) [NEEDS DATA]:** adopt REW's bracketed-timing-marker
   skew estimate + resample (REW reports ~12 ppm typical); confirm the magnitude on
   the owner's rig via measurement-suite/9 before hardening. See
   docs/decisions/2026-07-21-decision-engine-open-questions.md §Q6 and the plan
   doc's REW-comparison section. The technique is a port, not open research; what is
   left is R5 measuring the drift on ParaEQ's own EARS/UMIK rig before the runtime
   hardens around it. The answer still shapes the measurement runtime.
3. **Does the σ(f)-derived transition frequency hold up on real rooms? OPEN [NEEDS
   DATA]:** this novel confidence-derived transition metric needs owner-hardware
   validation and has no shipping precedent. Note that the σ_full/σ_none authority
   endpoints themselves are decided — σ_full = 1.0 dB, σ_none = 6.0 dB, see
   docs/decisions/2026-07-21-decision-engine-open-questions.md §Q5 — with the 200 Hz
   constant + variance-as-evidence fallback if the derived transition proves
   unstable.
4. **Where does `decide()` live? — RESOLVED.** The decision-engine spec settled
   this: a new crate **`paraeq-decide`** depending on `paraeq-dsp` only, keeping
   `paraeq-dsp` pure math rather than widening its charter to "pure math and pure
   policy." This spec's Decisions Log and Architecture reflect that. (Left in the
   list as a resolved pointer, not a live question.)
5. **Companion spec filenames — RESOLVED.** The sibling sessions landed as
   `room-dsp-design.md`, `decision-engine-design.md`, `wizard-design.md`,
   `measurement-safety-design.md` and `engine-hardening-design.md`; there is no
   separate `measurement-wizard` or `claims-and-limits` file (the claims material
   lives in `wizard-design.md`'s *Honest Limits* section and this spec's
   *Positioning*). The Companion Specs table above has been corrected to the five
   real filenames.

## Research Basis (2026-07-15)

This spec is downstream of a multi-agent research round plus an adversarial
verification pass. Every load-bearing number below was re-derived or re-verified
against primary sources or against this repo; the corrections file
(21 refutations) is the reason several widely-repeated claims do **not** appear
here.

**Verified against this repo** (read, not trusted): `paraeq-dsp` is exactly ten
modules — `autofit, biquad, compensation, deconvolution, fir, fr, peq, spline,
sweep, targets` (declared in `lib.rs`) totalling **1,145 lines**, or **1,172 lines
across 11 files** counting `lib.rs`'s 27-line crate root; `paraeq-engine` is 1,746
lines across the eight files `backend, chain, controller, convolver, iir, lib,
shared, status` (1,713 excluding `lib.rs`). State one counting convention and keep
it — the two halves are on the same basis here.
`fr.rs:61-88` sums dB and divides by count. `fr.rs:34-59` is a boxcar with an
O(N²) inner scan. `autofit.rs:13` masks `20.0..=20000.0`, `:19` picks on
`residual[i].abs()`, `:71` clamps Q to `0.5..=20.0`, and there is no gain limit.
`targets.rs:76` parses `category`; `targets.rs:193-211` ignores it. All six
bundled CSVs carry `category:` of `reference`, `in-ear` or `over-ear`.
`peq.rs:112` emits `"Preamp: 0.0 dB"` with the comment "Preamp is a fixed literal
(not computed)." `deconvolution.rs:6` takes `_sample_rate` and discards it.
`compensation.rs:11` dispatches on a leading double-quote; `:60` is `.skip(2)`.
`chain.rs:89` is a `mem::replace`; `chain.rs:171,183` clamp to ±1.0.
`iir.rs:52-58` is textbook DF2T; `iir.rs:66` is `reset()`. `tap.rs:26-48`,
`:57-61` and `:153-159` are as described, fail-open fallback included.
`backend.rs:121-130` carries the input-stream KNOWN LIMITATION; `:195` computes
`out_sample_time − in_sample_time`. `controller.rs:105` `build_correction` takes
no sample rate. `test_props.rs:7-42` holds four 64-case stability proptests.
`spline.rs` has no fixtures directory and zero mentions in
`generate_fixtures.py`; `test_spline.rs:14-21` pins scipy values inline.
`generate_fixtures.py:235-239` writes `scipy.__version__` at runtime;
`pyproject.toml:16` declares `scipy>=1.10`. `test_hardware.rs` drives the tap via
an `afplay` child process. `desktop/src-tauri/src/lib.rs` is a six-line default
builder.

**Key derived results.** The **FDW identity** is the highest-leverage result in the
set: a frequency-dependent window of half-amplitude width `n_c/f` is *exactly* a
constant-Q Gaussian smoothing of the complex spectrum, with `σ_f/f =
√(2·ln2)/(π·n_c)` and `FWHM = 4/(π·n_c)` octaves — verified numerically to under
2e-15 dB. The cycles↔octave conversion is exact (`n_c = 4N/π`), making REW's
15-cycle default 1/11.8 ≈ 1/12 octave: 750 ms at 20 Hz (effectively ungated) and
1.5 ms at 10 kHz (quasi-anechoic). An ideal exponential 20 Hz–20 kHz sweep spends
**exactly one third** of its duration in the 2–20 kHz decade, since `ln(10)/ln(1000)
= 1/3` — a property of the sweep law, **not** attributable to REW, which widens a
requested range to half-start/twice-end capped at Nyquist and explicitly warns that
long sweeps risk tweeter overheating and "are not recommended" for loudspeakers.

**Averaging, corrected.** A −30 dB null at **one of five** positions gives dB-avg
= **−6.0 dB** and power-avg = **−0.97 dB**. (The −5.0/−0.79 pair circulating in
the corpus is the **six**-position result.) A −40 dB null at one of three gives
−13.3 dB vs −1.76 dB. REW's actual wording is narrower than usually quoted: "dB
averaging may be useful when averaging smoothed traces to derive an EQ target,
with unsmoothed data the dips would have a disproportionate effect on the result"
— which is precisely the regime the prototype is in
(`measurement_wizard.py:481-484` smooths 1/6-octave *before* averaging).

**Sources and precedents.** REW help (cal-file format rule, averaging, variable
smoothing, FDW, resolution limits, sweep level conventions, the
non-minimum-phase-below-transition counterexamples at 44–56 Hz); Toole 2015 JAES
(minimum phase attributed to loudspeaker *transducers*); Klippel AN39 (splice:
mean-dB level match then raised-cosine blend, 1-oct RMS error as the auto-tune
objective; an unmatched splice produced a **13.28 dB** step in testing); Trinnov's
shipped excursion curve (±10 dB at or below 150 Hz tapering to ±2 dB by 500 Hz,
±2 dB above); Alvarez–Mazorra recursive Gaussian with Getreuer's q-correction;
CamillaDSP `src/utils/conversions.rs:89-106` (NaN sanitization at the capture
boundary); OnlyEQ (`kAudioSubDeviceInputChannelsKey: 0`, with the mic-prompt
rationale in a comment); `equaliser`
(`BiquadFilter.setCoefficients(_:setup:resetState:)` with `resetState: false`
preserving delay state); AutoEQ `frequency_response.py:211` and `fr.py:129,248,271,302,318,324`
(the preamp/headroom split); a shipping vendor cal file (`7005770_90deg.txt`)
containing a bogus `0.0000` at 19.611 Hz between neighbours of −3.13 and −3.11 —
inside the full-authority band; and the miniDSP EARS cal curve's encoded **2.1 dB
L/R capsule offset**, which is why **a cal curve must never be normalized** and why
that regression test must exist.
