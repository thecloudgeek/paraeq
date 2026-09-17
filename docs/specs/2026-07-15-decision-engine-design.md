# Auto-Decision Engine — Design Spec

**Date:** 2026-07-15
**Status:** Draft (written against the 2026-07-15 rescope; owner review pending)
**Amends:** `docs/specs/2026-07-02-rust-port-design.md` — the Measure tab (line 147), the port order's stage 6 (line 198), and the `paraeq-dsp`/`paraeq-engine` crate-boundary constraints (lines 72–76). The 2026-07-02 spec scopes measurement to a single EARS headphone sweep with no automatic decisions; this spec adds the layer that makes a four-transducer, two-front-end product possible on one engine.

## Summary

The 2026-07-15 rescope makes "easy" a first-class requirement: two front-ends — **"just fix my sound"** (auto; the app decides everything, every decision visible and overridable in an Advanced drawer) and **"walk me through it"** (guided; explains each step) — over **one** engine, chosen at first run and switchable later. The mechanism that makes that possible, and the only one that survives contact with a release, is a single pure function in Rust:

```rust
pub fn decide(bundle: &MeasurementBundle) -> DecisionSet
```

Every parameter a competitor asks the user (Dirac's 9/13/17 arrangement, Sonarworks' 37 positions) or hardcodes (a fixed 200 Hz transition, a fixed 6 ms gate) becomes a typed `Decision<T>` carrying its value, its legal domain, its provenance, the measured evidence that produced it, the rationale sentence guided mode renders, and the invalidation tier an override triggers. Both front-ends and the Advanced drawer are **pure renderers** over one `DecisionSet`. There is no second code path and no second set of defaults.

The four transducer types are **data, not branches**: a `PathProfile` parameterizes six values (reposition noun, N, gating mode, smoothing mode, authority kind, target class filter), and the two room profiles differ from each other in exactly one capture parameter. The low-frequency corner is derived from the measurement, which is why bookshelf-vs-floorstander is not a branch.

This spec is a contract. It specifies the types, the decision rules (formulas and thresholds), the refusal checks, the override semantics, and the storage those semantics require. It does **not** specify the DSP primitives the rules call (`fdw`, `gating`, `logf`, `authority`, `room`, the `fr` rework) — those belong to the room-DSP spec — nor the pre-capture level ladder, which belongs to the measurement-safety spec.

## Decisions Log

