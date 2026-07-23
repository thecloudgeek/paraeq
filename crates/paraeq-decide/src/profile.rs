//! The four transducer types as data, not branches.
//!
//! Headphone measurement is structurally the same operation as room
//! measurement: N captures at different positions, averaged, with a variance
//! gate. Reseat scatter *is* the coupler's version of spatial scatter, and
//! σ(f) means the same thing in both. The split is [`CouplingPath`], and it
//! changes six values.

use paraeq_dsp::targets::TransducerClass;
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

/// Not a wire type: it is a table, keyed by a wire type. What the drawer sees
/// of a profile it sees through the `Decision`s the profile seeded.
#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum GatingMode {
    /// Frequency-dependent window, half-amplitude width `n_c/f` centred on the
    /// peak. At 30 Hz the window is half a second; at 10 kHz it is 1.5 ms.
    Fdw { post: f64, pre: f64 },
    /// The coupler has no reflection problem to gate away.
    None,
}

/// PROVISIONAL TYPE: room-dsp's `fr::Smoothing` landed in Stage 4 and is the
/// canonical smoothing type — it additionally offers `Gaussian { fraction }`,
/// which this contract-side enum does not. The two merge when Stage 6 wires
/// `decide()` to the analysis pipeline; until then this stays the wire/domain
/// shape the spec's decision table names, and must not grow variants
/// `decide()` cannot yet honour.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SmoothingMode {
    /// Fractional-octave, e.g. `Fixed(6)` = 1/6 octave.
    Fixed(u32),
    None,
    /// REW's variable profile: fine in the bass where the corrector has
    /// authority, coarse up top where it does not. That inversion is the
    /// authority mechanism, not an oversight.
    Variable,
}

static BOOKSHELF: PathProfile = PathProfile {
    authority: AuthorityKind::RoomEnvelope,
    averaging: AveragingMode::Power,
    coupling: CouplingPath::Room,
    flatness_target_db: 3.0,
    gating: GatingMode::Fdw {
        post: 15.0,
        pre: 3.0,
    },
    positions_default: 9,
    positions_domain: 3..=15,
    reposition_noun: "move the mic ~30 cm",
    smoothing: SmoothingMode::Variable,
    // Excursion rises as 1/f² below box tuning and ported boxes unload
    // entirely: 20 Hz into a 50 Hz-tuned box is 6.2× at-tuning excursion.
    sweep_f_start_hz: 30.0,
};

static FLOORSTANDER: PathProfile = PathProfile {
    authority: AuthorityKind::RoomEnvelope,
    averaging: AveragingMode::Power,
    coupling: CouplingPath::Room,
    flatness_target_db: 3.0,
    gating: GatingMode::Fdw {
        post: 15.0,
        pre: 3.0,
    },
    positions_default: 9,
    positions_domain: 3..=15,
    reposition_noun: "move the mic ~30 cm",
    smoothing: SmoothingMode::Variable,
    sweep_f_start_hz: 20.0,
};

static IN_EAR: PathProfile = PathProfile {
    authority: AuthorityKind::CouplerEnvelope,
    averaging: AveragingMode::DbMean,
    coupling: CouplingPath::Coupler,
    flatness_target_db: 1.0,
    gating: GatingMode::None,
    positions_default: 5,
    positions_domain: 3..=10,
    reposition_noun: "reseat the tip",
    smoothing: SmoothingMode::Fixed(6),
    sweep_f_start_hz: 20.0,
};

static OVER_EAR: PathProfile = PathProfile {
    authority: AuthorityKind::CouplerEnvelope,
    averaging: AveragingMode::DbMean,
    coupling: CouplingPath::Coupler,
    flatness_target_db: 1.0,
    gating: GatingMode::None,
    positions_default: 5,
    positions_domain: 3..=10,
    reposition_noun: "reseat the headphone",
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
