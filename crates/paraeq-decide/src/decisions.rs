//! The 22 decisions, their erased render view, and the mirror the Advanced
//! drawer writes.

use crate::decision::{Decision, Evidence, InRange, Invalidation, Rationale, Source};
use crate::profile::{AveragingMode, SmoothingMode};
use paraeq_dsp::targets::{RoomTargetSpec, TransducerClass};
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
    /// The preset NAME, not the 957-point curve: the drawer labels a control
    /// with it and an override round-trips it. The **resolved**
    /// [`AuthorityCurve`] is published in `Analysis` as a product.
    pub authority: Decision<AuthorityPreset>,
    pub averaging: Decision<AveragingMode>,
    pub class: Decision<TransducerClass>,
    /// Whether the two-clock resample was applied. The 22nd field.
    pub clock_adjust: Decision<bool>,
    /// The decision-table row is still named `correction_kind`; only the TYPE
    /// was renamed, to stop it colliding with the engine's `CorrectionConfig`.
    pub correction_kind: Decision<CorrectionForm>,
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
    /// expectation from the struct's own field list so a 23rd field cannot be
    /// added without landing here.
    pub fn iter(&self) -> impl Iterator<Item = DecisionView<'_>> {
        [
            view("align_spl_band", &self.align_spl_band),
            view("authority", &self.authority),
            view("averaging", &self.averaging),
            view("class", &self.class),
            view("clock_adjust", &self.clock_adjust),
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
    pub authority: Option<AuthorityPreset>,
    pub averaging: Option<AveragingMode>,
    pub class: Option<TransducerClass>,
    pub clock_adjust: Option<bool>,
    pub correction_kind: Option<CorrectionForm>,
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
/// **Now a re-export** (Stage 5): `paraeq_dsp::authority` owns the type and the
/// `AuthorityPolicy` endpoints (σ_full = 1.0 dB, σ_none = 6.0 dB) that produce
/// it, exactly as the decision-engine spec requires — "`decide()` consumes the
/// `AuthorityCurve` that module produces and must not re-specify different
/// numbers." The provisional definition that lived here is gone, so there is no
/// longer a second shape to drift from. Note the type is now **sealed**: only
/// `authority::build_authority` constructs one, and deserialization is guarded,
/// so `decide()` cannot fabricate a ceiling.
///
/// **The former OPEN-for-owner note is CLOSED** (plan item 10). It read that
/// the preset NAMES had nowhere to live, because the spec's decision table
/// gives `authority` the domain `Choice: Standard, Conservative (x0.5),
/// Custom(curve)` while typing the decision `Decision<AuthorityCurve>`, so
/// `Domain<AuthorityCurve>::Choice` could hold only concrete curves.
/// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-D rules it:
/// "Re-type to `Decision<AuthorityPreset>` with `{ Conservative,
/// Custom(AuthorityCurve), Standard }`; put the **resolved** curve in
/// `Analysis`." See [`AuthorityPreset`]. This type stays the re-export it
/// became in Stage 5 and is still what the drawer plots.
pub use paraeq_dsp::authority::AuthorityCurve;

/// The `authority` decision's value: a NAME the drawer can label a control
/// with, not a 957-point curve.
///
/// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-D, verbatim:
/// "Re-type to `Decision<AuthorityPreset>` with `{ Conservative,
/// Custom(AuthorityCurve), Standard }`; put the **resolved** curve in
/// `Analysis`. … It matches decision-engine's domain text verbatim, keeps
/// `Decisions` at 21 fields so the exhaustiveness test stays green, keeps
/// `AuthorityCurve` sealed, and makes an override round-trip a **name** rather
/// than a 957-point curve."
///
/// The resolved curve is an `Analysis` field because `Analysis` is explicitly
/// "products, not decisions" — there is exactly one resolved curve per run and
/// nothing about it has a domain the drawer may edit directly. Resolution
/// (which policy each preset selects) is a decision RULE and lands with the
/// rules, not here.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum AuthorityPreset {
    /// The spec's `Conservative (x0.5)` column: half the standard authority.
    Conservative,
    /// An explicit curve from the drawer. Sealed on the way in —
    /// [`AuthorityCurve`] deserializes only through its guarded `TryFrom`, so an
    /// override cannot fabricate a ceiling that skipped validation.
    Custom(AuthorityCurve),
    /// The default on every path.
    Standard,
}

