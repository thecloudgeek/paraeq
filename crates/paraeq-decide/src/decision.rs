//! One decision: its value, its legal domain, its provenance, the measured
//! evidence that produced it, the rationale guided mode renders, and the
//! invalidation tier an override triggers. Every parameter a competitor asks
//! or hardcodes is one of these.

use crate::decisions::{AuthorityPreset, CorrectionForm, TargetChoice, WindowType};
use crate::profile::{AveragingMode, SmoothingMode};
use paraeq_dsp::authority::QCapPolicy;
use paraeq_dsp::targets::TransducerClass;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Domain<T> {
    /// Enumerable alternatives (target curves, window types).
    Choice(Vec<T>),
    /// Derived and not user-settable directly — override its inputs instead.
    /// Rendered read-only in the drawer, never hidden.
    Derived,
    Range {
        max: T,
        min: T,
        step: Option<T>,
    },
}

impl<T> Domain<T> {
    /// Whether the drawer draws a control or read-only text. A decision with
    /// no domain would be invisible to the drawer — a hidden constant with
    /// extra steps — so every domain answers this, including `Derived`.
    pub fn is_user_settable(&self) -> bool {
        !matches!(self, Domain::Derived)
    }
}

/// Whether a value lies inside a [`Domain::Range`]'s bounds.
///
/// Deliberately NOT `PartialOrd`. The two band-valued decisions
/// (`align_spl_band`, `correction_range`) are `(f64, f64)`, and Rust's tuple
/// ordering is LEXICOGRAPHIC — it looks at the second element only when the
/// first is equal — so `min <= value && value <= max` bounds a band's lower
/// edge and leaves its upper edge free: `(500.0, 99_999.0)` would satisfy
/// `align_spl_band`'s documented `Range` within 100..=8000, because
/// `500 < 8000` settles the comparison before the upper edge is ever read.
/// A band is bounded when BOTH endpoints are, so tuples compare element-wise.
pub trait InRange {
    /// `false` by default: a `Range` over a type with no ordering is
    /// meaningless, and reporting "not contained" surfaces that as the
    /// invariant violation it is instead of silently passing. Choice-only
    /// types take this default.
    fn in_range(&self, _min: &Self, _max: &Self) -> bool {
        false
    }

    /// Whether this value is legal no matter what a [`Domain::Choice`] list
    /// enumerates, because its own type already guarantees it.
    ///
    /// `false` by default, which is the ordinary case: a `Choice` is an
    /// enumeration and membership is the whole test. **Two decisions need the
    /// escape**, and neither can be expressed by lengthening a list:
    ///
    /// * `authority` — `AuthorityPreset::Custom(curve)` carries a 957-point
    ///   [`paraeq_dsp::authority::AuthorityCurve`], which is SEALED: only
    ///   `build_authority` constructs one and deserialization goes through a
    ///   guarded `TryFrom`, so every representable `Custom` has already been
    ///   validated. Enumerating them is impossible and enumerating THIS run's
    ///   one was the defect ruling R-A4 closes — the domain carried a byte-for-byte
    ///   copy of `Analysis::authority`, 77–89 KB per frozen case, to say
    ///   "Custom = whatever Standard produced".
    /// * `target` — `TargetChoice::Parametric { .. }` is a SHAPE with four
    ///   numbers, and the list holds one representative (the room default), so
    ///   moving the tilt anywhere inside `targets`' own `-1.5..=0.0` would read
    ///   as illegal. `build_room_target` is the validator; a spec outside its
    ///   range produces no curve and the fit emits nothing, which is a
    ///   different failure from an out-of-domain override.
    ///
    /// Expressed as a trait method rather than as an omission from the
    /// out-of-domain row list, so that the exception is one named, tested place
    /// instead of two call sites that have to remember it.
    fn legal_by_construction(&self) -> bool {
        false
    }
}

