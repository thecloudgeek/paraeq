//! The decision rules — one per row of the spec's decision table.
//!
//! **B7a ships the SKELETON: every value below is a clearly-marked DEFAULT and
//! carries `Source::Default` to say so. B7b replaces the bodies with the real
//! rules.** `Source::Default` is not a placeholder convention invented here; it
//! is the enum's own documented meaning — "`decide()` fell back to the
//! `PathProfile` default (evidence absent or inconclusive). Distinct from `Auto`
//! ON PURPOSE: the drawer must be able to say 'we could not measure this, so we
//! used the default'." A skeleton that has not measured anything is exactly that
//! case, so the drawer tells the truth about this tree without a special mode.
//!
//! What IS final here, because it is contract rather than policy:
//!
//! - **The domains.** Each one is the decision table's, with the three rulings
//!   that moved one: § D-C's two-item `q_cap` `Choice` (a `Range` over
//!   `QCapPolicy` answered `contains == false` for the room path's own value),
//!   § D-D's `AuthorityPreset` (so the preset NAMES have somewhere to live), and
//!   § D-K's `fdw_post_cycles` ceiling, derived at runtime.
//! - **The invalidation tiers.** § Override Semantics' three-way partition,
//!   plus `clock_adjust` at `Recapture`. The tier is a property of the
//!   `Decision`, "not a lookup table in the caller".
//! - **The rationale KEYS.** The copy is B7b's; the key is the machine-readable
//!   half and is already correct.
//! - **The override path.** [`resolve`] is the ONE place an override lands, so
//!   B7b's rules inherit it by construction rather than each remembering to
//!   honour `Overrides`.
//!
//! **The phase split is the spec's own dependency DAG, and it is the hook B7b
//! builds in.** Eleven decisions are read BEFORE any curve exists and feed the
//! analysis ([`analysis_decisions`]); ten are read OFF the analysis
//! ([`design_decisions`]); `preamp_db` is last ([`preamp_decision`]) because it
//! is a property of the bands the fit emitted, not of the curve they were fitted
//! to. That is the `Recapture`/`Reanalyze` → `Redesign` order the tier table
//! describes, spelled as three function calls.

use crate::analysis::{fdw_post_ceiling, realized_preamp_db, AnalysisProducts, Geometry};
use crate::bundle::MeasurementBundle;
use crate::decision::{
    Decision, Domain, Evidence, EvidenceLabel, Invalidation, RationaleKey, Source, Unit,
};
use crate::decisions::{
    AuthorityPreset, CorrectionForm, Decisions, QCapPolicy, TargetChoice, WindowType,
};
use crate::profile::{AveragingMode, CouplingPath, GatingMode, PathProfile, SmoothingMode};
use crate::rationale::placeholder;
use paraeq_dsp::authority::{AuthorityCurve, COUPLER_Q_CEILING, ROOM_Q_CEILING};
use paraeq_dsp::fr::DEFAULT_SPL_ALIGN_BAND;
use paraeq_dsp::gating::{
    farina_h2_bound_s, resolution_limit_hz, SweepParams, RESOLUTION_FRACTION,
};
use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::peq::EQBand;
use paraeq_dsp::targets::TransducerClass;

/// `M`'s band: "mean magnitude over 200 Hz–2 kHz of the aligned, averaged,
/// smoothed curve". The same band `align_spl_band` defaults to, which is one of
/// § D-B's two grounds for that default.
const MIDBAND_HZ: (f64, f64) = (200.0, 2000.0);

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

