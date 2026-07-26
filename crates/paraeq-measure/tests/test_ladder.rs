//! Tier 3 (analytic/policy) for the level ladder: MS-7's step order, MS-8's
//! SNR table and remedy sequence, MS-13's length cap, MS-17's envelope.
//!
//! Every number here comes from `docs/specs/2026-07-15-measurement-safety-
//! design.md` § The Level Ladder, § SNR criterion, § Targets and caps, and
//! § Hard Caps and Refusals. Nothing is a synthetic convenience.

use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::targets::TransducerClass;
use paraeq_measure::ladder::{
    snr_band_hz, LadderError, LevelLadder, NoiseFloor, Remedy, SnrOutcome, MAX_REMEDIES,
    MAX_RUNG_STEP_DB, NOISE_FLOOR_MAX_DBFS, RUNG_PROJECTION_TOLERANCE_DB, SNR_MEDIAN_ACCEPT_DB,
    SNR_MEDIAN_WARN_DB, SNR_MIN_ACCEPT_DB,
};
use paraeq_measure::stimulus::PILOT_LEVEL_DBFS_RMS;
use paraeq_measure::{caps_for, CalSensitivity, CalSummary, MeasurementDiagnostic, SweepLevel};

const CLASSES: [TransducerClass; 4] = [
    TransducerClass::Bookshelf,
    TransducerClass::Floorstander,
    TransducerClass::InEar,
    TransducerClass::OverEar,
];

/// A UMIK-1-shaped Sens Factor: -18 dBFS at 94 dB SPL, so full scale is
/// 112 dB SPL — above every class cap, which is what MS-10 requires.
const SENS_FACTOR_DBFS: f64 = -18.0;
const REFERENCE_GAIN: f64 = 1.0;

fn cal(class: TransducerClass) -> CalSummary {
    CalSummary::validate(
        class,
        "test-cal".to_owned(),
        CalSensitivity::Parsed(SENS_FACTOR_DBFS),
        REFERENCE_GAIN,
    )
    .expect("a -18 dBFS sens factor clears MS-9 and MS-10 for every class")
}

/// A ladder with the noise floor already accepted, at the cal's reference gain
/// (so no MS-11 derate applies unless a test asks for one).
fn ready(class: TransducerClass) -> LevelLadder {
    let mut ladder = LevelLadder::new(&cal(class), REFERENCE_GAIN, 5.0);
    ladder
        .accept_noise_floor(&floor_at(-70.0, 2.0))
        .expect("-70 dBFS over 2 s clears the gate");
    ladder
}

/// The pilot dBFS reading that produces a chain sensitivity of exactly
/// `sensitivity` dB SPL per dBFS RMS.
///
/// Inverts the spec's own conversions: `S = 94 + (dBFS - Sens) - L_pilot`.
fn pilot_reading_for(sensitivity: f64) -> f64 {
    sensitivity + PILOT_LEVEL_DBFS_RMS - 94.0 + SENS_FACTOR_DBFS
}

/// A synthetic noise floor at a known broadband level and duration. Built
/// through `NoiseFloor::measure` on real samples so the gate under test is the
/// shipping one, not a hand-filled struct.
fn floor_at(dbfs: f64, duration_s: f64) -> NoiseFloor {
    let rate = 48_000u32;
    let n = (duration_s * f64::from(rate)) as usize;
    let amplitude = 10f64.powf(dbfs / 20.0);
    // A deterministic full-band-ish signal at a known RMS: alternating +/-a is
    // Nyquist, whose RMS is exactly `a`.
    let samples: Vec<f64> = (0..n)
        .map(|i| if i % 2 == 0 { amplitude } else { -amplitude })
        .collect();
    NoiseFloor::measure(&samples, rate, &LogGrid::standard()).expect("valid capture")
}