/// `authority` is a `Choice` over the two NAMED presets — unordered — plus
/// `Custom`, which is legal by construction rather than by enumeration.
///
/// Ruling R-A4: [`AuthorityCurve`] is sealed (only `build_authority` makes one,
/// and deserialization goes through a guarded `TryFrom`), so every
/// representable `Custom` has already been validated. The domain lists
/// `Conservative` and `Standard`; it does NOT carry a curve, because a domain
/// is what the drawer draws and the resolved curve is an `Analysis` product.
impl InRange for AuthorityPreset {
    fn legal_by_construction(&self) -> bool {
        matches!(self, AuthorityPreset::Custom(_))
    }
}

/// Which SHAPE of correction the plan carries.
///
/// Renamed from `CorrectionKind` (plan item 11) so it stops colliding with the
/// engine's `CorrectionConfig { Fir, Iir, Peq }` across the wire: two types
/// named `*Kind` on one wire, with different variant sets, is the kind of
/// collision a reader resolves by guessing. The `Decisions` field and the
/// [`crate::RationaleKey`] keep the decision table's own row name,
/// `correction_kind`, so no serialized key moves; only the type's name changes,
/// and an externally-tagged enum does not put its own type name on the wire.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum CorrectionForm {
    MinPhaseFir,
    /// The default on all four paths: no added block latency onto an already
    /// 46–62 ms budget, and it can carry the per-band Q cap and excursion
    /// envelope as constraints.
    Peq,
}

/// `correction_kind` is a `Choice` over the two forms — the derived `Ord` is
/// declaration order, which is not a magnitude.
impl InRange for CorrectionForm {}

/// The path ceiling on boost Q that `decide()` consumes.
///
/// **Now a re-export** (Stage 6), for the reason [`AuthorityCurve`] moved in
/// Stage 5 and to the same module: `paraeq_dsp::authority` owns the excursion
/// envelope and both path ceilings, and composes the ceiling with REW's
/// gain-dependent cap in `max_q_for_boost_capped` — the "min of the two" this
/// type's own doc comment names. Keeping the definition here left the ceiling
/// applied nowhere and `decide()` free to re-specify numbers the
/// decision-engine spec says it must not. The variants, their fields and the
/// externally-tagged wire form are unchanged, so nothing under
/// `fixtures/decide/` and no persisted profile moves with the type.
///
/// The `PartialOrd` ruling and its OPEN-for-owner note travelled with the
/// definition; read them there. `paraeq-dsp` cannot carry the answer, because
/// `Domain` and `InRange` are this crate's types — `q_cap`'s domain is decided
/// on this side, exactly as `TransducerClass`'s is, and § D-C decides it as a
/// `Choice` over [`paraeq_dsp::authority::COUPLER_Q_CEILING`] and
/// [`paraeq_dsp::authority::ROOM_Q_CEILING`]. The `InRange` impl that makes that
/// domain answerable lives beside the trait, in [`crate::decision`].
pub use paraeq_dsp::authority::QCapPolicy;

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

impl TargetChoice {
    /// The room path's parametric default, READ from
    /// [`paraeq_dsp::targets::RoomTargetSpec::default`] rather than spelled as
    /// literals here.
    ///
    /// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-A rules
    /// that `decide()`'s room `Parametric` default "**reads
    /// `RoomTargetSpec::default()`** rather than carrying a literal", so an ears
    /// ruling on the shelf costs one line in `paraeq-dsp` and nothing here. The
    /// cross-crate field-for-field test is
    /// `the_room_target_default_equals_room_target_spec_default`.
    ///
    /// `RoomTargetSpec::pivot_hz` has no home on this variant: the wire shape
    /// the decision table names carries four numbers, and `build_room_target`
    /// reads the pivot from the spec it is handed. The test pins the pivot
    /// separately so the omission stays deliberate rather than silent.
    pub fn room_default() -> Self {
        let spec = RoomTargetSpec::default();
        Self::Parametric {
            shelf_db: spec.shelf_gain_db,
            shelf_fc: spec.shelf_hz,
            shelf_q: spec.shelf_q,
            tilt_db_per_oct: spec.tilt_db_per_oct,
        }
    }
}

/// `target` is a `Choice` over the class-filtered candidate set plus
/// `Parametric` — unordered.
///
/// `Parametric` is legal by construction: the list holds one REPRESENTATIVE
/// (the room default), so moving the tilt anywhere inside `targets`' own
/// `-1.5..=0.0` would read as illegal against a membership test.
/// `build_room_target` is the validator for the four numbers, and a spec it
/// refuses produces no curve at all — a different failure from an
/// out-of-domain override. See [`InRange::legal_by_construction`].
impl InRange for TargetChoice {
    fn legal_by_construction(&self) -> bool {
        matches!(self, TargetChoice::Parametric { .. })
    }
}

/// `window_type` is a `Choice` over the four window shapes — unordered.
impl InRange for WindowType {}