/// A decision as the rule leaves it, before any override: the default value,
/// its domain, its rationale key and its tier.
fn decision<T>(
    value: T,
    domain: Domain<T>,
    key: RationaleKey,
    invalidates: Invalidation,
) -> Decision<T> {
    Decision {
        domain,
        evidence: Vec::new(),
        invalidates,
        rationale: placeholder(key),
        source: Source::Default,
        value,
    }
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
        decision(
            bundle.class,
            Domain::Choice(vec![
                TransducerClass::Bookshelf,
                TransducerClass::Floorstander,
                TransducerClass::InEar,
                TransducerClass::OverEar,
            ]),
            RationaleKey::Class,
            Invalidation::Recapture,
        ),
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

    // `right_window_ms`: "Room: 400. Coupler: full (no gate)." "Full" is
    // expressed as the domain's own ceiling rather than as the recording's
    // length, so the value stays a decision the drawer can read rather than a
    // fact about one capture: at 1000 ms the right taper is spread so far past
    // a coupler IR's decay that the data sees no window at all, which is what
    // "no gate" means. The analysis stage bounds the gate it APPLIES by the
    // data the recording holds; see `AnalysisSettings::right_window_ms`.
    let right_ms = match profile.coupling {
        CouplingPath::Coupler => 1000.0,
        CouplingPath::Room => 400.0,
    };

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
                RationaleKey::AlignSplBand,
                Invalidation::Reanalyze,
            ),
            over.align_spl_band.as_ref(),
        ),
        averaging: resolve(
            decision(
                profile.averaging,
                Domain::Choice(vec![AveragingMode::DbMean, AveragingMode::Power]),
                RationaleKey::Averaging,
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
                RationaleKey::ClockAdjust,
                Invalidation::Recapture,
            ),
            over.clock_adjust.as_ref(),
        ),
        fdw_post_cycles: resolve(
            decision(
                match profile.gating {
                    GatingMode::Fdw { post, .. } => post,
                    // The coupler has no reflection problem to gate away, so the
                    // pass does not run; the decision still has a value, because
                    // the drawer still has a control and "off" is the profile's
                    // `GatingMode`, not a cycle count of zero.
                    GatingMode::None => 15.0,
                },
                Domain::Range {
                    max: fdw_post_ceiling(grid),
                    min: 3.0,
                    step: None,
                },
                RationaleKey::FdwPostCycles,
                Invalidation::Reanalyze,
            ),
            over.fdw_post_cycles.as_ref(),
        ),
        fdw_pre_cycles: resolve(
            decision(
                // "Asymmetric pre-lobe. Default 3" — a noise gate on pre-peak
                // artifacts, not a resolution control, and DR1 § Q3 calls it
                // "the lowest-confidence knob".
                match profile.gating {
                    GatingMode::Fdw { pre, .. } => pre,
                    GatingMode::None => 3.0,
                },
                // The decision table's `Range 1.0..=61.0`, verbatim. § D-K
                // narrowed the POST ceiling to what the implementation can
                // honour and said nothing about this one, and the row's own rule
                // ("clamped `≤ fdw_post_cycles`") is a rule rather than a
                // domain — so the ceiling stands as written and the clamp is
                // B7b's to apply. Flagged rather than quietly narrowed: a domain
                // is what the drawer draws, and changing one is a wire change.
                Domain::Range {
                    max: 61.0,
                    min: 1.0,
                    step: None,
                },
                RationaleKey::FdwPreCycles,
                Invalidation::Reanalyze,
            ),
            over.fdw_pre_cycles.as_ref(),
        ),
        left_window_ms: resolve(
            with_evidence(
                decision(
                    left_ms,
                    Domain::Range {
                        max: peak_ms,
                        min: 1.0,
                        step: None,
                    },
                    RationaleKey::LeftWindowMs,
                    Invalidation::Reanalyze,
                ),
                vec![Evidence::Scalar {
                    label: EvidenceLabel::PeakArrival,
                    unit: Unit::Milliseconds,
                    value: geometry.peak_time_ms,
                }],
            ),
            over.left_window_ms.as_ref(),
        ),
        positions_n: resolve(
            decision(
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
                RationaleKey::PositionsN,
                Invalidation::Recapture,
            ),
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
                    RationaleKey::RightWindowMs,
                    Invalidation::Reanalyze,
                ),
                // "Publish the **stricter** resolution limit … not `1/T`."
                vec![Evidence::Scalar {
                    label: EvidenceLabel::ResolutionLimit,
                    unit: Unit::Hz,
                    value: resolution_limit_hz(right_ms / 1000.0, RESOLUTION_FRACTION),
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
                RationaleKey::Smoothing,
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
                RationaleKey::WindowType,
                Invalidation::Reanalyze,
            ),
            over.window_type.as_ref(),
        ),
    }
}