fn refusal_of(err: LadderError) -> MeasurementDiagnostic {
    match err {
        LadderError::Refused(r) => r.diagnostic(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// ───────────────────────────── step 1: the floor ─────────────────────────────

#[test]
fn a_quiet_room_of_at_least_a_second_passes_the_floor_gate() {
    let mut ladder = LevelLadder::new(&cal(TransducerClass::OverEar), REFERENCE_GAIN, 5.0);
    assert!(ladder.accept_noise_floor(&floor_at(-70.0, 1.0)).is_ok());
}

#[test]
fn a_noisy_room_refuses_at_the_floor_gate() {
    let mut ladder = LevelLadder::new(&cal(TransducerClass::OverEar), REFERENCE_GAIN, 5.0);
    let noisy = floor_at(NOISE_FLOOR_MAX_DBFS + 3.0, 2.0);
    let refusal = ladder
        .accept_noise_floor(&noisy)
        .expect_err("above -60 dBFS must refuse");
    assert_eq!(
        refusal.diagnostic(),
        MeasurementDiagnostic::SnrUnachievable,
        "and the only remedies are the room and the input — never output level"
    );
}

#[test]
fn a_short_silence_capture_refuses() {
    let mut ladder = LevelLadder::new(&cal(TransducerClass::OverEar), REFERENCE_GAIN, 5.0);
    assert!(ladder.accept_noise_floor(&floor_at(-80.0, 0.5)).is_err());
}

#[test]
fn nothing_solves_before_the_floor_is_accepted() {
    let mut ladder = LevelLadder::new(&cal(TransducerClass::OverEar), REFERENCE_GAIN, 5.0);
    assert!(matches!(
        ladder.solve(pilot_reading_for(104.0)),
        Err(LadderError::NoFloor)
    ));
}

#[test]
fn the_floor_gate_measures_the_level_it_claims_to() {
    // The gate is only as good as the analysis behind it, so pin that too.
    let floor = floor_at(-70.0, 1.0);
    assert!(
        (floor.broadband_dbfs() - (-70.0)).abs() < 1e-9,
        "got {}",
        floor.broadband_dbfs()
    );
    assert!((floor.duration_s() - 1.0).abs() < 1e-9);
    assert_eq!(floor.spectrum_db().len(), LogGrid::standard().len());
}

// ──────────────────────── steps 2-4: solve and envelope ──────────────────────

#[test]
fn the_solve_is_the_specs_closed_form() {
    let class = TransducerClass::OverEar;
    let mut ladder = ready(class);
    let sensitivity = 104.0;
    let outcome = ladder
        .solve(pilot_reading_for(sensitivity))
        .expect("in-envelope");
    let caps = caps_for(class);
    assert!(
        (outcome.chain_sensitivity_spl_per_dbfs - sensitivity).abs() < 1e-9,
        "S = SPL_measured - L_pilot"
    );
    assert!(
        (outcome.solved_dbfs_rms - (caps.spl_target_db - sensitivity)).abs() < 1e-9,
        "L = SPL_target - S"
    );
    // 84 - 104 = -20 dBFS, exactly the class cap: the coupler target is
    // reachable at the cap on a nominal chain, which is the table's whole
    // internal consistency claim.
    assert!((outcome.solved_dbfs_rms - (-20.0)).abs() < 1e-9);
    assert!(
        (outcome.projected_spl_db - caps.spl_target_db).abs() < 1e-9,
        "with no MS-11 derate the projection IS the target"
    );
}

#[test]
fn the_projection_is_of_the_level_that_will_actually_be_emitted() {
    // MS-11: a gain read-back that disagrees with the cal derates the emitted
    // level by 6 dB. The acknowledged SPL must be the SPL that will really
    // happen, not the pre-margin one — MS-18 asks the user to confirm a
    // number, so it has to be the true number.
    let class = TransducerClass::OverEar;
    let mut ladder = LevelLadder::new(&cal(class), REFERENCE_GAIN - 0.5, 5.0);
    ladder.accept_noise_floor(&floor_at(-70.0, 2.0)).unwrap();
    let outcome = ladder.solve(pilot_reading_for(104.0)).expect("in-envelope");
    assert!(
        (outcome.solved_dbfs_rms - (-20.0)).abs() < 1e-9,
        "pre-margin"
    );
    assert!(
        (outcome.projected_spl_db - (caps_for(class).spl_target_db - 6.0)).abs() < 1e-9,
        "projection must carry the derate, got {}",
        outcome.projected_spl_db
    );
}

#[test]
fn an_empty_jig_refuses_at_the_envelope_rather_than_escalating() {
    // MS-17, and the reason it exists: escalating into an empty jig is
    // precisely how the 122 dB scenario happens, because the user then puts
    // the IEM in.
    let mut ladder = ready(TransducerClass::InEar);
    let err = ladder
        .solve(pilot_reading_for(40.0))
        .expect_err("an empty jig is far outside the envelope");
    assert_eq!(
        refusal_of(err),
        MeasurementDiagnostic::SensitivityOutOfEnvelope
    );
}

#[test]
fn an_implausibly_hot_chain_also_refuses() {
    let mut ladder = ready(TransducerClass::OverEar);
    let err = ladder.solve(pilot_reading_for(150.0)).expect_err("too hot");
    assert_eq!(
        refusal_of(err),
        MeasurementDiagnostic::SensitivityOutOfEnvelope
    );
}

#[test]
fn a_non_finite_pilot_reading_refuses_rather_than_passing_every_comparison() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut ladder = ready(TransducerClass::OverEar);
        let err = ladder.solve(bad).expect_err("must refuse");
        assert_eq!(
            refusal_of(err),
            MeasurementDiagnostic::SensitivityOutOfEnvelope,
            "reading {bad} must land in the refusal arm"
        );
    }
}

#[test]
fn the_envelope_admits_every_chain_that_can_reach_target() {
    // The one end of the OPEN [NEEDS DATA] envelope that is NOT a guess: the
    // envelope must contain the least sensitive chain the caps table still
    // calls legal, or the two tables contradict each other.
    for class in CLASSES {
        let caps = caps_for(class);
        let s_min = caps.spl_target_db - caps.sweep_level_dbfs_rms;
        assert!(
            caps.sensitivity_envelope_spl_per_dbfs.contains(&s_min),
            "{class:?}: envelope {:?} excludes {s_min} dB SPL/dBFS, the chain \
             that just reaches {} dB at the {} dBFS cap",
            caps.sensitivity_envelope_spl_per_dbfs,
            caps.spl_target_db,
            caps.sweep_level_dbfs_rms
        );
        // ...and a chain at exactly that sensitivity really does solve to the
        // cap, i.e. `SweepLevel::new` accepts it.
        let mut ladder = ready(class);
        let outcome = ladder.solve(pilot_reading_for(s_min)).expect("in-envelope");
        assert!(
            SweepLevel::new(outcome.solved_dbfs_rms, class).is_ok(),
            "{class:?}: the marginal chain must produce a legal level"
        );
    }
}

// ─────────────────────────── step 5: the rungs ───────────────────────────────

#[test]
fn no_rung_step_exceeds_six_db_and_the_last_rung_is_the_emitted_level() {
    for class in CLASSES {
        let mut ladder = ready(class);
        let caps = caps_for(class);
        let outcome = ladder
            .solve(pilot_reading_for(
                caps.spl_target_db - caps.sweep_level_dbfs_rms,
            ))
            .expect("in-envelope");
        let rungs = ladder.rungs().expect("solved");
        assert!(
            !rungs.is_empty(),
            "{class:?}: the climb from -40 dBFS is real"
        );
        let mut previous = PILOT_LEVEL_DBFS_RMS;
        for rung in &rungs {
            let step = rung.level_dbfs_rms - previous;
            assert!(
                step > 0.0 && step <= MAX_RUNG_STEP_DB + 1e-9,
                "{class:?}: step {step} dB is not a legal rung"
            );
            previous = rung.level_dbfs_rms;
        }
        assert!(
            (rungs.last().unwrap().level_dbfs_rms - outcome.solved_dbfs_rms).abs() < 1e-9,
            "{class:?}: the final rung must be the level the sweep will play at"
        );
        let indices: Vec<u32> = rungs.iter().map(|r| r.index).collect();
        assert_eq!(indices, (0..rungs.len() as u32).collect::<Vec<_>>());
    }
}

#[test]
fn a_chain_already_quiet_enough_needs_no_rungs() {
    // A very sensitive chain solves below the -40 dBFS pilot: there is nothing
    // to climb, so the plan is empty rather than descending.
    let mut ladder = ready(TransducerClass::OverEar);
    ladder.solve(pilot_reading_for(128.0)).expect("in-envelope");
    assert!(ladder.rungs().expect("solved").is_empty());
}

#[test]
fn a_rung_over_the_cap_aborts_the_climb() {
    let class = TransducerClass::OverEar;
    let mut ladder = ready(class);
    ladder.solve(pilot_reading_for(104.0)).expect("in-envelope");
    let over = caps_for(class).spl_refuse_db + 1.0;
    let err = ladder.check_rung(0, over).expect_err("over cap");
    assert_eq!(refusal_of(err), MeasurementDiagnostic::RungOverCap);
}

#[test]
fn a_rung_that_does_not_track_its_projection_aborts_the_climb() {
    let class = TransducerClass::OverEar;
    let mut ladder = ready(class);
    ladder.solve(pilot_reading_for(104.0)).expect("in-envelope");
    let rung = ladder.rungs().expect("solved")[0];
    // Inside the tolerance: fine.
    assert!(ladder
        .check_rung(
            0,
            rung.projected_spl_db - RUNG_PROJECTION_TOLERANCE_DB + 0.1
        )
        .is_ok());
    // Outside it: the signature of a wrong device, a stale cal, or a broken
    // chain.
    let err = ladder
        .check_rung(
            0,
            rung.projected_spl_db - RUNG_PROJECTION_TOLERANCE_DB - 0.1,
        )
        .expect_err("mismatch");
    assert_eq!(
        refusal_of(err),
        MeasurementDiagnostic::SplProjectionMismatch
    );
}

#[test]
fn a_non_finite_rung_measurement_refuses_rather_than_passing_the_cap() {
    let mut ladder = ready(TransducerClass::OverEar);
    ladder.solve(pilot_reading_for(104.0)).expect("in-envelope");
    let err = ladder.check_rung(0, f64::NAN).expect_err("NaN must refuse");
    assert_eq!(
        refusal_of(err),
        MeasurementDiagnostic::SplProjectionMismatch
    );
}

#[test]
fn rungs_and_checks_refuse_before_a_solve_exists() {
    let ladder = ready(TransducerClass::OverEar);
    assert!(matches!(ladder.rungs(), Err(LadderError::NoSolve)));
    assert!(matches!(
        ladder.check_rung(0, 80.0),
        Err(LadderError::NoSolve)
    ));
}

#[test]
fn an_out_of_plan_rung_index_is_an_error_not_a_pass() {
    let mut ladder = ready(TransducerClass::OverEar);
    ladder.solve(pilot_reading_for(104.0)).expect("in-envelope");
    let planned = ladder.rungs().expect("solved").len();
    assert!(matches!(
        ladder.check_rung(planned as u32, 80.0),
        Err(LadderError::NoSuchRung { .. })
    ));
}

// ─────────────────────────────── MS-8: the SNR gate ──────────────────────────

/// A sweep magnitude that sits exactly `snr` dB above `floor` in every bin,
/// except that `dip_bins` bins sit `dip` dB above it instead.
fn sweep_at_snr(floor: &NoiseFloor, snr: f64, dip: f64, dip_bins: usize) -> Vec<f64> {
    floor
        .spectrum_db()
        .iter()
        .enumerate()
        .map(|(i, n)| n + if i < dip_bins { dip } else { snr })
        .collect()
}

#[test]
fn a_clean_measurement_accepts() {
    let class = TransducerClass::OverEar;
    let mut ladder = ready(class);
    let floor = floor_at(-80.0, 2.0);
    let sweep = sweep_at_snr(&floor, SNR_MEDIAN_ACCEPT_DB + 5.0, 0.0, 0);
    match ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class)) {
        Ok(SnrOutcome::Accept { median_db, .. }) => {
            assert!(median_db >= SNR_MEDIAN_ACCEPT_DB)
        }
        other => panic!("expected Accept, got {other:?}"),
    }
    assert_eq!(ladder.remedies_used(), 0);
}

