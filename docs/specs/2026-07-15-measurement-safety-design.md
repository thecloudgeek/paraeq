# Measurement Safety — Design Spec

**Date:** 2026-07-15
**Status:** Proposed (rescope series; owner review pending)
**Amends:** `docs/specs/2026-07-02-rust-port-design.md` — adds a safety contract to the stage-6 "Measure tab" line (spec line 147) and to the port table, which maps DSP modules only and therefore has no row for output-level policy.

## Summary

ParaEQ is being rescoped from an EARS-jig headphone tool into a general measurement suite covering IEMs, headphones, bookshelf and floorstanding speakers, with a "just fix my sound" front-end aimed at novices. That combination changes the safety problem qualitatively: sweeps are loud by construction, IEMs sit millimetres from an eardrum, and the target user will not sanity-check a level before pressing Start.

This spec fixes the output-level policy for every stimulus ParaEQ emits. The core structural finding is that **the policy has no home in the Rust tree today, and the place it obviously belongs is the one place it must not go.** `paraeq-dsp::sweep::generate_sweep` returns an unscaled sine (peak 1.0) — correctly, because it is bit-exact against its oracle `prototype/paraeq/measurement/sweep.py`, which is also unscaled, and because `noise.py`'s docstring states the convention outright ("normalized to a peak of 1.0 so the caller can apply any output amplitude") and CLAUDE.md scopes `paraeq-dsp` to pure math. The prototype's `SWEEP_AMPLITUDE = 0.5` lives one layer up, in `prototype/app/wizard/measurement_wizard.py:39` — the playback layer, which is not ported and has no Rust counterpart. **Nothing was dropped and nothing currently ships hot; `generate_sweep` has zero non-test callers in the workspace (verified: only `crates/paraeq-dsp/tests/test_sweep.rs:8`).** This is a forward-looking requirement, not a regression.

