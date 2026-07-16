//! PathProfile is the spec's "four transducer types as data" claim, so these
//! tests pin the data table itself — every field of every instance — and the
//! claim the argument rests on: the two room profiles differ in exactly one
//! capture-safety parameter, not in any analysis parameter.

use paraeq_decide::{
    profile_for, AuthorityKind, AveragingMode, CouplingPath, GatingMode, SmoothingMode,
    TransducerClass,
};

#[test]
fn over_ear_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::OverEar);
    assert_eq!(p.authority, AuthorityKind::CouplerEnvelope);
    assert_eq!(p.averaging, AveragingMode::DbMean);
    assert_eq!(p.coupling, CouplingPath::Coupler);
    assert_eq!(p.flatness_target_db, 1.0);
    assert_eq!(p.gating, GatingMode::None);
    assert_eq!(p.positions_default, 5);
    assert_eq!(p.positions_domain, 3..=10);
    assert_eq!(p.reposition_noun, "reseat the headphone");
    assert_eq!(p.smoothing, SmoothingMode::Fixed(6));
    assert_eq!(p.sweep_f_start_hz, 20.0);
}

#[test]
fn in_ear_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::InEar);
    assert_eq!(p.authority, AuthorityKind::CouplerEnvelope);
    assert_eq!(p.averaging, AveragingMode::DbMean);
    assert_eq!(p.coupling, CouplingPath::Coupler);
    assert_eq!(p.flatness_target_db, 1.0);
    assert_eq!(p.gating, GatingMode::None);
    assert_eq!(p.positions_default, 5);
    assert_eq!(p.positions_domain, 3..=10);
    assert_eq!(p.reposition_noun, "reseat the tip");
    assert_eq!(p.smoothing, SmoothingMode::Fixed(6));
    assert_eq!(p.sweep_f_start_hz, 20.0);
}

#[test]
fn bookshelf_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::Bookshelf);
    assert_eq!(p.authority, AuthorityKind::RoomEnvelope);
    assert_eq!(p.averaging, AveragingMode::Power);
    assert_eq!(p.coupling, CouplingPath::Room);
    assert_eq!(p.flatness_target_db, 3.0);
    assert_eq!(
        p.gating,
        GatingMode::Fdw {
            post: 15.0,
            pre: 3.0
        }
    );
    assert_eq!(p.positions_default, 9);
    assert_eq!(p.positions_domain, 3..=15);
    assert_eq!(p.reposition_noun, "move the mic ~30 cm");
    assert_eq!(p.smoothing, SmoothingMode::Variable);
    // The excursion prior: a 50 Hz-tuned bookshelf must never be swept to 20.
    assert_eq!(p.sweep_f_start_hz, 30.0);
}

#[test]
fn floorstander_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::Floorstander);
    assert_eq!(p.authority, AuthorityKind::RoomEnvelope);
    assert_eq!(p.averaging, AveragingMode::Power);
    assert_eq!(p.coupling, CouplingPath::Room);
    assert_eq!(p.flatness_target_db, 3.0);
    assert_eq!(
        p.gating,
        GatingMode::Fdw {
            post: 15.0,
            pre: 3.0
        }
    );
    assert_eq!(p.positions_default, 9);
    assert_eq!(p.positions_domain, 3..=15);
    assert_eq!(p.reposition_noun, "move the mic ~30 cm");
    assert_eq!(p.smoothing, SmoothingMode::Variable);
    assert_eq!(p.sweep_f_start_hz, 20.0);
}

/// The load-bearing test. "Bookshelf vs floorstander is not an analysis
/// branch" is the whole reason `low_corner_hz` is derived rather than
/// declared; the moment a second field diverges here, that claim is false and
/// the four-types-as-data argument has quietly become four code branches.
#[test]
fn the_two_room_profiles_differ_only_in_sweep_f_start_hz() {
    let bookshelf = profile_for(TransducerClass::Bookshelf);
    let floorstander = profile_for(TransducerClass::Floorstander);
    assert_ne!(bookshelf.sweep_f_start_hz, floorstander.sweep_f_start_hz);

    let mut normalized = bookshelf.clone();
    normalized.sweep_f_start_hz = floorstander.sweep_f_start_hz;
    assert_eq!(&normalized, floorstander);
}

/// The other half of the same claim: the coupler profiles differ only in the
/// guided-mode noun, because a tip and an earcup are the same measurement.
#[test]
fn the_two_coupler_profiles_differ_only_in_reposition_noun() {
    let in_ear = profile_for(TransducerClass::InEar);
    let over_ear = profile_for(TransducerClass::OverEar);
    assert_ne!(in_ear.reposition_noun, over_ear.reposition_noun);

    let mut normalized = in_ear.clone();
    normalized.reposition_noun = over_ear.reposition_noun;
    assert_eq!(&normalized, over_ear);
}

#[test]
fn coupling_splits_the_four_classes_two_and_two() {
    for class in [TransducerClass::InEar, TransducerClass::OverEar] {
        assert_eq!(profile_for(class).coupling, CouplingPath::Coupler);
    }
    for class in [TransducerClass::Bookshelf, TransducerClass::Floorstander] {
        assert_eq!(profile_for(class).coupling, CouplingPath::Room);
    }
}

/// `profile_for` returns a `&'static` from a table, so two calls must be the
/// same object — a profile is not something a caller can be handed a copy of
/// and tune.
#[test]
fn profile_for_returns_a_stable_static() {
    assert!(std::ptr::eq(
        profile_for(TransducerClass::InEar),
        profile_for(TransducerClass::InEar)
    ));
}