#[test]
fn a_good_median_with_one_bad_band_accepts_with_a_warning() {
    // The table's second row exists for exactly this: median clears 40 but a
    // band is under 20, so it is usable and the user is told.
    let class = TransducerClass::OverEar;
    let mut ladder = ready(class);
    let floor = floor_at(-80.0, 2.0);
    let sweep = sweep_at_snr(
        &floor,
        SNR_MEDIAN_ACCEPT_DB + 5.0,
        SNR_MIN_ACCEPT_DB - 5.0,
        20,
    );
    match ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class)) {
        Ok(SnrOutcome::AcceptWithWarning { diagnostic, .. }) => {
            assert_eq!(diagnostic, MeasurementDiagnostic::LowSnr)
        }
        other => panic!("expected AcceptWithWarning, got {other:?}"),
    }
}

#[test]
fn a_marginal_median_accepts_with_low_snr() {
    let class = TransducerClass::OverEar;
    let mut ladder = ready(class);
    let floor = floor_at(-80.0, 2.0);
    let sweep = sweep_at_snr(&floor, SNR_MEDIAN_WARN_DB + 1.0, 0.0, 0);
    assert!(matches!(
        ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class)),
        Ok(SnrOutcome::AcceptWithWarning { .. })
    ));
}

#[test]
fn the_remedy_sequence_is_input_gain_then_length_then_refuse() {
    // MS-8, in full: the order, the budget, and the refusal at the end of it.
    let class = TransducerClass::OverEar;
    let mut ladder = LevelLadder::new(&cal(class), 0.5, 2.0);
    ladder.accept_noise_floor(&floor_at(-80.0, 2.0)).unwrap();
    let floor = floor_at(-80.0, 2.0);
    let sweep = sweep_at_snr(&floor, 10.0, 0.0, 0);
    let band = snr_band_hz(class);

    match ladder.evaluate_snr(&sweep, &floor, band) {
        Ok(SnrOutcome::Remedy {
            remedy: Remedy::RaiseInputGain { from, to },
            ..
        }) => {
            assert!((from - 0.5).abs() < 1e-9);
            assert!(to > from && to <= 1.0);
        }
        other => panic!("first remedy must be input gain, got {other:?}"),
    }
    match ladder.evaluate_snr(&sweep, &floor, band) {
        Ok(SnrOutcome::Remedy {
            remedy: Remedy::ExtendSweep { from_s, to_s },
            ..
        }) => {
            assert!((from_s - 2.0).abs() < 1e-9);
            assert!((to_s - 4.0).abs() < 1e-9, "doubling buys ~3 dB");
        }
        other => panic!("second remedy must be length, got {other:?}"),
    }
    assert_eq!(ladder.remedies_used(), MAX_REMEDIES);
    match ladder.evaluate_snr(&sweep, &floor, band) {
        Ok(SnrOutcome::Refuse { refusal, .. }) => {
            assert_eq!(refusal.diagnostic(), MeasurementDiagnostic::SnrUnachievable)
        }
        other => panic!("the budget is spent; expected Refuse, got {other:?}"),
    }
}