macro_rules! in_range_scalar {
    ($($t:ty),*) => {$(
        impl InRange for $t {
            fn in_range(&self, min: &Self, max: &Self) -> bool {
                min <= self && self <= max
            }
        }
    )*};
}
in_range_scalar!(f64, u32, usize);

/// A band: both endpoints bounded, element-wise.
impl InRange for (f64, f64) {
    fn in_range(&self, min: &Self, max: &Self) -> bool {
        self.0.in_range(&min.0, &max.0) && self.1.in_range(&min.1, &max.1)
    }
}

/// `shelves` is a `Choice` over `{false, true}` — a two-item select, not a
/// range.
impl InRange for bool {}

/// `class` is a `Choice` — the four transducer types are unordered.
impl InRange for TransducerClass {}

/// `q_cap` is a `Choice` over the two path policies, so the default `false`
/// never runs.
///
/// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-C, verbatim:
/// "`Domain::Choice(vec![Ceiling(5.0), LogLinear { hi: (10000.0, 3.0), lo:
/// (200.0, 10.0) }])` … the drawer's control becomes a two-item select over the
/// two path policies, which is what the value actually is."
///
/// The impl exists rather than being omitted because [`Domain::contains`] is
/// bounded on this trait: without it `Domain<QCapPolicy>` has no `contains` at
/// all and the standing "every `Decision::value` is inside its `Domain`"
/// invariant is not merely unfalsifiable but unwritable. The `Range` form it
/// used to take answered `contains == false` for the room path's own value,
/// which is the defect D-C closes. `QCapPolicy` is deliberately not
/// `PartialOrd` (`paraeq_dsp::authority`), so there is nothing to order here.
impl InRange for QCapPolicy {}

impl<T: InRange + PartialEq> Domain<T> {
    /// The spec's standing invariant: every `Decision::value` is inside its
    /// `Domain`. `Derived` constrains nothing — the rule that produced the
    /// value is the constraint, and there is no control to bound.
    pub fn contains(&self, value: &T) -> bool {
        match self {
            // The by-construction escape comes FIRST: a `Choice` list cannot
            // enumerate a sealed 957-point curve or a four-number shape, and a
            // membership test over such a list answers `false` for values that
            // are legal by their own type. See [`InRange::legal_by_construction`].
            Domain::Choice(alternatives) => {
                value.legal_by_construction() || alternatives.iter().any(|a| a == value)
            }
            Domain::Derived => true,
            Domain::Range { max, min, .. } => value.in_range(min, max),
        }
    }
}

/// § D-N's half of an out-of-domain override: bring the requested value INSIDE
/// the domain rather than obeying it or ignoring it.
///
/// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-N, verbatim:
/// "**Clamp into the domain, set `source: UserOverride` with the clamped value,
/// and emit `DiagnosticCode::OverrideOutOfDomain` at `Severity::Warn`**, naming
/// the decision and both numbers." `decide()` has no `Result`, so the only two
/// candidates were clamp-and-warn and ignore-and-keep-Auto, and silently
/// ignoring the user's intent recreates exactly the "second, lying source of
/// truth about what the app did" this design exists to prevent.
///
/// **The default is the `Choice` rule, because a `Choice` has no nearest legal
/// value.** "Halfway between `Hann` and `Rect`" is not a window, so an illegal
/// choice falls back to the value the RULE decided — never to an invented
/// member of the list, and never to the first one, which would be
/// alphabetical-order-as-policy. Scalars, bands and counts override this with a
/// real clamp; see the impls below.
///
/// `source` stays `UserOverride` either way: the value moved and the intent did
/// not, and the row the refusal table emits is what tells the user which is
/// which.
pub trait ClampIntoDomain: Clone + InRange + PartialEq + Sized {
    fn clamp_into_domain(self, domain: &Domain<Self>, rule_value: &Self) -> Self {
        if domain.contains(&self) {
            self
        } else {
            rule_value.clone()
        }
    }
}

