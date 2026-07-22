# Engine Hardening (R1 Safety Floor) — Design Spec

**Date:** 2026-07-15
**Status:** Draft — for owner review
**Amends:** `docs/specs/2026-07-02-rust-port-design.md` (the realtime-pipeline and DSP-core sections; nothing there is superseded, but three of its stated invariants are shown below to be aspirational rather than enforced)

## Summary

The 2026-07-15 rescope moves ParaEQ from headphone correction to a general measurement + correction suite covering couplers (headphones, IEMs) and rooms (bookshelf, floorstanding), with an "auto" front-end that decides everything and shows its work. That rescope does not change the tap architecture — the Mac is always the source, the Core Audio process tap is unchanged and correct — but it does change the *inputs* the DSP and engine see. Headphone EQ mostly cuts, deviates ±10 dB, and always has a human choosing the bands. Room correction routinely boosts +10 dB in the modal region, its residuals contain deep non-minimum-phase nulls that EQ cannot fill, and under "just fix my sound" nobody is looking at the bands before they go live.

This spec is the **R1 safety floor**: ten defects in code that already exists, each of which was tolerable at headphone parity and is not tolerable under the rescope. Items are ranked by severity *under the rescope*, not by intrinsic ugliness. Six block the release; four do not, and this spec says so plainly rather than padding the list — R1-9 (denormals) and R1-10 (convolver partitioning) in particular are documented, ranked, and deliberately deferred with the evidence that justifies deferring them.

Every file:line citation below was read against `main` at commit `179cd34` (and against `feature/rust-port-tauri-shell` where noted). Where the research corpus and the code disagreed, the code won and the discrepancy is called out.

## Decisions Log