#[test]
fn remedies_never_touch_the_output_level() {
    // REW's imperative, as a test: "If input levels are low DO NOT KEEP MAKING
    // THE TEST SIGNAL LOUDER." A remedy may invalidate the solve — raising the
    // input gain does, see below — but it may never RAISE the emitted level.
    let class = TransducerClass::OverEar;
    let mut ladder = LevelLadder::new(&cal(class), 0.5, 2.0);
    ladder.accept_noise_floor(&floor_at(-80.0, 2.0)).unwrap();
    ladder.solve(pilot_reading_for(104.0)).expect("in-envelope");
    let before = ladder.solved_level_dbfs_rms().expect("solved");
    let floor = floor_at(-80.0, 2.0);
    let sweep = sweep_at_snr(&floor, 10.0, 0.0, 0);
    for _ in 0..MAX_REMEDIES + 1 {
        let _ = ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class));
        if let Some(after) = ladder.solved_level_dbfs_rms() {
            assert!(
                after <= before,
                "a remedy raised the output level: {before} -> {after}"
            );
        }
        // Re-solving on the same chain must not drift upward either.
        if ladder.solved_level_dbfs_rms().is_none() {
            let again = ladder.solve(pilot_reading_for(104.0)).expect("re-solve");
            assert!(again.solved_dbfs_rms <= before + 1e-9);
        }
    }
}

