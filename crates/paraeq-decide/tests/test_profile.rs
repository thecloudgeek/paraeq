//! PathProfile is the spec's "four transducer types as data" claim, so these
//! tests pin the data table itself — every field of every instance — and the
//! claim the argument rests on: the two room profiles differ in exactly one
//! capture-safety parameter, not in any analysis parameter.

use paraeq_decide::{
    profile_for, AuthorityKind, AveragingMode, CouplingPath, GatingMode, SmoothingMode,
    TargetChoice, TransducerClass,
};
use paraeq_dsp::authority::{
    COUPLER_EXCURSION_DB, COUPLER_Q_CEILING, DEFAULT_BOOST_RATIO, DEFAULT_EXCURSION_DB,
    ROOM_Q_CEILING,
};
use paraeq_dsp::fr::Smoothing;
use paraeq_dsp::targets::RoomTargetSpec;

#[test]
fn over_ear_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::OverEar);
    assert_eq!(p.authority, AuthorityKind::CouplerEnvelope);
    assert_eq!(p.averaging, AveragingMode::DbMean);
    assert_eq!(p.boost_ratio, DEFAULT_BOOST_RATIO);
    assert_eq!(p.coupling, CouplingPath::Coupler);
    assert_eq!(p.excursion_breakpoints_db, COUPLER_EXCURSION_DB);
    assert_eq!(p.flatness_target_db, 1.0);
    assert_eq!(p.gating, GatingMode::None);
    assert_eq!(p.positions_default, 5);
    assert_eq!(p.positions_domain, 3..=10);
    assert_eq!(p.positions_floor, 5);
    assert_eq!(p.q_cap, COUPLER_Q_CEILING);
    assert_eq!(p.reposition_noun, "reseat the headphone");
    assert_eq!(p.sensitivity_envelope_spl_per_dbfs, 85.0..=130.0);
    assert_eq!(p.smoothing, SmoothingMode::Fixed(6));
    assert_eq!(p.sweep_f_start_hz, 20.0);
}

#[test]
fn in_ear_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::InEar);
    assert_eq!(p.authority, AuthorityKind::CouplerEnvelope);
    assert_eq!(p.averaging, AveragingMode::DbMean);
    assert_eq!(p.boost_ratio, DEFAULT_BOOST_RATIO);
    assert_eq!(p.coupling, CouplingPath::Coupler);
    assert_eq!(p.excursion_breakpoints_db, COUPLER_EXCURSION_DB);
    assert_eq!(p.flatness_target_db, 1.0);
    assert_eq!(p.gating, GatingMode::None);
    assert_eq!(p.positions_default, 5);
    assert_eq!(p.positions_domain, 3..=10);
    assert_eq!(p.positions_floor, 5);
    assert_eq!(p.q_cap, COUPLER_Q_CEILING);
    assert_eq!(p.reposition_noun, "reseat the tip");
    assert_eq!(p.sensitivity_envelope_spl_per_dbfs, 85.0..=130.0);
    assert_eq!(p.smoothing, SmoothingMode::Fixed(6));
    assert_eq!(p.sweep_f_start_hz, 20.0);
}

#[test]
fn bookshelf_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::Bookshelf);
    assert_eq!(p.authority, AuthorityKind::RoomEnvelope);
    assert_eq!(p.averaging, AveragingMode::Power);
    // D-E: the room auto path is cut-only by default.
    assert_eq!(p.boost_ratio, 0.0);
    assert_eq!(p.coupling, CouplingPath::Room);
    assert_eq!(p.excursion_breakpoints_db, DEFAULT_EXCURSION_DB);
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
    assert_eq!(p.positions_floor, 5);
    assert_eq!(p.q_cap, ROOM_Q_CEILING);
    assert_eq!(p.reposition_noun, "move the mic ~30 cm");
    assert_eq!(p.sensitivity_envelope_spl_per_dbfs, 65.0..=120.0);
    assert_eq!(p.smoothing, SmoothingMode::Variable);
    // The excursion prior: a 50 Hz-tuned bookshelf must never be swept to 20.
    assert_eq!(p.sweep_f_start_hz, 30.0);
}

