//! MS-2: the cap is the type, and the table is data.
//!
//! Tier 3 (analytic) per the four-tier convention: output-level policy is not
//! DSP and has no oracle — the prototype's `SWEEP_AMPLITUDE = 0.5` is a single
//! constant in a PyQt worker, and porting it would launder a guess about an
//! unknown chain into a fixture. What is pinned instead is the safety table
//! field-by-field and every refusal edge of the sole constructor.

use paraeq_dsp::targets::TransducerClass;
use paraeq_measure::{caps_for, LevelError, SweepLevel, ABSOLUTE_MAX_DBFS_RMS};

const ALL_CLASSES: [TransducerClass; 4] = [
    TransducerClass::Bookshelf,
    TransducerClass::Floorstander,
    TransducerClass::InEar,
    TransducerClass::OverEar,
];

/// MS-2's named test: "every class's cap rejects cap+0.1 dB".
#[test]
fn every_class_cap_rejects_cap_plus_a_tenth_of_a_db() {
    for class in ALL_CLASSES {
        let cap = caps_for(class).sweep_level_dbfs_rms;
        assert_eq!(
            SweepLevel::new(cap + 0.1, class),
            Err(LevelError::OverClassCap {
                cap_dbfs_rms: cap,
                class,
                dbfs_rms: cap + 0.1,
            }),
            "{class:?} accepted a level 0.1 dB over its cap"
        );
    }
}

#[test]
fn every_class_accepts_its_cap_exactly_and_anything_quieter() {
    for class in ALL_CLASSES {
        let cap = caps_for(class).sweep_level_dbfs_rms;
        assert_eq!(SweepLevel::new(cap, class).unwrap().dbfs_rms(), cap);
        assert_eq!(
            SweepLevel::new(cap - 0.1, class).unwrap().dbfs_rms(),
            cap - 0.1
        );
        // The pilot: 300 Hz at -40 dBFS RMS, deliberately far below any cap
        // because it is the probe that discovers an unknown chain.
        assert!(SweepLevel::new(-40.0, class).is_ok());
    }
}

/// The unconditional ceiling is checked before the table, so it is reachable
/// (and this test is what proves it): were the table consulted first, every
/// level above -3 dBFS would already be over its class cap and the invariant
/// would be untestable dead code.
#[test]
fn the_absolute_maximum_binds_before_the_table_does() {
    for class in ALL_CLASSES {
        // -3.01 dBFS RMS is the unscaled `generate_sweep` output and REW's
        // absolute maximum; -2.9 is past it.
        assert_eq!(
            SweepLevel::new(-2.9, class),
            Err(LevelError::OverAbsoluteMax { dbfs_rms: -2.9 })
        );
        assert_eq!(
            SweepLevel::new(0.0, class),
            Err(LevelError::OverAbsoluteMax { dbfs_rms: 0.0 })
        );
    }
}

#[test]
fn no_class_cap_exceeds_the_absolute_maximum() {
    for class in ALL_CLASSES {
        assert!(
            caps_for(class).sweep_level_dbfs_rms <= ABSOLUTE_MAX_DBFS_RMS,
            "{class:?}'s cap is above the absolute maximum, which would make \
             the cap unreachable through `new`"
        );
    }
}

/// § Non-Finite. A NaN level is not a numerics nit: `NaN > cap` is false, so a
/// NaN would pass both ceiling comparisons and reach a sink as an unbounded
/// level. This is the same NaN-blindness the spec records at
/// `shared.rs::process_block` (`if a > peak`), guarded here the way
/// `targets.rs:128-134` guards the CSV parser.
#[test]
fn non_finite_levels_are_refused_rather_than_slipping_past_the_comparisons() {
    for class in ALL_CLASSES {
        for bad in [f64::INFINITY, f64::NAN, f64::NEG_INFINITY] {
            match SweepLevel::new(bad, class) {
                Err(LevelError::NotFinite { dbfs_rms }) => {
                    assert_eq!(dbfs_rms.is_nan(), bad.is_nan());
                }
                other => panic!("{class:?} at {bad} gave {other:?}, want NotFinite"),
            }
        }
    }
}

/// The safety table, pinned value-by-value against
/// measurement-safety-design.md § "Targets and caps, per transducer class".
/// A diff here is a policy change and must be argued for in the PR.
#[test]
fn the_caps_table_matches_the_spec_row_for_row() {
    for class in [TransducerClass::InEar, TransducerClass::OverEar] {
        let c = caps_for(class);
        assert_eq!(c.sweep_level_dbfs_rms, -20.0, "{class:?} sweep level");
        assert_eq!(c.spl_target_db, 84.0, "{class:?} SPL target");
        assert_eq!(c.spl_warn_db, 85.0, "{class:?} warn");
        assert_eq!(c.spl_refuse_db, 100.0, "{class:?} hard refuse");
        assert_eq!(c.f_start_hz, None, "{class:?} f_start is per-DUT");
        assert_eq!(c.max_sweep_len_s, 5.5, "{class:?} max sweep length");
    }
    for class in [TransducerClass::Bookshelf, TransducerClass::Floorstander] {
        let c = caps_for(class);
        assert_eq!(c.sweep_level_dbfs_rms, -12.0, "{class:?} sweep level");
        assert_eq!(c.spl_target_db, 75.0, "{class:?} SPL target");
        assert_eq!(c.spl_warn_db, 85.0, "{class:?} warn");
        assert_eq!(c.spl_refuse_db, 90.0, "{class:?} hard refuse");
        assert_eq!(c.max_sweep_len_s, 5.5, "{class:?} max sweep length");
    }
    assert_eq!(caps_for(TransducerClass::Bookshelf).f_start_hz, Some(30.0));
    assert_eq!(
        caps_for(TransducerClass::Floorstander).f_start_hz,
        Some(20.0)
    );
}