/// Phase 2. See the module header.
///
/// **B7b's four policy scans land here** and none of them is implemented:
/// `low_corner_hz`'s scan up from the lowest valid bin, `transition_hz`'s
/// sustained σ ≥ 3 dB crossing, `correction_range`'s per-bin SNR floor, and
/// `correction_range`'s scan DOWN for the last crossing of `measured − target`.
/// The defaults below are deliberately the widest honest answer in each case, so
/// that a missing scan shows up as "we corrected more than we should have" in a
/// review rather than as a plausible number nobody re-derives.
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
            RationaleKey::Authority,
            Invalidation::Redesign,
        ),
        over.authority.as_ref(),
    );
    let authority_curve =
        crate::analysis::resolve_authority(&preset.value, grid, &analysis.sigma_db, profile);
    let authority = with_evidence(
        Decision {
            domain: Domain::Choice(vec![
                AuthorityPreset::Conservative,
                AuthorityPreset::Custom(authority_curve.clone()),
                AuthorityPreset::Standard,
            ]),
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
    // `f′ ∈ [f, 200]`". DEFAULT: the sweep's own start, which is the lowest
    // frequency the capture can possibly support an answer at. B7b's scan
    // replaces it, and the midband reference `M` it scans against is already
    // attached below as evidence.
    let low_corner_hz = bundle
        .capture
        .sweep
        .f_start_hz
        .clamp(grid.f_min(), grid.f_max());
    let midband_db = midband_level_db(analysis, grid);

    let design = DesignDecisions {
        authority,
        correction_kind: resolve(
            decision(
                // "`Peq` on all four paths": no added block latency onto an
                // already 46–62 ms budget, and it can carry the per-band Q cap
                // and excursion envelope as constraints.
                CorrectionForm::Peq,
                Domain::Choice(vec![CorrectionForm::MinPhaseFir, CorrectionForm::Peq]),
                RationaleKey::CorrectionKind,
                Invalidation::Redesign,
            ),
            over.correction_kind.as_ref(),
        ),
        correction_range: resolve(
            decision(
                // DEFAULT: the analysis band, uncut. The three bounds that
                // narrow it — the right window's resolution limit, the per-bin
                // SNR floor, and the last crossing of target — are B7b's.
                (low_corner_hz, grid.f_max()),
                Domain::Range {
                    max: (20000.0, 20000.0),
                    min: (20.0, 20.0),
                    step: None,
                },
                RationaleKey::CorrectionRange,
                Invalidation::Redesign,
            ),
            over.correction_range.as_ref(),
        ),
        flatness_target_db: resolve(
            decision(
                // The profile constant: 3.0 room, 1.0 coupler. Not a scan —
                // this row's Rule column is empty in the table.
                profile.flatness_target_db,
                Domain::Range {
                    max: 6.0,
                    min: 0.5,
                    step: None,
                },
                RationaleKey::FlatnessTargetDb,
                Invalidation::Redesign,
            ),
            over.flatness_target_db.as_ref(),
        ),
        low_corner_hz: resolve(
            with_evidence(
                decision(
                    low_corner_hz,
                    // `Derived` and not `Range`: "override its inputs instead".
                    // Rendered read-only in the drawer, never hidden.
                    Domain::Derived,
                    RationaleKey::LowCornerHz,
                    Invalidation::Reanalyze,
                ),
                averaged_evidence(analysis, grid, midband_db),
            ),
            over.low_corner_hz.as_ref(),
        ),
        max_filters: resolve(
            decision(
                // The table's default. The greedy loop that would spend fewer
                // of them — "stop when residual RMS over the authority band
                // `< flatness_target_db`" — is `autofit::auto_fit_room`'s, and
                // its CALL is B7b's.
                10,
                Domain::Range {
                    max: 20,
                    min: 1,
                    step: None,
                },
                RationaleKey::MaxFilters,
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
                RationaleKey::QCap,
                Invalidation::Redesign,
            ),
            over.q_cap.as_ref(),
        ),
        shelves: resolve(
            decision(
                true,
                Domain::Choice(vec![false, true]),
                RationaleKey::Shelves,
                Invalidation::Redesign,
            ),
            over.shelves.as_ref(),
        ),
        target: resolve(target_default(bundle, class, profile), over.target.as_ref()),
        transition_hz: resolve(
            with_evidence(
                decision(
                    // The spec's own fallback, verbatim: "No crossing ⇒ 200.0,
                    // `source: Default`". B7b's σ-crossing scan turns this into
                    // an `Auto` on the bundles that have a crossing; B17 makes
                    // the fallback SAY it is a fallback.
                    200.0,
                    Domain::Range {
                        max: 400.0,
                        min: 80.0,
                        step: None,
                    },
                    RationaleKey::TransitionHz,
                    Invalidation::Reanalyze,
                ),
                vec![Evidence::Curve {
                    db: to_f32(&analysis.sigma_db),
                    hz: to_f32(grid.freqs()),
                    label: EvidenceLabel::Sigma,
                }],
            ),
            over.transition_hz.as_ref(),
        ),
    };
    (design, authority_curve)
}

/// Phase 3 — `preamp_db`, "`-max(0, max_f of the REALIZED cascade)`; no
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
        decision(
            preamp_db,
            Domain::Derived,
            RationaleKey::PreampDb,
            Invalidation::Redesign,
        ),
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

/// `target`'s domain and its default value.
///
/// The DOMAIN is final: "the class-filtered candidate set + `Parametric`". The
/// filter is not a UI default — an unfiltered match returns `harman_oe_2018` for
/// a loudspeaker, which is +8.3 dB at 3 kHz against the B&K room curve's
/// −3.0 dB, and applying it double-applies ear gain the loudspeaker already
/// delivers acoustically.
///
/// The VALUE is a default on both paths. Room is final too — "Rooms don't get
/// matched to a curve", and `TargetChoice::room_default()` reads
/// `RoomTargetSpec::default()` per § D-A rather than carrying literals. The
/// coupler's is not: B7b's three selection cases (`match_closest_target` on the
/// class-filtered set, the EARS HEQ/HPN/IDF force to `flat`, and the room arm)
/// replace it, and the rationale KEY moves with them — `TargetCalBakedIn` and
/// `TargetMatched` exist for exactly that split.
fn target_default(
    bundle: &MeasurementBundle,
    class: TransducerClass,
    profile: &PathProfile,
) -> Decision<TargetChoice> {
    let mut candidates: Vec<TargetChoice> = bundle
        .targets
        .iter()
        .filter(|t| t.classes.contains(&class))
        .map(|t| TargetChoice::Curve {
            name: t.name.clone(),
        })
        .collect();
    let parametric = TargetChoice::room_default();
    let value = match profile.coupling {
        CouplingPath::Coupler => candidates.first().cloned().unwrap_or(parametric.clone()),
        CouplingPath::Room => parametric.clone(),
    };
    candidates.push(parametric);
    decision(
        value,
        Domain::Choice(candidates),
        match profile.coupling {
            CouplingPath::Coupler => RationaleKey::TargetMatched,
            CouplingPath::Room => RationaleKey::TargetRoomParametric,
        },
        Invalidation::Reanalyze,
    )
}

/// `M`: "mean magnitude over 200 Hz–2 kHz of the aligned, averaged, smoothed
/// curve".
///
/// Averaged across channels as well as across the band, because `M` is one
/// number and the curve it references is per channel. `0.0` when there is no
/// curve to read it off, which is the unanalysable case — the evidence is then
/// omitted entirely rather than carrying a fabricated level.
fn midband_level_db(analysis: &AnalysisProducts, grid: &LogGrid) -> f64 {
    let (low, high) = MIDBAND_HZ;
    let mut sum = 0.0;
    let mut count = 0usize;
    for curve in &analysis.averaged_db {
        for (f, v) in grid.freqs().iter().zip(curve) {
            if *f >= low && *f <= high {
                sum += v;
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// The averaged response and the midband reference it is read against — the two
/// things `low_corner_hz`'s scan looks at, attached whether or not the scan has
/// been written yet.
fn averaged_evidence(
    analysis: &AnalysisProducts,
    grid: &LogGrid,
    midband_db: f64,
) -> Vec<Evidence> {
    let Some(first) = analysis.averaged_db.first() else {
        return Vec::new();
    };
    vec![
        Evidence::Curve {
            db: to_f32(first),
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

/// `Evidence::Curve` carries `f32`: it is plot data, and halving the wire size
/// of a 957-point curve matters more there than the fifteenth significant
/// figure does.
fn to_f32(values: &[f64]) -> Vec<f32> {
    values.iter().map(|v| *v as f32).collect()
}