| Decision | Choice | Alternatives rejected |
|---|---|---|
| Preamp computation | `preamp = −max(0, peak of the REALIZED cascade)` on a log grid unioned with every band's `fc`; **no headroom constant** | Sum of per-band gains (wrong: bands interact); AutoEQ's exact `−max_gain` (would *boost* a pure-cut EQ); `PREAMP_HEADROOM = 0.2` (that constant is AutoEQ's GraphicEQ/FIR path, not its ParametricEQ path) |
| Preamp carrier | A `preamp_lin` field **inside** `Correction`, applied only on the corrected path | A second atomic alongside `gain_bits` (correction and preamp would swap non-atomically → a window of un-preamped boost → clipping) |
| NaN handling | Sanitize at the **capture boundary** (fused into `backend.rs`'s deinterleave copy) + a fused non-finite guard on the output gain/clamp pass that triggers `Correction::reset()` | Fixing it "at the clamp" (`f32::clamp` returning NaN for NaN is documented and intentional — not a bug); reset-on-input-NaN alone (a click, and unnecessary once input is sanitized) |
| Stability guard placement | Jury test at **build time** in `chain::build_iir`, unstable sections → identity, counted and published | Making `biquad::*` designers return `Result` (they are fixture-pinned to an infallible oracle; the guard belongs at the boundary); a realtime guard (nothing to do about it at that point) |
| Autofit limits | Asymmetric picking + Trinnov excursion curve + REW boost-Q cap, via a **new** `auto_fit_parametric_eq_with_authority`; the existing fn stays a bit-exact wrapper | Changing `auto_fit_parametric_eq` in place (breaks the golden fixture, and "fixtures are sacred") |
| Aggregate input channels | `kAudioSubDeviceInputChannelsKey: 0` on the sub-device dict | A stream-identification heuristic in the deinterleave loop (the known-limitation workaround the one-key fix makes unnecessary) |
| Sample-rate staleness | `design_rate` becomes a field of `CorrectionConfig`; `build_correction` **refuses** a mismatch and publishes a flag | Leaving it to the Tauri forwarder (`eq::resend_decision`) alone — that is a UI policy, not an engine invariant, and it leaves a ~250 ms window of live stale coefficients |
| IIR swap | Transplant `zi` on the realtime thread inside `set_correction` (a bounded memcpy) | A control-plane transplant (the old processor lives on the RT side until the swap) |
| FIR swap | Dual-convolver **linear** crossfade over `n_fft − block_size` samples | Equal-power crossfade (+3 dB bump where the two are coherent — and they are); tail-run-out (cheaper, but wrong in the middle of the window); doing nothing (CamillaDSP does nothing, but it has no source to copy from either — this is ParaEQ's own invention) |
| Meters | Decaying `input_peak` with a controller-supplied per-block coefficient + session max + output peak + clip counter, all fused into existing passes | Controller-side decay (needs an atomic swap-on-read that races the RT store); a clock call on the RT lane (forbidden) |
| Denormals | **Not fixed in R1**, exposure recorded | Blanket FTZ/DAZ now (no evidence it bites; see R1-9) |
| Convolver partitioning | **Deferred** to the FIR room path, behind a measured gate (>25% of the block period) | Uniform partitioning now (0.2–0.4% measured CPU today; a rewrite costs fixture bit-exactness for no benefit); adopting `fft-convolver` unaudited |

## Scope and Sequencing

R1 sits between the stage-4 merge and stage 5. Three hard ordering constraints:

1. **R1-6 lands *after* `feature/rust-port-tauri-shell` merges.** The branch's `desktop/src-tauri/src/eq.rs` already contains the partial fix; changing `CorrectionConfig`'s shape before the merge produces a three-way conflict across `eq.rs`, `engine_bridge.rs`, and `controller.rs` for no gain.
2. **R1-4 depends on `authority.rs`**, a new `paraeq-dsp` module owned by the room-DSP spec. R1-4 is the *consumer*; if `authority.rs` slips, R1-4 slips with it.
3. **R1-8's clip counter is R1-1's falsifier.** Ship them together or R1-1's central claim ("the clamp never engages") is unfalsifiable.

Everything else is independent and parallelizable.

**Status (2026-07-21):** R1-1's DSP half (`ParametricEQ::preamp_db`), R1-2, R1-3, R1-5, and R1-7a landed in Stage 1 on `feature/rescope-stage1`; R1-1's engine half, R1-6, and R1-8 are the post-merge Stage-2 remainder (per `docs/plans/2026-07-16-rescope-implementation.md`).

---

## R1-1 — Auto-preamp

**Defect.** `crates/paraeq-dsp/src/peq.rs:112`:

```rust
let mut lines = vec!["Preamp: 0.0 dB".to_string()];
```

The doc comment above it (`peq.rs:108`) states the fact plainly: *"Preamp is a fixed literal (not computed)."* Nothing in the tree computes a preamp from a filter set.

Partial progress on `feature/rust-port-tauri-shell`: `peq.rs:182` becomes `export_autoeq_format(&self, preamp_db: f64)` and writes the value it is handed (logged as a deliberate divergence in that branch's doc comment); `desktop/src-tauri/src/commands.rs:115` `engine_set_preamp_db` validates a **user-supplied** number via `eq::validate_preamp` and routes it to `EngineCommand::SetGainDb` → `RtShared::set_gain` → the chain's gain stage. So the branch builds the *plumbing* — a preamp value can flow from the UI to the gain stage and to the export text — but **nothing computes one**. The default is whatever the user last typed, and out of the box that is `0.0`.

**Why the rescope raises severity.** The 2026-07-02 spec (line 83) is explicit that the ±1.0 clamp is *"a last-resort protection against filter overshoot beyond the preamp headroom, not a mastering limiter (the AutoEQ preamp convention remains the real headroom mechanism)."* That architecture is sound and the clamp is correctly placed. The problem is that the stated headroom mechanism does not exist: the preamp is a literal on `main` and a user-typed number on the branch. At headphone parity this was survivable because headphone correction mostly cuts and users importing AutoEQ presets got a preamp line in the file. Room modal correction routinely boosts — Trinnov ships ±10 dB of excursion at or below 150 Hz — and the auto front-end generates those boosts with nobody in the loop to notice the missing preamp. The result is the last-resort clamp becoming the primary headroom mechanism: hard clipping on bass transients, invisibly (there is no clip counter — R1-8).

**Fix.**

*1. The computation (`paraeq-dsp::peq`).* Add:

```rust
impl ParametricEQ {
    /// Preamp for this band set, in dB: `-max(0, peak of the realized
    /// cascade)`. Never positive — a pure-cut EQ gets 0.0, not a boost.
    pub fn preamp_db(&self) -> f64 { /* see grid below */ }
}
```

The peak must be taken over the **realized cascade**, not the sum of per-band gains: two +6 dB bands an octave apart sum to more than +6 dB where they overlap, and a +6 dB peak against a −6 dB shelf may not reach +6 dB at all. `peq.rs:91–96` `frequency_response` already computes the single cascaded |H| product (via `biquad::sos_frequency_response_db`, `biquad.rs:71–89`), which is exactly the right quantity — this is a call, not new math.

*Evaluation grid (decided).* The sorted union of:
- a 1/48-octave log grid from 1.0 Hz to `0.499 · sample_rate` (~700 points at 48 kHz — trivial),
- **every band's `fc`**, clamped into that range,
- the two endpoints `1.0` and `0.499 · sample_rate`.

The band-`fc` union is load-bearing: a peaking biquad's own maximum sits at its `fc`, and a Q=20 band at 60 Hz has a ~3 Hz bandwidth that a 1/48-octave grid (1.5% spacing → 0.9 Hz at 60 Hz) resolves, but a Q=20 band at 8 kHz (400 Hz bandwidth, 116 Hz grid spacing) is resolved with much less margin. Including `fc` makes the peaks exact regardless of grid density. The endpoints catch shelves, whose maxima are at DC and Nyquist.

*2. The AutoEQ convention (correcting the corpus).* AutoEQ's `ParametricEQ.txt` preamp is exactly `−compound.max_gain` with **no headroom** (`frequency_response.py:211`). `PREAMP_HEADROOM = 0.2` applies **only** to the GraphicEQ string and to the min/linear-phase FIR impulse responses, and it subtracts from the *normalized equalization curve*, not from the parametric preamp. The README-quoted parametric preamp uses a hardcoded `0.1`, not the constant. **ParaEQ follows the `ParametricEQ.txt` convention: no headroom constant.** `−max_gain` already restores a unity ceiling, so a 0 dBFS source stays at 0 dBFS.

*3. One deliberate divergence from AutoEQ.* AutoEQ's `−max_gain` is signed: a pure-cut EQ has `max_gain < 0`, so AutoEQ emits a *positive* preamp and boosts the whole signal back to unity. ParaEQ clamps at zero (`−max(0, peak)`), so a pure-cut EQ gets `0.0`. Rationale: auto-boosting a source that may already be at 0 dBFS to recover headroom we did not spend is a clipping risk we take on for nothing. This is DIVERGENCES.md entry 14 (headline: *"`ParametricEQ::preamp_db` is clamped at ≤ 0; AutoEQ's is signed"*), and the export text carries the clamped value.

*4. The carrier (`paraeq-engine`).* The preamp must swap **atomically with the correction it protects**. Routing it through `RtShared.gain_bits` would not: the correction arrives via the `rtrb` swap ring (`shared.rs:150–163`) and the gain via a relaxed atomic store, and there is no ordering between them. If the correction lands first, there is a window in which a +12 dB boost runs un-preamped — exactly the case the preamp exists to prevent.

So: `Correction` gains the preamp.

```rust
pub struct Correction {
    kind: CorrectionKind,   // the current enum, renamed
    preamp_lin: f32,
}
```

`chain.rs:171` becomes:

```rust
*o = ((*y as f32) * preamp_lin * gain).clamp(-1.0, 1.0);
```

One extra multiply per sample on a loop that already exists. Critically, **the preamp applies only on the corrected path** — `chain.rs:181–185`'s pass-through (bypass, `frame_mismatch`, no correction) must not attenuate, because there is no boost to compensate. The existing two-path structure in `chain.rs:136`/`:178` gives this for free.

*5. Wiring.* `CorrectionConfig` grows nothing new: the controller computes `preamp_db` from the config at `build_correction` time (`controller.rs:105`) and hands it to `build_iir`/`build_fir`. For the FIR arm, "the realized cascade" is the FIR's own magnitude response — `preamp_lin = 1.0 / max(1.0, max|H(f)|)` over the same grid, computed with one FFT of the tap vector on the control plane.

*6. Visibility (the rescope's "each decision visible" requirement).* `EngineState` gains `auto_preamp_db: Option<f32>`. The Advanced drawer shows "we pulled you down 9.4 dB to make room for the 45 Hz boost." The auto front-end must never apply a number it cannot explain.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| Single +10 dB Q=1 peaking @ 1 kHz | `paraeq-dsp/tests/test_peq.rs` | `preamp_db() ≈ −10.0` within 0.02 dB |
| Two overlapping +6 dB bands @ 100/120 Hz | `paraeq-dsp/tests/test_peq.rs` | `preamp_db() < −6.0`, and equals `−max` of a dense 1/384-octave `frequency_response` sweep within 0.02 dB |
| Pure-cut band set | `paraeq-dsp/tests/test_peq.rs` | `preamp_db() == 0.0` **exactly** (pins the divergence) |
| High-Q band off the log grid (Q=20 @ 8137 Hz) | `paraeq-dsp/tests/test_peq.rs` | `preamp_db()` matches the dense sweep — proves the `fc` union carries its weight |
| +12 dB band, full-scale sine at `fc`, auto-preamp live | `paraeq-engine` synthetic-block | `clipped_samples == 0` (**requires R1-8** — this is the falsifier) |
| Export/engine agreement | `desktop/src-tauri` unit | The number in the exported text equals `EngineState.auto_preamp_db` |

**Effort:** ~1 day. **Blocks release: YES.**

---

## R1-2 — NaN sanitization at the capture boundary

**Defect.** Verified directly: `(f32::NAN).clamp(-1.0, 1.0).is_nan() == true`. This is documented, intentional Rust behavior — `f32::clamp` propagates NaN and is not a sanitizer. Infinity *is* clamped correctly (±inf → ±1.0). So `chain.rs:171` and `:183` stop overload but not NaN.

Downstream, the two arms behave differently:

- **IIR poisons permanently.** `iir.rs:56–60` is a DF2T section: `z[0] = b1*x − a1*y + z[1]; z[1] = b2*x − a2*y`. One NaN `x` makes `y` NaN, which makes both `z` NaN, and every subsequent output is NaN for the life of that `IIRProcessor`. Nothing clears it in normal operation: `Correction::reset()` (`chain.rs:22–28`, `iir.rs:66–72`) fires only on the bypass→active edge (`chain.rs:114–118`) or a `frame_mismatch` (`chain.rs:193–195`).
- **FIR self-heals.** `convolver.rs:142` `self.overlaps[ch].copy_from_slice(&self.full_scratch[self.block_size..])` **overwrites** the tail each block rather than accumulating forever, so NaN flushes after one tail length (`n_fft − block_size` samples).

Core Audio taps deliver f32 — the format class that *can* encode NaN. CamillaDSP gates its sanitizer per-format for exactly this reason (integer formats cannot encode NaN, so the check is skipped there); we are unconditionally in the exposed class.

**Honest framing:** this is defensive, not a reproduced bug. ParaEQ's own math does not generate NaN on the realtime path (R1-3 closes the design-side hole that could). The exposure is a misbehaving source app writing NaN into the system mix, an uninitialized HAL buffer, or a driver bug. The cost of the fix is one predictable branch per sample; the cost of being wrong is permanent silence-or-noise until the user restarts the app.

**Fix — two fused guards, zero new passes.**

*Guard 1: the capture boundary.* A correction to the corpus first: the existing peak-detection pass is **not** in `paraeq-coreaudio`. It is in `paraeq-engine/src/shared.rs:224–232` (`RtProcessor::process_block`), and it reads `input: &[&[f32]]` — immutable, so it cannot sanitize in place without an API change. The place where samples first become ours *and* are already being written is `backend.rs:174–176`, the deinterleave copy:

```rust
let dst = &mut in_scratch[ch][..f];
for (i, d) in dst.iter_mut().enumerate() {
    *d = data[i * bc + c];
}
```

Fuse the CamillaDSP pattern (`src/utils/conversions.rs:89–106`) into it:

```rust
for (i, d) in dst.iter_mut().enumerate() {
    let v = data[i * bc + c];
    if !v.is_finite() { invalid += 1; *d = 0.0; } else { *d = v; }
}
```

Then, after the deinterleave, `processor.note_invalid_samples(invalid)` — a relaxed atomic add on a new `RtShared::invalid_samples: AtomicU64`. **The `warn!` moves off the realtime thread**: `controller.rs` `on_tick` reads the counter, rate-limits, and logs `"ignored {n} non-finite input samples"`. This is a deliberate improvement over CamillaDSP, which `warn!`s from inside the audio callback (its own live device paths pass `check_for_nan: false`, so it shares the exposure — the pattern is liftable, the placement is not).

*Guard 2: the output.* Guard 1 makes it impossible for NaN to reach the filter state *from the input*. It does not cover NaN generated *inside* the filter (a pathological coefficient — R1-3's territory, but a backstop costs nothing here). `chain.rs:169–172` already iterates every output sample to apply gain and clamp; fuse the check into that same loop:

```rust
let v = (*y as f32) * preamp_lin * gain;
if v.is_finite() { *o = v.clamp(-1.0, 1.0); } else { nonfinite += 1; *o = 0.0; }
```

and if `nonfinite > 0` at the end of the block, call `self.correction.as_mut().reset()` — `reset` is `fill(0.0)`-only and realtime-safe (`chain.rs:22–28`). This self-heals a poisoned DF2T within one block instead of forever. It *is* an audible discontinuity, which is why it is a backstop and not the primary fix — but a click beats permanent noise, and the published counter tells us it happened.

**Explicit non-decision:** we do **not** reset on input NaN. Once Guard 1 replaces it with 0.0 before the filter, there is nothing to reset, and a reset would be a gratuitous click.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| Block with one `f32::NAN`, IIR correction active | `paraeq-engine` synthetic | Output all-finite; the **next** clean sine block is bit-correct (state not poisoned); `invalid_samples` incremented by exactly 1 |
| Block with ±inf, pass-through path | `paraeq-engine` synthetic | Output is exactly ±1.0 (documents the pre-existing correct behavior so it cannot regress) |
| Fabricated unstable SOS (bypasses R1-3 in-test) driving output to inf | `paraeq-engine` synthetic | Guard 2 fires, correction reset, counter incremented, output finite |
| Deinterleave sanitize over a synthetic `AudioBufferList` | `paraeq-coreaudio/tests/test_buffers.rs` (non-hardware, already exists) | NaN in → 0.0 out, count == 1, finite samples untouched bit-for-bit |

**Effort:** ~0.5–1 day. **Blocks release: YES.**

---

## R1-3 — Runtime stability guard

**Defect.** The four `biquad.rs` designers — `peaking:15`, `low_shelf:28`, `high_shelf:42`, `notch:56` — return `[f64; 6]` infallibly. There is no Q clamp, no `fc`/Nyquist validation, no `Result`. Concrete failure modes:

- `wa()` (`biquad.rs:6–9`) computes `alpha = sin(w0) / (2q)`. At `q = 0`, `alpha = inf`; `row()` (`:11–13`) then divides `inf/inf` → **NaN coefficients**.
- At `fc = sample_rate/2` exactly, `w0 = π`, `sin(w0) ≈ 1.2e-16`, `alpha ≈ 0`, and `peaking`'s denominator collapses to `1 + 0` — a degenerate, coefficient-noise filter.
- At `fc > sample_rate/2` the math is finite but meaningless (aliased around Nyquist).

`tests/test_props.rs:7–42` has four stability proptests (64 cases each) over `fc ∈ 20..20000`, `gain ∈ −24..24`, `q ∈ 0.1..20` at 48 kHz. They prove the designers are stable **inside that box** and say exactly nothing outside it. The only wall today is `desktop/src-tauri/src/eq.rs:41` `validate_bands` (`fc ∈ (0, sample_rate/2)` exclusive) — and it lives in the **Tauri crate**. Its own `design_correction` doc says so: *"Callers MUST have run `validate_bands` at this same `sample_rate` first — this function designs coefficients unconditionally and does not itself guard against `fc >= Nyquist` / `q = 0` producing NaN/Inf."* The daemon-ready seam the 2026-07-02 spec promises (line 75) inherits none of it.

**Why the rescope raises severity.** Two new inputs escape the proptest box:

1. **The auto front-end designs from a measured room curve with no human in the loop.** `autofit.rs:30–35` takes `fc` straight from `freqs[idx]` — a raw FFT bin, bounded only by the hardcoded `20.0..=20000.0` mask (`autofit.rs:13`). At 44.1 kHz, Nyquist is 22050 Hz and the mask permits 20 kHz — fine. At a hypothetical low-rate device, or once the mask becomes a parameter (R1-4), that guarantee evaporates.
2. **The two-clock problem.** A UMIK-1 runs at its own fixed rate while the output device runs at another. A band designed at one rate and installed at another (R1-6) can land above Nyquist.

**Fix — two layers.**

*Design-side (primary).* REW's boost-Q cap `Q_max = 0.227 · f₀ / A`, `A = 10^(G/40)`, derived from a 500 ms T60 rule and verified against ParaEQ's own `biquad.rs` coefficients. Applied to **boosts only**, it gives stability by construction *and* is the musical limit we want anyway. It lives in `authority.rs` and is applied by autofit — see R1-4.

*Build-side (backstop).* A Jury / Schur–Cohn check in `paraeq-dsp::biquad`:

```rust
/// Jury stability conditions for a real-coefficient second-order section:
/// both poles strictly inside the unit circle.
pub fn is_stable(sos: &[f64; 6]) -> bool {
    let (a1, a2) = (sos[4], sos[5]);
    sos.iter().all(|c| c.is_finite()) && a2.abs() < 1.0 && a1.abs() < a2 + 1.0
}
```

(This is the strict form; `test_props.rs:13–14` uses the same two conditions with `1e-12`/`1e-9` slack for its *design* assertions. A designed filter should sit inside with margin; an *installed* filter must satisfy the strict form.)

**Applied in `chain::build_iir`** (`chain.rs:234–251`) — the single funnel every SOS row passes through on its way to the realtime thread. Sections failing `is_stable` are replaced with the identity section `[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]` and counted; `build_iir` returns the substitution count so `controller.rs` can `log::warn!` and publish it.

*Why identity, not error:* the auto front-end must never be bricked by one bad band. Dropping a band degrades audibly only as "that band did nothing," and the published count makes it visible in the Advanced drawer. An error would mean *no correction at all* from one bad row.

*Why not `Result` on the designers:* they are golden-fixture-pinned against the Python oracle (`tests/test_biquad.rs`), the oracle itself is infallible, and a signature change churns every call site to buy a guarantee we can enforce more cheaply at the one boundary that matters.

**DECIDED (owner, 2026-07-22):** `validate_bands`'s range check moves out of `desktop/src-tauri/src/eq.rs` into `paraeq-engine`, next to `validate_correction` (`controller.rs:80`), so the daemon seam inherits it and `paraeq-dsp` stays free of policy. See docs/decisions/2026-07-22-owner-value-calls.md. This is **distinct from** decision-engine Q2 (whether the four designers themselves become fallible — decided **No**: the guard stays at the install boundary, see `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q2); that question is settled, and this one is only about *where the range check physically lives*.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| Hostile-box proptest: `fc ∈ 0..1e6`, `gain ∈ −60..60`, `q ∈ 1e-6..1e6`, `sr ∈ {44100, 48000, 96000}` | `paraeq-engine/tests` | `build_iir` never panics and never installs a section failing `is_stable` |
| `q = 0.0` | `paraeq-dsp` + `paraeq-engine` | Designer yields NaN coefficients; `build_iir` substitutes identity, count == 1 |
| `fc == sr/2` exactly; `fc > sr/2` | `paraeq-engine` | Identity substitution, counted |
| Substitution count reaches `EngineState` | `paraeq-engine` synthetic | Count published, not silently swallowed |
| Existing `test_props.rs:7–42` | unchanged | Still green — the design-side box is unaffected |

**Effort:** ~1 day. **Blocks release: YES.**

---

## R1-4 — Boost caps in autofit

**Defect.** `crates/paraeq-dsp/src/autofit.rs`, three separate holes:

- **`:13`** — `let mask: Vec<bool> = freqs.iter().map(|f| (20.0..=20000.0).contains(f)).collect();` The audible band is hardcoded.
- **`:19`** — `if mask[i] && residual[i].abs() > peak_abs` — candidates are ranked by **symmetric absolute value**. A −30 dB residual outranks a +6 dB one.
- **`:30–35`** — `gain_db: peak_gain` — the band takes the **full** residual. There is no gain limit anywhere in the file.
- **`:71`** — `(f_center / (hi - lo)).clamp(0.5, 20.0)` — a single global Q clamp, independent of gain and of frequency.

So a +20 dB residual at 45 Hz becomes a +20 dB, Q≤20 boost at 45 Hz.

**Why the rescope raises severity — this is the highest-severity DSP defect in the tree.** Against a headphone curve this was survivable: headphone deviations are broadly smooth, and the residual rarely contains a deep null. Against a **room** curve at a listening position, the deepest feature in the response is a modal **dip** — destructive interference between direct sound and a reflection, which is *not* minimum-phase and is *physically uncorrectable by EQ*. Symmetric picking finds it first, and the uncapped gain tries to fill it at maximum boost. What actually happens: enormous power delivered into a cancellation that does not fill, driver excursion, and clipping. This is precisely what Toole warns automated algorithms do.

The distinction that survives adversarial review and that this fix encodes: **modal peaks are poles — locally minimum-phase — and a matched parametric filter corrects amplitude and phase together, reducing ringing at the measurement position. Modal dips are destructive interference — non-minimum-phase — and EQ cannot fix them.** Authority must be established *per region*, via excess group delay, never assumed from a frequency threshold; "full authority below 200 Hz" is unsafe as a blanket rule. That per-region machinery is the room-DSP spec's job (`authority.rs`); R1-4 is the consumer that stops autofit from doing damage in the meantime.

**Sign convention (get this right).** `correction_db` is what the EQ must **add**. A room **peak** → correction is **negative** (a cut). A room **dip** → correction is **positive** (a boost). So *negative residual = cutting a peak = safe and free; positive residual = filling a dip = restricted*.

**Fix — four limits, all cheap, all from `authority.rs` (cross-ref: the room-DSP spec).**

*1. Asymmetric picking.* Replace the `residual[i].abs()` ranking with a signed score:

```rust
let score = if residual[i] < 0.0 { -residual[i] } else { residual[i] * BOOST_WEIGHT };
```

`BOOST_WEIGHT ≈ 0.5` — **OPEN [OWNER]:** needs the owner's ears; the interim safe default (symmetric Trinnov curve, reject boosts above Q=3) stands. Plus a hard rule: **reject any positive-gain candidate whose estimated Q exceeds a narrow-dip threshold** (skip it and zero that residual bin so the greedy loop moves on). Narrow dips are interference; wide dips may be real response. Proposed threshold `Q > 3.0` — **OPEN [OWNER]:** needs the owner's ears; the interim safe default (symmetric Trinnov curve, reject boosts above Q=3) stands.

*2. Frequency-indexed excursion curve* — Trinnov's shipped values: **±10 dB at or below 150 Hz, tapering to ±2 dB by 500 Hz, ±2 dB above.** Implemented as `authority::excursion_db(f) -> f64`, linear-in-dB over log-f between 150 and 500 Hz. Applied as `gain_db = peak_gain.clamp(-cut_limit, excursion_db(fc))`.

*Cut limit:* cuts cannot clip and cannot over-excurse a driver — they only remove energy — so a symmetric application of Trinnov's curve is stricter than necessary. Proposed: `cut_limit = 2 × excursion_db(fc)` (so −20 dB below 150 Hz, −4 dB above 500 Hz). **OPEN [OWNER]:** the honest, defensible default until the owner has listened is to apply Trinnov's curve **symmetrically** exactly as shipped and relax only on evidence.

**Note — the autofit *shape* these three constants live in is itself an open cross-spec reconciliation, not settled here.** Which function signature owns them (`auto_fit_parametric_eq` mutated in place / `auto_fit_parametric_eq_with_authority` added / a per-channel `auto_fit_room`) is tracked in `docs/plans/2026-07-16-rescope-implementation.md` (cross-spec question 2, blocks Stage 5). And room-dsp's Open Q4 (`boost_ratio = 0.5`, `min_dip_width_oct = 1/6`) is **the same decision** as this section's `BOOST_WEIGHT` and narrow-dip Q threshold — the two specs must reconcile to one definition together, not be tuned independently.

*3. REW boost-Q cap* — `Q_max = 0.227 · f₀ / A`, `A = 10^(G/40)`, boosts only, replacing the blanket `0.5..=20.0` for positive-gain bands. Worked values:

| f₀ | G | A | Q_max |
|---|---|---|---|
| 40 Hz | +10 dB | 1.778 | 5.11 |
| 60 Hz | +6 dB | 1.413 | 9.64 |
| 150 Hz | +10 dB | 1.778 | 19.2 |
| 1000 Hz | +6 dB | 1.413 | 160.7 |

Note the cap is effectively **non-binding above the modal region** — which is correct: it is a bass-ringing limit derived from a 500 ms T60 rule, not a general Q policy. It also doubles as R1-3's design-side stability guarantee.

*4. The `20..=20000` mask becomes a parameter.* It is wrong for the room path in both directions, and the correct upper edge depends on the rate and on the σ(f)-derived authority curve (room-DSP spec).

**API shape — additive, so the fixture stays sacred.** `autofit.rs` is golden-fixture-tested (`tests/test_autofit.rs`). Keep `auto_fit_parametric_eq` as a thin wrapper that passes the old defaults (unlimited gain, global Q clamp, 20–20000 mask) and add:

```rust
pub fn auto_fit_parametric_eq_with_authority(
    correction_db: &[f64],
    freqs: &[f64],
    sample_rate: f64,
    max_bands: usize,
    min_gain_db: f64,
    authority: &authority::AuthorityCurve,
) -> Vec<EQBand>
```

The existing fixture stays bit-exact through the wrapper; the new behavior is additive. This is the same pattern the room-DSP spec applies to smoothing (`Fixed(n)` stays bit-exact on the old path; new modes are additive) — and, as `spline.rs` (commit `309d71d`) already demonstrates, a `paraeq-dsp` module verified by analytic invariants rather than a generated fixture is an established, merged precedent, not a rule violation. "Fixtures are sacred" is a **provenance** rule whose own text sanctions editing the generator; it does not require every module to have a fixture.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| Existing `test_autofit.rs` | unchanged | Bit-exact through the wrapper |
| Trinnov curve pin | `paraeq-dsp/tests` | `excursion_db` is monotone non-increasing; `== 10.0` at 150 Hz; `== 2.0` at 500 Hz and at 20 kHz |
| +30 dB narrow dip-fill demand @ 45 Hz | `paraeq-dsp/tests` | No emitted band has `gain > 10.0`; none has `Q > 0.227·45/10^(10/40) = 5.75` |
| −20 dB cut demand @ 60 Hz vs +20 dB fill demand @ 65 Hz | `paraeq-dsp/tests` | The **cut** is selected first (asymmetric ranking) |
| Narrow-dip rejection | `paraeq-dsp/tests` | A Q=8 positive residual emits **no** band; a Q=1 positive residual does |
| Proptest over arbitrary residuals | `paraeq-dsp/tests` | Every emitted band satisfies `|gain| ≤ limit(fc)`, and every **boost** satisfies `Q ≤ 0.227·fc/10^(gain/40)` |

**Effort:** 2–3 days, **gated on `authority.rs`** from the room-DSP spec. **Blocks release: YES.**

---

## R1-5 — `kAudioSubDeviceInputChannelsKey: 0`

**Defect.** `crates/paraeq-coreaudio/src/tap.rs:57–61`:

```rust
// sub-device entry: { uid: <output device UID> }
let sub_dev: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
    &[&*key(kAudioSubDeviceUIDKey)],
    &[out_uid.as_ref() as &CFType],
);
```

One key. OnlyEQ's one-line fix adds `kAudioSubDeviceInputChannelsKey: 0`, with the reason stated in its own source: *"otherwise running our IOProc counts as microphone access and macOS shows a mic permission prompt."*

**Two consequences, both now serious.**

*1. It dissolves the documented KNOWN LIMITATION.* `backend.rs:121–130` says it outright:

> KNOWN LIMITATION — input-stream identification: the deinterleave below assumes the tap's stream(s) are the only (or first) buffers in the aggregate's input list. A default output device that ALSO exposes input streams (AirPods, USB headsets with mics) may contribute mic buffers whose position in the input buffer list is undocumented; if such a buffer preceded the tap stream, mic samples would be treated as tap input.

The deinterleave (`backend.rs:145–179`) walks `block.input.buffers()` in list order and fills `in_scratch` from the front. If a mic buffer came first, **ParaEQ would play the user's microphone back through their headphones.** Setting the sub-device's input channels to 0 removes those buffers from the aggregate entirely — the limitation is *dissolved*, not heuristically worked around. No stream-identification logic is needed, and the comment block gets deleted rather than expanded.

*2. It removes a spurious microphone permission prompt — which now matters disproportionately.* Under the rescope, ParaEQ **legitimately** asks for microphone access: the measurement mic (UMIK-1, EARS, any mic with a cal file). A user who has already been shown an unexplained mic prompt at engine start cannot distinguish the legitimate one from the spurious one. The entire "just fix my sound" front-end rests on the user trusting the prompt the app raises at the moment it raises it. A spurious prompt poisons the wizard. (Sonarworks, by contrast, *refuses* USB microphones outright, which kills both the UMIK-1 and the EARS — the mic path is a competitive advantage we cannot afford to make creepy.)

**Fix.** In `create_aggregate` (`tap.rs:57–61`), two keys, alphabetically ordered per repo convention:

```rust
let sub_dev: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
    &[
        &*key(kAudioSubDeviceInputChannelsKey),
        &*key(kAudioSubDeviceUIDKey),
    ],
    &[
        CFNumber::new_i32(0).as_ref() as &CFType,   // NOT CFBoolean
        out_uid.as_ref() as &CFType,
    ],
);
```

**VERIFY AT IMPLEMENTATION TIME (2-minute check):** that `kAudioSubDeviceInputChannelsKey` is re-exported by `objc2-core-audio` 0.3.x. `kAudioSubDeviceUIDKey` and `kAudioSubTapDriftCompensationKey` already are (`tap.rs:12–13`), so the constant family is present, but this specific key is not currently imported anywhere in the tree. If it is absent, define the CFString literal locally with a comment citing `AudioHardware.h`. Note the value is a **CFNumber**, not a CFBoolean — every other boolean-ish key in this dictionary uses `CFBoolean::new(...)` (`tap.rs:94–96`) and copying that pattern here would be wrong.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| Aggregate input-stream configuration | `paraeq-coreaudio/tests/test_hardware.rs`, `#[ignore]` | With a mic-capable default output selected, the aggregate reports **zero** input buffers beyond the tap's |
| Single-prompt manual check | `docs/CONTEXT.md` checklist | On a fresh TCC state, exactly **one** prompt appears (System Audio Recording); **no** microphone prompt |
| Comment deletion | the diff itself | `backend.rs:121–130` is removed in the same commit — the code comment is part of the deliverable |

**Effort:** ~1 hour plus a hardware run. **Blocks release: YES** — the mic-playback path is a real data path on common hardware, and the trust argument is a release-quality argument, not a polish argument.

---

## R1-6 — Sample-rate coefficient staleness

**Defect.** `controller.rs:105`:

```rust
pub fn build_correction(config: &CorrectionConfig, channels: usize, block_size: usize) -> Correction
```

No sample rate. `CorrectionConfig::Iir { sos_per_channel }` (`controller.rs:58`) carries designed **coefficients**, not design *intent*, and the rate they were designed at is recorded nowhere in `paraeq-engine`. Meanwhile `backend.rs:284` maps `kAudioDevicePropertyNominalSampleRate` → `BackendEvent::FormatChanged`, `controller.rs:492–497` turns any event into a `rebuild()`, and `rebuild()` → `try_start()` → `start_with()` → `controller.rs:583–586` faithfully re-sends **the same coefficients** at the new rate.

The realized filter lands at `fc' = fc · (new_rate / design_rate)`:

- A 48 kHz design run at 44.1 kHz lands **8.1% low** (a 1000 Hz band becomes 919 Hz).
- A 44.1 kHz design run at 48 kHz lands **8.8% high** (a 1000 Hz band becomes 1088 Hz).

(Both directions are wrong by the same audible amount; state the ratio, not a single percentage — the corpus's bare "8.8%" is only the second case.) Q and bandwidth shift with it.

**Status: PARTIALLY CLOSED on `feature/rust-port-tauri-shell` — verified.** `desktop/src-tauri/src/eq.rs:120–134` `resend_decision(last_rate, snapshot, have_bands)` returns `Some(new_rate)` when the snapshot's stream rate differs from the last seen (or on the first-ever stream); `engine_bridge.rs:143–159` re-validates the bands at the new rate and re-sends `SetCorrection(design_correction(&bands, rate))`, or `ClearCorrection` on validation failure. It is unit-tested at `eq.rs:452–482` (first stream, unchanged, changed, no-bands, no-stream). **Verify this before merge and keep those tests** — they are the regression guard the task asks for.

**What remains open after the merge — the engine is still structurally rate-blind:**

**DECIDED (2026-07-21):** the rate-independence *shape* is settled — adopt decision-engine fix (1): `CorrectionConfig` carries the design *inputs* (`bands` + `design_rate`) and `build_correction` re-derives coefficients at the live rate on every rebuild, so a correction normally *survives* a rate switch rather than being refused (a 47 Hz mode filter stays at 47 Hz across an AirPods 44.1↔48 kHz handoff). R1-6's refuse-on-mismatch + fail-open-to-flat is retained as the last-resort net for configs that genuinely cannot be re-derived (a legacy baked-SOS `Iir { sos_per_channel }`). See `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q1. The three structural gaps this fix must close remain:

1. **The fix lives in the Tauri crate.** `paraeq-engine` is the daemon-ready crate (2026-07-02 spec, line 75). Any other consumer — a future `paraeqd`, the auto front-end calling the engine directly — gets no protection. The forwarder is a *policy*; the engine needs an *invariant*.
2. **It only covers the band path.** `eq.rs:97` `design_correction` only ever emits `CorrectionConfig::Iir`; stage 4 has no FIR path. The FIR room correction (stage 6) designs taps against a frequency grid, and a rate change makes those taps wrong the same way — but `resend_decision` would have nothing to redesign *from*, because the Tauri layer would be holding **taps**, not intent.
3. **It is reactive, with a live window.** `controller.rs:583–586` re-sends the old correction *inside* `start_with`, **before** any snapshot is published. The forwarder only learns about the rate change from that snapshot. So for one snapshot round-trip — up to `tick_ms` (250 ms default) plus the forwarder's `recv` — the stale coefficients are **live and audible**.

**Fix — make it an engine invariant.**

1. Hoist the rate into the config:

```rust
pub struct CorrectionConfig {
    pub design_rate: f64,
    pub kind: CorrectionKind,   // Fir { firs } | Iir { sos_per_channel }
}
```

2. `build_correction(config, channels, block_size, stream_rate) -> Result<Correction, EngineError>`, refusing on `config.design_rate != stream_rate`. **Exact comparison**, not a tolerance: these are device-reported f64s (`48000.0`, `44100.0`) that round-trip exactly, and a fuzzy compare would silently accept a genuinely different rate. Extend `validate_correction` (`controller.rs:80`) with the same check.

3. On refusal the controller sends **no** correction — flat pass-through. Audibly un-EQ'd is always better than audibly wrong-EQ'd, and it is the same fail-open philosophy as the existing `AutoDisabledNoInput` path (`controller.rs:474–487`).

4. `EngineState` gains `correction_rate_mismatch: Option<f64>` — the stream rate the correction must be redesigned for. `resend_decision` then keys off **that field** instead of diffing rates itself. Same behavior for the UI, but now the *engine* is the one that noticed, so the daemon seam inherits it and the window in (3) closes: the refusal happens in the same call that currently installs the stale coefficients.

**Why this matters beyond correctness.** The 2026-07-02 spec (line 93) claims rebuild-on-change "covers unplug, AirPods handoff, sample-rate switches," and the rescope's pitch is "the tap follows your device automatically." Today the first two are true and the third silently detunes every filter by ~8%. This attacks the differentiator directly: it must be true before it is said.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| Synthetic backend reporting 48000 then 44100 | `paraeq-engine` | `build_correction` refuses; the chain runs **pass-through** (feed an impulse, assert output == input × gain — *not* stale-corrected); `correction_rate_mismatch` is published |
| Rate unchanged | `paraeq-engine` | Correction installs normally, no flag |
| `resend_decision` suite (`eq.rs:452–482`) | `desktop/src-tauri` | Retained, re-pointed at the new flag. **Whoever merges the branch must not delete these.** |
| Hardware | `docs/CONTEXT.md` checklist | With a +12 dB 1 kHz band live, change the device's rate in Audio MIDI Setup; sweep the analyzer and confirm the band is still at 1 kHz, not 919 Hz |

**Effort:** 1–2 days. **Sequence after the stage-4 merge.** **Blocks release: YES.**

---

## R1-7 — Filter-state preservation on swap

**Defect.** `chain.rs:88–90`:

```rust
pub fn set_correction(&mut self, c: Option<Correction>) -> Option<Correction> {
    std::mem::replace(&mut self.correction, c)
}
```

The incoming `Correction` is built by `build_iir`/`build_fir` (`chain.rs:221–251`), which warm it up (`warm_up`, `:209–218`) and then explicitly `correction.reset()`. So the installed processor's state is **zero by construction**:

- **IIR:** `iir.rs:23` `let zi = vec![[0.0; 2]; sos.len()]`. The DF2T delay lines restart from zero, so the filter's response to prior input vanishes — a step discontinuity, worst at high Q and low frequency where the stored state is largest.
- **FIR:** the overlap tail (`convolver.rs:70`, `:97`, length `n_fft − block_size`) is discarded. At the stage-4 geometry (512-frame block) that is 3584 samples for a 4096-tap FIR and **32256 samples for a 16384-tap room FIR**. The new block's output starts from a convolution with no history, so the first `fir_len − 1` samples are missing the previous block's contribution — a click.

**When does it fire?** Every `SetCorrection`. That means every band edit — *including a drag on the EQ curve*, which is exactly what the stage-4 EQ tab is — every AutoEQ import, every profile switch, and every rate-change redesign (R1-6). The auto front-end will swap corrections constantly.

**Note the mitigating context, because it matters for the diagnosis.** `chain.rs:114–118` deliberately resets on the bypass→active edge (*"so no stale transient thumps into the re-engaged output"*) and `:193–195` on a `frame_mismatch` (*"the samples it never saw make its overlap tail / biquad state stale"*). **Both are correct** — they follow a discontinuity that already happened. A swap is different: nothing discontinuous happened, and preserving state is both possible and correct.

**Reference.** Apple's `Equaliser` sample uses `BiquadFilter.setCoefficients(_:setup:resetState:)` with `resetState: false` on incremental edits, which preserves the delay state — that is how it avoids clicks *without* a crossfade. CamillaDSP does not crossfade either, so for the FIR arm **there is no source to copy: this is ParaEQ's own design.**

**Fix — split by arm, because the effort and the release gate differ.**

### R1-7a — IIR state transplant (cheap, exact)

A swap is almost always a coefficient change on the same topology. Add:

```rust
impl IIRProcessor {
    /// Replace coefficients, preserving delay state. Sections beyond the
    /// old cascade's length start zeroed; state beyond the new length is
    /// discarded. Realtime-safe: no allocation (zi is already sized).
    pub fn adopt_state_from(&mut self, old: &IIRProcessor);
}
```

and on `Correction`, a kind-aware `adopt_state_from(&mut self, old: &Correction)` that is a no-op across kinds (Fir↔Iir) and across a channel-count change. Then `chain.rs:88` becomes:

```rust
pub fn set_correction(&mut self, mut c: Option<Correction>) -> Option<Correction> {
    if let (Some(new), Some(old)) = (c.as_mut(), self.correction.as_ref()) {
        new.adopt_state_from(old);
    }
    std::mem::replace(&mut self.correction, c)
}
```

**Is this realtime-safe?** Yes. The transplant is a bounded memcpy over `zi` — `Vec<[f64; 2]>`, one entry per section per channel. A 10-band stereo EQ is 40 f64 = 320 bytes. No allocation occurs because `build_iir` already sized the new `zi` (`iir.rs:23`). It happens on the RT thread inside `RtLink::poll` (`shared.rs:150–163`), which is where the old processor still lives — a control-plane transplant is impossible for exactly that reason.

### R1-7b — FIR dual-convolver crossfade

An overlap tail **cannot** be transplanted: the tail is `x_prev ⊛ h_old`, and the new filter's history should be `x_prev ⊛ h_new`, which we do not have and cannot cheaply reconstruct.

**Decided: dual-convolver linear crossfade** over `W = n_fft − block_size` samples (exactly the length of the old tail — the window over which the old filter's contribution would have decayed anyway).

- Both convolvers run, fed the same input, for the window. Cost: 2× convolver CPU transiently — against a measured 0.2–0.4% baseline, that is 0.4–0.8%. Irrelevant.
- **Linear, not equal-power.** The two outputs are the *same input through two similar filters* — highly correlated. Equal-power crossfade would bump the level by up to +3 dB where they are coherent. `w = k/W`; `out = (1−w)·old + w·new`.
- At `k=0` the output is the old filter's exact continuation; at `k=W` it is the new filter's exact steady state. Both ends are exactly right, which is what rules out tail-run-out (zeroing the old convolver's input): that is correct at neither end of the middle.

*Structure.* `RealtimeChain` gains `fading_out: Option<(Correction, usize /* remaining */)>`. `set_correction` moves the old into `fading_out` and returns whatever was **already** fading out — so the retire ring still receives exactly one value per swap and `poll`'s slot accounting (`shared.rs:151`) is unchanged. When a fade finishes mid-block, the finished processor must reach the retire ring, and `poll` is the only place that pushes it. Give `RtLink::poll` a second responsibility: after applying swaps, drain `chain.take_finished_fade() -> Option<Correction>` into the retire ring, still gated on `retire_tx.slots() > 0`. If there is no slot, the finished fade-out simply sits (silent, unread) until there is — realtime-safe, no allocation, and **never a drop on the RT thread**, which is the invariant `shared.rs:147–149` exists to protect.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| **No-op swap** — 100 Hz sine, Q=10 +12 dB @ 100 Hz, swap to *identical* coefficients at a block boundary | `paraeq-engine` synthetic | Output is **sample-for-sample identical** to never having swapped. This is the sharpest possible test: a no-op swap must be a no-op. |
| Gain-only swap | `paraeq-engine` synthetic | `max |y[n] − y[n−1]|` across the boundary ≤ 1.1× the surrounding steady state's max first-difference (no step) |
| Band count 3 → 5 | `paraeq-engine` synthetic (white-box) | First 3 sections' `zi` survived; last 2 are zero |
| Band count 5 → 3 | `paraeq-engine` synthetic (white-box) | First 3 survived; no panic on the discarded state |
| FIR swap over a full-scale sine | `paraeq-engine` synthetic | No output discontinuity exceeds the input's per-sample slew; after `W` samples output matches the new FIR's steady state to 1e-12; the retired convolver reaches the retire ring **exactly once** |
| Proptest: arbitrary Correction pairs, arbitrary swap point | `paraeq-engine` | Output finite and within ±1.0 |

**Effort:** R1-7a ~0.5 day; R1-7b ~2 days. **Blocks release: R1-7a YES** (the EQ tab ships with drag interactions). **R1-7b only if the FIR path ships in R1** — cross-ref the roadmap.

---

## R1-8 — Meters

**Defect.** `shared.rs:237–240`:

```rust
// Single writer (this thread): load-compare-store is race-free.
if peak > shared.peak_in() {
    shared.peak_in_bits.store(peak.to_bits(), Ordering::Relaxed);
}
```

A **monotonic session maximum**. It never decays. It is surfaced as `EngineState.input_peak` (`controller.rs:139`, `:672`) and `controller.rs:705` quantizes it at 1e-3 for change-detection *as if it were a meter* — but a monotonic max reaches its final value within the first seconds of a session and then never publishes again. It is a session statistic wearing a meter's clothes.

There is **no output peak at all** and **no clip counter**. `chain.rs:171` and `:183` `.clamp(-1.0, 1.0)` engages completely invisibly.

**Why the rescope raises severity.** R1-1 makes the preamp the headroom mechanism, and the whole argument for auto-preamp is *"the clamp should never engage."* That claim is currently **unfalsifiable** — we cannot tell whether it engaged. Worse, the auto front-end promises the app decided correctly, and the Advanced drawer's job is to prove it. A room correction with a +10 dB modal boost is exactly the case where the preamp arithmetic could be wrong (a rate-change redesign, a stale config, an identity substitution from R1-3 changing the realized peak) and the *only* symptom is clipping.

**Fix — all fused into passes that already exist.**

*New `RtShared` fields* (alphabetical among the existing set, `shared.rs:26–53`): `clipped_samples: AtomicU64`, `decay_per_block_bits: AtomicU32`, `invalid_samples: AtomicU64` (R1-2), `peak_in_session_bits: AtomicU32`, `peak_out_bits: AtomicU32`.

*Decay, without a clock on the RT lane.* Replace the monotonic store with a per-block multiplicative decay:

```rust
let decayed = f32::from_bits(shared.decay_per_block_bits.load(Relaxed)) * shared.peak_in();
shared.peak_in_bits.store(peak.max(decayed).to_bits(), Relaxed);
```

One extra multiply **per block** (not per sample). The coefficient is computed by the **controller** from the negotiated `StreamInfo` at `start_with` (`controller.rs:565`), before the backend starts — so the RT thread is not yet running and there is no race:

```
decay = 10^( -(20.0 / 1.7) * (block_size / sample_rate) / 20.0 )
```

using the broadcast-standard 20 dB / 1.7 s release. At 512 frames / 48 kHz (10.67 ms/block) that is 0.1255 dB/block → `decay = 0.98565`. Computing it from the *reported* geometry means the release rate stays correct at every buffer size and rate, which a hardcoded constant would not.

*Why RT-side decay and not controller-side:* the controller ticks every 250 ms and has a clock, but reading-and-resetting the peak would need `peak_in_bits.swap(0, Relaxed)` on the control side while the RT thread does load-compare-store — a race in which the RT thread can clobber the reset. The window is small and the consequence is one stale tick, but it is a documented race for no benefit. The RT-side version preserves the existing single-writer invariant that `shared.rs:237` already relies on.

*Keep the session max.* It is genuinely useful and free: it becomes `peak_in_session_bits` with the current monotonic logic unchanged. Both are published; each field then means exactly one thing.

*Output peak + clip count.* Fuse into the gain/clamp loops — **both** of them, since `chain.rs:181–185`'s pass-through path also clamps:

```rust
let v = (*y as f32) * preamp_lin * gain;
let a = v.abs();
if a > blk_peak { blk_peak = a; }
if a > 1.0 { clipped += 1; }
*o = v.clamp(-1.0, 1.0);
```

Two extra compares per sample on a loop that already does a clamp (itself two compares). At 48 kHz stereo that is 96k extra compares/second — unmeasurable. `ChainOutcome` (`chain.rs:31–38`) gains `clipped: u32` and `peak_out: f32`; `RtProcessor::process_block` folds them into the atomics exactly as it already folds `frame_mismatch` (`shared.rs:248–250`).

*Publish.* `EngineState` gains `clipped_samples: u64`, `input_peak_session: f32`, `invalid_samples: u64`, `output_peak: f32`; `input_peak` keeps its name and becomes the decaying one. `effectively_equal` (`controller.rs:703–713`) quantizes the peaks at 1e-3 as today, but the **counters compare exactly** — a clip must publish.

*UI contract.* The Advanced drawer shows output peak and a clip indicator that latches until reset. **A nonzero `clipped_samples` while `auto_preamp_db` is active is a bug signal**, and this spec says so: it is R1-1's falsifier.

**Tests.**

| Test | Location | Asserts |
|---|---|---|
| Full-scale 1 kHz sine, gain +6 dB | `paraeq-engine` synthetic | `clipped_samples > 0`; `output_peak == 1.0` |
| Same, gain −6 dB | `paraeq-engine` synthetic | `clipped_samples == 0`; `output_peak ≈ 0.5` |
| 10 full-scale blocks then 100 silent | `paraeq-engine` synthetic | `input_peak` decays monotonically and crosses 0.1 (−20 dB) within `1.7 s / block_duration` blocks ±1; `input_peak_session` stays 1.0 |
| R1-1's +12 dB band, full-scale sine at `fc`, auto-preamp live | `paraeq-engine` synthetic | `clipped_samples == 0` — **the test that makes R1-1's claim falsifiable** |

**Effort:** ~1 day. **Blocks release: the clip counter YES** (as R1-1's falsifier — without it R1-1 is an unverified assertion). **The decaying meter NO** — it is a UI-quality improvement and could slip to stage 5 without endangering anything.

---

## R1-9 — Denormals

**Defect.** No handling anywhere in the tree — grep-verified: no FTZ/DAZ setup, no `_MM_SET_FLUSH_ZERO_MODE`, no `fpcr` manipulation, no anti-denormal dither. The exposure is `iir.rs:56–60`'s DF2T state: when the input goes to exact zero, `z[0]`/`z[1]` decay geometrically and pass through the denormal range, where a denormal operand can cost 100+ cycles via a microcode assist on x86.

**Honest ranking — the evidence that this bites is weak, and this spec ranks it accordingly rather than padding the list.**

1. **The state is f64.** The denormal threshold is 2⁻¹⁰²² ≈ 1e-308, not f32's 2⁻¹²⁶ ≈ 1e-38. A biquad state decaying at even 0.999/sample from 1.0 needs ~709,000 samples — **14.8 seconds of continuous exact-zero input** — to reach the denormal range, and only if nothing re-excites it.
2. **CamillaDSP has none either.** It is the most mature Rust realtime-audio project in this space, with a far larger user base and the same f64 DF2T topology. That is meaningful negative evidence, not merely an excuse.
3. **The tap IOProc does not cycle during silence at all.** This is a hardware finding from 2026-07-11, cited in the code at `controller.rs:452–456`: *"the tap aggregate's IOProc only cycles while the system renders audio."* The single most common denormal scenario in a plugin — "the DAW keeps calling you with silence" — **does not occur here**. When the system is idle, we are not running.
4. **Apple Silicon is the only shipping target.** ARM64's default FPCR has FZ=0 (denormals are computed), but ARM64 handles them in the standard FP pipeline without the x86 microcode-assist penalty.

**DECIDED: not fixed in R1.** The exposure is recorded here so a future session does not have to re-derive it. If a CPU profile ever shows a spike correlated with fade-to-silence, the fix is one line — set FZ (FPCR bit 24) at IOProc entry, or inject a ±1e-20 anti-denormal DC offset into the biquad input. Both are cheap and easy to add later. **The cost of being wrong here is a transient CPU spike, not incorrect audio** — which is precisely why it ranks below every item above it.

**Test, if it ever lands:** feed 10⁶ zero samples after a full-scale burst and assert the per-block wall time does not exceed the busy-block time by >2×. Note such a test is inherently flaky on CI and would live behind `#[ignore]`.

**Effort:** ~2 hours if it ever bites. **Blocks release: NO.**

---

## R1-10 — Convolver partitioning

**Defect.** `convolver.rs:49`:

```rust
let n_fft = (block_size + fir_len - 1).next_power_of_two();
```

Single-partition overlap-add. For the room geometry (512-frame block, 16384-tap FIR), `n_fft = next_pow2(16895) = 32768`. Every block runs a 32768-point real FFT and its inverse over a buffer whose first 512 samples are input and whose remaining 32256 are zeros (`convolver.rs:108–109`) — a **64× oversized transform on a 1/64-populated buffer**.

Order-of-magnitude arithmetic: single-partition ≈ 32768·log₂(32768) = 491k butterfly-units per block. Uniform-partitioned at B = 512 (`n_fft = 1024`, K = 32 partitions): 32 complex MACs over 513 bins (≈16k ops) + two 1024-point transforms (≈20k) ≈ 36k — roughly a **13× reduction**. The real factor depends on the FFT constant and on how well the MAC loop vectorizes.

**Honest ranking.**

1. **Measured CPU today is 0.2–0.4%** (stage-3 hardware). Even the 64× oversized transform at the *current* geometry — the parity FIR is short — is free.
2. **The problem only exists for the room FIR, which does not exist yet.** And it may never: the room-DSP argument favors **matched parametric filters for modal peaks** (a modal peak is a pole pair; a matched biquad corrects amplitude and phase together and reduces ringing at the measurement position), which is an **IIR** path, not a FIR one.
3. **It costs bit-exactness.** The convolver is parity-pinned block-for-block against `prototype/paraeq/engine/convolver.py` (`convolver.rs:1–2`). Partitioning changes the arithmetic order, so outputs would match to ~1e-12, not bit-exactly. That is inside the 2026-07-02 spec's stated tolerance class for single-FFT paths (~1e-9 relative, spec line 124) and so is not a blocker — but it means the rewrite ships with a DIVERGENCES.md entry, and that is a real cost to pay for a benefit we cannot yet measure.

**DECIDED: deferred to the FIR room path (stage 6), behind a measured gate.**

- **Gate:** add an `#[ignore]`d bench running the convolver at 16384 taps / 512 block, reporting µs/block. The threshold that justifies the work is **>25% of the block period** — 512/48000 = 10.67 ms, so **2.67 ms/block**. Under that, leave it alone.
- **If it lands:** **uniform** partitioning at `B = block_size`. Not non-uniform/Gardner: those schemes exist to buy back latency, and we have no latency to buy back (the measured tap floor is ~41 ms fixed and the buffer size cannot touch it — and note that `backend.rs:195`'s `out_sample_time − in_sample_time` includes the output device's own safety offset and DAC latency, which exist with or without ParaEQ, so "ParaEQ adds 41 ms" is an over-claim; the *added* latency is not established). Zero-latency partitioning complexity buys nothing here.
- **Do not reach for `fft-convolver` without an audit** — the same due diligence the 2026-07-02 spec (line 116) applied to `scirs2` before rejecting it.

**Effort:** 2–3 days if and when it lands. **Blocks release: NO.**

---

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| R1-4 lands before `authority.rs` is designed, and the excursion/Q constants get invented locally | R1-4 is explicitly *gated* on the room-DSP spec's `authority.rs`. If it slips, ship the wrapper + the existing fixture and hold the new fn — do **not** hardcode Trinnov's numbers in `autofit.rs`. |
| `BOOST_WEIGHT`, the narrow-dip Q threshold, and the cut limit are guesses that ship untuned | All three are marked **OPEN [OWNER]**. The safe default is Trinnov's curve applied **symmetrically** and boosts rejected above Q=3; relax only after the owner has listened. Every one of them is a named constant in one module, changeable without touching the algorithm. |
| R1-6's `CorrectionConfig` change conflicts with the unmerged stage-4 branch | Sequenced explicitly: R1-6 lands **after** the merge. The branch's `resend_decision` tests (`eq.rs:452–482`) are retained and re-pointed, not deleted. |
| `kAudioSubDeviceInputChannelsKey` is not exported by `objc2-core-audio` 0.3.x | Fall back to a locally defined CFString literal citing `AudioHardware.h`. The key's *value* is a CFNumber, not a CFBoolean — a 2-minute check at implementation time, called out at the point of use. |
| The R1-7b FIR crossfade is ParaEQ's own invention with no reference implementation to check against | The no-op-swap test (swap to identical coefficients → bit-identical output) is a total-correctness oracle for the state machine that needs no external reference. Ship the IIR arm (R1-7a) first; the FIR arm only gates on the FIR path existing. |
| R1-2's output guard resets the filter and produces an audible click on a false positive | It can only fire on a non-finite *output*, which R1-2 Guard 1 and R1-3 together make unreachable through any known path. The published `invalid_samples` counter makes a false positive visible rather than mysterious, and a click strictly beats permanent noise. |
| Auto-preamp attenuates so much on a boost-heavy room correction that the result is quiet and users think it broke | This is *correct* behavior with a *UX* problem, not a DSP problem. `EngineState.auto_preamp_db` is published for exactly this reason; the Advanced drawer must explain it ("−9.4 dB to make room for the 45 Hz boost"), and R1-4's excursion cap bounds how bad it can get (±10 dB below 150 Hz → the preamp cannot exceed roughly −10 dB from modal correction alone). |
| Deferring R1-9 and R1-10 is wrong, and one of them bites in the field | Both have named, cheap fixes recorded here and a stated trigger (a CPU profile correlated with fade-to-silence; a bench over 2.67 ms/block). Neither can cause *incorrect audio* — only CPU cost — which is why they are the two that defer. |

## Out of Scope

- **Anything in the room-DSP workstream**: `authority.rs` itself, σ(f) inter-position variance, the FDW identity, gating, spatial averaging, Schroeder estimation, splicing. R1-4 *consumes* `authority.rs`; it does not define it.
- **The cal-file parser** (`compensation.rs:11` dispatching on a leading double-quote, `:60` hardcoding `.skip(2)`). It is brittle to real-world UMIK-1 header variation and silently drops the first data row on single-header files, and the robust fix is REW's own documented rule — *"Only lines which begin with a number are loaded, others are ignored"*. But it is a **measurement-path** defect, not an engine defect, and the oracle (`prototype/paraeq/measurement/compensation.py`, `np.loadtxt(skiprows=2)`) shares the flaw, so both must change together or a divergence is manufactured. It belongs to the measurement spec.
- **The `fixtures/manifest.json` scipy pin.** The manifest is a *generated provenance record* (`generate_fixtures.py:235–239` writes `scipy.__version__` at runtime); the only dependency *declaration* is `prototype/pyproject.toml:16` `"scipy>=1.10"`, a floor. Nothing enforces 1.18.0, and regenerating under a newer scipy silently rewrites the manifest. This is a real latent defect — and it is a **fixture-provenance** defect, not an engine one. It is owned by the master spec's R0 (`measurement-suite-design.md`) and detailed in `room-dsp-design.md`.
- **The `-6 dBFS` output-level policy.** The prototype's `SWEEP_AMPLITUDE = 0.5` lives at `prototype/app/wizard/measurement_wizard.py:39` — the **playback** layer, which is not ported (there is no sweep playback path anywhere in Rust; `generate_sweep`'s only caller is its own test). `sweep.rs` faithfully matches its oracle `prototype/paraeq/measurement/sweep.py`, which is also unscaled, and the peak-1.0 convention is deliberate (`noise.py`: *"normalized to a peak of 1.0 so the caller can apply any output amplitude"*). **This is not a regression.** It is a forward-looking gap: the output-level policy has no home in the Rust tree, and stage 6 must create one. That belongs to the measurement spec.
- **Closed-loop verification.** Measuring the *corrected* output requires the stimulus to go through the engine, which tap self-exclusion (`tap.rs:26–48`, `:153–154`) prevents by design — and correctly so: the 2026-07-02 spec (line 147) states *"correction state cannot contaminate the measurement."* Do **not** pre-convolve the sweep with the active correction; that defeats the design. The answer is a **helper child process** playing the stimulus (not excluded from the tap) — see the `afplay` reference in `crates/paraeq-coreaudio/tests/test_hardware.rs`. Measurement spec.
- **`fr::average_measurements`'s dB-domain averaging.** It has **zero non-test callers** in Rust today, and the prototype smooths 1/6-octave *before* averaging (`measurement_wizard.py:481–484`), which is the regime REW explicitly endorses (*"dB averaging may be useful when averaging smoothed traces to derive an EQ target"*). This is a **latent design decision for new room code**, not a live bug. Room-DSP spec.
- **Two-clock risk (UMIK-1 at its own rate vs. the output device).** The 2026-07-02 spec's claim that "the Farina method tolerates their small clock skew (prototype proved it)" was proven for an **ungated coupler magnitude** measurement. Gating needs a trustworthy t=0 and an undistorted IR shape; the claim does not transfer. This is the biggest unscheduled cost in the rescope, and it is a measurement-path problem. It touches R1-3 only as a *motivation* (a band designed at one rate installed at another), which R1-6 closes.

## Research Basis

Verified against the code at `179cd34` (and `feature/rust-port-tauri-shell` where noted):

- `f32::clamp` NaN propagation confirmed by execution: `(f32::NAN).clamp(-1.0, 1.0).is_nan() == true`; `f32::INFINITY.clamp(-1.0, 1.0) == 1.0`. Documented, intentional — the fix belongs at the capture boundary, not the clamp.
- CamillaDSP `src/utils/conversions.rs:89–106` read directly: `if !value.is_finite() { invalid_values += 1; *value = 0.0; }` fused into the min/max scan, followed by `warn!("Ignored {invalid_values} infinite or NaN values in channel {ch}")` — from inside the callback, which ParaEQ deliberately does not copy. Gated per-format on `check_for_nan`; CamillaDSP's own live device paths pass `false`, so it shares the exposure. The pattern is present, tested in-tree, and directly liftable; the placement is not.
- `eq::resend_decision` verified present on `feature/rust-port-tauri-shell` at `desktop/src-tauri/src/eq.rs:120–134`, called from `engine_bridge.rs:143`, unit-tested at `eq.rs:452–482`. `design_correction` (`eq.rs:97`) emits only `CorrectionConfig::Iir` — no FIR path exists on the branch.
- `export_autoeq_format` on the branch (`peq.rs:182`) already takes `preamp_db: f64` and writes it, with the divergence documented in-line. The plumbing exists; the computation does not.
- `tests/test_props.rs:7–42` — four stability proptests, 64 cases each, over `fc ∈ 20..20000`, `gain ∈ −24..24`, `q ∈ 0.1..20` at 48 kHz, asserting `|a2| < 1 + 1e-12` and `|a1| < 1 + a2 + 1e-9`. Build-time only; no runtime or build-boundary guard exists in library code.
- `paraeq-dsp` module set is exactly: `autofit`, `biquad`, `compensation`, `deconvolution`, `fir`, `fr`, `peq`, `spline`, `sweep`, `targets` (ten — `spline.rs` exists). `fir.rs:85` `windowed_ir()` applies a Hann window to a **designed** impulse response, live on both arms of `design_fir_correction` — so it is not true that `paraeq-dsp` "contains no IR windowing." What *is* true: **nothing time-gates a measured IR.**
- REW boost-Q cap `Q_max = 0.227·f₀/A`, `A = 10^(G/40)`, derived from a 500 ms T60 rule, verified against ParaEQ's own `biquad.rs` coefficients.
- Trinnov's shipped excursion curve: ±10 dB at or below 150 Hz, tapering to ±2 dB by 500 Hz, ±2 dB above.
- OnlyEQ (Unlicense, 2026-07-03) sets `kAudioSubDeviceInputChannelsKey: 0` with the stated reason *"otherwise running our IOProc counts as microphone access and macOS shows a mic permission prompt."* It, iQualize (MIT, March 2026), and eqtune all ship ParaEQ's exact tap architecture; **none of them measure.** Dirac shipped Mac ART on 2026-06-30 ($499/$899, HAL driver + root LaunchDaemon, no macOS uninstaller). Sonarworks SoundID Reference (€249) measures and corrects system-wide on Mac but **refuses USB microphones**, which kills both the UMIK-1 and the EARS. The window before one of the tap-based projects bolts on a sweep is roughly **12–18 months**.
- Apple's `Equaliser` sample: `BiquadFilter.setCoefficients(_:setup:resetState:)` with `resetState: false` on incremental edits preserves delay state — how it avoids clicks without a crossfade. CamillaDSP does not crossfade; there is no source for the FIR arm.
- Latency: measured 46–62 ms with a ~41 ms fixed tap floor, fitting `40.96 ms + 2.0 × buffer_period`, with a 1966-sample floor invariant across a 4× buffer sweep. Competitors' "~10 ms" claims are arithmetic, not measurement (OnlyEQ: `estimatedLatency = ioBufferFrames * 2 / sampleRate`); iQualize removed its "Low Latency" toggle because ring capacity *"did not meaningfully reduce latency."* `backend.rs:195` computes `out_sample_time − in_sample_time`, which includes the output device's own safety offset and DAC latency that exist with or without ParaEQ — so **"ParaEQ adds 41 ms" is an over-claim**; the added latency is not established.
- `spline.rs` (commit `309d71d`) is the merged precedent for a `paraeq-dsp` module with no fixtures directory, zero mentions in `generate_fixtures.py`, and no prototype addition — verified by analytic invariants (`passes_through_knots`, `n2_is_linear_and_n3_is_parabola`) plus inline pinned scipy values. A tiered oracle strategy is a **clarification** of the methodology, not a resolution of a contradiction: "fixtures are sacred" is a provenance rule whose own text sanctions editing the generator.
- `fir.rs:1–4` cites **"Oracles:"** (plural), listing `prototype/paraeq/correction/fir_filter.py` **first**; scipy is a transitive dependency of the prototype. `spline.rs:1–5` cites `scipy.interpolate.CubicSpline` alone — and even that is not an independent third-party oracle, since the prototype uses `CubicSpline` too. The honest precedent is **"cite the library the prototype itself delegates to."**

---

## Item Summary

| Item | Severity under rescope | Effort | Blocks release? |
|---|---|---|---|
| **R1-1** Auto-preamp (`peq.rs:112` literal; nothing computes a preamp) | **Critical** — the spec's stated headroom mechanism does not exist, and room correction is the first workload that boosts | ~1 day | **Yes** |
| **R1-2** NaN sanitization at the capture boundary (`backend.rs:174–176`; DF2T poisoning at `iir.rs:56–60`) | **High** — permanent, unrecoverable output corruption from one sample; taps deliver the format that can encode it | 0.5–1 day | **Yes** |
| **R1-3** Runtime stability guard (`biquad.rs` designers infallible; `validate_bands` lives in the Tauri crate) | **High** — the auto front-end designs with no human in the loop, and the daemon seam inherits no wall | ~1 day | **Yes** |
| **R1-4** Boost caps in autofit (`autofit.rs:13`, `:19`, `:30–35`, `:71`) | **Critical** — symmetric picking + uncapped gain aims maximum boost at non-minimum-phase modal dips that cannot be filled | 2–3 days (gated on `authority.rs`) | **Yes** |
| **R1-5** `kAudioSubDeviceInputChannelsKey: 0` (`tap.rs:57–61`) | **High** — dissolves the mic-samples-as-tap-input limitation (`backend.rs:121–130`) *and* removes a spurious mic prompt that would be indistinguishable from the measurement prompt | ~1 hour + hardware run | **Yes** |
| **R1-6** Sample-rate coefficient staleness (`controller.rs:105`; partially closed on the stage-4 branch) | **High** — every filter detunes by ~8% on a rate switch; "the tap follows your device automatically" must be true before it is said | 1–2 days (after the stage-4 merge) | **Yes** |
| **R1-7a** IIR state preservation on swap (`chain.rs:88–90`) | **Medium** — a click on every band edit; the EQ tab *is* a drag interaction | ~0.5 day | **Yes** |
| **R1-7b** FIR crossfade on swap (`convolver.rs:70`, `:97`) | **Medium** — up to 32256 discarded tail samples for a room FIR | ~2 days | Only if the FIR path ships in R1 |
| **R1-8** Meters: clip counter + output peak (`chain.rs:171`, `:183` clamp invisibly) | **Medium** — the clamp engages silently; without the counter, R1-1's central claim is unfalsifiable | ~1 day | **Yes** (clip counter) |
| **R1-8** Meters: decaying `input_peak` (`shared.rs:237–240` monotonic session max) | **Low** — a session statistic wearing a meter's clothes | (in the above) | No |
| **R1-9** Denormals (none anywhere) | **Low** — f64 state, ~14.8 s to reach denormal range; CamillaDSP has none either; the tap IOProc does not cycle during silence; ARM64 has no assist penalty | ~2 hours if it ever bites | No |
| **R1-10** Convolver partitioning (`convolver.rs:49`, 64× oversized transform at room geometry) | **Low** — 0.2–0.4% measured CPU today; the room FIR does not exist yet and may never (modal peaks want IIR) | 2–3 days when it lands | No |