/// A scalar clamps to the `Range`'s own ends.
///
/// A non-finite request falls back to the rule's value rather than clamping:
/// `f64::clamp` propagates NaN, so clamping one would leave the decision
/// carrying a NaN that `serde_json` then writes as `null`.
impl ClampIntoDomain for f64 {
    fn clamp_into_domain(self, domain: &Domain<Self>, rule_value: &Self) -> Self {
        match domain {
            Domain::Range { max, min, .. } if self.is_finite() && min <= max => {
                self.clamp(*min, *max)
            }
            _ if domain.contains(&self) => self,
            _ => *rule_value,
        }
    }
}

/// A count clamps to the `Range`'s own ends.
impl ClampIntoDomain for usize {
    fn clamp_into_domain(self, domain: &Domain<Self>, rule_value: &Self) -> Self {
        match domain {
            Domain::Range { max, min, .. } if min <= max => self.clamp(*min, *max),
            _ if domain.contains(&self) => self,
            _ => *rule_value,
        }
    }
}

/// A band clamps **per edge**, for the same reason [`InRange`] compares per
/// edge: a band is two independent numbers, and clamping the pair as a tuple
/// would leave one edge outside the domain whenever the other settled the
/// comparison first.
impl ClampIntoDomain for (f64, f64) {
    fn clamp_into_domain(self, domain: &Domain<Self>, rule_value: &Self) -> Self {
        match domain {
            Domain::Range { max, min, .. } => (
                self.0.clamp_into_domain(
                    &Domain::Range {
                        max: max.0,
                        min: min.0,
                        step: None,
                    },
                    &rule_value.0,
                ),
                self.1.clamp_into_domain(
                    &Domain::Range {
                        max: max.1,
                        min: min.1,
                        step: None,
                    },
                    &rule_value.1,
                ),
            ),
            _ if domain.contains(&self) => self,
            _ => *rule_value,
        }
    }
}

impl ClampIntoDomain for AuthorityPreset {}
impl ClampIntoDomain for CorrectionForm {}
impl ClampIntoDomain for QCapPolicy {}
impl ClampIntoDomain for SmoothingMode {}
impl ClampIntoDomain for TargetChoice {}
impl ClampIntoDomain for TransducerClass {}
impl ClampIntoDomain for WindowType {}
impl ClampIntoDomain for bool {}
impl ClampIntoDomain for AveragingMode {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
///
/// The tiers nest, so [`Ord`] is cost order rather than declaration order: a
/// cascade of overrides re-runs the max, and `Reanalyze` already ends in
/// `Redesign`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Invalidation {
    /// Re-run gating/FDW/smoothing/averaging/compensation from cached IRs,
    /// then Redesign. ~200 ms.
    Reanalyze,
    /// Must re-measure. Mic, output device, sample rate, N-increase only.
    Recapture,
    /// Re-run autofit + preamp from the cached analysis curve. ~50 ms.
    Redesign,
}

impl Invalidation {
    fn cost_rank(self) -> u8 {
        match self {
            Invalidation::Redesign => 0,
            Invalidation::Reanalyze => 1,
            Invalidation::Recapture => 2,
        }
    }
}

impl Ord for Invalidation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cost_rank().cmp(&other.cost_rank())
    }
}

impl PartialOrd for Invalidation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Rationale {
    pub key: RationaleKey,
    /// Rendered here, in Rust. The UI is a text field, not an author.
    pub text: String,
}