#[test]
fn floorstander_profile_is_the_spec_table_column() {
    let p = profile_for(TransducerClass::Floorstander);
    assert_eq!(p.authority, AuthorityKind::RoomEnvelope);
    assert_eq!(p.averaging, AveragingMode::Power);
    // D-E: the room auto path is cut-only by default.
    assert_eq!(p.boost_ratio, 0.0);
    assert_eq!(p.coupling, CouplingPath::Room);
    assert_eq!(p.excursion_breakpoints_db, DEFAULT_EXCURSION_DB);
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
    assert_eq!(p.positions_floor, 5);
    assert_eq!(p.q_cap, ROOM_Q_CEILING);
    assert_eq!(p.reposition_noun, "move the mic ~30 cm");
    assert_eq!(p.sensitivity_envelope_spl_per_dbfs, 65.0..=120.0);
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

/// The room target default is READ from `paraeq-dsp`, not spelled here.
///
/// D-A: "`decide()`'s room `Parametric` default **reads
/// `RoomTargetSpec::default()`** rather than carrying a literal — which ships
/// **+4.0** today." Field for field, so an ears ruling on the shelf costs one
/// line in `targets.rs` and nothing in this crate.
///
/// If this fails, do not "fix" it by editing `TargetChoice::room_default` —
/// `RoomTargetSpec` owns the numbers.
#[test]
fn the_room_target_default_equals_room_target_spec_default() {
    let spec = RoomTargetSpec::default();
    let TargetChoice::Parametric {
        shelf_db,
        shelf_fc,
        shelf_q,
        tilt_db_per_oct,
    } = TargetChoice::room_default()
    else {
        panic!("the room default is the Parametric variant; rooms are never matched");
    };

    assert_eq!(shelf_db, spec.shelf_gain_db);
    assert_eq!(shelf_fc, spec.shelf_hz);
    assert_eq!(shelf_q, spec.shelf_q);
    assert_eq!(tilt_db_per_oct, spec.tilt_db_per_oct);

    // `pivot_hz` has no home on the wire shape the decision table names, so it
    // is pinned separately: the omission is deliberate and only safe while
    // `build_room_target` reads the pivot from the spec it is handed.
    assert_eq!(spec.pivot_hz, 1000.0);
}

/// The path Q ceilings are READ from `paraeq_dsp::authority`, which owns the
/// excursion envelope and composes the ceiling with REW's gain-dependent cap.
/// A literal here would let `decide()` re-specify numbers the decision-engine
/// spec says it must not.
#[test]
fn every_profile_reads_its_q_ceiling_from_paraeq_dsp() {
    for class in [TransducerClass::InEar, TransducerClass::OverEar] {
        assert_eq!(profile_for(class).q_cap, COUPLER_Q_CEILING);
    }
    for class in [TransducerClass::Bookshelf, TransducerClass::Floorstander] {
        assert_eq!(profile_for(class).q_cap, ROOM_Q_CEILING);
    }
    assert_ne!(
        COUPLER_Q_CEILING, ROOM_Q_CEILING,
        "the two paths must not collapse to one policy, or the split is a no-op"
    );
}

/// Same argument for the excursion breakpoints: the envelope is a physics
/// ceiling `paraeq-dsp` owns, and the profile points at it rather than copying
/// it.
#[test]
fn every_profile_reads_its_excursion_breakpoints_from_paraeq_dsp() {
    for class in [TransducerClass::InEar, TransducerClass::OverEar] {
        assert_eq!(
            profile_for(class).excursion_breakpoints_db,
            COUPLER_EXCURSION_DB
        );
    }
    for class in [TransducerClass::Bookshelf, TransducerClass::Floorstander] {
        assert_eq!(
            profile_for(class).excursion_breakpoints_db,
            DEFAULT_EXCURSION_DB
        );
    }
}

/// `SmoothingMode` and `paraeq_dsp::fr::Smoothing` are now one variant set seen
/// from two crates. Both `From` impls are exhaustive with no wildcard, so a
/// variant added on either side stops the crate compiling; this test is the
/// other half — it pins that the correspondence is the IDENTITY, not just
/// total. A mapping that quietly sent `Gaussian` to `Variable` would compile.
#[test]
fn every_smoothing_mode_maps_to_its_dsp_twin_and_back() {
    let modes = [
        SmoothingMode::Fixed(6),
        SmoothingMode::Gaussian {
            fraction: 1.0 / 6.0,
        },
        SmoothingMode::None,
        SmoothingMode::Variable,
    ];
    let twins = [
        Smoothing::Fixed(6),
        Smoothing::Gaussian {
            fraction: 1.0 / 6.0,
        },
        Smoothing::None,
        Smoothing::Variable,
    ];

    for (mode, twin) in modes.into_iter().zip(twins) {
        assert_eq!(Smoothing::from(mode), twin);
        assert_eq!(SmoothingMode::from(twin), mode);
    }
}

/// The merge's own point: `Gaussian` exists on this side now, so `decide()` can
/// name a mode the analysis pipeline can actually run. Before the merge the
/// wire shape could not express it at all.
#[test]
fn the_smoothing_wire_shape_can_express_gaussian() {
    let mode = SmoothingMode::Gaussian {
        fraction: 1.0 / 6.0,
    };
    let json = serde_json::to_string(&mode).expect("serializes");
    assert_eq!(
        serde_json::from_str::<SmoothingMode>(&json).expect("deserializes"),
        mode
    );
}

/// The profile table is where every path-scoped default lives, so the defaults
/// it owns must be internally consistent as well as present. The four column
/// tests above pin the VALUES; this pins the RELATIONSHIPS between them, which
/// is what a later edit to one number breaks silently.
#[test]
fn path_profile_seeds_every_default_it_owns() {
    for class in [
        TransducerClass::Bookshelf,
        TransducerClass::Floorstander,
        TransducerClass::InEar,
        TransducerClass::OverEar,
    ] {
        let p = profile_for(class);

        // Q7's ladder: hard-min <= floor <= default <= ceiling.
        assert!(
            p.positions_domain.start() <= &p.positions_floor,
            "{class:?}: the enforced floor is below the hard minimum"
        );
        assert!(
            p.positions_floor <= p.positions_default,
            "{class:?}: the default is below the floor it is supposed to clear"
        );
        assert!(
            &p.positions_default <= p.positions_domain.end(),
            "{class:?}: the default is above its own ceiling"
        );

        // A boost ratio is a fraction of the cut ceiling, so anything outside
        // 0..=1 is either cut-only-with-extra-steps or boosts above the cut
        // ceiling the excursion envelope set.
        assert!(
            (0.0..=1.0).contains(&p.boost_ratio),
            "{class:?}: boost_ratio {} is not a fraction",
            p.boost_ratio
        );

        // MS-17's envelope must be a non-empty range, or the class cross-check
        // refuses every chain.
        assert!(
            p.sensitivity_envelope_spl_per_dbfs.start() < p.sensitivity_envelope_spl_per_dbfs.end(),
            "{class:?}: the sensitivity envelope is empty or inverted"
        );

        // Breakpoints are log-f interpolated, so they must be strictly
        // increasing in frequency and non-empty.
        assert!(!p.excursion_breakpoints_db.is_empty());
        assert!(
            p.excursion_breakpoints_db
                .windows(2)
                .all(|w| w[0].0 < w[1].0),
            "{class:?}: excursion breakpoints are not strictly increasing in f"
        );

        assert!(p.flatness_target_db > 0.0);
        assert!(!p.reposition_noun.is_empty());
        assert!(p.sweep_f_start_hz > 0.0);
    }
}
