//! The decision rules — one per row of the spec's decision table.
//!
//! One function (or one clearly-labelled arm) per row, each reading only what
//! its "Input" column names, so the spec's dependency DAG is visible in the
//! signatures rather than in a comment.
//!
//! What is contract rather than policy, and therefore B7a's:
//!
//! - **The domains.** Each one is the decision table's, with the three rulings
//!   that moved one: § D-C's two-item `q_cap` `Choice` (a `Range` over
//!   `QCapPolicy` answered `contains == false` for the room path's own value),
//!   § D-D's `AuthorityPreset` (so the preset NAMES have somewhere to live), and
//!   § D-K's `fdw_post_cycles` ceiling, derived at runtime.
//! - **The invalidation tiers.** § Override Semantics' three-way partition,
//!   plus `clock_adjust` at `Recapture`. The tier is a property of the
//!   `Decision`, "not a lookup table in the caller".
//! - **The override path.** [`resolve`] is the ONE place an override lands, so
//!   every rule inherits it by construction rather than each remembering to
//!   honour `Overrides`.
//!
//! **What B7b added: the four policy scans, the target selection, the fit, and
//! the copy.** The scans live here rather than in `paraeq-dsp` because each is
//! policy — a threshold the decision table names — over curves the DSP already
//! produced:
//!
//! - [`low_corner_scan`] (`low_corner_hz`) — the `M − 10 dB` crossing that
//!   makes bookshelf-vs-floorstander a measurement rather than an analysis
//!   branch.
//! - [`transition_scan`] (`transition_hz`) — the sustained σ ≥ 3.0 dB crossing.
//! - [`snr_db`] (`correction_range`'s low edge and the top of its search) — the
//!   per-bin SNR floor.
//! - [`last_target_crossing`] (`correction_range`'s high edge) — "no filters
//!   above the frequency where measured **last drops below** target", scanned
//!   DOWN. Scanning up returns the first dip instead of the roll-off knee, and
//!   on a rolled-off tweeter the two answers differ by an octave.
//!
//! **Thresholds are `decide()`'s own named constants.** Each is stated once
//! here with the decision-table row it comes from, so a policy change is a
//! one-line diff in a file whose whole subject is policy.
//!
//! **The phase split is the spec's own dependency DAG.** Eleven decisions are
//! read BEFORE any curve exists and feed the analysis ([`analysis_decisions`]);
//! ten are read OFF the analysis ([`design_decisions`]); the fit
//! ([`fit_correction`]) turns those ten into bands; and `preamp_db` is last
//! ([`preamp_decision`]) because it is a property of the bands the fit emitted,
//! not of the curve they were fitted to. That is the
//! `Recapture`/`Reanalyze` → `Redesign` order the tier table describes, spelled
//! as four function calls.

use crate::analysis::{fdw_post_ceiling, realized_preamp_db, AnalysisProducts, Geometry};
use crate::bundle::{CalVariant, MeasurementBundle};
use crate::decision::{
    Decision, Domain, Evidence, EvidenceLabel, Invalidation, Rationale, Source, Unit,
};
use crate::decisions::{
    AuthorityPreset, CorrectionForm, Decisions, QCapPolicy, TargetChoice, WindowType,
};
use crate::profile::{AveragingMode, CouplingPath, GatingMode, PathProfile, SmoothingMode};
use crate::rationale;
use paraeq_dsp::authority::{AuthorityCurve, Clamp, COUPLER_Q_CEILING, ROOM_Q_CEILING};
use paraeq_dsp::autofit::{auto_fit_room, RoomFitPolicy};
use paraeq_dsp::fr::{normalize_to_reference_band, DEFAULT_SPL_ALIGN_BAND};
use paraeq_dsp::gating::{
    farina_h2_bound_s, resolution_limit_hz, SweepParams, RESOLUTION_FRACTION,
};
use paraeq_dsp::logf::{resample_db_to_log_grid, LogGrid, Prefilter};
use paraeq_dsp::peq::EQBand;
use paraeq_dsp::room::{transition_range, TRANSITION_FALLBACK_HZ};
use paraeq_dsp::targets::{
    build_room_target, compute_correction, match_closest_target, RoomTargetSpec, TransducerClass,
};
use paraeq_dsp::PerChannel;

/// `M`'s band: "mean magnitude over 200 Hz–2 kHz of the aligned, averaged,
/// smoothed curve". The same band `align_spl_band` defaults to, which is one of
/// § D-B's two grounds for that default — and the band the measured curve and
/// the target curve are level-matched over before a correction is computed, so
/// that the correction is a SHAPE and never a level.
const MIDBAND_HZ: (f64, f64) = (200.0, 2000.0);

/// `correction_range`'s domain floor and ceiling — the table's `Range` within
/// `20..=20000`, and the `min(20000, …)` in the row's own high-edge rule.
const CORRECTION_RANGE_HZ: (f64, f64) = (20.0, 20_000.0);

/// `low_corner_hz`: "Lowest `f` such that `mag(f′) ≥ M − 10 dB`".
const LOW_CORNER_DROP_DB: f64 = 10.0;

/// `low_corner_hz`: "…for all `f′ ∈ [f, 200]`". The top of the scan, and the
/// top of the midband reference `M` is measured over — one band, two uses.
const LOW_CORNER_SCAN_TOP_HZ: f64 = 200.0;

/// `correction_range`: "lowest bin with SNR ≥ 25 dB" / "highest bin with
/// SNR ≥ 25 dB".
///
/// **This is the post-capture analysis gate and is deliberately NOT
/// `paraeq_measure::ladder`'s pre-capture numbers** (`SNR_MEDIAN_ACCEPT_DB`
/// 40.0, `SNR_MIN_ACCEPT_DB` 20.0). Those fire during the level ladder and
/// decide whether to sweep at all; this one fires on the bundle afterwards and
/// decides where a filter may be placed. Same layering posture as § D-M.
const SNR_MIN_DB: f64 = 25.0;

/// `transition_hz`: "Lowest `f` where `σ(f) ≥ 3.0 dB`".
const SIGMA_TRANSITION_DB: f64 = 3.0;

/// `transition_hz`: "…and stays ≥ for ≥ 1/3 octave". The clause that makes the
/// crossing a region rather than one noisy bin.
const SIGMA_SUSTAIN_OCT: f64 = 1.0 / 3.0;

/// `transition_hz`: the no-volume clamp, "else `[80, 400]`".
///
/// § D-L keeps this and `room::transition_range` BOTH, because they answer
/// different questions: this is the clamp on the scan, and `transition_range`
/// is a display-only range around a Schroeder estimate, used here only as the
/// cross-check evidence. Room volume is never asked for anywhere in the
/// wizard's two-plus-one question budget, so the Schroeder branch is in
/// practice always the fallback one.
const TRANSITION_CLAMP_HZ: (f64, f64) = (80.0, 400.0);

// `transition_hz`: "No crossing ⇒ 200.0, `source: Default`" — and the number
// itself is `paraeq_dsp::room::TRANSITION_FALLBACK_HZ`, imported above rather
// than re-declared here, so `decide()` and `room::transition_range` cannot
// drift apart. **A FALLBACK, not a rule** (`wizard-design.md:566`); the
// constant's own doc carries that ruling. `Source::Default` is the
// machine-readable half of the label, `rationale::transition_hz_fallback` is
// the half a user reads, and one `log::info!` at the desktop boundary (B14) is
// the third. The requirement is discharged only when all three land.