| Decision | Choice | Alternatives rejected |
|---|---|---|
| Decision-engine language | Rust, one pure `decide()` | Policy in TypeScript (unoverridable, untestable, forks within a release); rationale strings emitted from Rust with policy in TS (same fork, later) |
| Decision-engine home | **New crate `paraeq-decide`**, depends on `paraeq-dsp` only; no Tauri, no CoreAudio, no engine dep | A `decide.rs` module inside `paraeq-dsp` (policy is not pure math; would drag target/cal domain types into the math crate); inside `desktop/src-tauri` (Tauri dep kills daemon-readiness and fixture testing) |
| `decide()` signature | One argument, one return; no `Result`; no I/O, no clock, no RNG | `decide(&bundle, &profile)` (profile is derivable from `bundle.class` — a second argument is a second place to disagree); `Result<DecisionSet, E>` (a refusal must still carry the decisions and evidence that produced it) |
| Overrides input | A field of `MeasurementBundle` | A second argument (breaks "the whole input is one serializable value") |
| Mode awareness | `decide()` has **no** knowledge of which front-end renders it | A `mode: Auto \| Guided` argument — the exact seam that forks the two modes |
| Decision container | Named struct fields + an erased `iter()` view for rendering | `HashMap<String, Decision>` (stringly-typed; lets the drawer reference keys that do not exist; no exhaustiveness) |
| Rationale | Rust renders the string from a typed `RationaleKey` + params; the key is retained for tests and future l10n | Rust emits only a key and TypeScript owns copy (copy is then unreviewable here and drifts per mode); Rust emits only a string (untestable, unlocalizable) |
| Four transducer types | One `PathProfile` table indexed by `TransducerClass`; the two room profiles differ only in `sweep_f_start_hz` | Four code branches; two wizards |
| Low-frequency corner | Derived from the measurement (lowest frequency within 10 dB of midband) | A per-class constant — which is what would have forced bookshelf/floorstander into separate analysis branches |
| Transition frequency | Derived from the inter-position variance curve σ(f), Schroeder-range cross-checked, 200 Hz fallback | A hardcoded 200 Hz (does not adapt to a treated room); Schroeder alone (needs a room volume the user does not know) |
| Boost authority | Per-region, gated on flat excess group delay **and** low σ(f) | "Full authority below 200 Hz" — **unsafe as a blanket rule**; see Authority below |
| Target selection | `TransducerClass`-filtered candidate set, filter is a **required argument** | Keeping `match_closest_target`'s unfiltered iteration (targets.rs:193–211) — it would return `harman_oe_2018` for a loudspeaker measurement |
| Room target | Parametric (tilt + low shelf), not `match_closest` | Matching a room measurement to a curve library (the room's own response is the thing being corrected — "closest" is meaningless) |
| Correction form | `ParametricEQ` (IIR) default on all four paths | Minimum-phase FIR default (adds block latency onto an already 46–62 ms budget; cannot carry the per-band Q cap and excursion envelope as constraints) |
| Preamp | `-max(0, peak of the REALIZED cascade)`, no headroom constant; applied to the engine gain stage | AutoEQ's `PREAMP_HEADROOM = 0.2` — it does not apply to parametric preamps; see Preamp below |
| Verification stimulus | Played from a **helper child process** (not tap-excluded) | Pre-convolving the sweep with the active correction — inverts the tap's documented design intent; see Verification below |
| Raw IR persistence | Mandatory: per-position IRs stored in the profile | Storing only smoothed post-compensation curves (the prototype's shape) — it makes every override a Recapture |

## Why the Decision Engine Lives in Rust

The product promise is that auto mode and guided mode are the **same decisions**, differently disclosed. That is a claim about code identity, and it is only enforceable if there is one piece of code.

Put the policy in TypeScript and the failure is not hypothetical, it is mechanical. Auto mode needs a value. Guided mode needs a value plus an explanation. The drawer needs a value, a legal range, and a way to write back. Three consumers, one loosely-typed module, and the first time someone tunes a threshold for the guided screen without touching the auto screen — or adds a `if (mode === 'auto')` guard — the modes have forked. Nothing catches it: there is no test that can assert "auto and guided agree", because with the policy in the renderer there is no value to compare, only two renderings. The fork is silent, it ships, and the Advanced drawer becomes a second, lying source of truth about what the app did.

In Rust with a typed `DecisionSet` the same properties are structural rather than disciplinary:

- **Auto/guided agreement is a type identity, not a test.** Both modes read the same `DecisionSet` value. They cannot disagree because there is nothing to disagree about — the difference between the modes is which fields of one struct get painted.
- **"Every decision visible and overridable" is enforced by the compiler.** A decision that is not a `Decision<T>` field is not renderable by the drawer's exhaustive `iter()`; a decision without a `domain` has no control to draw. Adding a hidden constant requires deliberately not using the type.
- **It is fixture-testable.** `decide()` is pure: same bundle, same `DecisionSet`, forever. A stored bundle plus its expected `DecisionSet` is a golden fixture. Policy in TypeScript is testable only against itself.
- **It is daemon-ready.** `paraeq-engine` was kept Tauri-free so a headless `paraeqd` is a move, not a rewrite (2026-07-02 spec, line 75). A decision engine in the webview re-couples the product to Tauri and undoes that.
- **It is attachable to a bug report.** One serializable input, one serializable output. "Auto picked something stupid" becomes a fixture.

The counter-argument — that policy is UX and belongs near the UI — is the argument that produced every "advanced settings do not match what the app actually did" bug in this category of product. The rationale *copy* is UX and is reviewable in the decision table below; the rationale *logic* is not.

**Crate placement.** `paraeq-decide` is a new workspace member depending on `paraeq-dsp` only. It does not depend on `paraeq-engine` (that would drag `rtrb`/`arc-swap` into a pure crate for the sake of one type) and it does not depend on Tauri. `paraeq-dsp` stays pure math: it does not learn what a transducer class or a cal file's provenance is. `decide()` performs **no filesystem I/O** — target curves and cal file contents arrive already parsed in the bundle. This is not fastidiousness: `targets::list_targets` reads a directory, and a `decide()` that could do that would not be a pure function and could not be fixture-tested.

## The Core Types

Sketches, not final code. Fields are alphabetical per repo convention.

### Input

```rust
/// The complete, serializable input to `decide()`. One value: it is the
/// fixture, the bug report attachment, and the profile's analysis record.
pub struct MeasurementBundle {
    /// Verbatim cal file + parsed curve + parsed metadata. NEVER normalized.
    pub cal: Option<CalFile>,
    /// What was actually captured (echoed as Recapture-tier decisions).
    pub capture: CapturePlan,
    /// The wizard's one unavoidable question.
    pub class: TransducerClass,
    /// Silence capture taken before the first sweep, per channel.
    pub noise_floor: NoiseFloor,
    /// User overrides. In the bundle, not a second argument, so the whole
    /// input stays one value.
    pub overrides: Overrides,
    /// Accepted positions only. A cancelled or rejected capture is absent.
    pub positions: Vec<Position>,
    /// Caller-loaded (`targets::list_targets` does I/O; `decide()` does not).
    pub targets: Vec<TargetCurve>,
    /// Present only after a closed-loop verification pass.
    pub verification: Option<Verification>,
}

pub struct Position {
    /// Provenance only. NEVER a decision input — `decide()` is deterministic.
    pub captured_at_ms: u64,
    pub index: usize,
    pub ir: ImpulseResponse,
    /// "Reseat 3", "Position 4 (left of centre)".
    pub label: String,
}

/// `deconvolution::deconvolve` must return this instead of a bare `Vec<f64>`.
/// It currently takes `_sample_rate: u32` and DISCARDS it (deconvolution.rs:6),
/// so there is no time axis — and every gating operation needs t=0.
pub struct ImpulseResponse {
    /// Direct-arrival index (argmax + parabolic refine). ~46–64 ms into the
    /// capture on this architecture: tap latency + propagation.
    pub peak: usize,
    pub sample_rate: u32,
    /// Per channel.
    pub samples: Vec<Vec<f64>>,
}

pub struct CalFile {
    /// Raw bytes as shipped. The parser's input and the provenance record.
    pub content: String,
    /// Interpolatable curve. NEVER normalized to 0 dB at any frequency: the
    /// EARS jig encodes a real 2.1 dB L/R capsule offset IN the curve, and
    /// normalizing would bake a channel imbalance into every measurement.
    pub curve: (Vec<f64>, Vec<f64>),
    pub gain_db: Option<f64>,
    pub sensitivity_db: Option<f64>,
    pub serial: Option<String>,
    /// EARS HEQ/HPN/IDF variants already bake a target into the cal.
    pub variant: CalVariant,
}
```

### Decisions

```rust
pub struct Decision<T> {
    /// What the Advanced drawer is allowed to offer.
    pub domain: Domain<T>,
    /// Structured, plottable. Never prose.
    pub evidence: Vec<Evidence>,
    /// What overriding this costs.
    pub invalidates: Invalidation,
    /// Rendered in Rust from a typed key so the copy cannot fork per mode.
    pub rationale: Rationale,
    pub source: Source,
    pub value: T,
}

pub enum Domain<T> {
    /// Enumerable alternatives (target curves, window types).
    Choice(Vec<T>),
    /// Derived and not user-settable directly — override its inputs instead.
    /// Rendered read-only in the drawer, never hidden.
    Derived,
    Range { max: T, min: T, step: Option<T> },
}

pub enum Source {
    /// decide() computed it from this measurement.
    Auto,
    /// decide() fell back to the PathProfile default (evidence absent or
    /// inconclusive). Distinct from Auto ON PURPOSE: the drawer must be able
    /// to say "we could not measure this, so we used the default".
    Default,
    UserOverride,
}

/// What must re-run when this decision changes. Wall-clock targets are for a
/// 9-position stereo room bundle at 48 kHz.
pub enum Invalidation {
    /// Re-run gating/FDW/smoothing/averaging/compensation from cached IRs,
    /// then Redesign. ~200 ms.
    Reanalyze,
    /// Must re-measure. Mic, output device, sample rate, N-increase only.
    Recapture,
    /// Re-run autofit + preamp from the cached analysis curve. ~50 ms.
    Redesign,
}

pub struct Rationale {
    pub key: RationaleKey,
    /// Rendered here, in Rust. The UI is a text field, not an author.
    pub text: String,
}

pub enum Evidence {
    /// A plottable curve: σ(f), excess group delay, the authority envelope.
    Curve { db: Vec<f32>, hz: Vec<f32>, label: EvidenceLabel },
    Scalar { label: EvidenceLabel, unit: Unit, value: f64 },
    /// A frequency span to shade on the plot (the authority split).
    Span { hz_hi: f64, hz_lo: f64, label: EvidenceLabel },
}
```

### Output

```rust
pub struct DecisionSet {
    /// The derived curves both front-ends plot. Not decisions — products.
    pub analysis: Analysis,
    /// `None` iff `verdict == Refuse`. Rate-independent by construction (see
    /// "The rate-independence requirement" below).
    pub correction: Option<CorrectionPlan>,
    pub decisions: Decisions,
    pub diagnostics: Vec<Diagnostic>,
    pub verdict: Verdict,
}

pub enum Verdict {
    Proceed,
    /// At least one Warn diagnostic. `correction` is present and installable.
    ProceedWithWarnings,
    /// At least one Refuse diagnostic. `correction` is None, but `decisions`,
    /// `analysis` and `evidence` are still populated as far as they got — a
    /// refusal must be able to explain itself and the guided path must be able
    /// to pick up where auto stopped. This is why `decide()` returns
    /// `DecisionSet`, not `Result<DecisionSet, E>`.
    Refuse,
}

/// Named fields, not a map: the drawer is exhaustive by construction and
/// cannot reference a key that does not exist. `iter()` gives the renderer a
/// uniform erased view without giving up the type.
pub struct Decisions {
    pub align_spl_band: Decision<(f64, f64)>,
    pub authority: Decision<AuthorityCurve>,
    pub averaging: Decision<AveragingMode>,
    pub class: Decision<TransducerClass>,
    pub correction_kind: Decision<CorrectionKind>,
    pub correction_range: Decision<(f64, f64)>,
    pub fdw_post_cycles: Decision<f64>,
    pub fdw_pre_cycles: Decision<f64>,
    pub flatness_target_db: Decision<f64>,
    pub left_window_ms: Decision<f64>,
    pub low_corner_hz: Decision<f64>,
    pub max_filters: Decision<usize>,
    pub positions_n: Decision<usize>,
    pub preamp_db: Decision<f64>,
    pub q_cap: Decision<QCapPolicy>,
    pub right_window_ms: Decision<f64>,
    pub shelves: Decision<bool>,
    pub smoothing: Decision<SmoothingMode>,
    pub target: Decision<TargetChoice>,
    pub transition_hz: Decision<f64>,
    pub window_type: Decision<WindowType>,
}

impl Decisions {
    /// Erased view for rendering: id, JSON value, JSON domain, source,
    /// rationale, evidence, invalidation tier. Both front-ends and the drawer
    /// consume this; nothing else.
    pub fn iter(&self) -> impl Iterator<Item = DecisionView<'_>>;
}

/// Mirrors `Decisions` field-for-field with `Option<T>`. Written only by the
/// Advanced drawer. Anything else writing here is a bug.
pub struct Overrides {
    pub align_spl_band: Option<(f64, f64)>,
    pub authority: Option<AuthorityCurve>,
    // ... one Option<T> per Decisions field
}
```

### The rate-independence requirement

`CorrectionPlan` carries `ParametricEQ { bands, sample_rate }` plus `preamp_db` — **bands, not baked coefficients**. This is a required change, not a preference. Verified: `controller::build_correction(config, channels, block_size)` (controller.rs:105) takes **no sample rate**, and `CorrectionConfig::Iir { sos_per_channel }` (controller.rs:58) holds coefficients already designed at some rate. The controller rebuilds from that retained config on every stream rebuild (controller.rs:584, :630), so a sample-rate change — an AirPods handoff, a 44.1→48 kHz switch, exactly the events the rebuild-on-change listeners exist to catch — **reinstalls coefficients designed for the old rate**. At 44.1→48 kHz a 47 Hz mode filter lands at 51 Hz with the wrong Q.

Two acceptable fixes; this spec requires one of them and does not pick:

1. `CorrectionConfig` gains a `Peq { bands, design_rate }` variant and `build_correction` takes the live `sample_rate`, re-deriving SOS per rebuild. Preferred — it puts the invariant in the type.
2. The Tauri backend re-derives `sos_per_channel` from the retained `CorrectionPlan` on every `FormatChanged` event before re-sending `SetCorrection`. Cheaper, but the invariant then lives in a call site.

**DECIDED (2026-07-21):** fix (1) — carry bands + `design_rate`, re-derive at the live rate; engine-hardening R1-6's refuse-and-fail-open is the last-resort net. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q1.

## PathProfile — Four Transducer Types as Data

```rust
pub enum CouplingPath { Coupler, Room }

pub enum TransducerClass { Bookshelf, Floorstander, InEar, OverEar }

pub struct PathProfile {
    pub authority: AuthorityKind,
    pub averaging: AveragingMode,
    pub coupling: CouplingPath,
    pub flatness_target_db: f64,
    pub gating: GatingMode,
    pub positions_default: usize,
    pub positions_domain: RangeInclusive<usize>,
    /// Guided-mode copy for "do the thing again, differently".
    pub reposition_noun: &'static str,
    pub smoothing: SmoothingMode,
    /// Capture-plan parameter. The ONLY field the two room profiles differ in.
    pub sweep_f_start_hz: f64,
}

pub fn profile_for(class: TransducerClass) -> &'static PathProfile;
```

The four instances, as data:

| Field | OverEar | InEar | Bookshelf | Floorstander |
|---|---|---|---|---|
| `coupling` | `Coupler` | `Coupler` | `Room` | `Room` |
| `reposition_noun` | "reseat the headphone" | "reseat the tip" | "move the mic ~30 cm" | "move the mic ~30 cm" |
| `positions_default` | 5 | 5 | 9 | 9 |
| `positions_domain` | 3..=10 | 3..=10 | 3..=15 | 3..=15 |
| `gating` | `None` | `None` | `Fdw { post: 15.0, pre: 3.0 }` | `Fdw { post: 15.0, pre: 3.0 }` |
| `smoothing` | `Fixed(6)` + 6–8 kHz taper | `Fixed(6)` + 6–8 kHz taper | `Variable` | `Variable` |
| `averaging` | `DbMean` | `DbMean` | `Power` (Align SPL first) | `Power` (Align SPL first) |
| `authority` | `CouplerEnvelope` | `CouplerEnvelope` | `RoomEnvelope` | `RoomEnvelope` |
| `flatness_target_db` | 1.0 | 1.0 | 3.0 | 3.0 |
| `sweep_f_start_hz` | 20.0 | 20.0 | **30.0** | **20.0** |

**Why the four types are nearly free.** Headphone measurement is structurally the same operation as room measurement: N captures at different positions, averaged, with a variance gate. Reseat scatter *is* the coupler's version of spatial scatter, and σ(f) means the same thing in both — the degree to which a feature is a property of the system rather than of where you put the transducer. The split is `CouplingPath`, and it changes six values.

**Why bookshelf and floorstander are not an analysis branch.** They differ in one capture-plan number: a 50 Hz-tuned bookshelf must not be swept below its tuning (excursion rises as 1/f² below tuning and ported boxes unload entirely; 20 Hz into a 50 Hz-tuned box is 6.2× at-tuning excursion). That is a **safety** parameter, not an analysis parameter. Everything downstream — gate, smoothing, averaging, authority, target, correction range — is identical, because the thing that actually distinguishes a bookshelf from a floorstander in the analysis is **where its response rolls off**, and that is measured, not declared (`low_corner_hz`, below). Ask the user which they own and you get the wrong answer for a ported bookshelf that reaches 38 Hz. Measure it and you get 38 Hz.

**Why `class` is still a question.** Chain sensitivity distinguishes a coupler from a room, so in principle it is inferable. It is asked anyway because (a) the answer is needed *before* the first sweep to set `sweep_f_start_hz` and the SPL cap, and inference requires a capture, and (b) getting it wrong is a safety event, not a quality event. The wizard's question budget stays at two-plus-one-conditional: transducer class; mic (auto if exactly one plausible input exists); UMIK serial only if no cal is cached.

## The Decision Table

The heart of this spec. `f_t` = the decided transition frequency. `M` = midband reference level (mean magnitude over 200 Hz–2 kHz of the aligned, averaged, smoothed curve). Rationale text is the exact string `decide()` renders; auto mode shows none of them, guided mode shows all of them, the drawer shows the one you are touching.

| Decision | Input | Rule | Default | Domain | Rationale (rendered by Rust) | Tier |
|---|---|---|---|---|---|---|
| `class` | `bundle.class`; solved chain sensitivity | Echo the declared class. Cross-check solved sensitivity against the path envelope; outside ⇒ `Refuse(WrongTransducer)` | — | `Choice`: all 4 | "You told us these are {class}. The levels we measured are consistent with that." | Recapture |
| `positions_n` | `bundle.positions.len()` | Echo accepted count. `< 3` ⇒ `Refuse(TooFewPositions)`. Below `positions_default` ⇒ `Warn(FewPositions)` | 5 coupler / 9 room | `Range 3..=10` / `3..=15` | "We averaged {n} {noun}s. More positions mean we can tell your room's problems apart from your chair's." | Recapture |
| `window_type` | — | Tukey α=0.25, applied independently left and right | `Tukey(0.25)` | `Choice`: Rect, Tukey(α), Hann, BlackmanHarris | "A soft-edged window. A hard cut smears the measurement across frequency." | Reanalyze |
| `left_window_ms` | `ir.peak`, `capture.sweep` | `min(requested, t_peak, 0.5·dt₂)` where `t_peak = peak/sr` and `dt₂ = T·ln2/ln(f_end/f_start)` (Farina H2 arrival) | clamped; ~46–64 ms in practice | `Range 1.0..=t_peak` | "Your Mac's audio path means there are only {t_peak:.0} ms before the impulse arrives. We use all of them." | Reanalyze |
| `right_window_ms` | `PathProfile.coupling` | Room: 400. Coupler: full (no gate). Publish the **stricter** resolution limit `f = (1/T)/(2^(1/2N) − 2^(−1/2N))`, not `1/T` | 400 (room) | `Range 100.0..=1000.0` | "We keep {t} ms of your room's decay. Below {f_min:.0} Hz this measurement can't resolve 1/6-octave detail, so we grey it out." | Reanalyze |
| `fdw_post_cycles` | `PathProfile.gating` | FDW half-amplitude width `n_c/f` centred on the peak, after the L/R windows. Coupler: off | 15.0 | `Range 3.0..=30.0`, **derived at runtime** as `STORE_POST_MS/1000 · grid.f_min()` and additionally floored by the grid's `ppo/1.849` limit (`fdw::apply_fdw` errors when `sigma_bins < 1.0`, i.e. `n_c > 51.9` on the 96-ppo standard grid). The former `61.0` ceiling was unattainable | "At 30 Hz the window is half a second, so we keep your room's bass. At 10 kHz it's 1.5 ms, so we throw the reflections away. That's what your ear does." | Reanalyze |
| `fdw_pre_cycles` | `left_window_ms` | Asymmetric pre-lobe. Default 3; clamped `≤ fdw_post_cycles` | 3.0 | `Range 1.0..=61.0` | "We look further after the impulse than before it — there's nothing before it but noise." | Reanalyze |
| `smoothing` | `PathProfile` | Room: `Variable`, `b(f) = clamp(1/48 + (1/3 − 1/48)·(log₁₀f − 2)/2, 1/48, 1/3)` octaves. Coupler: `Fixed(6)` + sigmoid taper toward 1/3 across 6–8 kHz. **`Fixed(6)` (1/6-octave) is required, not `Fixed(12)`: it is the pinned-oracle value (`measurement_wizard.py:481–484` smooths 1/6-octave before averaging) that keeps the coupler dB-average inside REW's endorsed regime. See the wizard spec's Open Q4.** | Variable / Fixed(6) | `Choice`: Fixed(3,6,12,24,48), Variable, None | "Fine detail in the bass where we correct hard; coarse up top where we only shape tone." | Reanalyze |
| `align_spl_band` | `PathProfile.coupling` | Room: mandatory, `normalize_to_reference_band(200, 2000)` per position **before** averaging. Coupler: same band, also mandatory. The default **reads `fr::DEFAULT_SPL_ALIGN_BAND`**, the owning module's constant — same band as the midband reference `M` below | (200.0, 2000.0) | `Range` within 100..=8000 | "We level-matched the positions first — otherwise the mic positions nearest the speaker would dominate the average." | Reanalyze |
| `averaging` | `PathProfile.coupling` | Room: `Power`. Coupler: `DbMean`. Multi-position data reaching a **vector/coherent** routine ⇒ `Refuse(CoherentAveragingRejected)` | Power / DbMean | `Choice`: DbMean, Power | "One mic position sitting in a cancellation would otherwise drag the average down and make us boost a hole that isn't really there." | Reanalyze |
| `transition_hz` | σ(f); Schroeder range | Lowest `f` where `σ(f) ≥ 3.0 dB` and stays ≥ for ≥ 1/3 octave. Clamp to `[max(80, 0.5·f_s), min(400, 2·f_s)]` when volume is known (`f_s = 2000·√(T60/V)`), else `[80, 400]`. No crossing ⇒ 200.0, `source: Default` | derived; 200.0 fallback | `Range 80.0..=400.0` | "Below {f_t:.0} Hz your room's problems are the same everywhere you sit, so we fix them. Above that they change with every head movement." | Reanalyze |
| `low_corner_hz` | averaged curve, `M` | Lowest `f` such that `mag(f′) ≥ M − 10 dB` for all `f′ ∈ [f, 200]` — i.e. scan up from the lowest valid bin, first frequency that gets within 10 dB of midband and stays there | derived (~50–60 Hz bookshelf, ~30–35 Hz floorstander, ~20 Hz coupler) | `Derived` | "Your speakers reach down to about {f:.0} Hz. We measured that; we didn't assume it." | Reanalyze |
| `correction_range` | `low_corner_hz`, target, SNR | Low = `max(low_corner_hz, right-window resolution limit, lowest bin with SNR ≥ 25 dB)`. High = `min(20000, highest bin with SNR ≥ 25 dB)`, and no filters above the frequency where measured **last drops below** target | derived | `Range` within 20..=20000 | "We don't try to flatten your speaker's natural roll-off. That only burns headroom and drives the woofer past where it can go." | Redesign |
| `authority` | `transition_hz`, EGD mask, σ(f) | See **Authority** below. Typed `Decision<AuthorityPreset>`, **not** `Decision<AuthorityCurve>`: the preset carries the name the drawer labels and an override round-trips, and the **resolved** curve is published in `Analysis` | preset `Standard` | `Choice`: `Conservative`, `Custom(AuthorityCurve)`, `Standard` | "Above {f_t:.0} Hz the dips are sound cancelling itself out. No EQ can fix that — it just makes it louder and still cancelled." | Redesign |
| `q_cap` | f₀, gain | Boosts only: `Q_max = 0.227·f₀/A`, `A = 10^(G/40)`. **And** the path ceiling: 5.0 coupler; room `10.0 @ 200 Hz → 3.0 @ 10 kHz` log-linear. Take the min. Cuts: existing `0.5..=20.0` clamp (autofit.rs:71) | both, min | `Choice`: `Ceiling(5.0)` │ `LogLinear { hi: (10000.0, 3.0), lo: (200.0, 10.0) }` — the two path policies, so the value has representable membership and the drawer's control is a two-item select | "A boost filter is a resonance. We won't build one that rings longer than the room problem it's fixing." | Redesign |
| `max_filters` | `flatness_target_db` | Greedy worst-first; stop when residual RMS over the authority band `< flatness_target_db` or the cap is hit. Drop any band with `\|gain\| < flatness/2` | 10 | `Range 1..=20` | "We used {n} filters because that's what it took to get within {flat} dB — not because {n} is a nice number." | Redesign |
| `flatness_target_db` | `PathProfile` | — | 3.0 room / 1.0 coupler | `Range 0.5..=6.0` | "Chasing flatter than {flat} dB in a room means fighting your chair, not your speakers." | Redesign |
| `shelves` | residual at range ends | Emit a shelf where a ≥0.5-octave one-signed excursion exists at either end of `correction_range`; shelf Q clamped `[0.4, 0.7]` | true | `Choice`: true, false | "This is a broad tilt, not a bump — a shelf fixes it with one filter instead of six." | Redesign |
| `target` | `class`, `cal.variant` | See **Target selection** below | class-dependent | `Choice`: the class-filtered candidate set + `Parametric` | "{reason}" (see below) | Reanalyze |
| `correction_kind` | — | `Peq` on all four paths | `Peq` | `Choice`: Peq, MinPhaseFir | "We used {n} tone controls rather than a convolution filter — same result, no extra delay." | Redesign |
| `preamp_db` | realized cascade | `-max(0, max_f∈[20,20000] of the REALIZED cascade response)`; no headroom constant. Applied to the engine gain stage, not just the export text | computed | `Derived` | "We turned everything down {p:.1} dB to make room for the boosts. That's normal, and it's why it may sound quieter at first." | Redesign |

### Target selection

`match_closest_target` (targets.rs:193–211) iterates **every** curve with no category filter, despite `category: Option<String>` existing and being parsed at targets.rs:76. Bundle a room target today and it will happily return `harman_oe_2018` for a loudspeaker: `harman_oe_2018` is **+8.3 dB at 3 kHz** (+0.22 dB/oct tilt) while the B&K room curve is **−3.0 dB at 3 kHz** (−0.84 dB/oct) — an **11.3 dB** gap at the ear's most sensitive frequency. That peak is ear gain a headphone bypasses and a loudspeaker delivers acoustically via the listener's own ear. Applying it to a speaker double-applies it. This is a category error, not a gap.

Required changes:

1. **`match_closest_target` takes `TransducerClass` as a required argument** and matches only within the filtered candidate set. A type-level constraint, not a UI default.
2. **`# category:` (one string) is superseded by `# classes:` (a set)**, because `reference` is not universally safe: `diffuse_field.csv` is tagged `reference` today but is a 5128 **coupler**-referenced curve (−4.25 dB at 20 Hz) and must never be offered for a room. Verified current tags: `diffuse_field` = reference, `flat` = reference, `harman_ie_2019{,_without_bass}` = in-ear, `harman_oe_2018{,_without_bass}` = over-ear. Zero room targets are bundled.
3. **Back-compat mapping** so no existing CSV silently becomes a room target: `over-ear → {OverEar}`, `in-ear → {InEar}`, `reference → {InEar, OverEar}` — deliberately **not** room. `# classes:` supersedes when present.

New tags and additions:

| Curve | `classes` |
|---|---|
| `bk_1974.csv` (new) | Bookshelf, Floorstander |
| `diffuse_field.csv` | InEar, OverEar |
| `flat.csv` | Bookshelf, Floorstander, InEar, OverEar |
| `harman_ie_2019{,_without_bass}.csv` | InEar |
| `harman_oe_2018{,_without_bass}.csv` | OverEar |

Selection rules:

| Case | Rule | Source | Rationale text |
|---|---|---|---|
| Coupler, normal cal | `match_closest_target(class-filtered candidates)` | Auto | "Your headphones are closest to {name} — that's the curve we matched, out of {k} we tried." |
| Coupler, EARS HEQ/HPN/IDF cal | Force `flat.csv` + `Warn(CalHasTargetBakedIn)` | Auto | "Your EARS calibration already has a target baked into it. Applying another would apply it twice." |
| Room | `Parametric { shelf_db, shelf_fc: 105.0, shelf_q: 0.71, tilt_db_per_oct: -0.9 }` | Default | "Rooms don't get matched to a curve — the room's own response is the thing we're fixing. We used a {tilt} dB/octave house curve with a gentle bass lift." |

Room `Parametric` domains: `tilt_db_per_oct ∈ -1.5..=0.0` (default −0.9), `shelf_db ∈ 0.0..=6.0` (default **reads `paraeq_dsp::targets::RoomTargetSpec::default()`**, which is **+4.0** today — one source, pinned by a cross-crate field-for-field equality test; an ears ruling costs one line in `targets.rs`), `shelf_fc ∈ 40.0..=200.0` (default 105.0), `shelf_q` fixed 0.71 (RBJ). The **X-curve is explicitly refused**: it is scoped to rooms > 125 m³ and its −3 dB/oct is 3× the domestic consensus.

Room targets are matched, never guessed: `match_closest` is not offered on the room path at all, because "which curve does your room's response most resemble" answers a question nobody asked.

## Authority — Where We Are Allowed to Boost

This is the section where the naive rule is wrong and the correct rule is more useful anyway.

**The refuted rule.** "Room resonances below the transition frequency are minimum-phase, so correcting amplitude corrects phase; therefore full authority below 200 Hz." This is false as a blanket. An individual mode is a pole pair and *is* minimum-phase — but the measured in-room response is the **sum** of many modes plus direct sound, and summing minimum-phase systems does not preserve minimum phase. Real in-room responses are **mixed-phase throughout the modal region**. REW's own help is explicit: *"we cannot simply say a response is minimum phase below some specific cutoff"*, and it documents non-minimum-phase regions at **44–56 Hz** (below any plausible transition) alongside minimum-phase regions at **300–500 Hz** (above it). Toole 2015 (JAES) attributes minimum-phase behaviour to loudspeaker **transducers**, never to rooms. "Full authority below 200 Hz" is unsafe as a blanket rule and must not appear in ParaEQ.

**What survives, and it is enough.** Modal **peaks** are poles: locally minimum-phase, and a matched parametric filter corrects amplitude and phase together and genuinely reduces ringing at the measurement position. Modal **dips** are destructive interference: non-minimum-phase, and uncorrectable by EQ — boosting one burns headroom and excursion to produce the same cancellation, louder. So authority must be established **per region, from the measurement**, never assumed from a frequency threshold.

Three gates compose. `A(f)` is the per-frequency ceiling in dB, evaluated on the log-f analysis grid.

**1. Excursion envelope (both directions).** Trinnov's shipped curve: ±10 dB at or below 150 Hz, tapering to ±2 dB by 500 Hz, ±2 dB above.

```
A_base(f) = 10.0                                              f ≤ 150
          = 10.0 − 8.0·(log₁₀f − log₁₀150)/(log₁₀500 − log₁₀150)   150 < f < 500
          = 2.0                                               f ≥ 500
```
(Check: `A_base(300) = 5.39 dB`.) On the coupler path, `A_base(f) = 0` above 10 kHz.

**2. Excess-group-delay gate (boost only).** Compute EGD(f) as the measured phase minus the phase of its minimum-phase reconstruction — `fir::minimum_phase_homomorphic` already implements the reconstruction, so this is reuse, not new math. Flat EGD over a region ⇒ that region is minimum-phase ⇒ boost is meaningful there. The test, per 1/3-octave band centred at `f_c`: peak-to-peak EGD deviation within the band `< 1/(4·f_c)` (a quarter period — self-scaling: 5 ms at 50 Hz, 0.5 ms at 500 Hz). Bands failing the test get `boost_ceiling = 0`.

**3. Confidence gate (boost only).** σ(f) is the per-bin standard deviation in dB across positions, computed on the Align-SPL'd, smoothed, per-position curves — one extra pass over data already held.

```
w(f) = clamp((σ_none − σ(f)) / (σ_none − σ_full), 0.0, 1.0)  σ_full = 1.0 dB, σ_none = 6.0 dB
boost_ceiling(f) = A_base(f) · w(f) · [EGD flat at f]
cut_ceiling(f)   = A_base(f)
```

**The `σ_full = 1.0 / σ_none = 6.0 dB` endpoints are the canonical `AuthorityPolicy` defaults, owned by room-dsp's `authority.rs` — `decide()` consumes the `AuthorityCurve` that module produces and must not re-specify different numbers.** (They sit just above the 5.57 dB diffuse-field asymptote, so authority reaches zero only as a region becomes fully statistical.) σ separates correctable-everywhere features (σ ≈ 0.6–0.8 dB) from position-dependent junk (σ ≈ 10 dB). So a confidence-derived authority **reproduces the ~200 Hz rule without hardcoding it** — and adapts upward in a treated room, where the real number is higher. `transition_hz` is a *report* of where `σ(f)` first crosses **3 dB** (roughly the midpoint of the authority taper, where `w ≈ 0.6`), not where `w(f)` reaches zero, and is never an input to the authority weight.

**4. Asymmetric picking.** `autofit.rs` picks bands by symmetric `residual[i].abs()` (autofit.rs:19), which fills dips as eagerly as it cuts peaks — exactly what Toole warns automated algorithms do. Since `correction = target − measured` (targets.rs:140–146), a positive residual is a boost. Required: a boost is admissible only where the positive excursion is **≥ 1/6 octave wide** at half its peak value. Narrow positive residuals are interference nulls; they are skipped, not filled.

**5. Hard limits.** `autofit.rs` today has a global Q clamp of `0.5..=20.0` (autofit.rs:71), a hardcoded `20.0..=20000.0` mask (autofit.rs:13), `Peaking` only, and — verified — **no gain limit of any kind**. `auto_fit_parametric_eq` gains `authority: &AuthorityCurve` and clamps each band's `gain_db` to `[−cut_ceiling(f₀), +boost_ceiling(f₀)]` before emitting it. Room auto mode ships **cut-only by default** (`boost_ceiling` scaled to 0 unless the user raises it in the drawer): cuts are free, boosts cost headroom and can damage drivers.

**Runtime stability.** The Jury test for a biquad — `|a₂| < 1` and `|a₁| < a₂ + 1` — exists in this repo only as build-time proptests (`crates/paraeq-dsp/tests/test_props.rs:7–42`, 4 tests × 64 cases). Library code has **no runtime guard**: `biquad::peaking/low_shelf/high_shelf/notch` return `[f64; 6]` infallibly with no Q clamp, no Nyquist/fc validation, and no `Result`. The `Q_max = 0.227·f₀/A` cap doubles as that guard on the design path, and `decide()` must never emit a band that fails Jury. **DECIDED (2026-07-21):** No — Stage-1 Jury `is_stable` + `Q_max` cap already guard at the install boundary, so `biquad`'s designers stay infallible (`[f64; 6]`) rather than churning every fixture test's call site. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q2.

## Preamp

```
preamp_db = −max(0.0, max over f ∈ [20, 20000] of sos_frequency_response_db(realized_cascade, f, sr))
```

Three things must be right about this.

**It is the realized cascade, not the requested correction.** After the Q cap, the excursion clamp, and band-dropping, the filters that actually run are not the filters autofit first proposed. Taking the peak of the requested curve under-reserves headroom when bands interact constructively and over-reserves when the clamp bit. `biquad::sos_frequency_response_db` (biquad.rs:71) computes the true cascaded product and is exactly the right primitive.

**No headroom constant.** AutoEQ's `ParametricEQ.txt` preamp is exactly `-compound.max_gain` with **zero** headroom (`frequency_response.py:211`). The `PREAMP_HEADROOM = 0.2` constant applies only to the GraphicEQ string and the min/linear-phase FIR impulse responses, where it subtracts from the normalized equalization **curve** — it never touches a PEQ cascade. (AutoEQ's README-quoted parametric preamp uses a hardcoded `0.1`, not the constant, and is a third thing again.) ParaEQ adopts the `-max_gain` rule and no constant. The ±1.0 safety clamp at chain.rs:171 remains the last-resort guard it was specified as, not the headroom mechanism.

**It must reach the engine.** `ParametricEQ::export_autoeq_format` emits a hardcoded `"Preamp: 0.0 dB"` literal (peq.rs:112) — faithful to its oracle, and a clipping bug the moment modal boosts exist. Required: (a) `export_autoeq_format` emits the computed preamp; (b) the preamp reaches the engine as a `preamp_lin` field **inside** `Correction`, applied on the **corrected path only**, so it swaps atomically with the correction it protects (`chain.rs`'s corrected loop) and leaves the pass-through path un-attenuated (`chain.rs`'s pass-through loop). **Not** a separate `EngineCommand::SetGainDb(preamp_db)` alongside `SetCorrection`: `gain_bits` is read once and applied on *both* chain paths, so that carrier would leave the bypassed side quieter than the corrected side by the whole preamp and silently bias the A/B control. See engine-hardening R1-1 §4 and `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` §D-1. A preamp that lives only in exported text protects other people's EQ software and not ParaEQ.

## Refusal and Sanity Checks

Refusal is what earns auto mode the right to hide everything. Each check produces a typed `Diagnostic`; the front-ends differ only in how much of it they render.

```rust
pub struct Diagnostic {
    pub code: DiagnosticCode,
    /// Which position, if position-scoped.
    pub position: Option<usize>,
    /// Plain-language remedy. Rendered in Rust, same reason as Rationale.
    pub remedy: String,
    pub severity: Severity,
    /// The number that tripped it — so the drawer can show the margin.
    pub value: Option<f64>,
}

pub enum Severity {
    /// Cannot proceed. `verdict = Refuse`, `correction = None`.
    Refuse,
    /// Proceed with a caveat; may de-weight a position.
    Warn,
}
```

`Refuse` means *we do not know how to do this correctly and will not guess*. `Warn` means *we did it, and here is what you should know*. The distinction is not severity theatre — a `Refuse` produces no installable correction, so the system stays as it was.

| Check | Detection | Threshold | Severity | Remedy copy |
|---|---|---|---|---|
| No signal | Per-position capture RMS and peak | RMS `< −60 dBFS` or peak `< −50 dBFS` | Refuse | "We heard nothing. Check the mic is selected as the input and the sweep is going to the right speakers." |
| Mic not connected / wrong device | `capture.input_uid` no longer resolves | — | Refuse | "The microphone we measured with ({name}) isn't connected any more." |
| Wrong transducer / coupling failure | Solved chain sensitivity vs the path's expected envelope | outside envelope | Refuse | "The levels don't look like {class}. An empty jig, headphones sitting on a desk, or a sweep going to the laptop speakers all look like this." |
| Clipping (session) | Fraction of samples at `\|x\| ≥ 0.997` in any input block (REW's rule) | `> 30%` of a block | Refuse | "The mic input clipped. Turn the input gain down 6 dB and measure again." |
| Clipping (position) | Any sample `≥ −0.3 dBFS` (0.9661) | any | Refuse *that position* | "Position {i} clipped. We dropped it — reduce input gain by 6 dB and re-measure just that one." |
| Noise floor | Silence capture RMS in the analysis band | `> −24 dBFS` (Dirac's gate; target −36) | Refuse | "The room is too noisy to measure. Turn off fans/AC and try again." |
| Low SNR (soft) | capture RMS − floor RMS in `correction_range` | `< 25 dB` | Warn + de-weight | "Position {i} was noisy ({snr:.0} dB). We used it, but weighted it down." |
| Low SNR (hard) | same | `< 15 dB` | Refuse | "Too much background noise to trust this. Fix the noise — **do not** turn the sweep up; that doesn't help." |
| Absurd curve | Span of the averaged curve over `[low_corner, 10 kHz]`; or midband tilt over 200 Hz–2 kHz | span `> 40 dB` or tilt `> 20 dB/decade` | Refuse | "This doesn't look like a loudspeaker or a headphone. Check the mic calibration file matches the mic." |
| Excessive inter-position variance | median σ(f) over `[low_corner, f_t]` | `> 6.0 dB` | Refuse | "Your positions disagree even in the bass, where they shouldn't. Did the mic move rooms, or is one speaker off?" |
| Position outlier — coupler LF | mean \|deviation\| from cohort over `[20, 200]` | `> 6 dB` | Refuse *that position* | "Position {i}'s bass is {d:.0} dB below the others. That's a seal problem, not the headphone. Reseat and measure again." |
| Position outlier — coupler HF | mean \|deviation\| from cohort above 1 kHz | `> 6 dB` | Warn (average it) | "Position {i} differs up top. That's normal placement scatter — we averaged it in." |
| Position outlier — room | mean \|deviation\| from cohort | `> 6 dB` | Warn | "Position {i} is unusual. Averaged in; check the mic wasn't against a wall." |
| Cal missing / unparseable | `bundle.cal` | `None` or parse error | Refuse | "We need your mic's calibration file. Without it we're measuring the mic, not your speakers." |
| Cal non-finite / non-monotonic | curve scan | any | Refuse | "The calibration file is malformed at row {i}." |
| Cal neighbour outlier | `\|g[i] − (g[i−1]+g[i+1])/2\|` | `> 1.5 dB` | Refuse | "The calibration file has a bad value at {f:.3} Hz ({g:.2} dB between neighbours of {a:.2} and {b:.2})." |
| Sweep-rate mismatch | `ir.sample_rate != capture.sweep_rate` | any | Refuse | Internal error — this is a bug, not a user condition. |
| Two-clock (gated paths) | `capture.input_rate != capture.output_rate` | any, on `Room` | Warn | "Your mic and speakers run on different clocks. We compensate, but timing-sensitive results are approximate." |
| Too few positions | accepted count | `< 3` | Refuse | "We need at least 3 {noun}s to tell your system apart from where you put the mic." |
| Verification residual | `residual_vs_prediction` RMS over the authority band — `correction_range ∩ { f : authority.at(f).max_boost_db > 0 || max_cut_db > 0 }`, taken on `Analysis::freqs_hz`. `residual_vs_target` is attached as `Evidence` and **never** gated | `> 2·flatness_target_db` — **OPEN [OWNER + NEEDS DATA]** on the multiplier | Refuse | "We checked our work and it didn't land. Rather than ship something wrong, let's walk through it together." |

**The verification gate's quantity is `residual_vs_prediction`, and its threshold is not settled.** This row previously read "residual RMS vs **target**"; the wizard spec's Decisions Log and its §"The two residuals" both say `residual_vs_target` is reported and *never* gated, and the shipped type agrees — `Verification { ir, installed, position_index }` (`crates/paraeq-decide/src/bundle.rs`) carries `installed` **only** so a prediction can be re-derived, and carries no target at all. The quantity, band and location are decided; the `2×` multiplier is **escalated**, because the wizard's own worked example of a *passing* verification is "to within 1.8 dB RMS", which the coupler gate (`flatness_target_db = 1.0` → 2.0 dB) refuses by 0.2 dB while the room gate (3.0 → 6.0 dB) is more than 3× looser. Ship it as one named constant read off `decisions.flatness_target_db.value`. See `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` §D-G and §E1 (plan cross-spec Open Question 5).

**The cal neighbour-outlier check is not hypothetical.** A shipping vendor file, `7005770_90deg.txt`, contains a bogus `0.0000` at 19.611 Hz sitting between neighbours of −3.13 and −3.11 dB — a 3.12 dB error **inside the full-authority band**, which trips the 1.5 dB threshold cleanly.

**Cal parsing is upstream of all of this and is brittle.** Verified: `compensation.rs:11` dispatches format on a leading double-quote, and `parse_minidsp` hardcodes `.skip(2)` (compensation.rs:60). UMIK-1 0-degree files have **one** header line while 90-degree files have **two**, so the header count is not a constant, and the parser silently drops the first data row on single-header files. Real-world header variation (quoted vs bare `Sens Factor =…`) is not reliably characterized and this spec **states no failure rate**. The robust fix is REW's own documented rule: *"Only lines which begin with a number are loaded, others are ignored."* Two hard constraints on that rewrite: the oracle (`prototype/paraeq/measurement/compensation.py`, `np.loadtxt(skiprows=2)`) **shares the flaw**, so both must change together or the fixtures manufacture a divergence; and the cal curve is **never normalized** — the EARS jig encodes a real 2.1 dB L/R capsule offset in the curve, and a helpful normalize-to-0-dB-at-1-kHz would bake a channel imbalance into every measurement. Write that regression test.

**User cancelled mid-sweep.** A cancelled or partially-captured position never enters `bundle.positions` — the type makes it unrepresentable rather than the analysis defending against it. If the survivors fall below 3, `TooFewPositions` fires. Per-position clear-and-remeasure (Dirac's precedent) is the interaction; never force a full restart on one bad capture.

**Closed-loop verification.** Neither Dirac nor Sonarworks re-measures; ParaEQ should, and sweep + deconvolution already exist. But the mechanism the research corpus proposed — pre-convolving the sweep with the active `CorrectionConfig` — is **wrong and must not be built**. `tap.rs:26–48` and the `TapSystem::create` exclusion-list build exclude ParaEQ's own process precisely *so that* a measurement sweep plays unprocessed; the 2026-07-02 spec (line 147) states the intent outright: "correction state cannot contaminate the measurement". Pre-convolution defeats the design.

The correct mechanism, which the same spec's stage-4 carry-forward already points at: **play the verification stimulus from a helper child process**. A child process has its own PID, is not in the exclusion list, and its audio therefore goes through the tap and the real `RealtimeChain` — a genuine acoustic measurement of the real filter, with no special-casing anywhere in the engine. Repo precedent: the `afplay` helper in `crates/paraeq-coreaudio/tests/test_hardware.rs`.

One caveat to carry: self-exclusion has a documented fail-open fallback. If `translate_pid` returns 0 after a 200 ms retry, the exclusion list is **empty** and ParaEQ's own audio *is* tapped (`tap.rs`'s `TapSystem::create` exclusion-list build, logging "watch for feedback"). On that degraded path a normal measurement sweep would be processed. `decide()` cannot see this; the capture layer must record `capture.self_excluded: bool` in the bundle, and `false` on a measurement (not verification) capture is a `Refuse`.

## The One-Engine-Two-Front-Ends Seam

```
                    ┌──────────────────────────────┐
   MeasurementBundle │  paraeq-decide::decide()     │  DecisionSet
   ──────────────────▶  pure; no I/O, clock, RNG;   ├──────────────┐
                    │  no knowledge of front-ends  │              │
                    └──────────────────────────────┘              │
                                                                  ▼
          ┌──────────────────────┬──────────────────────┬─────────────────────┐
          │ "just fix my sound"  │ "walk me through it" │  Advanced drawer    │
          │ renders: value       │ renders: value +     │  renders: domain    │
          │ hides:  rationale,   │   rationale +        │  writes:  Overrides │
          │         evidence,    │   evidence, one      │                     │
          │         domain       │   screen at a time   │                     │
          └──────────────────────┴──────────────────────┴─────────────────────┘
                         three renderers, one value, zero policy
```

Auto mode renders `decisions.iter().map(|d| d.value)` and nothing else — the result screen reports **numbered events** ("Tamed a +9.1 dB room mode at 47 Hz"), not a curve. Guided mode renders `value + rationale.text + evidence`, one screen at a time. The drawer renders `domain` as a control and writes `Overrides`. All three are formatters.

**The argument.** The two modes are not two products; they are two disclosure levels of one product. If that is true, then the *only* difference between them should be which fields of one struct get painted — and any other difference is a defect waiting to be found by a user in the drawer, discovering that the app says it did one thing and did another. Making disclosure the only axis of variation turns "the modes agree" from a promise into a tautology.

**What would break it — each of these is a review-blocking violation:**

1. **Any default living in TypeScript.** The moment a `?? 15` appears in the drawer for `fdw_post_cycles`, there are two defaults and one of them is invisible to every test in the repo.
2. **`decide()` taking a mode argument.** `decide(bundle, Mode::Auto)` is the fork, spelled out. Non-negotiable: `decide()` must not be able to know.
3. **A decision computed for display only.** If guided mode derives a number to show that auto mode doesn't compute, that number is policy and it has escaped.
4. **Rationale assembled in the renderer.** Copy is reviewable in the decision table above precisely because Rust renders it. String-templating in TypeScript means the two modes' explanations drift and no test notices.
5. **The drawer writing anything but `Overrides`.** A drawer that mutates `Decisions` directly, or sends a `SetCorrection` of its own, has become a second decision engine.
6. **A `Decision` without a `domain`.** It is then invisible to the drawer, which means it is a hidden constant with extra steps.
7. **A decision that skips `decide()`.** Any parameter the Tauri backend computes on its way to the engine is outside the seam and outside the tests.

Adding a decision is therefore a mechanical, reviewable act: add a `Decision<T>` field to `Decisions`, an `Option<T>` to `Overrides`, a `RationaleKey`, a row to the table above, and a fixture. Nothing in the UI changes.

## Override Semantics

Overriding an auto decision sets `source: UserOverride` and triggers that decision's invalidation tier. This is the mechanism that makes the drawer feel alive rather than punitive — and it is what a competitor's "recalibrate to change anything" flow cannot offer.

| Tier | What re-runs | Budget | Decisions in this tier |
|---|---|---|---|
| `Redesign` | autofit → Q cap → excursion clamp → band-drop → preamp, from the cached analysis curve | ~50 ms | `authority`, `correction_kind`, `correction_range`, `flatness_target_db`, `max_filters`, `preamp_db`, `q_cap`, `shelves` |
| `Reanalyze` | window → FDW → compensation → smoothing → Align SPL → averaging → σ(f) → transition, from cached IRs; then Redesign | ~200 ms | `align_spl_band`, `averaging`, `fdw_post_cycles`, `fdw_pre_cycles`, `left_window_ms`, `low_corner_hz`, `right_window_ms`, `smoothing`, `target`, `transition_hz`, `window_type` |
| `Recapture` | Everything. Re-measure. | minutes | `class`, `positions_n` (increase only) |

The tier is a property of the `Decision`, not a lookup table in the caller, so the front-end's update loop is one match on `d.invalidates`.

**Two invariants:**

- **Idempotence.** Overriding a decision to the value auto already chose must produce a `DecisionSet` identical in every field except that decision's `source`. This is a property test, and it is the test that catches "the override path recomputes differently from the auto path" — the classic form of the fork this whole design exists to prevent.
- **Cascade.** An override to a `Reanalyze`-tier decision re-runs `decide()` in full, so downstream `Auto` decisions move. Overriding `smoothing` changes σ(f), which moves `transition_hz`, which moves `authority`, which changes `max_filters` and `preamp_db`. Downstream decisions with `source: UserOverride` are **pinned** and do not move; everything else recomputes. Guided mode must show the cascade ("changing this also changed 4 other things"); auto mode shows the result.

### This is why raw per-position IRs must be persisted

`Reanalyze` is only a 200 ms operation if the impulse responses are still there. The prototype appends only smoothed post-compensation curves and keeps only the last `_current_ir`, so changing the cal file discards every measurement — even though compensation is applied *after* the FFT and could be re-applied for free. That is a storage bug masquerading as physics, and the Rust wizard must not inherit it. Without cached IRs, every row in the `Reanalyze` tier collapses into `Recapture` and the Advanced drawer becomes a reason to re-measure — which is precisely the experience the rescope exists to avoid.

**What a profile stores:**

| Item | Format | Notes |
|---|---|---|
| Per-position IRs | f32 WAV via `hound`, one file per position per channel | Window: `[peak − 100 ms, peak + 1100 ms]` — see below; **shipped as `+1500 ms`**, see the amendment |
| `CapturePlan` | serde JSON | Device UIDs, rates, sweep params, `self_excluded` |
| Cal file | verbatim text + parsed metadata | Never normalized; re-parsed on load so a parser fix retro-fixes old profiles |
| `Overrides` | serde JSON | The user's intent, replayable against a re-`decide()` |
| Last `DecisionSet` | serde JSON | Cache + audit trail; recomputable from the above |

> **AMENDED (2026-07-25, Stage 5) — the post-window is `+1500 ms`.** The
> wizard spec derives its post figure from the drawer's FDW domain (the largest
> post-peak extent any in-scope decision can request is `n_c / f_min`, so 1.5 s
> bounds the cycles domain at `n_c ≤ 30`) — a constraint this section's
> derivation does not bind. The larger number satisfies both, so it wins; the
> `− 100 ms` pre-window below is unchanged and is the figure that won over the
> wizard's 64 ms. Cost at the shipped window: ~307 KB per position per channel,
> so a 9-position stereo room profile is ~5.5 MB rather than the ~4 MB below.
> Plan item 19; implementation in `crates/paraeq-measure/src/store.rs`.

**The storage window is derived, not guessed.** `[peak − 100 ms, peak + 1100 ms]` because: 100 ms of pre-peak comfortably covers the entire `left_window_ms` domain (the IR peak sits at only ~46–64 ms — tap latency plus propagation — so REW's 125 ms left window is *physically impossible* here and left windows clamp to `min(requested, peak_index)`); and +1100 ms covers the `right_window_ms` domain ceiling of 1000 ms with margin for the noise-floor tail. It also discards the Farina harmonic products, which for a 5 s 20 Hz–20 kHz sweep arrive at `dt₂ = T·ln2/ln(f₂/f₁) = 502 ms` **before** the peak — data we never want in a "linear" response. Size: ~1.2 s × 48 kHz × 4 bytes ≈ 230 KB per position per channel; a 9-position stereo room profile is ~4 MB.

## Testing Strategy

Per CLAUDE.md, DSP modules are TDD'd against failing golden-fixture tests. `decide()` is policy, not DSP, and the oracle question needs a **clarification** — not an amendment, and certainly not a claim that the rules are jointly unsatisfiable.

**The precedent is already merged.** `crates/paraeq-dsp/src/spline.rs` (commit 309d71d) is a `paraeq-dsp` module with **no** fixtures directory, **zero** mentions in `generate_fixtures.py`, and **no** prototype addition. It is verified by analytic invariants (`passes_through_knots`, `n2_is_linear_and_n3_is_parabola`) plus inline pinned scipy values. "Fixtures are sacred" is a **provenance** rule — its own text sanctions editing the generator ("regenerate and commit script + output together") — and it survives untouched. What does not apply to `paraeq-decide` is *"the Rust DSP core must match the Python prototype"*: there is no prototype decision engine, and writing one this afternoon for the same purpose would not be an independent oracle, it would be theatre that laundered a design bug into a golden fixture.

(Related precedent, stated correctly: `fir.rs:1–4` says "Oracles:" **plural** and lists `prototype/paraeq/correction/fir_filter.py` **first**; scipy is a transitive dependency of the prototype. `spline.rs:1–5` cites `scipy.interpolate.CubicSpline` alone — and even that is not an independent third-party oracle, since the prototype uses `CubicSpline` too. The honest precedent is "cite the library the prototype itself delegates to.")

`paraeq-decide`'s tests, in order of authority:

1. **Golden bundles (the workhorse).** `fixtures/decide/<case>/bundle.json` + `expected.json`, committed. Cases: one per `TransducerClass`; a clean room; a room with a −30 dB null at one of five positions; a lost-seal coupler reseat; a cal file with the `7005770_90deg.txt` outlier; a clipped position; a noisy room at the SNR boundary; an EARS HEQ cal. These are **not** oracle fixtures — they are characterization fixtures whose expected values are reviewed by the owner once and then frozen. A diff in `expected.json` is a policy change and must be argued for in the PR, which is the entire point.
2. **Analytic invariants (stronger than fixtures).** `decide()` is deterministic (same bundle → same set, N times); every `Decision.value` is inside its `Domain`; `verdict == Refuse ⟺ correction.is_none()`; `verdict == Refuse ⟺ any diagnostic has `Severity::Refuse``; every band in the emitted `ParametricEQ` passes Jury (`|a₂| < 1`, `|a₁| < a₂ + 1`) at the design rate; every band's `gain_db` is within the authority envelope at its `f₀`; every boost band's Q ≤ `0.227·f₀/A`.
3. **The idempotence property.** Override each decision in turn to its own auto value; assert the `DecisionSet` is unchanged but for `source`. This is the anti-fork test.
4. **Arithmetic pin tests.** The averaging divergence is pinned by literal: a −30 dB null at **one of five** positions gives dB-avg = **−6.0 dB** and power-avg = **−0.97 dB**. (The −5.0/−0.79 pair is the *six*-position result; a −40 dB null at one of three gives −13.3 vs −1.76.) Note the two estimators agree **if and only if** magnitude is identical at every position; the gap is ≈ `0.1151·σ_dB²` and RMS ≥ dB always, by the power-mean inequality — a feature present everywhere but with spread still diverges. And power averaging is null-**resistant**, not null-immune: `10log₁₀((N−k)/N)` is the asymptotic floor as depth → −∞, reached within 0.01 dB only by about −20 dB depth (at −6 dB depth with N=5, k=1 the value is −0.705 dB, not −0.969); for `k = N` — a null common to every position, e.g. floor bounce or SBIR — the power average passes the null through **exactly**.
5. **Frozen upstream.** `test_fr.rs::averaging_is_in_db_domain` must keep passing untouched. `fr::average_measurements` is an intentional, oracle-documented, fixture-locked dB-domain mean, and it is *correct for the coupler path*. It has zero non-test callers in Rust today, so the room-averaging question is a **design decision for new code** (`average_measurements_rms` is additive), not a live bug. REW's actual position is narrower than folklore: *"dB averaging may be useful when averaging smoothed traces to derive an EQ target, with unsmoothed data the dips would have a disproportionate effect on the result"* — and the prototype smooths 1/6-octave **before** averaging (`measurement_wizard.py:481–484`), which is squarely REW's endorsed regime. The genuinely actionable neighbour was always `autofit.rs`'s missing boost cap.

`decide()` has no synthetic-block or hardware tests: it touches no audio thread and no device.

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| **The DSP `decide()` calls does not exist.** No gating, FDW, σ(f), Schroeder, room targets, authority, or logf module; ~two-thirds of the decision table is greenfield. | This spec is deliberately written against the *interfaces*, so `paraeq-decide` can be TDD'd against stub DSP with golden bundles from day one. But the schedule is serial: the room-DSP spec's `logf.rs` gates FDW, smoothing, splice and targets, and must land first even though it looks like plumbing. |
| **σ(f)-derived `transition_hz` is the one novel piece with no shipping precedent.** If it proves unstable on real rooms it will produce a jumpy authority band. | The fallback is already in the type: `source: Default`, value 200.0, with the σ(f) curve retained as `Evidence` and plotted. Ship the evidence even if the derivation is demoted. |
| **Two-clock (UMIK-1 at its own fixed rate vs the output device).** The 2026-07-02 spec's "the Farina method tolerates their small clock skew (prototype proved it)" was proven for an **ungated coupler magnitude** measurement. Gating needs a trustworthy t=0 and an undistorted IR shape. **That claim does not transfer**, and this is the biggest unscheduled cost in the rescope. | `Warn(TwoClock)` on gated paths from day one so the limitation is disclosed rather than discovered. Drift estimate recorded in the bundle. Resolution is out of scope here and is an owner-level scheduling decision (see Open Questions). |
| **`decide()` gets pulled toward the UI** the moment the drawer needs interactivity (live preview while dragging). | Preview is a `Redesign`-tier `decide()` call at ~50 ms — fast enough to run per drag-end, and the ~70 ms debounce precedent already exists in the Target tab. If a live-drag preview genuinely needs sub-frame latency, it renders the *previous* `DecisionSet`'s curve locally and re-`decide()`s on release. It never computes a decision. |
| **NaN poisons the IIR state permanently.** One NaN in `iir.rs`'s DF2T feedback (`z[0] = b1·x − a1·y + z[1]`, iir.rs:56–60) sticks forever; the FIR path flushes after its tail. Verified: `(f32::NAN).clamp(-1.0, 1.0).is_nan() == true` — Rust's clamp returning NaN for NaN is documented and intentional, and infinity **is** clamped correctly. | **Do not "fix" the clamp.** Sanitize at the **capture boundary**, before filter state, per CamillaDSP `src/utils/conversions.rs:89–106`: `if !value.is_finite() { invalid_values += 1; *value = 0.0; }`, plus a `warn!` with the count off the realtime thread. **Owned by `engine-hardening-design.md` R1-2, which places the input-side guard in `paraeq-coreaudio`'s `backend.rs:174–176` deinterleave copy — `RtProcessor::process_block`'s peak scan (shared.rs:225–232) reads immutable `&[&[f32]]` and cannot sanitize in place** — with an output-side backstop in the `chain.rs` gain/clamp loop. A NaN-triggered `Correction::reset()` (chain.rs:22, realtime-safe) is reasonable belt-and-braces; input sanitization is the primary fix. Note CamillaDSP gates this per-format and its own live device paths pass `check_for_nan: false`, so it shares the exposure — but the pattern is in-tree, tested, and directly liftable. |
| **Coefficient staleness across sample-rate change** (`build_correction` takes no rate; see "The rate-independence requirement"). | A `Refuse`-grade correctness bug that this spec's `CorrectionPlan` shape prevents by construction — provided fix (1) or (2) lands with it. Regression test: send `SetCorrection`, force a `FormatChanged` to a different rate, assert the installed SOS were re-derived. |
| **Advanced drawer surface area** — 21 decisions is a lot of UI. | It is one generic renderer over `DecisionView`, driven by `Domain`. `Range` → slider, `Choice` → select, `Derived` → read-only text. Adding a decision adds no UI code. |
| **Auto mode ships a bad correction silently.** | Closed-loop verification is the safety net that earns auto mode its silence, and it refuses rather than shipping. Neither Dirac nor Sonarworks does this — it is an open differentiator and it is cheap (sweep and deconvolution already exist). Competitive clock: the tap-architecture cohort (OnlyEQ, iQualize, eqtune) does not measure at all; the window before one bolts on a sweep is ~12–18 months. |

## Out of Scope

- **The room DSP itself** (`fdw`, `gating`, `logf`, `room`, `splice`, `authority`, the `fr`/`targets`/`compensation` reworks). Specified separately; `decide()` calls them.
- **The pre-capture level ladder** (pilot tone → chain-sensitivity solve → ≤6 dB escalation → SPL caps → fade → abort). Owned by the measurement-safety spec. Note the `-6 dBFS` output-level policy has **no home in the Rust tree**: `SWEEP_AMPLITUDE = 0.5` lives at `prototype/app/wizard/measurement_wizard.py:39`, the **playback** layer, which is not ported — there is no sweep playback path anywhere in Rust, and `generate_sweep`'s only caller is its own test. `sweep.rs` faithfully matches its oracle `prototype/paraeq/measurement/sweep.py`, which is also unscaled, and the peak-1.0 convention is deliberate (`noise.py`: "normalized to a peak of 1.0 so the caller can apply any output amplitude"; CLAUDE.md scopes `paraeq-dsp` to pure math). The prototype's actual sweep is −9 dBFS RMS = 3 dB **above** REW's −12 dBFS default and 6 dB **below** its −3 dBFS maximum. This is a forward-looking requirement for stage 6, **not a regression** to fix.
- **Time alignment, subwoofer integration, crossovers.** Rescope-excluded this release; the seams are `PathProfile` (a new `Subwoofer` variant) and `AuthorityKind`.
- **Wizard screen flow, copy layout, progress rendering.** Owned by the wizard spec. This spec pins the rationale *strings*, not where they appear.
- **The AutoEQ browser, profiles CRUD, analyzer.** Unchanged from the 2026-07-02 spec.
- **`fixtures/manifest.json`'s version pin.** It is a **generated provenance record** (`generate_fixtures.py:235–239` writes `scipy.__version__` at runtime), and the only dependency *declaration* is `prototype/pyproject.toml:16`'s `scipy>=1.10` — a floor. Nothing enforces 1.18.0, and regenerating under a newer scipy silently rewrites the manifest. That is a real latent defect and it needs fixing, but it is owned by the master spec's R0 (`measurement-suite-design.md`, *The manifest-is-not-a-pin defect*) and detailed in `room-dsp-design.md`, not here.
- **The `Bypass`/`Disable` semantics, teardown ordering, tap lifecycle.** Unchanged; `decide()` never touches the engine's lifecycle.

## Open Questions

1. **Rate-independence fix (1) or (2)?** **DECIDED (2026-07-21):** fix (1) — carry bands + `design_rate`, re-derive at the live rate; engine-hardening R1-6's refuse-and-fail-open is the last-resort net. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q1. Fix (1) puts the invariant in the type — `CorrectionConfig` carries bands and `build_correction` takes the live rate, re-deriving SOS per rebuild — rather than in a Tauri call site that can be missed; R1-6 then fires only for a legacy baked-SOS config that genuinely cannot be re-derived at the live rate.
2. **Should `biquad`'s designers become fallible?** **DECIDED (2026-07-21):** No — Stage-1 Jury `is_stable` + `Q_max` cap already guard at the install boundary. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q2. `build_iir` substitutes the identity section for any row failing Jury and the `Q_max = 0.227·f₀/A` cap prevents the degenerate cases on the design path, so the designers stay infallible (`[f64; 6]`) rather than churning every fixture test's call site; revisit only if a manual-EQ path wants per-band validation messages.
3. **Does the FDW pre/post asymmetry earn its keep?** **DECIDED (default) [NEEDS DATA]:** retain the FDW pre/post asymmetry, default `n_c_pre = 3.0`; lowest-confidence knob, collapse to symmetric if real rooms show no effect. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q3. The left-window clamp to `min(requested, peak_index)` already zeroes the pre-peak region, and a causal acoustic IR has no linear signal before the direct arrival — so the pre-lobe mostly windows zeros and `n_c_pre` may be a knob with no effect. It is retained because it bounds how much noise floor the smoother integrates and makes the asymmetry explicit rather than an accident of the clamp. Note the exact identity — an FDW of half-amplitude width `n_c/f` is *exactly* a constant-Q Gaussian smoothing of the **complex** spectrum, `σ_f/f = √(2ln2)/(π·n_c)`, `FWHM = 4/(π·n_c)` octaves, verified to < 2e-15 dB — holds only for the symmetric case; the asymmetric case is two half-Gaussians on the log-f axis, an approximation whose symmetric limit must reproduce the identity exactly. REW ships a single symmetric window and is adequate, so the asymmetry is an Acourate-class refinement to validate against the first real room measurements.
4. **EGD flatness threshold.** **DECIDED (starting value) [NEEDS DATA]:** ship `1/(4·f_c)` peak-to-peak EGD over a 1/3-oct window (a 90° excess-phase cap); validate on real rooms. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q4. `1/(4·f_c)` is the defensible middle of the `1/(2·f_c)`…`1/(8·f_c)` bracket (180° can invert polarity within the band; 1/8 rejects real modal peaks whose EGD is never perfectly zero) and self-scaling, but is not measured against real rooms — needs empirical tuning, and the σ(f) gate is the belt to its braces.
5. **σ authority thresholds `σ_full = 1.0` / `σ_none = 6.0` dB.** **DECIDED (starting value) [NEEDS DATA]:** ship `σ_full = 1.0` dB, `σ_none = 6.0` dB (6.0 sits just above the 5.571 dB diffuse-field asymptote); keep the wide gap. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q5. These are the canonical `AuthorityPolicy` defaults from room-dsp's `authority.rs` (this spec previously carried a divergent `σ_hi = 3.0`; reconciled to the owning module), anchored between the observed correctable-everywhere range (0.6–0.8 dB) and the asymptote. Do not narrow the gap — with only ~5–9 positions the sample σ of a high-variance quantity carries ~30–50% uncertainty. The separate **3 dB `transition_hz` detection threshold** is a reporting marker, not the authority endpoint. Ship the σ(f) curve as evidence so the owner can retune from real data.
6. **Two-clock resolution.** **DECIDED (2026-07-21):** adopt REW's bracketed-timing-marker skew estimate + resample, default on; `Warn(TwoClock)` fallback. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q6. A timing marker at the start and end of the sweep gives the sample-count skew (REW reports ~12 ppm across devices) and the capture is resampled to correct it — a port of a known technique, not open research. Still run measurement-suite/9 to confirm the magnitude on the owner's rig and set the "clock adjustment too large" reject bound; the drawer exposes a clock-adjust toggle with the estimated ppm, defaulted on.
7. **Room `positions_domain` ceiling of 15.** **DECIDED (2026-07-21):** default 9 positions, ceiling 15, floor 5, hard-min 3. See docs/decisions/2026-07-21-decision-engine-open-questions.md §Q7. Default 9 lands on the knee of the mean-convergence curve (standard error ∝ 1/√N) and matches Dirac's single-listener preset; ceiling 15 sits near the half-wavelength decorrelation limit, beyond which added points are mostly correlated. Bias the default up toward 11–13 in multi-seat mode, where per-frequency variance is the count-hungry statistic.
8. **Commercial posture.** **DECIDED (owner, 2026-07-22):** stay free & non-commercial, retaining the PLD Art. 2(2) FOSS exemption; no paid tier, no pay-with-data. See docs/decisions/2026-07-22-owner-value-calls.md and docs/decisions/2026-07-21-decision-engine-open-questions.md §Q8. (Legal counsel still verifies the cited clause numbers before a published release — safety spec S-1/S-3.) Recorded here because it constrains the safety design, not because this spec resolves it: PLD 2024/2853 (transposition due 2026-12-09, inside this release's life) makes software a product, Art. 6(1)(a) covers personal injury, and Art. 14 voids contractual exclusion — the MIT "AS IS" disclaimer is legally inert against it. The protection is Art. 2(2)'s FOSS carve-out, which survives only while ParaEQ is supplied entirely outside commercial activity (recital 15 kills it for software supplied "in exchange for a price, or for personal data"). Any paid tier forfeits the exemption.
9. **`q_cap`'s domain vs its `QCapPolicy` type.** **DECIDED (2026-09-16):** make the domain a `Choice` over the **two path policies** — `Ceiling(5.0)` and `LogLinear { hi: (10000.0, 3.0), lo: (200.0, 10.0) }` — so the room's `LogLinear` value has representable membership and the "every value is inside its `Domain`" invariant becomes mechanically checkable. See docs/decisions/2026-09-16-post-merge-and-stage6-calls.md §D-C. This is the smaller of the two options this question itself sanctioned and the only one with no `Decisions`/`Overrides` shape churn: the drawer's control becomes a two-item select over the two policies, which is what the value actually is. The alternative (split `QCapPolicy` into a decided ceiling scalar + a profile-owned shape) changes both shapes and is the bigger, later move. **Must land before `fixtures/decide/` is frozen** — domains are serialized into `expected.json`. Surfaced while building Stages 1–2; see docs/plans/2026-07-16-rescope-implementation.md (item 9).
10. **`authority`'s domain vs its `AuthorityCurve` type.** **DECIDED (2026-09-16):** re-type the decision to `Decision<AuthorityPreset>` with `AuthorityPreset { Conservative, Custom(AuthorityCurve), Standard }`, and publish the **resolved** curve in `Analysis`. See docs/decisions/2026-09-16-post-merge-and-stage6-calls.md §D-D. The preset names now have a home, so the drawer can label its control and an override round-trips a **name** rather than a 957-point curve — which is what the owner-directed REW-parity drawer needs ("resolving open question 10 above — the preset *names* need a home — is what makes this override presentable", docs/plans/2026-07-16-rescope-implementation.md). It matches this table's domain text verbatim, keeps `Decisions` at 21 fields so the exhaustiveness test stays green, and keeps `AuthorityCurve` sealed. **Must land before `fixtures/decide/` is frozen.** Surfaced while building Stages 1–2; see docs/plans/2026-07-16-rescope-implementation.md (item 10).

## Research Basis (2026-07-15)

Facts this spec is built on, with the corrections that shaped it. Where a design choice was driven by a refuted claim, the correct fact is stated above and the refuted one does not appear.

- **Refuted — "rooms are minimum-phase below the transition."** Summing minimum-phase systems does not preserve minimum phase; real in-room responses are mixed-phase throughout the modal region. REW: "we cannot simply say a response is minimum phase below some specific cutoff", documenting non-minimum-phase at 44–56 Hz and minimum-phase at 300–500 Hz. Toole 2015 (JAES) attributes minimum-phase behaviour to transducers, never to rooms. Consequence: authority is established per-region via excess group delay. "Full authority below 200 Hz" is unsafe as a blanket rule.
- **Refuted — "an omni mic at the listening position samples the pressure the ear receives below ~500 Hz."** At 500 Hz, ka ≈ 0.82 and the ipsilateral ear is +2.3 dB above free field. Honest thresholds: ≤0.5 dB needs f below ~270 Hz; ≤1 dB needs f below ~340 Hz. Use **~300 Hz**. And the head **diffracts strongly** at low frequency — that is *why* ILD is small.
- **Refuted — "dB and RMS averaging are identical for features present at all positions."** They agree iff magnitude is identical at every position. Gap ≈ `0.1151·σ_dB²`; RMS ≥ dB always.
- **Refuted — "power averaging is null-immune."** `10log₁₀((N−k)/N)` is the asymptotic floor, reached within 0.01 dB only by ~−20 dB depth; at −6 dB depth (N=5, k=1) the value is −0.705 dB. For `k = N` the null passes through exactly. Null-**resistant**.
- **Corrected numbers.** −30 dB null at one of **five**: dB-avg −6.0, power-avg −0.97. (−5.0/−0.79 is the six-position result.) −40 dB at one of three: −13.3 vs −1.76.
- **Corrected — REW on dB averaging.** Scoped to unsmoothed data; the prototype smooths 1/6-octave first (`measurement_wizard.py:481–484`), REW's endorsed regime. `fr::average_measurements` has zero non-test callers in Rust — latent design decision, not live bug. The actionable neighbour is `autofit.rs`'s missing boost cap.
- **Corrected — the oracle question.** `spline.rs` (309d71d) is an existing counterexample: a `paraeq-dsp` module with no fixtures, no generator mention, no prototype addition. "Fixtures are sacred" is a provenance rule that survives. Clarification, not contradiction.
- **Corrected — AutoEQ's preamp.** `ParametricEQ.txt` preamp is `-compound.max_gain` with **no** headroom (`frequency_response.py:211`). `PREAMP_HEADROOM = 0.2` applies only to the GraphicEQ string and min/linear-phase FIRs, subtracting from the normalized equalization curve; the README-quoted parametric preamp uses a hardcoded `0.1`.
- **Corrected — tap self-exclusion.** It exists *so that* sweeps play unprocessed (2026-07-02 spec, line 147). Do not pre-convolve. Closed-loop verification uses a helper child process (precedent: `tests/test_hardware.rs`'s afplay). Fail-open fallback at `tap.rs`'s `TapSystem::create` exclusion-list build leaves the exclusion list empty and ParaEQ's own audio tapped.
- **Corrected — sweep level.** `SWEEP_AMPLITUDE = 0.5` lives in the unported playback layer; `sweep.rs` faithfully matches its unscaled oracle and the peak-1.0 convention is deliberate. The prototype's actual sweep is −9 dBFS RMS: 3 dB above REW's default, 6 dB below its maximum. Forward-looking requirement for stage 6, not a regression.
- **Corrected — the one-third figure.** An exponential 20 Hz–20 kHz sweep does spend exactly `ln(10)/ln(1000) = 1/3` of its duration in the 2–20 kHz band — but do **not** attribute it to REW, which widens a requested range to half-start/twice-end capped at Nyquist. REW does warn that long sweeps risk tweeter overheating and are not recommended for loudspeakers.
- **Corrected — cal parsing.** `compensation.rs:11` dispatches on a leading double-quote; `:60` hardcodes `.skip(2)`. 0-degree UMIK-1 files have one header line, 90-degree two; the parser silently drops the first data row on single-header files. Real-world header variation is contested and **no failure rate is claimed**. Fix per REW's rule ("only lines which begin with a number are loaded"); the oracle shares the flaw and must change in lockstep.
- **Corrected — IR windowing.** `fir.rs:85`'s `windowed_ir()` Hann-windows a **designed** IR, live on both arms of `design_fir_correction`. What is true: nothing time-gates a **measured** IR.
- **Verified — the FDW identity** (the highest-leverage result). Half-amplitude width `n_c/f` ≡ constant-Q Gaussian smoothing of the **complex** spectrum; `σ_f/f = √(2ln2)/(π·n_c)`, `FWHM = 4/(π·n_c)` octaves; verified to < 2e-15 dB. `n_c = 4N/π` makes REW's 15-cycle default 1/11.8 ≈ 1/12 octave. At 15 cycles: 750 ms at 20 Hz (effectively ungated), 1.5 ms at 10 kHz (quasi-anechoic). Implement as O(N log N) convolution on a log-f axis, never O(F·N).
- **Verified — gate resolution.** A window of length T resolves nothing below ~1/T; the stricter criterion is `f = (1/T)/(2^(1/2N) − 2^(−1/2N))`. A 3–6 ms gate is blind below 167–333 Hz. A 400 ms right window is 1/6-octave-valid above **21.6 Hz**. `deconvolve()` puts the IR peak at only ~46–64 ms, so REW's 125 ms left window is physically impossible.
- **Verified — Farina harmonic constraint.** `dt₂ = T·ln2/ln(f₂/f₁)`; a 5 s 20 Hz–20 kHz sweep puts H2 only **502 ms** before the peak.
- **Verified — target category error.** `harman_oe_2018` is +8.3 dB at 3 kHz (+0.22 dB/oct); B&K's room curve is −3.0 dB at 3 kHz (−0.84 dB/oct); the gap is **11.3 dB**. `match_closest_target` (targets.rs:193–211) has no category filter despite `category: Option<String>` at targets.rs:76.
- **Verified — smoothing performance.** `fractional_octave_smooth` is a rectangular boxcar with an O(N²) inner scan — 2586 ms for 65536 bins. It never bit because headphone IRs are short; room IRs are 10–100× longer. Alvarez–Mazorra recursive Gaussian (`λ = q²/(2K)`, `ν = (1+2λ−√(1+4λ))/(2λ)`, K ≈ 4, Getreuer's q-correction) is O(N·K). The kernel changes boxcar → Gaussian, so exact fixture parity is impossible: keep `Fixed(n)` bit-exact on the old path, make new modes **additive**.
- **Verified — REW variable smoothing** (1/48 oct below 100 Hz, 1/6 at 1 kHz, 1/3 above 10 kHz) "is recommended for responses that are to be equalised." It is *fine* in the bass and *coarse* in the treble — the inverse of psychoacoustic smoothing — and that inversion **is** the authority mechanism.
- **Verified — σ(f).** One extra pass over held data. Separates correctable-everywhere features (σ ≈ 0.6–0.8 dB) from position-dependent junk (σ ≈ 10 dB); rises toward the diffuse-field asymptote of **5.57 dB** above Schroeder, so confidence-derived authority reproduces the ~200 Hz rule without hardcoding it and adapts to a treated room. No competitor automates this.
- **Verified — Align SPL first, mandatory.** REW: "it is usually best to first use Align SPL to remove overall level differences due to different source distances." `normalize_to_reference_band` is the existing primitive. Without it, near positions dominate the power average **and** inflate σ(f).
- **Verified — vector averaging: reject and guard.** It collapses toward the incoherent floor `−10log₁₀(N)` once position spread approaches a wavelength (−10.94 dB at 1.5 kHz for ±40 cm). Error if multi-position data reaches a coherent routine.
- **Verified — REW boost-Q cap.** `Q_max = 0.227·f₀/A`, `A = 10^(G/40)`, derived from a 500 ms T60 rule and checked against ParaEQ's own `biquad.rs` coefficients. Doubles as the runtime stability guard.
- **Verified — Jury test** (`|a₂| < 1`, `|a₁| < a₂ + 1`) exists only as build-time proptests (`test_props.rs:7–42`, 4 tests × 64 cases). No runtime guard: `biquad`'s designers return `[f64;6]` infallibly, no Q clamp, no Nyquist/fc validation, no `Result`.
- **Verified — `autofit.rs`** picks by symmetric `residual[i].abs()`, global Q clamp `0.5..=20.0` (autofit.rs:71), **no gain limit**, hardcoded 20–20000 Hz mask (autofit.rs:13).
- **Verified — Trinnov's shipped excursion curve.** ±10 dB at or below 150 Hz tapering to ±2 dB by 500 Hz; ±2 dB above.
- **Verified — Schroeder.** `f_s = 2000·√(T60/V)`. Return a **range** (0.5·f_s to 2·f_s) with a 200 Hz fallback. T60 via Schroeder backward integration.
- **Verified — never normalize a cal curve.** EARS encodes a 2.1 dB L/R capsule offset **in** the curve. Outlier validator required: `7005770_90deg.txt` contains a bogus `0.0000` at 19.611 Hz between neighbours of −3.13 and −3.11, inside the full-authority band.
- **Verified — Klippel AN39 splice.** Mean-dB level match over the overlap, then raised-cosine blend, with 1-octave RMS error as the auto-tune objective. Unmatched splice produced a **13.28 dB** step. Level-matching is mandatory, not cosmetic. (Only relevant if FDW is not adopted; FDW subsumes it.)
- **Verified — engine state preservation.** Apple's `BiquadFilter.setCoefficients(_:setup:resetState:)` with `resetState: false` on incremental edits preserves delay state — how it avoids clicks without a crossfade. ParaEQ's `chain.rs:89` `mem::replace` starts the new processor from **zeroed** state, discarding up to 4095 samples of FIR overlap tail. The engine has no smoothing/ramping/crossfade (grep-verified), no denormal handling, no clip counter; `input_peak` is a monotonic session max with no decay.
- **Verified — the OnlyEQ one-line fix.** `kAudioSubDeviceInputChannelsKey: 0` on the aggregate sub-device, "otherwise running our IOProc counts as microphone access and macOS shows a mic permission prompt". ParaEQ's sub-device dict (tap.rs:57–61) is a bare `{kAudioSubDeviceUIDKey}`. This dissolves the known limitation at backend.rs:121–130 **and** removes a spurious mic prompt — which now matters far more, because a spurious prompt is indistinguishable from ParaEQ's legitimate measurement-mic prompt.
- **Verified — latency.** Measured 46–62 ms with a ~41 ms fixed tap floor, fitting `40.96 ms + 2.0·buffer_period`, with a 1966-sample floor invariant across a 4× buffer sweep. Competitors' "~10 ms" claims are arithmetic (OnlyEQ's `estimatedLatency = ioBufferFrames*2/sampleRate`), not measurement; iQualize removed its "Low Latency" toggle because ring capacity "did not meaningfully reduce latency". `backend.rs:195` computes `out_sample_time − in_sample_time`, which **includes** the output device's own safety offset and DAC latency that exist with or without ParaEQ — so "ParaEQ adds 41 ms" is an over-claim; the added latency is not established.
- **Competitive clock.** Dirac shipped Mac ART 2026-06-30 ($499/$899, HAL driver + root LaunchDaemon, no macOS uninstaller). Sonarworks SoundID Reference (€249) measures and corrects system-wide on Mac but **refuses USB microphones** — which kills both the UMIK-1 and the EARS — demanding XLR + phantom + interface. OnlyEQ (Unlicense, 2026-07-03), iQualize (MIT, March 2026) and eqtune ship ParaEQ's exact tap architecture; **none measure**. The window before one bolts on a sweep is ~12–18 months.