#[test]
fn raising_the_input_gain_invalidates_the_solve_and_re_pins() {
    // Moving the input gain does not change the acoustic output, but it does
    // change what the MIC reads — so the measured sensitivity, the projected
    // SPL, every rung's projection and the MS-11 pin all now describe a chain
    // that no longer exists. An SPL computed through a stale Sens Factor is a
    // plausible number, not an obviously broken one, so nothing downstream can
    // notice on its own: the ladder must drop the solve itself.
    let class = TransducerClass::OverEar;
    let mut ladder = LevelLadder::new(&cal(class), 0.5, 2.0);
    ladder.accept_noise_floor(&floor_at(-80.0, 2.0)).unwrap();
    ladder.solve(pilot_reading_for(104.0)).expect("in-envelope");
    assert!(ladder.solved_level_dbfs_rms().is_some());

    let floor = floor_at(-80.0, 2.0);
    let sweep = sweep_at_snr(&floor, 10.0, 0.0, 0);
    match ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class)) {
        Ok(SnrOutcome::Remedy {
            remedy: Remedy::RaiseInputGain { .. },
            ..
        }) => {}
        other => panic!("expected a gain remedy, got {other:?}"),
    }
    assert!(
        ladder.solved_level_dbfs_rms().is_none(),
        "the solve must be dropped, not silently reused"
    );
    assert!(
        ladder.rungs().is_err(),
        "and the rung plan with it — every projection was against the old \
         sensitivity"
    );

    // The gain read-back the device actually settled on re-pins MS-11: the cal
    // was captured at 1.0, the chain is now elsewhere, so the derate applies.
    ladder.confirm_input_gain(0.6);
    let re_solved = ladder.solve(pilot_reading_for(104.0)).expect("re-solve");
    let caps = caps_for(class);
    assert!(
        (re_solved.projected_spl_db - (caps.spl_target_db - 6.0)).abs() < 1e-9,
        "a gain that disagrees with the cal reference must carry the 6 dB \
         MS-11 derate into the projection, got {}",
        re_solved.projected_spl_db
    );
}