/// Phase 1 — every decision the ANALYSIS stage reads, plus the two echoes and
/// the clock toggle. Eleven of the twenty-two.
pub(crate) struct AnalysisDecisions {
    pub align_spl_band: Decision<(f64, f64)>,
    pub averaging: Decision<AveragingMode>,
    pub class: Decision<TransducerClass>,
    pub clock_adjust: Decision<bool>,
    pub fdw_post_cycles: Decision<f64>,
    pub fdw_pre_cycles: Decision<f64>,
    pub left_window_ms: Decision<f64>,
    pub positions_n: Decision<usize>,
    pub right_window_ms: Decision<f64>,
    pub smoothing: Decision<SmoothingMode>,
    pub window_type: Decision<WindowType>,
}

/// Phase 2 — every decision read OFF the analysis. Ten of the twenty-two. The
/// resolved [`AuthorityCurve`] rides alongside as a second return value rather
/// than as a field, because it is an `Analysis` PRODUCT, not a decision:
/// nothing about it has a domain the drawer may edit, a source or a tier.
pub(crate) struct DesignDecisions {
    pub authority: Decision<AuthorityPreset>,
    pub correction_kind: Decision<CorrectionForm>,
    pub correction_range: Decision<(f64, f64)>,
    pub flatness_target_db: Decision<f64>,
    pub low_corner_hz: Decision<f64>,
    pub max_filters: Decision<usize>,
    pub q_cap: Decision<QCapPolicy>,
    pub shelves: Decision<bool>,
    pub target: Decision<TargetChoice>,
    pub transition_hz: Decision<f64>,
}

/// What the fit produced, per channel: the bands the engine installs, every
/// clamp and veto that shaped them, and the count the Jury funnel dropped.
///
/// Index-parallel to `Analysis::averaged_db`, and always non-empty: one channel
/// of zero bands is the honest shape for "there is a plan and it contains
/// nothing", which is what an unanalysable bundle gets.
pub(crate) struct FitOutcome {
    pub bands: Vec<Vec<EQBand>>,
    pub clamps: Vec<Vec<Clamp>>,
    pub dropped: Vec<usize>,
}

