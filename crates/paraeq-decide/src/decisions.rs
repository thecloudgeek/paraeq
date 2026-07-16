//! The 21 decisions, their erased render view, and the mirror the Advanced
//! drawer writes.

use crate::decision::{Decision, Evidence, Invalidation, Rationale, Source};
use crate::profile::{AveragingMode, SmoothingMode};
use paraeq_dsp::targets::TransducerClass;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Named fields, not a map: the drawer is exhaustive by construction and
/// cannot reference a key that does not exist. [`Decisions::iter`] gives the
/// renderer a uniform erased view without giving up the type.
///
/// Adding a decision is mechanical and reviewable: a field here, an
/// `Option<T>` on [`Overrides`], a [`crate::RationaleKey`], a row in the
/// spec's decision table, an arm in `iter()`, and a fixture. No UI changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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

/// Erase one decision to the shape all three renderers consume.
///
/// `to_value` cannot fail here: every `T` in [`Decisions`] is plain derived
/// data with no fallible `Serialize` impl, and serde_json maps a non-finite
/// f64 to `null` rather than erroring.
fn view<'a, T: Serialize>(id: &'static str, d: &'a Decision<T>) -> DecisionView<'a> {
    DecisionView {
        domain: serde_json::to_value(&d.domain).expect("Domain<T> is plain derived data"),
        evidence: &d.evidence,
        id,
        invalidates: d.invalidates,
        rationale: &d.rationale,
        source: d.source,
        value: serde_json::to_value(&d.value).expect("Decisions' T are plain derived data"),
    }
}

impl Decisions {
    /// Erased view for rendering. Both front-ends and the drawer consume this;
    /// nothing else. Auto mode paints `value`; guided mode paints `value` +
    /// `rationale` + `evidence`; the drawer paints `domain` as a control.
    ///
    /// Exhaustive over every field — `tests/test_decisions.rs` derives its
    /// expectation from the struct's own field list so a 22nd field cannot be
    /// added without landing here.
    pub fn iter(&self) -> impl Iterator<Item = DecisionView<'_>> {
        [
            view("align_spl_band", &self.align_spl_band),
            view("authority", &self.authority),
            view("averaging", &self.averaging),
            view("class", &self.class),
            view("correction_kind", &self.correction_kind),
            view("correction_range", &self.correction_range),
            view("fdw_post_cycles", &self.fdw_post_cycles),
            view("fdw_pre_cycles", &self.fdw_pre_cycles),
            view("flatness_target_db", &self.flatness_target_db),
            view("left_window_ms", &self.left_window_ms),
            view("low_corner_hz", &self.low_corner_hz),
            view("max_filters", &self.max_filters),
            view("positions_n", &self.positions_n),
            view("preamp_db", &self.preamp_db),
            view("q_cap", &self.q_cap),
            view("right_window_ms", &self.right_window_ms),
            view("shelves", &self.shelves),
            view("smoothing", &self.smoothing),
            view("target", &self.target),
            view("transition_hz", &self.transition_hz),
            view("window_type", &self.window_type),
        ]
        .into_iter()
    }
}

/// One decision, type erased for rendering. `id` is the [`Decisions`] field
/// name — the stable key the drawer writes back against.
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionView<'a> {
    pub domain: Value,
    pub evidence: &'a [Evidence],
    pub id: &'static str,
    pub invalidates: Invalidation,
    pub rationale: &'a Rationale,
    pub source: Source,
    pub value: Value,
}

/// Mirrors [`Decisions`] field-for-field with `Option<T>`. Written only by the
/// Advanced drawer. Anything else writing here is a bug.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Overrides {
    pub align_spl_band: Option<(f64, f64)>,
    pub authority: Option<AuthorityCurve>,
    pub averaging: Option<AveragingMode>,
    pub class: Option<TransducerClass>,
    pub correction_kind: Option<CorrectionKind>,
    pub correction_range: Option<(f64, f64)>,
    pub fdw_post_cycles: Option<f64>,
    pub fdw_pre_cycles: Option<f64>,
    pub flatness_target_db: Option<f64>,
    pub left_window_ms: Option<f64>,
    pub low_corner_hz: Option<f64>,
    pub max_filters: Option<usize>,
    pub positions_n: Option<usize>,
    pub preamp_db: Option<f64>,
    pub q_cap: Option<QCapPolicy>,
    pub right_window_ms: Option<f64>,
    pub shelves: Option<bool>,
    pub smoothing: Option<SmoothingMode>,
    pub target: Option<TargetChoice>,
    pub transition_hz: Option<f64>,
    pub window_type: Option<WindowType>,
}