#[test]
fn an_snr_remedy_cannot_push_length_past_the_class_cap() {
    // MS-13, verbatim. Start at the cap so the length remedy has nowhere to go.
    for class in CLASSES {
        let cap = caps_for(class).max_sweep_len_s;
        let mut ladder = LevelLadder::new(&cal(class), 1.0, cap);
        ladder.accept_noise_floor(&floor_at(-80.0, 2.0)).unwrap();
        let floor = floor_at(-80.0, 2.0);
        let sweep = sweep_at_snr(&floor, 10.0, 0.0, 0);
        // Input gain is already 1.0 and the length is already at the cap, so
        // neither remedy can move and the run refuses immediately.
        match ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class)) {
            Ok(SnrOutcome::Refuse { .. }) => {}
            other => panic!("{class:?}: expected an immediate refusal, got {other:?}"),
        }
        assert!(ladder.sweep_len_s() <= cap);

        // And from just under the cap, one doubling clamps to it rather than
        // overshooting.
        let mut ladder = LevelLadder::new(&cal(class), 1.0, cap - 1.0);
        ladder.accept_noise_floor(&floor_at(-80.0, 2.0)).unwrap();
        let _ = ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class));
        assert!(
            ladder.sweep_len_s() <= cap,
            "{class:?}: length {} exceeded the {cap} s cap",
            ladder.sweep_len_s()
        );
    }
}

#[test]
fn an_unavailable_remedy_does_not_consume_an_attempt() {
    // Gain already at 1.0: the length remedy must still get its turn, rather
    // than the budget being spent on an option that could not move.
    let class = TransducerClass::OverEar;
    let mut ladder = LevelLadder::new(&cal(class), 1.0, 2.0);
    ladder.accept_noise_floor(&floor_at(-80.0, 2.0)).unwrap();
    let floor = floor_at(-80.0, 2.0);
    let sweep = sweep_at_snr(&floor, 10.0, 0.0, 0);
    assert!(matches!(
        ladder.evaluate_snr(&sweep, &floor, snr_band_hz(class)),
        Ok(SnrOutcome::Remedy {
            remedy: Remedy::ExtendSweep { .. },
            ..
        })
    ));
    assert_eq!(ladder.remedies_used(), 1);
}

#[test]
fn the_sweep_length_is_capped_on_construction() {
    for class in CLASSES {
        let cap = caps_for(class).max_sweep_len_s;
        let ladder = LevelLadder::new(&cal(class), 1.0, cap + 10.0);
        assert!((ladder.sweep_len_s() - cap).abs() < 1e-9);
        // A nonsense request falls back to the cap rather than propagating.
        let ladder = LevelLadder::new(&cal(class), 1.0, f64::NAN);
        assert!((ladder.sweep_len_s() - cap).abs() < 1e-9);
    }
}