impl FitOutcome {
    /// The number of filters the user actually got, which the `max_filters` and
    /// `correction_kind` copy both count.
    ///
    /// The MAX across channels rather than the sum: the bands are per channel
    /// and a stereo preset is two presets of this length, so the sum would
    /// double-count and the min would under-report the busier side.
    fn realized_bands(&self) -> usize {
        self.bands.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// A decision as the rule leaves it, before any override: the default value,
/// its domain, its rendered rationale and its tier.
fn decision<T>(
    value: T,
    domain: Domain<T>,
    rationale: Rationale,
    invalidates: Invalidation,
) -> Decision<T> {
    Decision {
        domain,
        evidence: Vec::new(),
        invalidates,
        rationale,
        source: Source::Default,
        value,
    }
}

/// Mark a decision as READ OFF the measurement rather than fallen back to.
///
/// The distinction is the enum's own and it is load-bearing: `Default` means
/// "evidence absent or inconclusive", so the drawer can say "we could not
/// measure this, so we used the default". A rule that ran a scan and got an
/// answer must not claim that.
fn measured<T>(mut d: Decision<T>) -> Decision<T> {
    d.source = Source::Auto;
    d
}

/// Attach the measured evidence a decision was read off. "Structured,
/// plottable. Never prose."
fn with_evidence<T>(mut d: Decision<T>, evidence: Vec<Evidence>) -> Decision<T> {
    d.evidence = evidence;
    d
}

/// The ONE place a user override lands.
///
/// An override sets the value and flips `source` to `UserOverride`; everything
/// else about the decision — its domain, its tier, its evidence, its rationale
/// key — is the rule's and does not move. That is what makes the spec's
/// idempotence property mechanical rather than disciplinary: overriding to the
/// value auto already chose reaches exactly one field.
///
/// **NOT IMPLEMENTED HERE: § D-N, the out-of-domain override.** The ruling is
/// "clamp into the domain, set `source: UserOverride` with the clamped value,
/// and emit `DiagnosticCode::OverrideOutOfDomain` at `Severity::Warn`, naming
/// the decision and both numbers", and the code exists
/// (`DiagnosticCode::OverrideOutOfDomain = 25`). It is not here because
/// clamping is per-type — a band clamps element-wise, a `Choice` has no nearest
/// legal value at all — and because it is flagged `OPEN [DESIGN]` rather than
/// settled. Whoever lands it lands it HERE, in this function, and pins it with
/// a test that an out-of-domain override arrives clamped and warned rather than
/// obeyed. Until then an override is taken as given.
fn resolve<T: Clone>(mut d: Decision<T>, over: Option<&T>) -> Decision<T> {
    if let Some(value) = over {
        d.source = Source::UserOverride;
        d.value = value.clone();
    }
    d
}

/// The path's word for one capture, for `positions_n`'s `{noun}`.
///
/// **NOT `PathProfile::reposition_noun`**, which is an imperative phrase for
/// the retry prompt ("move the mic ~30 cm", "reseat the tip"), not a noun that
/// can take an "s". The decision table interpolates `{noun}s`, and no field
/// anywhere carries that word — so it is stated here, once, and flagged: a copy
/// review owns these two words, not this file.
///
/// `pub(crate)` because the two position-count refusal rows interpolate the
/// same `{noun}s` (ruling R-A11): `refusal::position_count` used
/// `PathProfile::reposition_noun` and rendered "We need at least 3 move the mic
/// ~30 cms". One word, one source.
pub(crate) fn position_noun(profile: &PathProfile) -> &'static str {
    match profile.coupling {
        CouplingPath::Coupler => "reseat",
        CouplingPath::Room => "position",
    }
}

/// `fdw_pre_cycles`' own rule — "Default 3; clamped `≤ fdw_post_cycles`" —
/// applied to the RESOLVED value, so a drawer override obeys it too.
///
/// Ruling R-A11. The pre lobe's domain is the decision table's `1.0..=61.0`
/// verbatim, and § D-K narrowed only the POST ceiling, so an override of 61.0
/// against a 15-cycle post lobe is INSIDE its own domain: § D-N's out-of-domain
/// clamp never sees it and the row's rule was the only thing left to enforce
/// it. The clamp moves the value and never the `source` — the user asked for
/// 61 and the record must still say so — which is the same posture § D-N takes.
///
/// A debug assertion rather than a plain `min`, so that a future rule that
/// lets the post lobe move after this point fails loudly in tests instead of
/// publishing a pre lobe longer than the window it sits inside.
fn clamp_pre_to_post(mut d: Decision<f64>, post_cycles: f64) -> Decision<f64> {
    d.value = d.value.min(post_cycles);
    debug_assert!(
        d.value <= post_cycles,
        "fdw_pre_cycles {} exceeds fdw_post_cycles {post_cycles}",
        d.value
    );
    d
}

/// `right_window_ms`'s VALUE, resolved, without building the decision.
///
/// `design_decisions` and [`fit_correction`] both need the decided window — it
/// is what the 1/6-octave resolution limit is computed from, which is
/// `correction_range`'s low edge and the fit's `min_valid_freq_hz` — and
/// re-deriving it there would be a second source for one number. Reading the
/// override here mirrors [`resolve`] exactly, so the two cannot disagree.
///
/// "Room: 400. Coupler: full (no gate)." "Full" is expressed as the domain's
/// own ceiling rather than as the recording's length, so the value stays a
/// decision the drawer can read rather than a fact about one capture: at
/// 1000 ms the right taper is spread so far past a coupler IR's decay that the
/// data sees no window at all, which is what "no gate" means. The analysis
/// stage bounds the gate it APPLIES by the data the recording holds; see
/// `AnalysisSettings::right_window_ms`.
fn right_window_value(bundle: &MeasurementBundle, profile: &PathProfile) -> f64 {
    let base = match profile.coupling {
        CouplingPath::Coupler => 1000.0,
        CouplingPath::Room => 400.0,
    };
    bundle.overrides.right_window_ms.unwrap_or(base)
}

/// The 1/6-octave resolution limit the decided right window implies.
///
/// "Publish the **stricter** resolution limit `f = (1/T)/(2^(1/2N) −
/// 2^(−1/2N))`, not `1/T`": a 400 ms window is 1/6-octave-valid above ~21.6 Hz,
/// not 2.5 Hz, and `correction_range` is bounded by the stricter number for the
/// same reason the evidence publishes it.
fn resolution_limit_for(bundle: &MeasurementBundle, profile: &PathProfile) -> f64 {
    resolution_limit_hz(
        right_window_value(bundle, profile) / 1000.0,
        RESOLUTION_FRACTION,
    )
}

/// `class` — "Echo the declared class."
///
/// Its own function because the PROFILE is derived from it, so it is the one
/// decision that must be resolved before any other rule can run. The sensitivity
/// cross-check that turns an out-of-envelope solve into
/// `Refuse(WrongTransducer)` is the refusal table's (B7c); the envelope it
/// compares against is already mirrored on
/// `PathProfile::sensitivity_envelope_spl_per_dbfs`.
pub(crate) fn class(bundle: &MeasurementBundle) -> Decision<TransducerClass> {
    resolve(
        measured(decision(
            bundle.class,
            Domain::Choice(vec![
                TransducerClass::Bookshelf,
                TransducerClass::Floorstander,
                TransducerClass::InEar,
                TransducerClass::OverEar,
            ]),
            rationale::class(bundle.class),
            Invalidation::Recapture,
        )),
        bundle.overrides.class.as_ref(),
    )
}

/// Phase 1. See the module header for what is default and what is contract.
pub(crate) fn analysis_decisions(
    bundle: &MeasurementBundle,
    class: Decision<TransducerClass>,
    grid: &LogGrid,
    profile: &PathProfile,
    geometry: &Geometry,
) -> AnalysisDecisions {
    let over = &bundle.overrides;
    let sweep = &bundle.capture.sweep;

    // `left_window_ms`: `min(requested, t_peak, 0.5·dt₂)`, where § D-J takes
    // the STRICTER of the spec's half-dt₂ and `apply_gate`'s own full-dt₂
    // clamp — "`decide()` computes `requested_left_ms = min(profile_request,
    // 0.5·dt₂)` and hands it to `apply_gate`, whose own `min(·, t_peak, dt₂)`
    // then never binds". The domain's own ceiling is `t_peak`: the Mac's audio
    // path puts the impulse at ~46–64 ms and there is nothing before it to
    // window.
    let peak_ms = geometry.peak_time_ms.max(1.0);
    let harmonic_ms = 1000.0
        * farina_h2_bound_s(&SweepParams {
            duration_s: sweep.duration_s,
            f1: sweep.f_start_hz,
            f2: sweep.f_end_hz,
        });
    let left_ms = if harmonic_ms.is_finite() && harmonic_ms > 0.0 {
        peak_ms.min(0.5 * harmonic_ms)
    } else {
        peak_ms
    }
    .clamp(1.0, peak_ms);

    let right_ms = right_window_value(bundle, profile);
    let resolution_hz = resolution_limit_for(bundle, profile);

    // `fdw_post_cycles`, resolved first because `fdw_pre_cycles`'s own rule is
    // stated against it: "Default 3; clamped `≤ fdw_post_cycles`". The clamp is
    // applied to the RULE's value, before any override of the pre-lobe itself —
    // an override that lands outside its domain is § D-N's, not this row's.
    let post_cycles = over.fdw_post_cycles.unwrap_or(match profile.gating {
        GatingMode::Fdw { post, .. } => post,
        // The coupler has no reflection problem to gate away, so the pass does
        // not run; the decision still has a value, because the drawer still has
        // a control and "off" is the profile's `GatingMode`, not a cycle count
        // of zero.
        GatingMode::None => 15.0,
    });
    let pre_cycles = match profile.gating {
        GatingMode::Fdw { pre, .. } => pre,
        GatingMode::None => 3.0,
    }
    .min(post_cycles);

    AnalysisDecisions {
        align_spl_band: resolve(
            decision(
                // § D-B: the owning module's constant, `(200, 2000)`, which is
                // also `M`'s band — one band, two uses.
                DEFAULT_SPL_ALIGN_BAND,
                Domain::Range {
                    max: (8000.0, 8000.0),
                    min: (100.0, 100.0),
                    step: None,
                },
                rationale::align_spl_band(),
                Invalidation::Reanalyze,
            ),
            over.align_spl_band.as_ref(),
        ),
        averaging: resolve(
            decision(
                profile.averaging,
                Domain::Choice(vec![AveragingMode::DbMean, AveragingMode::Power]),
                rationale::averaging(),
                Invalidation::Reanalyze,
            ),
            over.averaging.as_ref(),
        ),
        class,
        clock_adjust: resolve(
            decision(
                // "Drawer override: clock-adjust toggle with the estimated ppm
                // shown, **defaulted on**." `CapturePlan::clock_adjusted` is the
                // witness that it RAN; this is the intent.
                true,
                Domain::Choice(vec![false, true]),
                rationale::clock_adjust(),
                Invalidation::Recapture,
            ),
            over.clock_adjust.as_ref(),
        ),
        fdw_post_cycles: resolve(
            decision(
                post_cycles,
                Domain::Range {
                    max: fdw_post_ceiling(grid),
                    min: 3.0,
                    step: None,
                },
                rationale::fdw_post_cycles(),
                Invalidation::Reanalyze,
            ),
            over.fdw_post_cycles.as_ref(),
        ),
        // The row's own rule — "clamped `≤ fdw_post_cycles`" — applied AFTER
        // `resolve`, so a drawer value obeys it too (ruling R-A11). 61.0 is the
        // top of this row's own domain, so § D-N's out-of-domain clamp never
        // sees it; without this the drawer could ask for a 61-cycle pre lobe
        // against a 15-cycle post lobe, which is in-domain and violates the
        // row. `source` stays `UserOverride`: the value moved, the intent did
        // not. Asserted below, and by `fdw_pre_cycles_never_exceeds_the_post_lobe`.
        fdw_pre_cycles: clamp_pre_to_post(
            resolve(
                decision(
                    // "Asymmetric pre-lobe. Default 3" — a noise gate on pre-peak
                    // artifacts, not a resolution control, and DR1 § Q3 calls it
                    // "the lowest-confidence knob".
                    pre_cycles,
                    // The decision table's `Range 1.0..=61.0`, verbatim. § D-K
                    // narrowed the POST ceiling to what the implementation can
                    // honour and said nothing about this one, and the row's own rule
                    // ("clamped `≤ fdw_post_cycles`") is a rule rather than a
                    // domain — so the ceiling stands as written and the clamp is
                    // applied to the VALUE below. Flagged rather than quietly
                    // narrowed: a domain is what the drawer draws, and changing one
                    // is a wire change.
                    // OPEN [OWNER] (§ D-K sibling): keep 61.0, or derive it too?
                    Domain::Range {
                        max: 61.0,
                        min: 1.0,
                        step: None,
                    },
                    rationale::fdw_pre_cycles(),
                    Invalidation::Reanalyze,
                ),
                over.fdw_pre_cycles.as_ref(),
            ),
            post_cycles,
        ),
        left_window_ms: resolve(
            with_evidence(
                measured(decision(
                    left_ms,
                    Domain::Range {
                        max: peak_ms,
                        min: 1.0,
                        step: None,
                    },
                    rationale::left_window_ms(geometry.peak_time_ms),
                    Invalidation::Reanalyze,
                )),
                vec![Evidence::Scalar {
                    label: EvidenceLabel::PeakArrival,
                    unit: Unit::Milliseconds,
                    value: geometry.peak_time_ms,
                }],
            ),
            over.left_window_ms.as_ref(),
        ),
        positions_n: resolve(
            measured(decision(
                // "Echo accepted count", verbatim — NOT clamped into the domain.
                // A count below the hard minimum is `TooFewPositions` (B7c), and
                // clamping the echo would hide the very number the refusal is
                // about.
                bundle.positions.len(),
                Domain::Range {
                    max: *profile.positions_domain.end(),
                    min: *profile.positions_domain.start(),
                    step: Some(1),
                },
                rationale::positions_n(bundle.positions.len(), position_noun(profile)),
                Invalidation::Recapture,
            )),
            over.positions_n.as_ref(),
        ),
        right_window_ms: resolve(
            with_evidence(
                decision(
                    right_ms,
                    Domain::Range {
                        max: 1000.0,
                        min: 100.0,
                        step: None,
                    },
                    rationale::right_window_ms(right_ms, resolution_hz),
                    Invalidation::Reanalyze,
                ),
                // "Publish the **stricter** resolution limit … not `1/T`."
                vec![Evidence::Scalar {
                    label: EvidenceLabel::ResolutionLimit,
                    unit: Unit::Hz,
                    value: resolution_hz,
                }],
            ),
            over.right_window_ms.as_ref(),
        ),
        smoothing: resolve(
            decision(
                profile.smoothing,
                // The decision table's own list, in its order. `Gaussian` exists
                // on the merged type but is not offered: the table does not list
                // it, and a domain is what the drawer draws.
                Domain::Choice(vec![
                    SmoothingMode::Fixed(3),
                    SmoothingMode::Fixed(6),
                    SmoothingMode::Fixed(12),
                    SmoothingMode::Fixed(24),
                    SmoothingMode::Fixed(48),
                    SmoothingMode::Variable,
                    SmoothingMode::None,
                ]),
                rationale::smoothing(),
                Invalidation::Reanalyze,
            ),
            over.smoothing.as_ref(),
        ),
        window_type: resolve(
            decision(
                // "Tukey α=0.25, applied independently left and right." A hard
                // cut smears the measurement across frequency.
                WindowType::Tukey(0.25),
                Domain::Choice(vec![
                    WindowType::BlackmanHarris,
                    WindowType::Hann,
                    WindowType::Rect,
                    WindowType::Tukey(0.25),
                ]),
                rationale::window_type(),
                Invalidation::Reanalyze,
            ),
            over.window_type.as_ref(),
        ),
    }
}

/// Phase 2. See the module header.
pub(crate) fn design_decisions(
    bundle: &MeasurementBundle,
    class: TransducerClass,
    grid: &LogGrid,
    profile: &PathProfile,
    analysis: &AnalysisProducts,
) -> (DesignDecisions, AuthorityCurve) {
    let over = &bundle.overrides;

    // § D-D: the decision carries the preset NAME; the resolved curve is an
    // `Analysis` product. Resolve the preset first, because which curve gets
    // built depends on it, and only then describe the domain — whose `Custom`
    // arm carries the curve this run actually produced, so a drawer offering
    // "Custom" starts from what it is already plotting.
    let preset = resolve(
        decision(
            AuthorityPreset::Standard,
            Domain::Derived,
            // Replaced below, once `transition_hz` has a value: the copy quotes
            // `f_t` even though the curve never reads it.
            rationale::authority(TRANSITION_FALLBACK_HZ),
            Invalidation::Redesign,
        ),
        over.authority.as_ref(),
    );
    let authority_curve =
        crate::analysis::resolve_authority(&preset.value, grid, &analysis.sigma_db, profile);

    // `transition_hz`: the sustained σ ≥ 3.0 dB crossing, clamped to [80, 400].
    // Computed BEFORE `authority` only so the authority copy can quote it —
    // "`transition_hz` … is never an input to the authority weight", and
    // `resolve_authority` above has already run without it.
    let crossing = transition_scan(grid.freqs(), &analysis.sigma_db);
    let transition_value = crossing
        .map(|f| f.clamp(TRANSITION_CLAMP_HZ.0, TRANSITION_CLAMP_HZ.1))
        .unwrap_or(TRANSITION_FALLBACK_HZ);
    // The copy is chosen by the SCAN, not by the decision's source and not by
    // its value: the table's sentence is written for a measurement, and
    // rendering it over the fallback would claim a transition nobody measured
    // (`wizard-design.md:566`). `source` would be the obvious selector and it
    // would fork idempotence — an override to the value auto already chose must
    // move `source` and nothing else — while `crossing` is an analysis fact no
    // override can reach.
    let copy = match crossing {
        None => rationale::transition_hz_fallback(transition_value),
        Some(_) => rationale::transition_hz(transition_value),
    };
    let mut transition = decision(
        transition_value,
        Domain::Range {
            max: TRANSITION_CLAMP_HZ.1,
            min: TRANSITION_CLAMP_HZ.0,
            step: None,
        },
        copy,
        Invalidation::Reanalyze,
    );
    if crossing.is_some() {
        transition = measured(transition);
    }
    let transition_hz = resolve(
        with_evidence(
            transition,
            vec![
                Evidence::Curve {
                    db: to_f32(&analysis.sigma_db),
                    hz: to_f32(grid.freqs()),
                    label: EvidenceLabel::Sigma,
                },
                // § D-L: `room::transition_range` is the CROSS-CHECK, never the
                // clamp. Volume is never asked for in the wizard's question
                // budget, so this is always the fallback range — which is the
                // honest thing to show next to a σ crossing.
                schroeder_evidence(),
            ],
        ),
        over.transition_hz.as_ref(),
    );

    let authority = with_evidence(
        Decision {
            // The two NAMED presets, and nothing else. Ruling R-A4: this list
            // used to carry `Custom(authority_curve.clone())` — a byte-for-byte
            // copy of `Analysis::authority`, 77–89 KB in every frozen case — to
            // express "Custom = whatever Standard just produced", which is not
            // an alternative the drawer can offer. `Custom` stays legal through
            // `InRange::legal_by_construction`: `AuthorityCurve` is sealed, so a
            // list could neither enumerate the legal curves nor need to.
            domain: Domain::Choice(vec![
                AuthorityPreset::Conservative,
                AuthorityPreset::Standard,
            ]),
            rationale: rationale::authority(transition_hz.value),
            ..preset
        },
        vec![
            Evidence::Curve {
                db: to_f32(authority_curve.excursion_db()),
                hz: to_f32(authority_curve.freqs()),
                label: EvidenceLabel::AuthorityEnvelope,
            },
            // **Evidence only in v1 (R7).** The EGD trace is attached HERE, on
            // the decision it would gate if the gate were armed, and
            // `resolve_authority` passes no mask — so the trace is visible and
            // inert. `Evidence::Curve::db` carries SECONDS on this label; the
            // field name is the shipped wire shape, not a unit claim.
            Evidence::Curve {
                db: to_f32(&analysis.excess_group_delay_s),
                hz: to_f32(grid.freqs()),
                label: EvidenceLabel::ExcessGroupDelay,
            },
        ],
    );

    // `low_corner_hz`: "Lowest `f` such that `mag(f′) ≥ M − 10 dB` for all
    // `f′ ∈ [f, 200]`". The curve it scans is the channel mean of the aligned,
    // averaged, smoothed response, which is also what `M` is read off — one
    // curve, one reference, no chance of scanning one and comparing to another.
    let averaged = channel_mean_db(analysis, grid);
    let midband_db = midband_level_db(averaged.as_deref(), grid);
    let corner = averaged
        .as_deref()
        .and_then(|curve| low_corner_scan(grid.freqs(), curve, midband_db));
    // The fallback is the TOP of the scan, not the bottom: "we never got within
    // 10 dB of midband anywhere below 200 Hz" means nothing below 200 Hz is
    // worth correcting, and answering with the lowest bin would claim the
    // opposite. Narrowing is the safe direction; widening burns excursion.
    //
    // **Ruling R-A5**, recorded because the number has no spec row of its own
    // and is load-bearing three times over: it is `correction_range`'s low
    // edge, the bottom of `AbsurdCurve`'s span band and the bottom of
    // `ExcessiveVariance`'s. `Source::Default` is the machine-readable half —
    // "we could not measure this" — and
    // `low_corner_falls_back_to_the_scan_top_with_source_default` is the test
    // that names it.
    let low_corner_value = corner.unwrap_or(LOW_CORNER_SCAN_TOP_HZ);
    let mut low_corner = decision(
        low_corner_value,
        // `Derived` and not `Range`: "override its inputs instead".
        // Rendered read-only in the drawer, never hidden.
        Domain::Derived,
        rationale::low_corner_hz(low_corner_value),
        Invalidation::Reanalyze,
    );
    if corner.is_some() {
        low_corner = measured(low_corner);
    }
    let low_corner_hz = resolve(
        with_evidence(
            low_corner,
            averaged_evidence(averaged.as_deref(), grid, midband_db),
        ),
        over.low_corner_hz.as_ref(),
    );

    let target = target_decision(bundle, class, profile, averaged.as_deref(), grid);
    let flatness_target_db = resolve(
        decision(
            // The profile constant: 3.0 room, 1.0 coupler. Not a scan —
            // this row's Rule column is empty in the table.
            profile.flatness_target_db,
            Domain::Range {
                max: 6.0,
                min: 0.5,
                step: None,
            },
            rationale::flatness_target_db(profile.flatness_target_db),
            Invalidation::Redesign,
        ),
        over.flatness_target_db.as_ref(),
    );

    // `correction_range`: the three lower bounds and the two upper ones, all of
    // them measured. See [`correction_range_scan`].
    let snr = averaged
        .as_deref()
        .and_then(|curve| snr_db(bundle, grid, curve));
    let target_db = averaged
        .as_deref()
        .and_then(|_| target_on_grid(bundle, &target.value, grid));
    let range_value = correction_range_scan(
        grid.freqs(),
        averaged.as_deref(),
        target_db.as_deref(),
        snr.as_deref(),
        low_corner_hz.value,
        resolution_limit_for(bundle, profile),
    );
    let mut range = decision(
        range_value,
        Domain::Range {
            max: (CORRECTION_RANGE_HZ.1, CORRECTION_RANGE_HZ.1),
            min: (CORRECTION_RANGE_HZ.0, CORRECTION_RANGE_HZ.0),
            step: None,
        },
        rationale::correction_range(),
        Invalidation::Redesign,
    );
    if snr.is_some() {
        range = measured(range);
    }
    let correction_range = resolve(
        match snr.as_deref() {
            Some(curve) => with_evidence(
                range,
                vec![Evidence::Curve {
                    db: to_f32(curve),
                    hz: to_f32(grid.freqs()),
                    label: EvidenceLabel::Snr,
                }],
            ),
            None => range,
        },
        over.correction_range.as_ref(),
    );

    let design = DesignDecisions {
        authority,
        correction_kind: resolve(
            decision(
                // "`Peq` on all four paths": no added block latency onto an
                // already 46–62 ms budget, and it can carry the per-band Q cap
                // and excursion envelope as constraints.
                CorrectionForm::Peq,
                Domain::Choice(vec![CorrectionForm::MinPhaseFir, CorrectionForm::Peq]),
                // Re-rendered once the fit has run; see [`render_realized`].
                rationale::correction_kind(0),
                Invalidation::Redesign,
            ),
            over.correction_kind.as_ref(),
        ),
        correction_range,
        flatness_target_db,
        low_corner_hz,
        max_filters: resolve(
            decision(
                // The table's default, and a CAP rather than a target: the
                // greedy loop stops as soon as "residual RMS over the authority
                // band `< flatness_target_db`", so the cap binds only on a curve
                // the budget cannot flatten.
                10,
                Domain::Range {
                    max: 20,
                    min: 1,
                    step: None,
                },
                // Re-rendered once the fit has run; see [`render_realized`].
                rationale::max_filters(0, profile.flatness_target_db),
                Invalidation::Redesign,
            ),
            over.max_filters.as_ref(),
        ),
        q_cap: resolve(
            decision(
                // § D-C: the two PATH policies, read from `paraeq_dsp::authority`
                // rather than spelled here, so the drawer's control is the
                // two-item select the value actually is.
                profile.q_cap,
                Domain::Choice(vec![COUPLER_Q_CEILING, ROOM_Q_CEILING]),
                rationale::q_cap(),
                Invalidation::Redesign,
            ),
            over.q_cap.as_ref(),
        ),
        shelves: resolve(
            decision(
                true,
                Domain::Choice(vec![false, true]),
                rationale::shelves(),
                Invalidation::Redesign,
            ),
            over.shelves.as_ref(),
        ),
        target,
        transition_hz,
    };
    (design, authority_curve)
}

/// Re-render the two rationales that count the bands the fit EMITTED.
///
/// They cannot be rendered in [`design_decisions`], which runs before the fit:
/// "We used {n} filters because that's what it took … not because {n} is a nice
/// number" is false of a cap, and `correction_kind`'s "{n} tone controls" is
/// the same number. The VALUES do not move — `max_filters` stays the cap the
/// drawer edits — only the copy, which is what the table says the copy is
/// about.
pub(crate) fn render_realized(design: &mut DesignDecisions, fit: &FitOutcome) {
    let bands = fit.realized_bands();
    design.max_filters.rationale = rationale::max_filters(bands, design.flatness_target_db.value);
    design.correction_kind.rationale = rationale::correction_kind(bands);
}

/// Phase 3 — the fit. `authority` + `q_cap` + `correction_range` + `target` +
/// `max_filters` + `shelves` → bands.
///
/// Everything safety-related was already composed into `authority` by
/// `analysis::authority_policy`, which reads the PathProfile: the excursion
/// breakpoints (Trinnov's for the room, the 10 kHz-zeroed set for the coupler),
/// § D-E's `boost_ratio` (0.0 on the room auto path — "Room auto mode ships
/// cut-only by default … cuts are free, boosts cost headroom and can damage
/// drivers" — and `DEFAULT_BOOST_RATIO` on the coupler), § D-F's 1/6-octave
/// narrow-dip veto, and the path Q ceiling. This function must not re-specify
/// any of them; it passes the decided GOALS.
///
/// **`min_gain_db = flatness_target_db / 2`** is the `max_filters` row's own
/// drop rule, "Drop any band with `|gain| < flatness/2`". The binding is
/// `decide()`'s and the number has ONE source — the `Decision`, not a constant —
/// so overriding `flatness_target_db` moves it.
///
/// **The correction is a SHAPE, not a level.** The measured curve carries the
/// capture's absolute level (mic sensitivity, sweep level, preamp) and the
/// target carries none, so `target − measured` on the raw curves would ask for
/// a broadband gain equal to the difference between the two conventions. Both
/// are normalized over [`MIDBAND_HZ`] first — the same band `align_spl` used and
/// the same band `M` is read over.
pub(crate) fn fit_correction(
    bundle: &MeasurementBundle,
    design: &DesignDecisions,
    analysis: &AnalysisProducts,
    authority: &AuthorityCurve,
    grid: &LogGrid,
    profile: &PathProfile,
    sample_rate: f64,
) -> FitOutcome {
    // The CAPTURE's channel count, not the curve count: a bundle the analysis
    // could not read publishes no curves but still says how wide it was, and
    // `.max(1)` because `PerChannel` is non-empty by construction.
    let channels = analysis.channels.max(1);
    let empty = FitOutcome {
        bands: vec![Vec::new(); channels],
        clamps: vec![Vec::new(); channels],
        dropped: vec![0; channels],
    };
    if analysis.averaged_db.is_empty() {
        return empty;
    }
    let Some(target_db) = target_on_grid(bundle, &design.target.value, grid) else {
        return empty;
    };
    let Ok(target_norm) =
        normalize_to_reference_band(grid.freqs(), &target_db, MIDBAND_HZ.0, MIDBAND_HZ.1)
    else {
        return empty;
    };
    let mut corrections = Vec::with_capacity(analysis.averaged_db.len());
    for measured_db in &analysis.averaged_db {
        let Ok(measured_norm) =
            normalize_to_reference_band(grid.freqs(), measured_db, MIDBAND_HZ.0, MIDBAND_HZ.1)
        else {
            return empty;
        };
        corrections.push(compute_correction(&measured_norm, &target_norm));
    }
    let Ok(per_channel) = PerChannel::new(corrections) else {
        return empty;
    };
    let policy = RoomFitPolicy {
        correction_range: design.correction_range.value,
        flatness_target_db: design.flatness_target_db.value,
        shelves: design.shelves.value,
    };
    let Ok(reports) = auto_fit_room(
        &per_channel,
        grid,
        sample_rate,
        authority,
        design.max_filters.value,
        design.flatness_target_db.value / 2.0,
        // The gate's own 1/6-octave limit: "never fit a band below the frequency
        // the gate can resolve". `correction_range`'s low edge already takes the
        // max with this number, so it can only agree — it is passed because
        // `auto_fit_room`'s item 4 asks for it, not because it binds.
        resolution_limit_for(bundle, profile),
        Some(&policy),
    ) else {
        return empty;
    };
    FitOutcome {
        bands: reports.iter().map(|r| r.bands.clone()).collect(),
        clamps: reports.iter().map(|r| r.clamps.clone()).collect(),
        dropped: reports.iter().map(|r| r.dropped).collect(),
    }
}

/// Phase 4 — `preamp_db`, "`-max(0, max_f of the REALIZED cascade)`; no
/// headroom constant."
///
/// Last, and separate, because it is a property of the bands the fit EMITTED:
/// "after the Q cap, the excursion clamp, and band-dropping, the filters that
/// actually run are not the filters autofit first proposed". With no bands it is
/// exactly 0.0, which is the same answer a pure-cut cascade gets.
pub(crate) fn preamp_decision(
    bundle: &MeasurementBundle,
    bands: &[Vec<EQBand>],
    design_rate: f64,
) -> Decision<f64> {
    // The worst channel wins: a stereo config must not clip on the loud side
    // because the quiet side needed less headroom. This is the same fold the
    // engine makes over its own per-channel preamps.
    let preamp_db = bands
        .iter()
        .map(|channel| realized_preamp_db(channel, design_rate))
        .fold(0.0f64, f64::min);
    resolve(
        measured(decision(
            preamp_db,
            Domain::Derived,
            rationale::preamp_db(preamp_db),
            Invalidation::Redesign,
        )),
        bundle.overrides.preamp_db.as_ref(),
    )
}

/// The three phases, assembled into the one struct the drawer enumerates.
pub(crate) fn assemble(
    analysis: AnalysisDecisions,
    design: DesignDecisions,
    preamp_db: Decision<f64>,
) -> Decisions {
    Decisions {
        align_spl_band: analysis.align_spl_band,
        authority: design.authority,
        averaging: analysis.averaging,
        class: analysis.class,
        clock_adjust: analysis.clock_adjust,
        correction_kind: design.correction_kind,
        correction_range: design.correction_range,
        fdw_post_cycles: analysis.fdw_post_cycles,
        fdw_pre_cycles: analysis.fdw_pre_cycles,
        flatness_target_db: design.flatness_target_db,
        left_window_ms: analysis.left_window_ms,
        low_corner_hz: design.low_corner_hz,
        max_filters: design.max_filters,
        positions_n: analysis.positions_n,
        preamp_db,
        q_cap: design.q_cap,
        right_window_ms: analysis.right_window_ms,
        shelves: design.shelves,
        smoothing: analysis.smoothing,
        target: design.target,
        transition_hz: design.transition_hz,
        window_type: analysis.window_type,
    }
}

// ---------------------------------------------------------------------------
// The four policy scans
// ---------------------------------------------------------------------------

/// `low_corner_hz`'s scan: "scan up from the lowest valid bin, first frequency
/// that gets within 10 dB of midband **and stays there**".
///
/// Written as a walk DOWN from 200 Hz because that is the same predicate with
/// no quadratic re-scan: the answer is the lowest bin from which every bin up
/// to 200 Hz is at or above `M − 10 dB`, and walking down stops at the first
/// bin that is not. The "and stays there" clause is the whole rule — without
/// it, an isolated notch below the corner would be read as the corner, and a
/// ported bookshelf that reaches 38 Hz would be reported as a 60 Hz one.
///
/// `None` when the curve never gets within 10 dB of midband anywhere at or
/// below 200 Hz, which is not a corner at any frequency the scan can name.
fn low_corner_scan(freqs: &[f64], curve: &[f64], midband_db: f64) -> Option<f64> {
    if freqs.len() != curve.len() || !midband_db.is_finite() {
        return None;
    }
    let threshold = midband_db - LOW_CORNER_DROP_DB;
    let top = freqs.iter().rposition(|f| *f <= LOW_CORNER_SCAN_TOP_HZ)?;
    if curve[top] < threshold {
        return None;
    }
    let mut i = top;
    while i > 0 && curve[i - 1] >= threshold {
        i -= 1;
    }
    Some(freqs[i])
}

/// `transition_hz`'s scan: "Lowest `f` where `σ(f) ≥ 3.0 dB` and stays ≥ for
/// ≥ 1/3 octave".
///
/// The sustain clause is what separates a transition from a noisy bin: a σ that
/// touches 3 dB for a sixth of an octave is scatter, and reporting it as the
/// frequency above which the room stops being correctable everywhere would be a
/// claim the measurement does not support.
///
/// Returns the UNCLAMPED crossing; the caller applies [`TRANSITION_CLAMP_HZ`].
/// `None` — the fallback case — when no bin anywhere on the grid starts a
/// sustained crossing, including when the grid runs out before the sustain
/// window could be checked (a crossing that cannot be shown to stay is not a
/// crossing this rule may report).
fn transition_scan(freqs: &[f64], sigma: &[f64]) -> Option<f64> {
    if freqs.len() != sigma.len() {
        return None;
    }
    let span = 2f64.powf(SIGMA_SUSTAIN_OCT);
    let &f_max = freqs.last()?;
    for (i, (&f, &s)) in freqs.iter().zip(sigma).enumerate() {
        if !f.is_finite() || f <= 0.0 {
            continue;
        }
        // Frequencies ascend, so once the sustain window runs off the top of
        // the grid it does so for every candidate above this one too.
        if f * span > f_max {
            return None;
        }
        if s < SIGMA_TRANSITION_DB {
            continue;
        }
        let sustained = freqs[i..]
            .iter()
            .zip(&sigma[i..])
            .take_while(|(g, _)| **g <= f * span)
            .all(|(_, s)| *s >= SIGMA_TRANSITION_DB);
        if sustained {
            return Some(f);
        }
    }
    None
}

/// The per-bin SNR `correction_range` is bounded by: the measured curve minus
/// the silence capture's own spectrum, both on the analysis grid.
///
/// `fr::align_spl` aligns each position to the ENSEMBLE mean rather than zeroing
/// it, so the averaged curve still carries the capture's absolute level and the
/// difference is a real signal-to-noise ratio rather than a shape comparison.
///
/// `None` when the bundle carries no usable floor — the row's bounds then do
/// not apply, which widens `correction_range` rather than narrowing it, and the
/// missing floor is the refusal table's to report (B7c).
fn snr_db(bundle: &MeasurementBundle, grid: &LogGrid, measured_db: &[f64]) -> Option<Vec<f64>> {
    let floor = &bundle.noise_floor;
    if floor.freqs_hz.len() < 2 || floor.spectrum_db.is_empty() {
        return None;
    }
    let channels = floor.spectrum_db.len();
    let mut mean = vec![0.0f64; floor.freqs_hz.len()];
    for channel in &floor.spectrum_db {
        if channel.len() != floor.freqs_hz.len() {
            return None;
        }
        for (m, v) in mean.iter_mut().zip(channel) {
            *m += v / channels as f64;
        }
    }
    if mean.iter().any(|v| !v.is_finite()) {
        return None;
    }
    // `Prefilter::None`: the anti-comb lowpass is justified for a gated impulse
    // spectrum, not for a noise floor that is already a power average.
    let on_grid = resample_db_to_log_grid(&floor.freqs_hz, &mean, grid, Prefilter::None).ok()?;
    if measured_db.len() != on_grid.len() {
        return None;
    }
    Some(
        measured_db
            .iter()
            .zip(&on_grid)
            .map(|(m, n)| m - n)
            .collect(),
    )
}

/// "…no filters above the frequency where measured **last drops below**
/// target", scanned DOWN from `from_idx`.
///
/// Down, not up, and the direction is the rule: scanning up returns the FIRST
/// place the curve dips under target, which on a speaker with a 6 kHz
/// suckout is 6 kHz — an octave and a half below the 14 kHz roll-off knee the
/// row is about, and an octave and a half of tweeter left uncorrected for no
/// reason. Scanning down finds the knee and steps over the dip.
///
/// `None` when the curve is at or above target at the top of the search, i.e.
/// it never "drops below" and the rule imposes no bound.
fn last_target_crossing(
    freqs: &[f64],
    measured_db: &[f64],
    target_db: &[f64],
    from_idx: usize,
) -> Option<f64> {
    if freqs.len() != measured_db.len() || freqs.len() != target_db.len() {
        return None;
    }
    let top = from_idx.min(freqs.len().checked_sub(1)?);
    if measured_db[top] >= target_db[top] {
        return None;
    }
    let mut i = top;
    while i > 0 && measured_db[i] < target_db[i] {
        i -= 1;
    }
    // `i` is the highest bin still at or above target; the curve drops below
    // target at the bin above it, and that bin is where filters stop.
    Some(freqs[i])
}

/// `correction_range` — the row's own rule, both edges.
///
/// Low = `max(low_corner_hz, right-window resolution limit, lowest bin with
/// SNR ≥ 25 dB)`. High = `min(20000, highest bin with SNR ≥ 25 dB)`, and no
/// filters above [`last_target_crossing`].
///
/// A range that comes out inverted — no bin clears the SNR floor, or the corner
/// sits above the roll-off knee — collapses to `(low, low)` rather than being
/// widened back: an empty correction band is the honest answer to "there is
/// nothing here we can measure well enough to correct", and the diagnosis
/// (`LowSnrHard`) is the refusal table's.
fn correction_range_scan(
    freqs: &[f64],
    measured_db: Option<&[f64]>,
    target_db: Option<&[f64]>,
    snr_db: Option<&[f64]>,
    low_corner_hz: f64,
    resolution_limit_hz: f64,
) -> (f64, f64) {
    let last = freqs.len().saturating_sub(1);
    let (snr_lo, snr_hi) = match snr_db {
        Some(snr) if snr.len() == freqs.len() => {
            let lo = snr.iter().position(|v| *v >= SNR_MIN_DB);
            let hi = snr.iter().rposition(|v| *v >= SNR_MIN_DB);
            match (lo, hi) {
                (Some(lo), Some(hi)) => (Some(lo), hi),
                // Nothing clears the floor: the low edge becomes unreachable,
                // which is what collapses the range below.
                _ => (None, 0),
            }
        }
        _ => (Some(0), last),
    };
    let mut low = low_corner_hz
        .max(resolution_limit_hz)
        .max(CORRECTION_RANGE_HZ.0);
    if let Some(idx) = snr_lo {
        low = low.max(freqs.get(idx).copied().unwrap_or(CORRECTION_RANGE_HZ.0));
    } else {
        low = CORRECTION_RANGE_HZ.1;
    }
    let mut high = freqs
        .get(snr_hi)
        .copied()
        .unwrap_or(CORRECTION_RANGE_HZ.1)
        .min(CORRECTION_RANGE_HZ.1);
    if let (Some(measured), Some(target)) = (measured_db, target_db) {
        if let Some(crossing) = last_target_crossing(freqs, measured, target, snr_hi) {
            high = high.min(crossing);
        }
    }
    low = low.clamp(CORRECTION_RANGE_HZ.0, CORRECTION_RANGE_HZ.1);
    high = high.clamp(CORRECTION_RANGE_HZ.0, CORRECTION_RANGE_HZ.1);
    (low, high.max(low))
}

// ---------------------------------------------------------------------------
// Target selection
// ---------------------------------------------------------------------------

/// `target`'s domain, its value and which of its three rationale keys applies.
///
/// The DOMAIN is "the class-filtered candidate set + `Parametric`". The filter
/// is not a UI default — an unfiltered match returns `harman_oe_2018` for a
/// loudspeaker, which is +8.3 dB at 3 kHz against the B&K room curve's
/// −3.0 dB, and applying it double-applies ear gain the loudspeaker already
/// delivers acoustically.
///
/// The three selection cases are the spec's, verbatim:
///
/// | Case | Rule | Source |
/// |---|---|---|
/// | Coupler, normal cal | `match_closest_target(class-filtered candidates)` | `Auto` |
/// | Coupler, EARS HEQ/HPN/IDF cal | force `flat.csv` (+ `Warn(CalHasTargetBakedIn)`, B7c's) | `Auto` |
/// | Room | `Parametric { … }` | `Default` |
///
/// The room arm reads `RoomTargetSpec::default()` through
/// `TargetChoice::room_default()` per § D-A rather than carrying literals, so an
/// ears ruling on the shelf costs one line in `paraeq-dsp` and nothing here.
fn target_decision(
    bundle: &MeasurementBundle,
    class: TransducerClass,
    profile: &PathProfile,
    measured_db: Option<&[f64]>,
    grid: &LogGrid,
) -> Decision<TargetChoice> {
    let candidates: Vec<&paraeq_dsp::targets::TargetCurve> = bundle
        .targets
        .iter()
        .filter(|t| t.classes.contains(&class))
        .collect();
    let parametric = TargetChoice::room_default();
    let mut domain: Vec<TargetChoice> = candidates
        .iter()
        .map(|t| TargetChoice::Curve {
            name: t.name.clone(),
        })
        .collect();
    domain.push(parametric.clone());

    let tilt = match &parametric {
        TargetChoice::Parametric {
            tilt_db_per_oct, ..
        } => *tilt_db_per_oct,
        TargetChoice::Curve { .. } => RoomTargetSpec::default().tilt_db_per_oct,
    };
    let room = |value: TargetChoice| {
        decision(
            value,
            Domain::Choice(domain.clone()),
            rationale::target_room_parametric(tilt),
            Invalidation::Reanalyze,
        )
    };

    let decided = match profile.coupling {
        // "Rooms don't get matched to a curve — the room's own response is the
        // thing we're fixing." `Source::Default`, per the table's own column:
        // nothing was matched, so nothing was measured to choose it.
        CouplingPath::Room => room(parametric.clone()),
        CouplingPath::Coupler => {
            // "Your EARS calibration already has a target baked into it.
            // Applying another would apply it twice." The forced curve is
            // `flat.csv` BY NAME, because a cal with a target baked in has
            // already delivered one and the second must be the identity.
            let baked_in = matches!(
                bundle.cal.as_ref().map(|c| c.variant),
                Some(CalVariant::EarsHeq | CalVariant::EarsHpn | CalVariant::EarsIdf)
            );
            let flat = candidates.iter().find(|t| t.name == "flat");
            match (baked_in, flat) {
                (true, Some(curve)) => measured(decision(
                    TargetChoice::Curve {
                        name: curve.name.clone(),
                    },
                    Domain::Choice(domain.clone()),
                    rationale::target_cal_baked_in(),
                    Invalidation::Reanalyze,
                )),
                // No `flat` among the class-legal candidates: the forced curve
                // does not exist, so there is nothing to force TO. Fall through
                // to the match rather than inventing one — and the warning the
                // baked-in cal earns is the refusal table's either way.
                _ => match measured_db.and_then(|curve| {
                    match_closest_target(class, grid.freqs(), curve, &bundle.targets).ok()
                }) {
                    Some(curve) => measured(decision(
                        TargetChoice::Curve {
                            name: curve.name.clone(),
                        },
                        Domain::Choice(domain.clone()),
                        rationale::target_matched(&curve.name, candidates.len()),
                        Invalidation::Reanalyze,
                    )),
                    // Nothing to match against — an unanalysable bundle, or no
                    // class-legal candidate at all. `Source::Default`, and the
                    // value is the first legal candidate if there is one, so the
                    // value stays inside its own domain.
                    //
                    // The COPY is `target_fallback`, not `target_matched`: this
                    // arm ran no comparison, so "that's the curve we matched,
                    // out of {k} we tried" would claim a match nobody made
                    // (ruling R-A11). `Source::Default` already says the same
                    // thing machine-readably; this is the half a user reads.
                    None => match candidates.first() {
                        Some(curve) => decision(
                            TargetChoice::Curve {
                                name: curve.name.clone(),
                            },
                            Domain::Choice(domain.clone()),
                            rationale::target_fallback(&curve.name),
                            Invalidation::Reanalyze,
                        ),
                        None => room(parametric.clone()),
                    },
                },
            }
        }
    };
    resolve(decided, bundle.overrides.target.as_ref())
}

/// The decided target, evaluated on the analysis grid.
///
/// `Curve` is looked up by name in the bundle's own candidate set and
/// interpolated; `Parametric` is built by `targets::build_room_target`, which
/// owns the shelf + tilt closed form and refuses a tilt outside `-1.5..=0.0`.
/// `None` when the named curve is not in the bundle or the parametric spec is
/// refused — the fit then emits nothing rather than correcting toward a curve
/// nobody named.
fn target_on_grid(
    bundle: &MeasurementBundle,
    target: &TargetChoice,
    grid: &LogGrid,
) -> Option<Vec<f64>> {
    match target {
        TargetChoice::Curve { name } => bundle
            .targets
            .iter()
            .find(|t| &t.name == name)
            .map(|t| t.interpolate(grid.freqs())),
        TargetChoice::Parametric {
            shelf_db,
            shelf_fc,
            shelf_q,
            tilt_db_per_oct,
        } => {
            let spec = RoomTargetSpec {
                shelf_hz: *shelf_fc,
                shelf_gain_db: *shelf_db,
                shelf_q: *shelf_q,
                tilt_db_per_oct: *tilt_db_per_oct,
                // The wire shape the decision table names carries four numbers
                // and the pivot is not one of them; `build_room_target` reads it
                // from the spec it is handed, so it comes from the same default
                // `TargetChoice::room_default()` read the other four from.
                pivot_hz: RoomTargetSpec::default().pivot_hz,
            };
            build_room_target(&spec, grid.freqs())
                .ok()
                .map(|curve| curve.gains_db)
        }
    }
}

// ---------------------------------------------------------------------------
// Curve helpers
// ---------------------------------------------------------------------------

/// The averaged response as ONE curve: the mean across channels of
/// `Analysis::averaged_db`.
///
/// `low_corner_hz`, `M` and the per-bin SNR are all one number or one curve,
/// and the response they are read off is per channel. Collapsing once, here,
/// keeps them read off the same curve — which is the difference between a
/// corner scanned on the left channel and compared against a midband taken
/// across both.
fn channel_mean_db(analysis: &AnalysisProducts, grid: &LogGrid) -> Option<Vec<f64>> {
    let channels = analysis.averaged_db.len();
    if channels == 0 {
        return None;
    }
    let mut mean = vec![0.0f64; grid.len()];
    for curve in &analysis.averaged_db {
        if curve.len() != grid.len() {
            return None;
        }
        for (m, v) in mean.iter_mut().zip(curve) {
            *m += v / channels as f64;
        }
    }
    Some(mean)
}

/// `M`: "mean magnitude over 200 Hz–2 kHz of the aligned, averaged, smoothed
/// curve".
///
/// `0.0` when there is no curve to read it off, which is the unanalysable case —
/// the evidence is then omitted entirely rather than carrying a fabricated
/// level, and `low_corner_scan` is never called with it.
fn midband_level_db(curve: Option<&[f64]>, grid: &LogGrid) -> f64 {
    let Some(curve) = curve else {
        return 0.0;
    };
    let (low, high) = MIDBAND_HZ;
    let mut sum = 0.0;
    let mut count = 0usize;
    for (f, v) in grid.freqs().iter().zip(curve) {
        if *f >= low && *f <= high {
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// The averaged response and the midband reference it is read against — the two
/// things `low_corner_hz`'s scan looks at.
fn averaged_evidence(curve: Option<&[f64]>, grid: &LogGrid, midband_db: f64) -> Vec<Evidence> {
    let Some(curve) = curve else {
        return Vec::new();
    };
    vec![
        Evidence::Curve {
            db: to_f32(curve),
            hz: to_f32(grid.freqs()),
            label: EvidenceLabel::AveragedResponse,
        },
        Evidence::Scalar {
            label: EvidenceLabel::MidbandLevel,
            unit: Unit::Db,
            value: midband_db,
        },
    ]
}

/// The Schroeder cross-check, as a span to shade next to σ(f).
///
/// § D-L: `room::transition_range` answers a different question from the scan's
/// clamp and is kept as evidence only. It is called with no T60 and no volume
/// because the wizard never asks for a volume, so this is always the honest
/// fallback range — which is exactly what the drawer should show beside a
/// measured crossing.
fn schroeder_evidence() -> Evidence {
    let range = transition_range(None, None);
    Evidence::Span {
        hz_hi: range.high_hz,
        hz_lo: range.low_hz,
        label: EvidenceLabel::SchroederRange,
    }
}

/// `Evidence::Curve` carries `f32`: it is plot data, and halving the wire size
/// of a 957-point curve matters more there than the fifteenth significant
/// figure does.
fn to_f32(values: &[f64]) -> Vec<f32> {
    values.iter().map(|v| *v as f32).collect()
}
