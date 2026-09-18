//! The four transducer types as data, not branches.
//!
//! Headphone measurement is structurally the same operation as room
//! measurement: N captures at different positions, averaged, with a variance
//! gate. Reseat scatter *is* the coupler's version of spatial scatter, and
//! σ(f) means the same thing in both. The split is [`CouplingPath`], and it
//! changes six values.

use crate::decision::InRange;
use paraeq_dsp::authority::{
    QCapPolicy, COUPLER_EXCURSION_DB, COUPLER_Q_CEILING, DEFAULT_BOOST_RATIO, DEFAULT_EXCURSION_DB,
    ROOM_Q_CEILING,
};
use paraeq_dsp::fr::Smoothing;
use paraeq_dsp::targets::TransducerClass;
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

/// Not a wire type: it is a table, keyed by a wire type. What the drawer sees
/// of a profile it sees through the `Decision`s the profile seeded.
#[derive(Clone, Debug, PartialEq)]
pub struct PathProfile {
    pub authority: AuthorityKind,
    pub averaging: AveragingMode,
    /// Boosts are capped at this fraction of the cut ceiling.
    ///
    /// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-E: the
    /// room auto path ships **cut-only by default** (0.0) while the coupler
    /// path keeps `paraeq_dsp::authority::DEFAULT_BOOST_RATIO`. The mechanism
    /// lives in `paraeq-dsp`; whether the room path passes 0.0 is this crate's
    /// policy call, expressible as a value — "cuts are free, boosts cost
    /// headroom and can damage drivers".
    pub boost_ratio: f64,
    pub coupling: CouplingPath,
    /// The excursion envelope's `(freq_hz, max_abs_db)` breakpoints for this
    /// path, READ from `paraeq_dsp::authority` rather than tabulated here.
    ///
    /// The coupler path's envelope stops above its step
    /// (`COUPLER_EXCURSION_DB`); the room path's is the Trinnov curve
    /// (`DEFAULT_EXCURSION_DB`). Pinned field-for-field against those constants
    /// by `every_profile_reads_its_excursion_breakpoints_from_paraeq_dsp`, so
    /// the two tables cannot drift.
    pub excursion_breakpoints_db: &'static [(f64, f64)],
    pub flatness_target_db: f64,
    pub gating: GatingMode,
    pub positions_default: usize,
    pub positions_domain: RangeInclusive<usize>,
    /// The ENFORCED floor, above `positions_domain`'s hard minimum.
    ///
    /// `docs/decisions/2026-07-21-decision-engine-open-questions.md` § Q7:
    /// "a default of **9** positions, a ceiling of **15**, an enforced floor of
    /// **5**, and a hard minimum of **3** before any average is computed."
    /// `positions_domain.start()` is the hard minimum; this is the floor the
    /// wizard holds the user to. It has no representation in the domain, which
    /// is why it needs a field of its own: a `Range 3..=15` cannot say "3 is
    /// representable but 5 is the floor".
    pub positions_floor: usize,
    /// The path ceiling on boost Q, READ from `paraeq_dsp::authority`'s two
    /// constants rather than spelled here. This is also the two-item `Choice`
    /// the `q_cap` decision's domain offers (§ D-C).
    pub q_cap: QCapPolicy,
    /// Guided-mode copy for "do the thing again, differently".
    pub reposition_noun: &'static str,
    /// MS-17's chain-sensitivity envelope, dB SPL per dBFS RMS.
    ///
    /// Mirrors `paraeq_measure::TransducerCaps::sensitivity_envelope_spl_per_dbfs`
    /// — two tables, one pair of numbers, by crate-DAG necessity. The
    /// measurement-safety table **owns** them (it is a safety limit that refuses
    /// before a sample is emitted); this copy exists so `decide()` can run the
    /// class cross-check without depending on `paraeq-measure`.
    /// `crates/paraeq-measure/tests/test_level.rs`'s
    /// `both_crates_tabulate_the_same_sensitivity_envelope` compares the two
    /// through a dev-dependency, so they cannot drift apart silently. If that
    /// test fails, correct THIS side.
    pub sensitivity_envelope_spl_per_dbfs: RangeInclusive<f64>,
    /// The coupler path additionally tapers toward 1/3 octave across
    /// 6–8 kHz. That taper is a property of the coupler analysis, not a
    /// smoothing mode — the spec's `smoothing` domain does not offer it.
    pub smoothing: SmoothingMode,
    /// Capture-plan parameter. The ONLY field the two room profiles differ in,
    /// and it is a safety parameter rather than an analysis one: everything
    /// downstream is identical, because what actually distinguishes a
    /// bookshelf from a floorstander is where its response rolls off, and that
    /// is measured (`low_corner_hz`), not declared.
    pub sweep_f_start_hz: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CouplingPath {
    Coupler,
    Room,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AuthorityKind {
    CouplerEnvelope,
    RoomEnvelope,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum AveragingMode {
    /// The coupler path. Correct there, and fixture-locked: the prototype
    /// smooths 1/6-octave before averaging, which is REW's endorsed regime for
    /// a dB-domain mean.
    DbMean,
    /// The room path, after Align SPL. Null-resistant, not null-immune.
    Power,
}

/// `averaging` is a `Choice` over the two modes — the derived `Ord` is
/// declaration order, which is not a magnitude.
impl InRange for AveragingMode {}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum GatingMode {
    /// Frequency-dependent window, half-amplitude width `n_c/f` centred on the
    /// peak. At 30 Hz the window is half a second; at 10 kHz it is 1.5 ms.
    Fdw { post: f64, pre: f64 },
    /// The coupler has no reflection problem to gate away.
    None,
}

/// The wire/domain shape of the `smoothing` decision.
///
/// **Merged with `paraeq_dsp::fr::Smoothing`** (plan item 11): the DSP type is
/// the canonical one and offers `Gaussian { fraction }`, which this enum did
/// not, so the two variant sets were not the same set and `decide()` could not
/// name a mode the pipeline can run. They now correspond one for one, pinned in
/// both directions by the [`From`] impls below and by
/// `every_smoothing_mode_maps_to_its_dsp_twin_and_back`.
///
/// It stays a separate type rather than becoming a re-export for one reason:
/// `fr::Smoothing` carries no serde derive, and adding one would make a third
/// serde-visible edit outside this crate in a window that is deliberately two
/// (`Clamp`'s derive and `PerChannel`'s). The correspondence is enforced by
/// exhaustive, wildcard-free matches instead, which is what would have caught
/// the divergence in the first place.
///
/// No longer `Eq`/`Ord`: `Gaussian { fraction: f64 }` has no total order. It was
/// never ordered for a reason — a smoothing mode is a choice, not a magnitude.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, PartialOrd, Serialize)]
pub enum SmoothingMode {
    /// Fractional-octave, e.g. `Fixed(6)` = 1/6 octave. Bit-exact legacy path:
    /// the coupler's fixture-pinned boxcar.
    Fixed(u32),
    /// Constant-Q Gaussian; `fraction` is the smoothing bandwidth in octaves
    /// (e.g. `1.0 / 6.0`).
    Gaussian {
        fraction: f64,
    },
    None,
    /// REW's variable profile: fine in the bass where the corrector has
    /// authority, coarse up top where it does not. That inversion is the
    /// authority mechanism, not an oversight.
    Variable,
}

/// `smoothing` is a `Choice` over the named modes — unordered.
impl InRange for SmoothingMode {}

/// Total, and exhaustive with no wildcard: a variant added on either side
/// stops this compiling, which is the whole point of the merge.
impl From<SmoothingMode> for Smoothing {
    fn from(mode: SmoothingMode) -> Self {
        match mode {
            SmoothingMode::Fixed(n) => Smoothing::Fixed(n),
            SmoothingMode::Gaussian { fraction } => Smoothing::Gaussian { fraction },
            SmoothingMode::None => Smoothing::None,
            SmoothingMode::Variable => Smoothing::Variable,
        }
    }
}

/// The other direction, equally total. See [`From<SmoothingMode> for Smoothing`].
impl From<Smoothing> for SmoothingMode {
    fn from(smoothing: Smoothing) -> Self {
        match smoothing {
            Smoothing::Fixed(n) => SmoothingMode::Fixed(n),
            Smoothing::Gaussian { fraction } => SmoothingMode::Gaussian { fraction },
            Smoothing::None => SmoothingMode::None,
            Smoothing::Variable => SmoothingMode::Variable,
        }
    }
}

static BOOKSHELF: PathProfile = PathProfile {
    authority: AuthorityKind::RoomEnvelope,
    averaging: AveragingMode::Power,
    // Cut-only on the room auto path (D-E).
    boost_ratio: 0.0,
    coupling: CouplingPath::Room,
    excursion_breakpoints_db: &DEFAULT_EXCURSION_DB,
    flatness_target_db: 3.0,
    gating: GatingMode::Fdw {
        post: 15.0,
        pre: 3.0,
    },
    positions_default: 9,
    positions_domain: 3..=15,
    positions_floor: 5,
    q_cap: ROOM_Q_CEILING,
    reposition_noun: "move the mic ~30 cm",
    sensitivity_envelope_spl_per_dbfs: 65.0..=120.0,
    smoothing: SmoothingMode::Variable,
    // Excursion rises as 1/f² below box tuning and ported boxes unload
    // entirely: 20 Hz into a 50 Hz-tuned box is 6.2× at-tuning excursion.
    sweep_f_start_hz: 30.0,
};

static FLOORSTANDER: PathProfile = PathProfile {
    authority: AuthorityKind::RoomEnvelope,
    averaging: AveragingMode::Power,
    boost_ratio: 0.0,
    coupling: CouplingPath::Room,
    excursion_breakpoints_db: &DEFAULT_EXCURSION_DB,
    flatness_target_db: 3.0,
    gating: GatingMode::Fdw {
        post: 15.0,
        pre: 3.0,
    },
    positions_default: 9,
    positions_domain: 3..=15,
    positions_floor: 5,
    q_cap: ROOM_Q_CEILING,
    reposition_noun: "move the mic ~30 cm",
    sensitivity_envelope_spl_per_dbfs: 65.0..=120.0,
    smoothing: SmoothingMode::Variable,
    sweep_f_start_hz: 20.0,
};

static IN_EAR: PathProfile = PathProfile {
    authority: AuthorityKind::CouplerEnvelope,
    averaging: AveragingMode::DbMean,
    boost_ratio: DEFAULT_BOOST_RATIO,
    coupling: CouplingPath::Coupler,
    excursion_breakpoints_db: &COUPLER_EXCURSION_DB,
    flatness_target_db: 1.0,
    gating: GatingMode::None,
    positions_default: 5,
    positions_domain: 3..=10,
    positions_floor: 5,
    q_cap: COUPLER_Q_CEILING,
    reposition_noun: "reseat the tip",
    sensitivity_envelope_spl_per_dbfs: 85.0..=130.0,
    smoothing: SmoothingMode::Fixed(6),
    sweep_f_start_hz: 20.0,
};

static OVER_EAR: PathProfile = PathProfile {
    authority: AuthorityKind::CouplerEnvelope,
    averaging: AveragingMode::DbMean,
    boost_ratio: DEFAULT_BOOST_RATIO,
    coupling: CouplingPath::Coupler,
    excursion_breakpoints_db: &COUPLER_EXCURSION_DB,
    flatness_target_db: 1.0,
    gating: GatingMode::None,
    positions_default: 5,
    positions_domain: 3..=10,
    positions_floor: 5,
    q_cap: COUPLER_Q_CEILING,
    reposition_noun: "reseat the headphone",
    sensitivity_envelope_spl_per_dbfs: 85.0..=130.0,
    smoothing: SmoothingMode::Fixed(6),
    sweep_f_start_hz: 20.0,
};

/// The profile is derivable from `bundle.class`, which is why `decide()` takes
/// one argument: a second would be a second place to disagree.
pub fn profile_for(class: TransducerClass) -> &'static PathProfile {
    match class {
        TransducerClass::Bookshelf => &BOOKSHELF,
        TransducerClass::Floorstander => &FLOORSTANDER,
        TransducerClass::InEar => &IN_EAR,
        TransducerClass::OverEar => &OVER_EAR,
    }
}
