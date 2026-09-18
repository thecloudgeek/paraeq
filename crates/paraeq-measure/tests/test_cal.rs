//! MS-9/MS-10/MS-11: the cal-load safety gate.
//!
//! Tier 3 (analytic/policy) per the four-tier convention: the margin constant,
//! the gain-match tolerance and the full-scale rule are product policy with no
//! oracle. What is pinned: every refusal edge of [`CalSummary::validate`]
//! (missing / unparseable / non-finite sensitivity, meaningless reference
//! gain, a cap the mic cannot witness — per class, at the exact boundary), the
//! full-scale identity `full_scale = 94 − sens_factor`, and MS-11's exact
//! guarantee: a gain read-back that disagrees with the cal's reference derates
//! the emitted level to ≤ solved − 6 dB.

use paraeq_dsp::targets::TransducerClass;
use paraeq_measure::{
    caps_for, margined_emit_dbfs, CalSensitivity, CalSummary, MeasurementDiagnostic as D,
    CAL_ERROR_MARGIN_DB, GAIN_MATCH_TOLERANCE,
};

/// A healthy synthetic UMIK-1-shaped cal: Sens Factor −18 dBFS at 94 dB SPL
/// (full scale 112 dB SPL — above every class cap), referenced at 100% gain.
fn healthy(class: TransducerClass) -> CalSummary {
    CalSummary::validate(
        class,
        "UMIK-1 #7001234 (umik-7001234.txt)".to_owned(),
        CalSensitivity::Parsed(-18.0),
        1.0,
    )
    .expect("healthy cal validates")
}

// ── MS-9: missing/unparseable sensitivity refuses at cal load ─────────────

/// The `Err` arm carries a `Refusal` and no summary: there is no value for an
/// uncapped-sweep fallback to be written against.
#[test]
fn missing_sensitivity_refuses_at_cal_load() {
    let refusal = CalSummary::validate(
        TransducerClass::OverEar,
        "no file".to_owned(),
        CalSensitivity::Missing,
        1.0,
    )
    .unwrap_err();
    assert_eq!(refusal.diagnostic(), D::SensitivityMissing);
    assert!(refusal.diagnostic().is_blocking());
}

#[test]
fn unparseable_sensitivity_refuses_at_cal_load() {
    let refusal = CalSummary::validate(
        TransducerClass::OverEar,
        "garbled.txt".to_owned(),
        CalSensitivity::Unparseable,
        1.0,
    )
    .unwrap_err();
    assert_eq!(refusal.diagnostic(), D::SensitivityUnparseable);
}

#[test]
fn non_finite_sensitivity_refuses_at_cal_load() {
    for bad in [f64::INFINITY, f64::NAN, f64::NEG_INFINITY] {
        let refusal = CalSummary::validate(
            TransducerClass::InEar,
            "nan.txt".to_owned(),
            CalSensitivity::Parsed(bad),
            1.0,
        )
        .unwrap_err();
        assert_eq!(refusal.diagnostic(), D::SensitivityUnparseable);
    }
}

/// A gain-referenced sensitivity with a meaningless reference gain did not
/// meaningfully parse: NaN, ±∞ and out-of-range scalars all refuse, so no
/// downstream gain comparison can be built on garbage.
#[test]
fn meaningless_reference_gain_refuses_at_cal_load() {
    for bad_gain in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
        let refusal = CalSummary::validate(
            TransducerClass::OverEar,
            "badgain.txt".to_owned(),
            CalSensitivity::Parsed(-18.0),
            bad_gain,
        )
        .unwrap_err();
        assert_eq!(
            refusal.diagnostic(),
            D::SensitivityUnparseable,
            "reference gain {bad_gain} must refuse"
        );
    }
    // The endpoints of the scalar range are legal.
    for legal in [0.0, 1.0] {
        assert!(CalSummary::validate(
            TransducerClass::OverEar,
            "ok.txt".to_owned(),
            CalSensitivity::Parsed(-18.0),
            legal,
        )
        .is_ok());
    }
}

// ── MS-10: the cap must be witnessable by the mic ─────────────────────────

/// The full-scale identity: the Sens Factor is dBFS at 94 dB SPL, so 0 dBFS
/// corresponds to `94 − sens_factor` dB SPL. That is the loudest sound the
/// mic can register — REW's rule that an SPL limit above the input's clipping
/// point offers no protection is enforced against this number at cal load.
#[test]
fn mic_full_scale_spl_is_derived_from_the_sens_factor() {
    let cal = healthy(TransducerClass::OverEar);
    assert_eq!(cal.mic_full_scale_spl_db(), 94.0 - -18.0);
}