/// One key per decision-table row. `target` carries FOUR because its rationale
/// depends on which selection rule fired, and the copy differs materially —
/// the three the spec's selection table names, plus the fallback arm where
/// nothing could be matched at all (ruling R-A11).
/// Retained alongside the rendered text for tests and future l10n.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RationaleKey {
    AlignSplBand,
    Authority,
    Averaging,
    Class,
    /// The 2026-07-21 record's §Q6 drawer toggle: "clock-adjust toggle with the
    /// estimated ppm shown, **defaulted on**".
    ClockAdjust,
    CorrectionKind,
    CorrectionRange,
    FdwPostCycles,
    FdwPreCycles,
    FlatnessTargetDb,
    LeftWindowMs,
    LowCornerHz,
    MaxFilters,
    PositionsN,
    PreampDb,
    QCap,
    RightWindowMs,
    Shelves,
    Smoothing,
    /// Coupler, EARS HEQ/HPN/IDF cal: forced to `flat`.
    TargetCalBakedIn,
    /// Coupler, nothing to match against: the first class-legal candidate, as a
    /// FALLBACK. Its own key because the matched copy ("that's the curve we
    /// matched") claims a comparison that never ran (ruling R-A11), and the
    /// `Source` is `Default` here rather than `Auto` for the same reason.
    TargetFallback,
    /// Coupler, normal cal: matched within the class-filtered candidate set.
    TargetMatched,
    /// Room: parametric, never matched.
    TargetRoomParametric,
    TransitionHz,
    WindowType,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Evidence {
    /// A plottable curve: σ(f), excess group delay, the authority envelope.
    Curve {
        db: Vec<f32>,
        hz: Vec<f32>,
        label: EvidenceLabel,
    },
    Scalar {
        label: EvidenceLabel,
        unit: Unit,
        value: f64,
    },
    /// A frequency span to shade on the plot (the authority split).
    Span {
        hz_hi: f64,
        hz_lo: f64,
        label: EvidenceLabel,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EvidenceLabel {
    AuthorityEnvelope,
    /// The span the authority split shades.
    AuthoritySplit,
    AveragedResponse,
    ExcessGroupDelay,
    /// `M`: mean magnitude over 200 Hz–2 kHz of the aligned, averaged,
    /// smoothed curve.
    MidbandLevel,
    /// `t_peak`: how much time exists before the impulse arrives.
    PeakArrival,
    /// Mean of the verification residual over the gated band, dB. Near zero is
    /// what says the level compensation `K` was right.
    ResidualMean,
    /// Spread of the verification residual about its mean, dB.
    ResidualScatter,
    /// RMS of `measured_corrected − (measured_baseline + designed_correction)`
    /// over the authority band, dB. **The gated quantity.**
    ///
    /// Attached once **per capture channel**, so this label may repeat inside
    /// one `Vec<Evidence>`: the gate is the worst channel, and the drawer has to
    /// be able to say which ear failed.
    ResidualVsPrediction,
    /// RMS of the corrected response against the decided target, dB. Reported,
    /// plotted, and **never gated** — on a coupler with a 1.0 dB flatness target
    /// it would refuse a correct correction whenever the rig's own error exceeds
    /// the gate, which EARS routinely does.
    ResidualVsTarget,
    /// The frequency below which the right window cannot resolve 1/N-octave
    /// detail.
    ResolutionLimit,
    SchroederRange,
    /// How many biquad rows the engine substituted IDENTITY for because they
    /// failed the Jury stability test at the live rate. Evidence, not a gate:
    /// the realized-cascade prediction already models the substitution.
    SectionsSubstituted,
    /// σ(f): the per-bin standard deviation in dB across positions.
    Sigma,
    Snr,
    /// RMS of the two-clock marker fit's residuals, capture samples.
    TwoClockResidual,
    /// The estimated mic-vs-output clock skew, parts per million.
    TwoClockSkewPpm,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Unit {
    Count,
    Db,
    DbPerDecade,
    DbPerOctave,
    Hz,
    Milliseconds,
    /// Clock skew. [`Unit::Count`] is an integer count and would misreport a
    /// fractional ppm; this is also the unit `Diagnostic::value` carries for
    /// D-Q's `TwoClock` warning.
    PartsPerMillion,
    /// Capture samples, and **fractional**: a matched-filter residual is not an
    /// integer, which is why this is not [`Unit::Count`], and it is a count of
    /// samples rather than a duration, which is why it is not
    /// [`Unit::Milliseconds`].
    Samples,
}