#[test]
fn evaluate_snr_refuses_malformed_input() {
    let class = TransducerClass::OverEar;
    let mut ladder = ready(class);
    let floor = floor_at(-80.0, 2.0);
    assert!(ladder
        .evaluate_snr(&[1.0, 2.0], &floor, snr_band_hz(class))
        .is_err());
    let sweep = sweep_at_snr(&floor, 50.0, 0.0, 0);
    assert!(
        ladder
            .evaluate_snr(&sweep, &floor, (30_000.0, 40_000.0))
            .is_err(),
        "a band that selects no bins is an error, not an empty accept"
    );
    let mut nan = sweep.clone();
    nan[10] = f64::NAN;
    assert!(ladder
        .evaluate_snr(&nan, &floor, snr_band_hz(class))
        .is_err());
}

#[test]
fn the_snr_band_starts_at_the_classs_sweep_start_where_one_exists() {
    for class in CLASSES {
        let (lo, hi) = snr_band_hz(class);
        assert_eq!(hi, 20_000.0);
        match caps_for(class).f_start_hz {
            Some(f) => assert_eq!(lo, f, "{class:?} tracks its driver-excursion limit"),
            None => assert_eq!(lo, 20.0, "{class:?} f_start is per-DUT; SNR uses 20 Hz"),
        }
    }
}

// ───────────────── the SNR gate's common scale (review regression) ───────────

/// Deterministic pseudo-noise at a known amplitude, `n` samples long.
fn noise(n: usize, amp: f64) -> Vec<f64> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    (0..n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            amp * ((state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0)
        })
        .collect()
}

#[test]
fn the_floor_spectrum_is_capture_length_invariant() {
    // SNR is a DIFFERENCE of two levels, so it is meaningless unless both were
    // measured on one scale — and a raw FFT magnitude scales with capture
    // length. Measured before `analyze_magnitude_db` normalized by sqrt(N):
    // the same acoustic floor read 6.6 dB lower over 1 s than over 5 s
    // (tracking 10*log10(5) = 7.0), so a 1 s floor against a 5 s sweep
    // OVERSTATED SNR by that much — the direction that promotes a
    // should-have-warned measurement into a clean accept.
    let grid = LogGrid::standard();
    let one = NoiseFloor::measure(&noise(48_000, 0.001), 48_000, &grid).expect("1 s");
    let five = NoiseFloor::measure(&noise(240_000, 0.001), 48_000, &grid).expect("5 s");

    let deltas: Vec<f64> = one
        .spectrum_db()
        .iter()
        .zip(five.spectrum_db())
        .map(|(a, b)| b - a)
        .collect();
    let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
    assert!(
        mean.abs() < 1.0,
        "the floor spectrum is not length-invariant: {mean:.3} dB of shift for \
         the same acoustic floor"
    );
    assert!(
        (one.broadband_dbfs() - five.broadband_dbfs()).abs() < 0.1,
        "the broadband figure was always length-invariant; it must stay so"
    );
}

#[test]
fn the_floor_estimate_sees_tonal_noise() {
    // Real room floors are dominated by TONAL components — mains hum and its
    // harmonics, fan whine — and they are exactly what the `min >= 20 dB`
    // criterion exists to catch. `logf`'s AntiComb prefilter averages whatever
    // values it is handed, so feeding it dB takes a geometric mean of
    // amplitudes and buries peaks: measured, a 60 Hz hum read **13.4 dB low**
    // that way. An understated floor OVERSTATES SNR, so the analysis feeds the
    // resampler power instead, making the neighbourhood average an energy
    // average.
    let rate = 48_000u32;
    let n = 96_000usize;
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let samples: Vec<f64> = (0..n)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let broadband = 0.0002 * ((state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0);
            let hum =
                0.002 * (2.0 * std::f64::consts::PI * 60.0 * i as f64 / f64::from(rate)).sin();
            broadband + hum
        })
        .collect();
    let grid = LogGrid::standard();
    let spectrum =
        paraeq_measure::ladder::analyze_magnitude_db(&samples, rate, &grid).expect("valid capture");
    let at = |f: f64| {
        let i = grid
            .freqs()
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs()))
            .map(|(i, _)| i)
            .expect("bin");
        spectrum[i]
    };
    assert!(
        at(60.0) - at(2000.0) > 10.0,
        "the floor estimate must SEE the hum: only {:.2} dB above the clean \
         band at 2 kHz",
        at(60.0) - at(2000.0)
    );
}