/// MS-10's required synthetic low-full-scale test: a mic whose full scale
/// sits below the class cap refuses `CapExceedsMicFullScale` at cal load —
/// the cap could never fire.
#[test]
fn a_cap_the_mic_cannot_witness_refuses_at_cal_load() {
    // Sens Factor 0 dBFS ⇒ full scale 94 dB SPL, below the 100 dB coupler cap.
    let refusal = CalSummary::validate(
        TransducerClass::InEar,
        "low-full-scale.txt".to_owned(),
        CalSensitivity::Parsed(0.0),
        1.0,
    )
    .unwrap_err();
    assert_eq!(refusal.diagnostic(), D::CapExceedsMicFullScale);
    assert!(refusal.diagnostic().is_blocking());
}

/// The boundary, per class: full scale exactly at the cap is the quietest mic
/// that can still witness the cap (accept); 0.1 dB below refuses.
#[test]
fn the_full_scale_boundary_is_exact_per_class() {
    for class in [
        TransducerClass::Bookshelf,
        TransducerClass::Floorstander,
        TransducerClass::InEar,
        TransducerClass::OverEar,
    ] {
        let cap = caps_for(class).spl_refuse_db;
        // full_scale == cap  ⇔  sens_factor == 94 − cap.
        let at_cap = 94.0 - cap;
        assert!(
            CalSummary::validate(
                class,
                "at-cap.txt".to_owned(),
                CalSensitivity::Parsed(at_cap),
                1.0,
            )
            .is_ok(),
            "{class:?}: a mic with full scale exactly at the cap must pass"
        );
        let refusal = CalSummary::validate(
            class,
            "under-cap.txt".to_owned(),
            CalSensitivity::Parsed(at_cap + 0.1),
            1.0,
        )
        .unwrap_err();
        assert_eq!(
            refusal.diagnostic(),
            D::CapExceedsMicFullScale,
            "{class:?}: full scale 0.1 dB under the cap must refuse"
        );
    }
}

// ── MS-11: gain pinned, read back, sensitivity treated as gain-referenced ─

/// The MS-11 unit test, exactly as the requirement words it: when the gain
/// read-back disagrees with the cal's reference gain, the emitted level is
/// ≤ solved − 6 dB.
#[test]
fn a_gain_readback_mismatch_derates_the_emitted_level_by_the_margin() {
    let cal = healthy(TransducerClass::OverEar);
    let solved = -14.0;
    // REW runs USB mics at unity; a real and common mismatch is a user gain
    // at 75%.
    let pin = cal.pin_gain(0.75);
    assert!(!pin.matches());
    let emitted = margined_emit_dbfs(solved, pin);
    assert!(
        emitted <= solved - 6.0,
        "emitted {emitted} dBFS must be ≤ solved − 6 dB on a gain mismatch"
    );
    assert_eq!(emitted, solved - CAL_ERROR_MARGIN_DB);
}

/// A read-back that agrees with the reference emits the solved level
/// unchanged — the margin is against *cal error evidence*, not a blanket pad
/// (the class caps and quiet targets already carry the blanket headroom).
#[test]
fn a_matching_readback_emits_the_solved_level() {
    let cal = healthy(TransducerClass::OverEar);
    let pin = cal.pin_gain(1.0);
    assert!(pin.matches());
    assert_eq!(margined_emit_dbfs(-14.0, pin), -14.0);
    // Provenance survives for the session log.
    assert_eq!(pin.reference(), 1.0);
    assert_eq!(pin.read_back(), 1.0);
}

/// The match tolerance binds in both directions: within `GAIN_MATCH_TOLERANCE`
/// of the reference is the same gain; beyond it is a mismatch and derates.
/// (Probed at half and double the tolerance, not at the exact edge — the edge
/// itself sits below f64 representability of `1.0 − 0.01` and pins nothing.)
#[test]
fn the_gain_match_tolerance_binds() {
    let cal = healthy(TransducerClass::OverEar);
    assert!(cal.pin_gain(1.0 - GAIN_MATCH_TOLERANCE / 2.0).matches());
    assert!(!cal.pin_gain(1.0 - 2.0 * GAIN_MATCH_TOLERANCE).matches());
}

/// A read-back that could not be formed is a mismatch, never a match: NaN
/// passes no comparison, so the test is written to land NaN in the derate
/// arm — the same NaN-blindness posture as `SweepLevel::new`.
#[test]
fn an_unreadable_gain_readback_derates() {
    let cal = healthy(TransducerClass::OverEar);
    let pin = cal.pin_gain(f64::NAN);
    assert!(!pin.matches());
    assert_eq!(margined_emit_dbfs(-20.0, pin), -20.0 - CAL_ERROR_MARGIN_DB);
}

/// The summary carries what MS-23's log needs verbatim: identity, class,
/// sensitivity and reference gain.
#[test]
fn the_summary_carries_its_provenance() {
    let cal = healthy(TransducerClass::InEar);
    assert_eq!(cal.class(), TransducerClass::InEar);
    assert_eq!(cal.file_identity(), "UMIK-1 #7001234 (umik-7001234.txt)");
    assert_eq!(cal.sensitivity().sens_factor_dbfs(), -18.0);
    assert_eq!(cal.reference_input_gain(), 1.0);
}