The decisions: create `crates/paraeq-measure` as the home for stimulus level policy and measurement orchestration; make level a type-enforced, per-transducer-class capped quantity that cannot be bypassed; establish level by a closed-loop solve from a quiet pilot tone rather than by a constant; and treat the caps as mandatory and non-overridable, because ParaEQ is the only backstop that exists on macOS (Apple's Headphone Safety / Reduce Loud Audio ships on iOS and iPadOS only).

One invariant is load-bearing and is stated here once so it is never re-litigated: **whenever tap self-exclusion succeeds, the sweep ParaEQ plays from its own process never passes through `RealtimeChain` and is therefore subject to neither the trim gain nor either of its two ±1.0 clamp sites. The measurement runtime's level control is the only protection on that path — and that control does not yet exist (§ Framing).** (Self-exclusion has a documented fail-open fallback; on that degraded path the sweep *is* tapped and processed — see § Invariant, consequence 3, which is why the fallback is a refusal condition.)

## Decisions Log

| Decision | Choice | Alternatives rejected |
|---|---|---|
| Home of output-level policy | New crate `crates/paraeq-measure`: stimulus assembly, level solve, session state. Depends on `paraeq-dsp`; no Tauri, no CoreAudio FFI (consumes a `StimulusSink`/`CaptureSource` trait pair implemented by `paraeq-coreaudio`, mirroring the existing `AudioBackend` seam) | A `level` param on `dsp::sweep::generate_sweep` (breaks the 1e-12 oracle parity and violates the pure-math crate boundary — see below); `paraeq-engine` (it is the realtime correction engine; measurement is a session-scoped control-plane activity and would bloat the daemon-extraction seam); `desktop/src-tauri` (would make the policy unreachable from a future `paraeqd`) |
| Level-setting method | Closed-loop solve: noise floor → pilot tone → chain-sensitivity solve → ≤6 dB escalation rungs with re-verification → sweep | A fixed constant (the prototype's `SWEEP_AMPLITUDE = 0.5`) — it is a guess about an unknown chain and is exactly as wrong on a 120 dB/Vrms IEM as it is on a 96 dB/W floorstander |
| Cap enforcement | Type-level: a `SweepLevel` newtype whose only constructor takes a `TransducerClass` and refuses out-of-cap values; no other path emits a stimulus | Runtime `if` at the playback site; a UI-level slider limit (both bypassable by the next caller) |
| Cap overridability | Hard caps are non-overridable, including from the Advanced drawer. Targets and warn thresholds are overridable *within* caps | REW's model (user-set, optional SPL abort) — appropriate for its expert audience, not for "just fix my sound" |
| Coupler SPL target | 84 dB SPL for both headphone and IEM paths | The corpus's 94 dB IEM target (the 1 Pa coupler reference). ParaEQ needs no absolute reference, and because the head-detection problem is unsolved (below), running 10 dB hot for no SNR benefit is unjustified. The hearing-safety and tweeter-safety optima both point quiet; take the quiet path |
| Fade | `dsp::sweep::apply_fade(&mut [f64], fade_in, fade_out)` — additive pure-math fn verified by analytic invariants (the `spline.rs` precedent, commit 309d71d), leaving `generate_sweep` bit-exact. Durations are policy and live in `paraeq-measure` | Fading inside `generate_sweep` (breaks the pinned fixture and forces an oracle change for a playback concern); fading only in `paraeq-measure` (hand-rolled window math outside the fixture-tested crate) |
| Low-frequency sweep start | Per transducer class, from a table; never extended below it | REW's convention of sweeping from half the requested start frequency — a 20 Hz request drives 10 Hz, where a ported box is fully unloaded |
| Sweep length ceiling | 5.5 s all classes, until the two-clock timing question is settled | Buying SNR with length (16× length per 12 dB of level cut). The "Farina tolerates the skew" evidence exists only at 5 s and only for an ungated coupler magnitude measurement |
| Head detection | Not attempted. Refuse out-of-envelope chain sensitivity; require explicit pre-sweep acknowledgement | Claiming detection we do not have |
| Dose accounting | Not built | Exposure tracking / listening-time budgets. At correct levels they never fire (a full 24-sweep session at 84 dB SPL is ~0.23% of the WHO/ITU H.870 weekly allowance); the entire risk is acute misconfiguration, which interlocks address and dosimetry does not |

## Framing: What Actually Changed

Three claims that circulate about ParaEQ's sweep level are wrong, and the design is worse if built on them. The corrected statements:

| Claim | Status | Correct statement |
|---|---|---|
| "The Rust port dropped `SWEEP_AMPLITUDE = 0.5`" | **False** | The 0.5 lives in `measurement_wizard.py:39`, the PyQt playback worker. `sweep.rs`'s declared oracle is `prototype/paraeq/measurement/sweep.py`, which is *also* unscaled (`sweep = np.sin(phase); return sweep`). `sweep.rs` faithfully matches its oracle. The 0.5 was never in the module `sweep.rs` ports, so it cannot have been dropped from it. Corroboration that it is playback policy rather than a sweep property: `measurement_wizard.py:296` reuses the same constant for the pink-noise tone |
| "ParaEQ's sweep runs at full scale / REW's maximum / 9 dB above REW's default" | **False** | It describes a runtime that does not exist. There is no sweep playback path anywhere in Rust; `generate_sweep`'s only caller is its own test. The sweep ParaEQ *actually* plays (the prototype's, peak 0.5) is **−9.03 dBFS RMS: 3 dB above REW's −12 dBFS default and 6 dB below REW's −3 dBFS maximum** |
| "This is a shipped safety regression" | **False** | It is an unbuilt requirement. Stage 6 is where it becomes true or false |

**The residual concern is real and is what this spec exists for.** If stage 6 wires playback without an equivalent policy, the false claims become true: an unscaled peak-1.0 sweep *is* −3.01 dBFS RMS, *is* REW's absolute maximum, and *is* 9 dB above its default. The −6 dBFS policy has no home in the Rust tree to inherit from, and the design spec's port table maps only DSP modules and therefore never allocated one.

Two numbers that make the stakes concrete, and that justify the interlock-first posture:

- **Acute misconfiguration is the whole risk.** A sensitive IEM at 120 dB SPL/Vrms on a MacBook jack (1.25 Vrms into <150 Ω at maximum volume) fed an unscaled sweep lands near 122 dB SPL. NIOSH's allowance `T = 480 / 2^((L−85)/3)` minutes gives **5.6 seconds at 122 dBA** — spent in its entirety by one 5.46-second mistake.
- **Dose accounting would never fire at correct levels.** The same math at the 84 dB coupler target makes a full 24-sweep session ~0.23% of the WHO/ITU H.870 weekly allowance (1.6 Pa²h at 80 dBA/40 h). Do not build dosimetry. Build interlocks.

## Where the Level Policy Lives

```
crates/paraeq-measure/          NEW. No Tauri, no CoreAudio FFI, no unsafe.
├── src/level.rs                SweepLevel newtype, caps table, the solve
├── src/stimulus.rs             sweep/pilot/noise assembly: dsp generate -> fade -> scale
├── src/session.rs              measurement state machine, abort, RAII restore
├── src/diagnostic.rs           MeasurementDiagnostic codes (errors vs warnings)
└── src/transducer.rs           TransducerClass + the per-class parameter table
        │
        │ depends on
        ▼
crates/paraeq-dsp               generate_sweep (unchanged, bit-exact), apply_fade (new, additive)
        ▲
        │ trait impls supplied by
crates/paraeq-coreaudio         StimulusSink, CaptureSource, volume get/set (all NEW)
```

The crate-boundary reasoning, stated so a future session does not "simplify" it away:

- **`paraeq-dsp` must not carry the level.** It is scoped to pure math with zero platform deps. An output-level hearing-safety constant is a platform/product policy, not math. Concretely: adding a `level` parameter would break `test_sweep.rs`'s bit-exact 1e-12 comparison against the pinned oracle, forcing an oracle edit for a reason the oracle does not share. The correction is a one-line multiply at the caller.
- **`apply_fade` is different and *does* belong in `paraeq-dsp`.** It is window math on a buffer, it is a correctness concern (an un-faded sweep is a measurement defect, not only a hazard), and it can be added *additively* without touching `generate_sweep` or any fixture. Precedent: `crates/paraeq-dsp/src/spline.rs` (commit 309d71d) is a `paraeq-dsp` module with no fixtures directory, zero mentions in `generate_fixtures.py`, and no prototype addition — verified by analytic invariants plus inline pinned values. "Fixtures are sacred" is a *provenance* rule, and its own text sanctions generator edits ("regenerate and commit script + output together"); a tiered oracle strategy is a clarification of that rule, not an exception to it.
- **`paraeq-engine` must not carry it either.** The engine is the realtime correction path. Measurement is a session-scoped control-plane activity that plays on a *separate, untapped* stream; putting it in the engine bloats exactly the crate the design spec wants extractable into a headless `paraeqd`.

## The Load-Bearing Invariant: ParaEQ's Own Gain Does Not Protect the Sweep

This is the single most important fact in this document.

Verified in the code:

- `crates/paraeq-coreaudio/src/tap.rs:26-48` builds the tap with `CATapDescription::initStereoGlobalTapButExcludeProcesses`, `MutedWhenTapped`, private.
- `tap.rs:153-154`: `let excluded = if own != 0 { vec![own] } else { vec![] };` — ParaEQ's own HAL process object, from `translate_pid(std::process::id())`.
- The trim gain and `.clamp(-1.0, 1.0)` are applied at **two** sites, both inside `RealtimeChain::process` and therefore both only on tapped audio: `crates/paraeq-engine/src/chain.rs:171` (the corrected path) and `chain.rs:183` (the pass-through path that runs on bypass, `correction == None`, and frame-mismatch). Neither is the "sole" clamp — a `grep -rn 'clamp(-1' crates/*/src` returns exactly these two (plus `backend.rs:93`, a channel-count clamp, not a sample clamp).

Therefore: **whenever self-exclusion succeeds, a sweep played by ParaEQ's own process is excluded from the tap, never reaches `RealtimeChain`, and is subject to neither the trim gain nor either ±1.0 clamp. The one signal in the product that can injure a person is the one signal the engine's safety clamps never see.** (Exclusion is a best-effort runtime HAL lookup, not a structural guarantee — consequence 3 below covers the fail-open path, on which the sweep *is* tapped and clamped.)

This exclusion is **correct and must not be "fixed."** It is the design goal, not an obstacle: design spec line 147 states it explicitly — the sweep plays cleanly on a direct output stream "so correction state cannot contaminate the measurement." Do not pre-convolve the stimulus with the active correction; that defeats the exclusion's entire purpose and corrupts the measurement it protects.

Four consequences that are requirements, not observations:

1. **`paraeq-measure`'s level control is the sole protection on the stimulus path.** It gets the scrutiny a safety interlock gets: type-enforced, non-bypassable, unit-tested, and reviewed as such.
2. **The stimulus emitter carries its own clamp and non-finite guard.** It cannot borrow the engine's. Clamp to ±1.0 and sanitize non-finites in the emitter, immediately before the buffer reaches the sink.
3. **Self-exclusion failure is a refusal condition.** `tap.rs:154-159` documents a fail-open fallback: if `translate_pid` returns 0 after a 200 ms retry, the exclusion list is **empty**, ParaEQ's own audio *is* tapped, and the code logs `"own process not in HAL registry after retry — no self-exclusion (watch for feedback)"`. In that state the stimulus topology is a different, unvalidated one — the sweep is muted at the device and processed by the chain. `TapSystem` does not currently expose this; it must (`self_excluded: bool`), and `paraeq-measure` must refuse to sweep when it is false. Refuse, do not adapt: an unvalidated audio topology is not a thing to guess at with a full-level stimulus armed.
4. **Closed-loop verification is the one exception, and it inverts the reasoning.** Measuring the *corrected* output requires the stimulus to go *through* the engine, which self-exclusion prevents. The resolution is the design spec's own stage-4 carry-forward: **play the verification stimulus from a helper child process**, which has a different PID and is therefore not excluded (reference implementation shape: the `afplay` child in `crates/paraeq-coreaudio/tests/test_hardware.rs:35`). Because that path *does* traverse `RealtimeChain`, it inherits the correction — including its positive gain. The verification stimulus level must therefore be solved as `L_verify = L_measure − max(0, peak_correction_gain_db)`, and the correction's realized peak gain must be known before the verification sweep is armed.

## The Level Ladder

Mandatory, in this order. Every step is a gate: failure of a step is failure of the run, not an input to a heuristic.

**0. Preconditions.** Mic identified and cal file loaded (§ Refusals); self-exclusion confirmed; output device confirmed and named to the user; system volume readable; transducer class selected.

**1. Noise floor.** Record ≥1 s of silence on the capture path. Compute a 1/6-octave-smoothed noise-floor magnitude over the class's correction band. Require the broadband floor ≤ −60 dBFS; target ≤ −36 dBFS relative to the planned sweep-at-mic level. **The only remedies are mic input gain and sweep length — never output level.** REW is imperative and correct here: *"If input levels are low DO NOT KEEP MAKING THE TEST SIGNAL LOUDER."*

**2. Pilot.** 300 Hz sine, **−40 dBFS RMS**, ≤1 s, faded. This is deliberately far below any cap: it is the probe that discovers the chain, and it must be safe on a chain we know nothing about. (Reference point: EARS uses 300 Hz at −20 dBFS.) Measure the mic's received level and convert: `SPL_measured = 94 + (dBFS_measured − Sens_Factor)`.

**3. Solve.** Chain sensitivity `S = SPL_measured − L_pilot` (dB SPL per dBFS RMS). Required sweep level `L = SPL_target − S`. Both `S` and the projected SPL are recorded in the session log and surfaced in the Advanced drawer.

**4. Envelope check.** Compare `S` against the class's expected envelope (§ Head Detection). Out of envelope ⇒ **refuse**. Do not escalate into an unknown chain.

**5. Escalate.** Climb from the pilot to `L` in rungs of **≤6 dB**, re-measuring SPL at each rung. Abort if any rung's measured SPL exceeds the cap, or if measured SPL deviates from the projection by **>6 dB** (the signature of a wrong device, a stale cal file, or a broken chain). At most **2** automatic remedy attempts before `SnrUnachievable` — and again, remedies are gain and length, never level.

**6. Sweep.** Faded, clamped, with abort armed and the pre-sweep acknowledgement recorded.

### SNR criterion

Defined on the smoothed (1/6-octave) magnitudes over the class's correction band:

```
SNR(f) = sweep_magnitude_db(f) − noise_floor_magnitude_db(f)
```

| Condition | Action |
|---|---|
| median SNR ≥ 40 dB **and** min in-band SNR ≥ 20 dB | Accept |
| median SNR ≥ 30 dB | Accept with a `LowSnr` warning |
| below that | Remedy (input gain, then length) up to 2 attempts, then refuse `SnrUnachievable` |

The measurement capture path needs its own metering. It cannot reuse the engine's: `RtShared::peak_in` is a **monotonic session maximum with no decay** and there is **no clip counter at all** (verified in `crates/paraeq-engine/src/shared.rs`) — and in any case those counters observe the *tap* path, which by construction never sees the stimulus.

### Targets and caps, per transducer class

| Path | Sweep level | SPL target | Warn (>) | Hard refuse (>) | f_start | Max sweep length |
|---|---|---|---|---|---|---|
| IEM | −20 dBFS RMS | 84 dB | 85 dB | **100 dB** | per-DUT | 5.5 s |
| Headphone | −20 dBFS RMS | 84 dB | 85 dB | **100 dB** | per-DUT | 5.5 s |
| Bookshelf | −12 dBFS RMS | 75 dB | 85 dB | **90 dB** | 30 Hz | 5.5 s |
| Floorstanding | −12 dBFS RMS | 75 dB | 85 dB | **90 dB** | 20 Hz | 5.5 s |

Notes on the table:

- **Sweep level is a starting point for the solve, not the emitted level.** The solve overrides it. It is the value used when no cal file is present — which is itself a refusal condition, so in practice the column documents intent and bounds the ladder's first rung.
- The coupler paths run **8 dB below REW's −12 dBFS default**.
- The 84 dB coupler target sits deliberately just under the 85 dB warn line: at target the warning never fires, and any solve that lands hot is immediately visible.
- Room paths refuse 10 dB lower than coupler paths because a room measurement necessarily happens in a space a person may be standing in, whereas a coupler measurement is nominally on a jig.
- **OPEN:** the 85 dB warn / 100 dB refuse thresholds are attributed in the corpus to EN 50332 and IEC 62368-1 cl. 10.6 (~85 dBA warn, 100 dBA maximum permitted output). Aligning to a named standard is the strongest available posture for an app with no certification path, but **I have not independently verified the clause numbers or their applicability to a software product that is not a portable audio player.** Confirm before any published compliance claim; the numbers are defensible as engineering thresholds regardless.

### Calibration margin

Carry **≥6 dB of margin against cal error**, because the sensitivity figure is less trustworthy than it looks: the UMIK-1 Sens Factor is captured at 100% input gain (REW runs USB mics at unity — a real and common mismatch), and community reports find factory values off by more than 4 dB. **Pin and read back the input gain; treat sensitivity as gain-referenced.** REW's own note applies too: an SPL limit set above the input's clipping point offers no protection, so validate every cap against the mic's full-scale SPL at cal-load time and refuse a cap the mic cannot witness.

## Hard Caps and Refusals

Refuse — do not warn, do not degrade, do not escalate — on any of:

| Condition | Diagnostic | Why |
|---|---|---|
| No cal file, or sensitivity unparseable | `NoMicSensitivity` | **A missing Sens Factor must never fall back to an uncapped sweep.** No SPL, no closed loop, no safety |
| Mic not identified / not in the validated list and not manually confirmed | `MicUnidentified` | Sensitivity is meaningless without knowing what produced it |
| Projected SPL exceeds the class cap before a single sample is emitted | `ProjectedSplOverCap` | Cheapest possible refusal |
| Measured SPL deviates >6 dB from projection at any rung | `SplProjectionMismatch` | Wrong device, stale cal, or broken chain |
| Measured SPL exceeds cap mid-sweep | `SplOverCap` | Abort path (§ Abort) |
| Chain sensitivity outside the class envelope | `SensitivityOutOfEnvelope` | Empty jig, misrouted output, or wrong class |
| System volume reads at maximum **and** is not settable **and** no SPL solve is available | `VolumeUncontrollable` | See below |
| >30% of samples in an input block clipped | `InputClipping` | REW's rule; the measurement is invalid regardless |
| Noise floor above the SNR gate after ≤2 remedies | `SnrUnachievable` | Audyssey's precedent |
| Self-exclusion not established (`translate_pid` fell back to an empty list) | `SelfExclusionUnavailable` | Unvalidated stimulus topology (§ Invariant, consequence 3) |
| Cap exceeds the mic's full-scale SPL | `CapExceedsMicFullScale` | The cap could never fire |

On the volume condition, the honest logic — the simple rule ("refuse if volume is at max") is wrong, because many USB DACs expose no software volume at all and refusing on them would refuse the whole class:

1. If volume is readable and settable: set it to the solved value and **restore it on every exit path**, including panic.
2. If volume is readable but not settable and reads at maximum: the SPL solve still measures the *actual acoustic result*, so the chain is still characterized and the caps still bind. Proceed, with a `FixedMaxVolume` warning.
3. Only if volume is uncontrollable **and** no SPL solve is available (which already refuses at `NoMicSensitivity`) is there no protection at all. Refuse.

**ParaEQ cannot read or set system volume today.** Verified: a workspace-wide grep for `volume` across `crates/` and `desktop/` returns zero hits. `paraeq-coreaudio` exposes exactly `default_output_device`, `device_uid`, `nominal_sample_rate`, `translate_pid`, `tap_format`, `buffer_frame_size`, `set_buffer_frame_size` (`properties.rs`). The design spec's promised "volume scalar" was never implemented. This is a prerequisite, not a nice-to-have.

### The headphones-not-on-head case: be honest

**You cannot detect a head.** ParaEQ has no sensor, no impedance probe, and no way to distinguish a headphone clamped on a jig from one clamped on a person. Any claim otherwise would be false, and a false detector is worse than none because it licenses escalation.

What is actually available:

- **Detect the chain, not the head.** The solved chain sensitivity `S` is a strong discriminator of gross error: headphones off the coupler, an empty jig, or a sweep misrouted to the laptop speakers all land far outside a per-class envelope. **Refuse rather than escalate** — escalating into an empty jig is precisely how the 122 dB scenario happens, because the user then puts the IEM in.
- **Gate on acknowledgement.** A pre-sweep confirmation naming the target device and the projected SPL, requiring an explicit, deliberate acknowledgement that the DUT is on the coupler and not on anyone's head. Dirac's master-output safety lock is the precedent: entering the red zone takes a deliberate click, not a drift.
- **Post-hoc plausibility, for the *next* sweep only.** A sealed coupler has a distinctive extended, flat LF response that on-head leakage does not reproduce. This can flag a completed measurement as suspicious and refuse to continue a multi-sweep run. It cannot gate the first sweep, and it must never be described to the user as head detection.

Stated plainly in the spec so nobody promises more later: **a headphone on a head with in-envelope sensitivity is indistinguishable from one on a jig. The acknowledgement gate and the quiet targets are the mitigation. There is no detector.**

And there is no backstop beneath us: Apple's Headphone Safety / Reduce Loud Audio (75/80/85/90/95/100 dB) exists on **iOS and iPadOS only — macOS ships nothing.** ParaEQ is the only thing between a novice and their eardrums.

## Driver Protection

A different optimum from hearing safety — and, usefully, one that points the same way.

**The exponential sweep law concentrates duration in the treble.** For an ideal exponential sweep the fraction of duration between `fa` and `fb` is `ln(fb/fa) / ln(f2/f1)`. For 20 Hz → 20 kHz, the 2–20 kHz decade is `ln(10)/ln(1000) = 1/3` **exactly** (verified numerically against a synthesized Farina sweep: 0.33333325). The 2 kHz crossing falls at `t = T·ln(100)/ln(1000) = ⅔T`, leaving exactly one third of the sweep driving the tweeter continuously.

**This is a property of the sweep law, not of any tool.** Do not attribute it to REW: REW widens a requested range to half the start / twice the end frequency (capped at Nyquist), so a 20 Hz–20 kHz request at 48 kHz actually sweeps 10 Hz → 24 kHz and the 2–20 kHz fraction there is `ln(10)/ln(2400) = 0.296`. REW does, separately and relevantly, warn that its long sweeps exist for high-SNR electronic measurements and that *"if they were used with loudspeakers the risk of overheating the tweeter would increase with the sweep length… Using long sweeps for loudspeaker measurement is not recommended."*

**Voice-coil steady-state temperature is set by power, not energy.** So "quieter and longer" is thermally safe while "louder and shorter" is not — and the hearing-safety optimum is also quieter. The two optima align. Take the quiet path. (The one thing that breaks this alignment is buying SNR with length, which is capped for an unrelated reason — see § Two Clocks.)

**Low-frequency excursion. Reject REW's half-start convention outright.** Excursion rises as `1/f²` below box tuning, and ported enclosures unload entirely below tuning — the driver loses its air spring and the cone moves freely. A 50 Hz-tuned bookshelf sees **6.25×** its at-tuning excursion at 20 Hz, and **25×, unloaded**, at the 10 Hz REW would actually drive for a 20 Hz request. Set `f_start` from the class table and never extend below it. `f_start` is not user-overridable downward.

**Sustained boost is the larger driver hazard, and it is not the sweep's.** A sweep is seconds; a correction runs for hours. Cross-referenced to the DSP/correction spec, because these are verified defects in the current tree and they compose:

- `autofit.rs` picks bands by symmetric `residual[i].abs()` and clamps only Q (`0.5..=20.0`, `autofit.rs:71`) — **there is no gain limit whatsoever** (verified: `gain_db: peak_gain`, unbounded).
- `peq.rs:112` emits a hardcoded `"Preamp: 0.0 dB"` string literal (verified) — the preamp is a fixed literal, not computed.
- The `biquad.rs` designers return `[f64; 6]` infallibly with no Q clamp, no Nyquist/fc validation, and no `Result`. The Jury stability test (`|a2| < 1` and `|a1| < a2 + 1`) exists only as build-time proptests (`crates/paraeq-dsp/tests/test_props.rs:7-42`), never as a runtime guard in library code.

The two limits that belong in the correction path and bound driver excursion:

- **Trinnov's shipped excursion curve:** ±10 dB at and below 150 Hz, tapering to ±2 dB by 500 Hz, ±2 dB above.
- **REW's boost-Q cap:** `Q_max = 0.227 · f0 / A` where `A = 10^(G/40)`, derived from a 500 ms T60 rule and verified against ParaEQ's own `biquad.rs` coefficients. It doubles as the runtime stability guard.

## Fade and DC

**An un-faded sweep ends in a broadband click at up to full scale.** Verified by direct computation of the terminal sample of `generate_sweep` (20 Hz–20 kHz):

| Case | Terminal sample | Step |
|---|---|---|
| 5.0 s / 48 kHz | **−0.5336** | −5.46 dBFS |
| 0.25 s / 48 kHz (the fixture) | **−0.9146** | −0.77 dBFS |

Because the sweep terminates near 20 kHz, that click's energy sits exactly where tweeters are most fragile and where it is most startling in a sealed IEM. It is simultaneously a hearing hazard, a driver hazard, and a measurement defect (a step discontinuity smears energy across the entire spectrum of the deconvolved IR).

The **start** is naturally continuous and needs less: `phase(0) = k·(rate⁰ − 1) = 0`, so `sin(0) = 0` exactly, and the initial slope is `2π·f_start = 125.7 rad/s` — a 0.0026 step over one sample at 48 kHz. The fade-in is belt-and-braces (partial buffers, DC offset, `f_start > 20 Hz` configurations), the fade-out is mandatory.

**Requirement.** `paraeq-dsp::sweep::apply_fade(x: &mut [f64], fade_in: usize, fade_out: usize)` applies a raised-cosine (half-Hann) amplitude envelope, `½(1 − cos(π·i/N))` ramping in and its mirror ramping out. Policy defaults in `paraeq-measure`: **10 ms in, 50 ms out.** The asymmetry is principled — 50 ms is ~1000 cycles at the 20 kHz stop frequency, so the fade is adiabatic there, while 10 ms at a 20 Hz start is only 0.2 cycles and a longer fade-in would cost real LF energy for no benefit given the natural zero-start.

Post-fade assertions, enforced in `paraeq-measure` before the buffer reaches the sink:

- First and last samples are exactly 0.0.
- The fade-out is monotonically non-increasing in envelope.
- `|mean(x)| < 1e-4` (DC).
- `max|x| ≤ 1.0` after scaling — clamp, and count any clamp as a defect (it means the solve is wrong).
- Every sample is finite (§ Non-Finite).

## Abort Guards

**Triggers**, any of which stops the stimulus:

| Trigger | Source |
|---|---|
| Esc / Space, or window close | UI |
| Measured SPL exceeds cap mid-sweep | Capture metering |
| >30% of samples in an input block clipped | Capture metering |
| Mic disconnected | `paraeq-coreaudio` listener |
| Output device changed / died | Existing backend listener events |
| Engine enters `Failed` | `EngineStatus::Failed` (controller-owned) |
| Panic anywhere in the session | RAII |

**Speed.** Abort must ramp the stimulus to zero over **~5 ms within one callback block**, and the ramp must begin on the first block after the trigger. **A hard stop is itself a full-scale click** — precisely the defect § Fade exists to prevent, so an abort path that hard-stops re-creates it at the worst possible moment. The 5 ms ramp is a raised-cosine on the sink's output buffer, not a re-render of the stimulus.

**On abort, in this order** — and this is the same shape as the existing tap-teardown invariant, deliberately:

1. Ramp stimulus to zero (≤5 ms).
2. Stop and destroy the measurement IOProc / stimulus stream.
3. Destroy the measurement aggregate (if one was created).
4. **Restore system volume to its pre-measurement value.**
5. Restore the engine's pre-measurement state (bypass flag as it was).
6. Publish the terminating `MeasurementDiagnostic`.

**Every exit path — command, drop, panic — runs the full sequence.** This mirrors CLAUDE.md's standing engine rule ("every exit path runs the full teardown sequence ending in tap destruction — the system must never be left muted") and its implementation shape: `TapSystem` guards with a `torn_down: bool`, collects rather than discards every `OSStatus` failure, logs each, and has a `Drop` impl calling `teardown()` (`tap.rs:198-230`). `MeasurementSession` takes the same shape, with volume restore in the same position tap destruction occupies. **The system must never be left at measurement volume.** A user who quits ParaEQ mid-sweep and then plays music must not be ambushed.

## The Two-Clock Complication

A UMIK-1 runs at its own fixed rate, on its own crystal, against an output device on a different crystal. This is the biggest unscheduled cost in the rescope, and it constrains the safety design directly.

**The existing reassurance does not transfer.** Design spec line 206 says "Farina sweep deconvolution tolerates small skew — empirically proven by the prototype with the same topology." That proof is real but narrow: it was established for an **ungated coupler magnitude measurement**, where a slowly accumulating phase error smears the IR slightly and barely perturbs the magnitude. The rescoped room path needs a **trustworthy t=0 and an undistorted IR shape** — gating, frequency-dependent windowing, and any future time alignment all depend on the IR's temporal integrity. Skew tolerance does not transfer to that regime, and no evidence exists for it at any length other than 5 s.

**Safety consequence.** The obvious way to buy safety margin is to trade level for length: cut 12 dB of level and recover the SNR with 16× the duration. That trade is **unavailable**, for two independent reasons — the skew evidence stops at 5 s, and 16× length is exactly the tweeter-overheating regime REW warns about. **The level ladder must not auto-extend sweep length beyond the 5.5 s class cap.** This is why the cap is in the table rather than being derived.

**Preferred resolution, consistent with the existing architecture:** build the measurement aggregate containing **both** the output device and the mic, with drift compensation enabled — the same mechanism `tap.rs:64-73` already uses for the tap sub-device (`kAudioSubTapDriftCompensationKey: true`). That puts play and record in one clock domain, which is the same move the engine already relies on ("one clock domain," design spec line 89).

**OPEN — must be settled before the room path ships:**

- Drift compensation resamples. Does that resampling perturb the IR enough to matter for gating? Unknown; needs a hardware measurement, not an argument.
- Rate reconciliation: UMIK-1 and EARS are fixed 48 kHz while the output device may be 44.1/96/192 kHz. Sweep generation, deconvolution, and the cal grid must all agree on one rate. This interacts with the known defect that `build_correction(config, channels, block_size)` **takes no sample rate** (verified at `controller.rs:105-117`), so a rate change reinstalls stale coefficients.
- The Farina harmonic constraint bounds any left window: `dt2 = T·ln2/ln(f2/f1)` puts H2 only **501.7 ms** before the peak for a 5 s 20 Hz–20 kHz sweep (verified), so a long left window folds harmonic distortion into the "linear" response. Separately, `deconvolve()` puts the IR peak at only ~46–64 ms (tap latency + propagation), which makes REW's 125 ms left window physically impossible here — clamp left to `min(requested, peak_index)`. Cross-referenced to the DSP spec; noted here because a distorted IR invalidates the SPL verification the caps depend on.

## Non-Finite Values

A NaN is a safety issue, not a numerics nit: a NaN correction is permanently broken audio, and a NaN storm is invisible to the watchdog.

Verified facts:

- `(f32::NAN).clamp(-1.0, 1.0).is_nan() == true`. This is documented, intentional Rust behaviour. **Do not "fix" it at the clamp.** Infinity *is* clamped correctly.
- One NaN poisons the DF2T feedback state permanently (`iir.rs:56-60`: `z[0] = b1*x - a1*y + z[1]` — once `z` is NaN, every subsequent output is NaN forever). The FIR path flushes after its tail instead.
- The existing input scan in `shared.rs::RtProcessor::process_block` compares `if a > peak`, which is **false for NaN**. So a block of NaNs leaves `peak == 0.0`, increments `zero_blocks`, and reads to the watchdog as **silence** — a NaN storm and an unplugged source are indistinguishable today.

**The fix**, lifted from CamillaDSP's `src/utils/conversions.rs` (verified present in the clone), fused into the existing peak pass so it costs one branch per sample:

```rust
for value in wf.iter_mut() {
    if !value.is_finite() { invalid_values += 1; *value = 0.0; }
    // ... existing peak/min tracking
}
if invalid_values > 0 { warn!("Ignored {invalid_values} infinite or NaN values in channel {ch}"); }
```

Applied at **three** boundaries:

1. **Engine capture** — sanitize before filter state, per the pattern above. **Exact placement is owned by `engine-hardening-design.md` R1-2, which verified that `shared.rs::process_block`'s peak scan reads `input: &[&[f32]]` *immutably* and so cannot sanitize in place: the input-side guard therefore lands in `paraeq-coreaudio`'s `backend.rs:174–176` deinterleave copy (where samples first become writable), with an output-side backstop fused into the `chain.rs` gain/clamp loop.** Note honestly that CamillaDSP's own live device paths pass `check_for_nan: false`, so it shares the exposure; the pattern is nonetheless present, tested in-tree, and directly liftable.
2. **Measurement capture** (`paraeq-measure`) — a NaN in the recorded IR propagates through `deconvolve` into NaN correction coefficients.
3. **Coefficient install** — `build_correction` must reject non-finite coefficients before they reach `RealtimeChain`, rather than installing them and discovering it acoustically.

A NaN-triggered `Correction::reset()` is reasonable belt-and-braces (`chain.rs:22-27` exists and is realtime-safe: `fill(0.0)`-only), but **input sanitization is the primary fix** — reset alone re-poisons on the next bad block.

## What Existing Tools Do, and Where We Are Stricter

| Tool | What it does | Where ParaEQ is stricter |
|---|---|---|
| **REW** | −12 dBFS RMS default, −3 dBFS max; sweep widened to `f1/2 … 2·f2` capped at Nyquist; SPL abort is **user-set and optional**; >30% clipped-samples rule; "DO NOT KEEP MAKING THE TEST SIGNAL LOUDER"; warns long sweeps overheat tweeters and are "not recommended" for loudspeakers; notes an SPL limit above the input's clipping point offers no protection | Caps are **mandatory and non-overridable**; coupler paths run **8 dB below** REW's default; the half-start convention is **rejected outright**; the clipping-point validation is enforced at cal load rather than left to the user |
| **Dirac** | Master-output safety lock — the red zone requires a deliberate click; input level window −15…−13 dBFS; noise-floor target −36 dBFS; re-sweeps the first speaker at each position to catch drift | We adopt the safety lock and the level window; we add non-overridable SPL caps and a closed-loop solve rather than a user-driven level |
| **Sonarworks** | Refuses USB microphones outright (killing UMIK-1 and EARS), demanding XLR + phantom + interface; "mono signal detected" check | We support the mics users actually own, which means we must own the safety problem they offload onto the interface |
| **Apple (macOS)** | **Nothing.** Headphone Safety / Reduce Loud Audio (75–100 dB) is iOS/iPadOS only | We are the only backstop that exists |
| **OnlyEQ / iQualize / eqtune** | Ship ParaEQ's exact tap architecture; **none measure** | Not comparable — they emit no stimulus, so they have no exposure |

We borrow REW's imperatives (never fix SNR with output level; the 30% clipping rule) and Dirac's interaction design (safety lock, target band not a number), and we depart from both on overridability, because their audiences are experts and ours will include people who have never seen a sweep before.

## Requirements

Each is independently implementable and testable. `MS-1` … `MS-6` gate stage 6; the rest gate the room path or ship.

| ID | Requirement | Test |
|---|---|---|
| **MS-1** | `crates/paraeq-measure` exists: depends on `paraeq-dsp`, no Tauri dep, no CoreAudio dep, no `unsafe`. Sinks/sources are traits | `cargo tree` assertion in CI; `#![forbid(unsafe_code)]` |
| **MS-2** | `SweepLevel` newtype: sole constructor `SweepLevel::new(dbfs_rms: f64, class: TransducerClass) -> Result<SweepLevel, LevelError>`, refusing above the class cap and above −3 dBFS RMS unconditionally. No other type reaches a `StimulusSink` | Unit: every class's cap rejects cap+0.1 dB; a compile-fail test (`trybuild`) that a bare `f64` cannot be emitted |
| **MS-3** | `paraeq-dsp::sweep::apply_fade` added, additive; `generate_sweep` byte-identical | Analytic invariants (first/last exactly 0.0, monotone fade-out envelope, unity plateau, length preserved) per the `spline.rs` precedent; existing `test_sweep.rs` fixture passes unchanged at 1e-12 |
| **MS-4** | Stimulus emitter clamps to ±1.0 and sanitizes non-finites immediately before the sink | Unit: inject NaN/∞/2.0 into a stimulus buffer; assert output finite and in-range; assert a warn with the count |
| **MS-5** | `paraeq-coreaudio` exposes volume get/set on the default output device, and `MeasurementSession` restores the pre-measurement value on **every** exit path including panic | `#[ignore]` hardware test; a panic-injection unit test against a mock sink asserting restore |
| **MS-6** | `TapSystem` exposes `self_excluded: bool`; `paraeq-measure` refuses to sweep when false (`SelfExclusionUnavailable`) | Unit against a mock reporting `self_excluded: false`; assert refusal before any sample is emitted |
| **MS-7** | Level ladder implemented in order: floor → pilot (300 Hz, −40 dBFS RMS, ≤1 s) → solve → envelope → ≤6 dB rungs with per-rung SPL re-verification → sweep | Unit with a synthetic chain of known sensitivity: assert rung count, that no rung exceeds cap, and that the solve converges to target ±1 dB |
| **MS-8** | SNR gate: median ≥40 dB / min ≥20 dB accept; ≥30 dB warn; ≤2 automatic remedies, and **remedies never touch output level** | Unit: assert the remedy sequence is (input gain, length) and that `SweepLevel` is unchanged across remedies |
| **MS-9** | Refusal on missing/unparseable sensitivity — never a fallback to an uncapped sweep | Unit per refusal row of the table |
| **MS-10** | Cap validated against the mic's full-scale SPL at cal load; refuse `CapExceedsMicFullScale` | Unit with a synthetic low-full-scale cal |
| **MS-11** | ≥6 dB cal-error margin applied; input gain pinned and read back; sensitivity treated as gain-referenced | Unit: assert emitted level ≤ solved − 6 dB when gain read-back disagrees with the cal's reference gain |
| **MS-12** | `f_start` per class, never extended downward; REW's half-start convention not implemented | Unit per class; a test asserting no code path produces `f_start < table value` |
| **MS-13** | Sweep length capped at 5.5 s per class; no auto-extension | Unit: assert an SNR remedy cannot push length past the cap |
| **MS-14** | Abort ramps to zero over ~5 ms starting on the first block after the trigger, then runs the full restore sequence in order | Unit against a mock sink: assert monotone ramp to exactly 0.0 within 5 ms of block time, and assert the restore call order |
| **MS-15** | Non-finite input sanitization at the engine capture boundary (per `engine-hardening-design.md` R1-2: the input-side guard in `backend.rs`'s deinterleave, since `shared.rs::process_block`'s peak scan reads immutable input; counter + off-thread `warn!`) | Unit: a NaN block leaves peak/zero counters correct and does **not** register as silence; a NaN block followed by good blocks yields finite IIR output (regression against the `iir.rs:56-60` permanent-poison path) |
| **MS-16** | `build_correction` rejects non-finite coefficients before install | Unit: NaN in an SOS row returns an error rather than installing |
| **MS-17** | Chain-sensitivity envelope per class; out-of-envelope refuses (`SensitivityOutOfEnvelope`) rather than escalating | Unit: an empty-jig sensitivity refuses at the envelope step, before any rung |
| **MS-18** | Pre-sweep acknowledgement naming device + projected SPL, recorded in the session log; a deliberate action, never a default-focused button | Manual smoke + a state-machine unit test asserting the sweep state is unreachable without the ack |
| **MS-19** | Verification sweeps play from a **helper child process** and are levelled at `L_measure − max(0, peak_correction_gain_db)` | `#[ignore]` hardware test following `test_hardware.rs:35`'s `afplay` shape |
| **MS-20** | `MeasurementDiagnostic`: a stable numbered enum, blocking Errors vs non-blocking Warnings, each mapping to a plain-language fix (easy mode) and an explanation (guided mode) | Exhaustive-match test; a test asserting every variant has both strings |
| **MS-21** | Measurement capture has its own peak + clip metering (the engine's `peak_in` is a monotonic session max with no decay and there is no clip counter) | Unit: assert per-block decay and a clip count that rises and can be reset |
| **MS-22** | Measurement aggregate contains both output and mic with drift compensation; refuse rather than proceed if it cannot be created | `#[ignore]` hardware test |
| **MS-23** | Session log records: solved sensitivity, projected SPL, every rung's measured SPL, final level, cap, class, cal file identity + gain read-back, and the terminating diagnostic — structured, one event per rung | Unit: assert the log is complete and machine-parseable after a synthetic run |

Cross-referenced to the DSP/correction spec but load-bearing for driver safety, and listed so they are not lost: a boost cap on `autofit.rs` (which has **none** today), a computed preamp replacing `peq.rs:112`'s hardcoded `"Preamp: 0.0 dB"` literal, the Trinnov excursion curve as the correction limit table, REW's `Q_max = 0.227·f0/A` boost-Q cap, and the Jury test (`|a2| < 1`, `|a1| < a2 + 1`) promoted from proptest-only into a runtime guard in `biquad.rs`.

## Liability

**I am not a lawyer and this is not legal advice.** The owner should get a real opinion before shipping a stimulus that can reach 100 dB SPL to novices. What follows is the factual picture the corpus assembled, flagged where it is unverified.

- **The MIT "AS IS" disclaimer does not do the work people assume.** Contractual limitations of liability for personal injury are unenforceable or heavily restricted in many jurisdictions. It is a copyright licence, not a product-safety shield.
- **EU PLD 2024/2853** replaces the 1985 directive, and **transposition is due 9 December 2026** — inside this release's life. It treats software as a product and covers personal injury. Per the corpus (**article numbers unverified — confirm with counsel**): Art. 6(1)(a) covers personal injury and Art. 14 voids contractual exclusion, meaning a disclaimer cannot contract out of it.
- **The FOSS carve-out is conditional.** Per the corpus, Art. 2(2) exempts free and open-source software supplied outside commercial activity, and recital 15 removes the exemption for software supplied "in exchange for a price, or for personal data." **The generally understood shape — a FOSS exemption conditioned on not charging — is the part worth planning around even before the article numbers are confirmed.**
- **Noise-induced hearing loss is the archetypal latent injury** these regimes' long windows (reported as 25 years) exist for. It is slow to emerge and hard to attribute, which cuts both ways and is exactly why the question deserves a real answer rather than an optimistic one.

**Action required from the owner, recorded in this spec because it is a design input and not only a business one:** decide the commercial posture explicitly. **Any paid tier plausibly forfeits the FOSS exemption and attaches strict liability that cannot be disclaimed.** If ParaEQ stays free and non-commercial, the exemption is the real protection and the MIT disclaimer is decoration. If a paid tier is ever contemplated, the safety case stops being an engineering preference and becomes a compliance artifact — which changes what this spec has to be, not merely how carefully it is implemented.

**OPEN:** commercial posture undecided. Blocking for a published release, not for implementation.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Stage 6 wires playback without `paraeq-measure`, and the −3 dBFS RMS / REW-maximum sweep becomes real | MS-1/MS-2 land **before** any playback path; `SweepLevel` is the only type a `StimulusSink` accepts, so the hot path does not typecheck |
| Cal sensitivity is wrong (UMIK-1 factory values reported off by >4 dB; Sens Factor captured at 100% input gain while the user runs another) | ≥6 dB margin (MS-11); gain read-back; the ≤6 dB projection-mismatch abort catches gross errors empirically at the first rung, before full level |
| A future session "simplifies" the level onto `generate_sweep` | The decision, the fixture consequence (1e-12 parity break), and the crate-boundary reasoning are recorded above; the fixture test fails loudly if attempted |
| A future session "fixes" tap self-exclusion to make verification sweeps work | The invariant section states the exclusion is the design goal (spec line 147) and gives the correct answer (helper child process, MS-19) |
| Two-clock skew distorts the IR, invalidating the SPL verification the caps rely on | Both-devices aggregate with drift compensation (MS-22); 5.5 s length cap until measured; the gated room path does not ship until the OPEN items close |
| Mic cal parser brittleness reaches the safety path | `compensation.rs:11` dispatches on a leading double-quote and `:60` hardcodes `.skip(2)`; UMIK-1 0-degree files have **one** header line while 90-degree files have **two**, so the first data row is silently dropped on single-header files. Adopt REW's own documented rule: *"Only lines which begin with a number are loaded, others are ignored."* The oracle (`prototype/paraeq/measurement/compensation.py`, `np.loadtxt(skiprows=2)`) **shares the flaw — both must change together or a divergence is manufactured.** (No failure *rate* is claimed: verifiers disagreed on whether real UMIK-1 files ship quoted or bare, and the "80% hard-error" figure is not established) |
| A cal curve gets normalized, baking in a channel imbalance | **Never normalize a cal curve.** EARS encodes a 2.1 dB L/R capsule offset *in* the curve; normalizing to 0 dB at 1 kHz would bake in the imbalance. Write the regression test. Add an outlier validator too: the shipping vendor file `7005770_90deg.txt` contains a bogus `0.0000` at 19.611 Hz between neighbours of −3.13 and −3.11 |
| Spurious mic-permission prompt becomes indistinguishable from ParaEQ's legitimate measurement-mic prompt | OnlyEQ's one-line fix: `kAudioSubDeviceInputChannelsKey: 0` on the aggregate sub-device, "otherwise running our IOProc counts as microphone access and macOS shows a mic permission prompt." ParaEQ's sub-device dict (`tap.rs:57-61`) is a bare `{kAudioSubDeviceUIDKey}`. This also dissolves the known limitation at `backend.rs:121-130`. Now a **safety** issue: a novice trained to dismiss a spurious prompt will dismiss the real one |
| PLD exposure via an unconsidered paid tier | Commercial posture decided and recorded before release; counsel consulted |
| Users perceive the quiet targets as broken ("it's too quiet to hear") | Say "this is supposed to be loud" *before* the sweep, and show a meter target band rather than a dB SPL number (Dirac's precedent: users hit a target, not a figure). Also say "don't touch the volume" — the physical device stays the system default, so volume keys bypass the chain and invalidate cross-position calibration |

## Out of Scope

- **Dosimetry, exposure tracking, listening-time budgets.** At correct levels they would never fire; at incorrect levels the interlocks have already refused. Building them would imply a level of protection that the caps, not the accounting, actually provide.
- **Head/coupler detection.** Not attempted (§ Head Detection). Revisit only with a real sensor.
- **A per-model transducer sensitivity database** (nominal sensitivity → volume → gain tables). The closed-loop solve measures the actual chain, which is strictly better and does not rot.
- **Correction-time limits** (Trinnov excursion curve, boost caps, Q caps, preamp) — cross-referenced here because they are driver-safety-relevant, but they are owned by the DSP/correction spec.
- **Time alignment, subwoofer integration, crossovers** — out of the rescope entirely; clean seams only.
- **MMM / moving-mic acquisition.** A wholly separate path with no IR and no phase; it cannot be gated and cannot feed `fir.rs`'s minimum-phase design.
- **REW import** over its localhost HTTP API — a power-user convenience, and ParaEQ must own its own capture path regardless.
- **Compliance certification.** No certification path exists for an app like this; aligning to named standards is a posture, not a claim.

## Research Basis (2026-07-15)

Verified directly against the repository during authorship (do not re-derive):

- `generate_sweep` has **zero non-test callers** in the workspace (`crates/paraeq-dsp/tests/test_sweep.rs:8` only). No sweep playback path exists in `crates/paraeq-engine/src`, `crates/paraeq-coreaudio/src`, or `desktop/`.
- `prototype/app/wizard/measurement_wizard.py:39`: `SWEEP_AMPLITUDE = 0.5  # -6 dBFS — hearing-safe headroom over full-scale`; applied at line 65; reused for the pink-noise tone at line 296. `prototype/paraeq/measurement/sweep.py` is unscaled, matching `sweep.rs`.
- Computed: unscaled sweep = **−3.01 dBFS RMS**; ×0.5 = **−9.03 dBFS RMS** (= +2.97 dB vs REW's −12 default, −6.02 dB vs REW's −3 max). Terminal sample **−0.5336** (5.0 s/48 kHz) and **−0.9146** (0.25 s fixture). `phase(0) = 0` exactly, so the sweep starts at zero.
- Computed: `ln(10)/ln(1000) = 0.333333…` exactly. Farina `dt2 = T·ln2/ln(f2/f1)` = **501.7 ms** at T=5 s, 20 Hz–20 kHz.
- `tap.rs:26-48` (self-exclusion via `initStereoGlobalTapButExcludeProcesses`, `MutedWhenTapped`, private), `tap.rs:153-159` (empty-list fail-open + the "watch for feedback" warn), `tap.rs:198-230` (`teardown` + `Drop`, error-collecting, `torn_down`-guarded), `tap.rs:57-61` (bare sub-device dict).
- `chain.rs:171` (corrected path) **and** `chain.rs:183` (pass-through path) — the two gain + `.clamp(-1.0, 1.0)` sites, both inside `RealtimeChain::process`. (`backend.rs:93` is a channel-count clamp, not a sample clamp.)
- `shared.rs::RtProcessor::process_block` — the input peak scan (`if a > peak`, NaN-blind), `peak_in` a monotonic session max, no clip counter.
- `iir.rs:56-60` — DF2T feedback state; `chain.rs:22-27` — `Correction::reset`, realtime-safe.
- `controller.rs:105-117` — `build_correction(config, channels, block_size)`, no sample rate.
- `properties.rs` — no volume API exists; workspace-wide grep for `volume` in `crates/` and `desktop/` returns zero hits.
- `compensation.rs:11` (leading-quote dispatch), `:60` (`.skip(2)`); `autofit.rs:71` (Q clamp `0.5..=20.0`, no gain limit); `peq.rs:112` (`"Preamp: 0.0 dB"` literal).
- CamillaDSP `src/utils/conversions.rs` — the `if !value.is_finite() { invalid_values += 1; *value = 0.0; }` pattern with a counted `warn!`, gated by `check_for_nan` (its own live device paths pass `false`).
- `crates/paraeq-coreaudio/tests/test_hardware.rs:35` — the `afplay` child-process player, the reference shape for MS-19.

Corrected upstream of this spec (do **not** reintroduce): the "port dropped SWEEP_AMPLITUDE" regression framing; the "runs at REW's maximum / 9 dB above default" figures; the REW "~30 seconds tweeter drive" citation (fabricated — REW documents no such figure, and its 4M sweep is not a 20 Hz–20 kHz sweep so the 1/3 identity does not apply to it); the "80% of real UMIK-1 files hard-error" statistic (not established); "power averaging is null-immune" (it is null-*resistant*); "an omni mic samples the pressure the ear receives below ~500 Hz" (~300 Hz, and the head diffracts *strongly* at low frequency — that is *why* ILD is small); "room resonances below the transition frequency are minimum-phase" as a blanket (authority must be established per-region via excess group delay, never from a frequency threshold).
