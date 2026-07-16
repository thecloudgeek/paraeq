//! One decision: its value, its legal domain, its provenance, the measured
//! evidence that produced it, the rationale guided mode renders, and the
//! invalidation tier an override triggers. Every parameter a competitor asks
//! or hardcodes is one of these.

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

/// `class` is a `Choice` — the four transducer types are unordered.
impl InRange for TransducerClass {}

impl<T: InRange + PartialEq> Domain<T> {
    /// The spec's standing invariant: every `Decision::value` is inside its
    /// `Domain`. `Derived` constrains nothing — the rule that produced the
    /// value is the constraint, and there is no control to bound.
    pub fn contains(&self, value: &T) -> bool {
        match self {
            Domain::Choice(alternatives) => alternatives.iter().any(|a| a == value),
            Domain::Derived => true,
            Domain::Range { max, min, .. } => value.in_range(min, max),
        }
    }
}

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

/// One key per decision-table row. `target` carries three because its rationale
/// depends on which selection rule fired, and the copy differs materially.
/// Retained alongside the rendered text for tests and future l10n.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RationaleKey {
    AlignSplBand,
    Authority,
    Averaging,
    Class,
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
    /// The frequency below which the right window cannot resolve 1/N-octave
    /// detail.
    ResolutionLimit,
    SchroederRange,
    /// σ(f): the per-bin standard deviation in dB across positions.
    Sigma,
    Snr,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Unit {
    Count,
    Db,
    DbPerDecade,
    DbPerOctave,
    Hz,
    Milliseconds,
}