/// The per-frequency boost/cut/Q ceiling `decide()` consumes.
///
/// PROVISIONAL LOCATION: room-dsp's `authority.rs` owns this type and the
/// `AuthorityPolicy` endpoints (σ_full = 1.0 dB, σ_none = 6.0 dB) that
/// produce it. This definition is the contract's placeholder until Stage 3
/// lands that module, at which point it becomes a re-export and `decide()`
/// must not re-specify different numbers.
///
/// OPEN for the owner — `authority`'s domain is not expressible as written.
/// The spec's decision table gives it as `Choice: Standard, Conservative
/// (x0.5), Custom(curve)` while typing the decision `Decision<AuthorityCurve>`,
/// so `Domain<AuthorityCurve>::Choice` can only hold concrete curves: the
/// drawer can offer the two precomputed ones and a custom curve arrives via
/// [`Overrides`], but the NAMES ("Standard", "Conservative") — the thing the
/// drawer would actually label its control with, and the thing an override
/// would round-trip — have nowhere to live. Either the domain becomes a
/// `Choice` over a named `AuthorityPreset` that resolves to a curve, or the
/// decision splits into a preset plus a derived curve. Left as the spec types
/// it rather than invented here.
#[derive(Clone, Debug, Deserialize, PartialEq, PartialOrd, Serialize)]
pub struct AuthorityCurve {
    pub freqs: Vec<f64>,
    pub max_boost_db: Vec<f64>,
    pub max_cut_db: Vec<f64>,
    pub max_q: Vec<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum CorrectionKind {
    MinPhaseFir,
    /// The default on all four paths: no added block latency onto an already
    /// 46–62 ms budget, and it can carry the per-band Q cap and excursion
    /// envelope as constraints.
    Peq,
}

/// The path ceiling on boost Q. REW's gain-dependent cap
/// (`Q_max = 0.227·f₀/A`) is applied unconditionally on top of this and is
/// not a policy choice — `decide()` takes the min of the two.
///
/// Deliberately NOT `PartialOrd`. The spec gives `q_cap` the domain
/// `Range 1.0..=20.0` "on the ceiling" — a range over a scalar, not over this
/// enum — so a `Domain<QCapPolicy>::Range` cannot answer `contains` for the
/// room's own `LogLinear` value. An ordering here would answer it `false`
/// rather than leaving the ambiguity visible. OPEN for the owner: either
/// `q_cap`'s domain is `Choice` over the two path policies, or `QCapPolicy`
/// splits into a decided ceiling scalar plus a profile-owned shape.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum QCapPolicy {
    /// A flat ceiling (coupler: 5.0). The drawer's `Range 1.0..=20.0` writes
    /// this variant.
    Ceiling(f64),
    /// Log-linear in frequency between two `(hz, q)` breakpoints. The room
    /// path: 10.0 @ 200 Hz → 3.0 @ 10 kHz.
    LogLinear { hi: (f64, f64), lo: (f64, f64) },
}

#[derive(Clone, Debug, Deserialize, PartialEq, PartialOrd, Serialize)]
pub enum TargetChoice {
    /// A loaded curve, by `TargetCurve::name`. The candidate set is always
    /// class-filtered — an unfiltered match returns `harman_oe_2018` for a
    /// loudspeaker and double-applies 11.3 dB of ear gain at 3 kHz.
    Curve { name: String },
    /// The room path. Rooms are never matched to a curve library: the room's
    /// own response is the thing being corrected, so "closest" is meaningless.
    Parametric {
        shelf_db: f64,
        shelf_fc: f64,
        shelf_q: f64,
        tilt_db_per_oct: f64,
    },
}

/// Applied independently left and right. A hard cut smears the measurement
/// across frequency.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, PartialOrd, Serialize)]
pub enum WindowType {
    BlackmanHarris,
    Hann,
    Rect,
    Tukey(f64),
}