/// The claims the table's shape makes, asserted rather than left to a reader:
/// coupler paths refuse 10 dB below room paths, and the 84 dB coupler target
/// sits just under the 85 dB warn line so that at target the warning never
/// fires and any solve that lands hot is immediately visible.
#[test]
fn the_tables_structural_claims_hold() {
    for class in ALL_CLASSES {
        let c = caps_for(class);
        assert!(
            c.spl_target_db < c.spl_warn_db,
            "{class:?} would warn at its own target"
        );
        assert!(
            c.spl_warn_db <= c.spl_refuse_db,
            "{class:?} refuses below its warn line"
        );
        // § Two Clocks: never derived, never auto-extended.
        assert_eq!(c.max_sweep_len_s, 5.5, "{class:?} length cap");
    }
    assert_eq!(
        caps_for(TransducerClass::InEar).spl_refuse_db
            - caps_for(TransducerClass::Bookshelf).spl_refuse_db,
        10.0,
        "a room measurement happens in a space a person may be standing in"
    );
}

/// `f_start` is never extended downward (MS-12's table half; the "no code path
/// produces f_start < table value" half is Stage 5's). Pinned here because the
/// room numbers are also `PathProfile::sweep_f_start_hz` in `paraeq-decide`,
/// and the two tables must not drift apart silently.
#[test]
fn room_f_start_never_reaches_rews_half_start_convention() {
    for (class, table) in [
        (TransducerClass::Bookshelf, 30.0),
        (TransducerClass::Floorstander, 20.0),
    ] {
        let f_start = caps_for(class).f_start_hz.unwrap();
        assert_eq!(f_start, table);
        // REW would drive half the request: 15 Hz and 10 Hz, where a ported
        // box is fully unloaded (25x at-tuning excursion at 10 Hz).
        assert!(f_start > table / 2.0);
    }
}

#[test]
fn a_sweep_level_carries_the_level_it_was_constructed_with() {
    let level = SweepLevel::new(-20.0, TransducerClass::OverEar).unwrap();
    assert_eq!(level.dbfs_rms(), -20.0);
    // Copy, so passing one to a sink cannot move it out of a session's log.
    let copy = level;
    assert_eq!(copy.dbfs_rms(), level.dbfs_rms());
}

/// The room sweep start is tabulated in two crates: `TransducerCaps.f_start_hz`
/// here (a driver-excursion safety limit) and `PathProfile.sweep_f_start_hz` in
/// paraeq-decide. The crate DAG forbids a normal dependency between them, so
/// nothing links the two numbers at compile time and they can drift apart in
/// review. This is that link: a dev-dependency, invisible to
/// `cargo tree -e normal` and therefore to MS-1's dependency assertion.
///
/// If this test fails, do not "fix" it by editing one side — decide which
/// table owns the number (measurement-safety's, since it is a safety limit)
/// and correct the other.
#[test]
fn both_crates_tabulate_the_same_room_sweep_start() {
    for class in [TransducerClass::Bookshelf, TransducerClass::Floorstander] {
        assert_eq!(
            caps_for(class).f_start_hz,
            Some(paraeq_decide::profile_for(class).sweep_f_start_hz),
            "{class:?}: the two f_start tables have drifted apart"
        );
    }
}

/// MS-17's chain-sensitivity envelope is tabulated in two crates too:
/// `TransducerCaps.sensitivity_envelope_spl_per_dbfs` here and
/// `PathProfile.sensitivity_envelope_spl_per_dbfs` in paraeq-decide. Same
/// crate-DAG necessity as the sweep start above, same dev-dependency link, same
/// instruction on failure: **this** table owns the numbers, because a solved
/// sensitivity outside the envelope refuses before a sample is emitted — a
/// safety limit, not an analysis parameter. `decide()` carries a copy only so
/// the class cross-check can run without dragging the capture layer into a pure
/// crate.
///
/// The consequence of a drift is user-visible and one-directional: if the
/// decide-side copy is WIDER, `decide()` grades a chain the capture layer has
/// already refused; if it is NARROWER, `decide()` reports `WrongTransducer` on
/// hardware that measured perfectly well. Both read to the user as "ParaEQ
/// won't measure my headphones".
#[test]
fn both_crates_tabulate_the_same_sensitivity_envelope() {
    for class in [
        TransducerClass::Bookshelf,
        TransducerClass::Floorstander,
        TransducerClass::InEar,
        TransducerClass::OverEar,
    ] {
        assert_eq!(
            caps_for(class).sensitivity_envelope_spl_per_dbfs,
            paraeq_decide::profile_for(class).sensitivity_envelope_spl_per_dbfs,
            "{class:?}: the two MS-17 envelope tables have drifted apart"
        );
    }
}
