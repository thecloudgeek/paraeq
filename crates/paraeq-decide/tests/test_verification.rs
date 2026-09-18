//! The verification gate (B8): `residual_vs_prediction` over the authority
//! band, the exact level compensation `K`, the channel rule, and the two
//! pre-checks that refuse rather than produce a meaningless number.
//!
//! Every test goes through the public `decide()`, never through the module:
//! `verification` is public but the gate's whole point is that it fires inside
//! the one pure function, and a residual that was computed but did not reach
//! `DecisionSet::verdict` would be a refusal that does not refuse.
//!
//! **How the fixtures are built.** `common::verified_bundle` synthesizes the
//! pass a CORRECT engine would have produced: baseline positions are bare
//! deltas, and the verification IR is that same delta driven through the
//! REALIZED cascade at the running rate and scaled by `10^(2·preamp_db/20)` —
//! one factor for MS-19's re-levelling and one for the preamp the corrected path
//! applies. So the residual is zero by construction, and every test below moves
//! exactly one term and asserts what that costs. The oracle is the level book's
//! own algebra, not a second copy of the gate.
//!
//! **Why the tolerances are not 1e-12.** The curves are produced by the real
//! gate → FFT → derotate → log-resample pipeline, which renders `H(f)` through a
//! finite window and an anti-comb prefilter. Where a test moves only a CONSTANT
//! (a level, a preamp) the pipeline is exactly scale-equivariant and the
//! tolerance is a float-noise figure; where a test's term is SHAPED, the
//! residual carries the pipeline's own rendering error for that shape, and the
//! assertion is "far below the gate" rather than "zero". Both are stated at each
//! site.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**.

mod common;

use common::{
    assert_refusal_is_consistent, diagnostics_with, eq_at, has_code, only_diagnostic,
    verified_bundle, VerifiedSpec,
};
use paraeq_decide::{
    decide, CaptureRouting, DiagnosticCode, Evidence, EvidenceLabel, MeasurementBundle, Severity,
    TransducerClass, Unit, Verdict, VERIFICATION_RESIDUAL_MULTIPLE,
};
use paraeq_dsp::peq::{EQBand, FilterType};

/// Float noise on a constant-only manipulation. `sos_frequency_response_db`'s
/// `+1e-10` additive magnitude floor puts the true bound near 1e-8 dB, not at
/// zero, which is why this is a tolerance and not an equality.
const CONSTANT_TOLERANCE_DB: f64 = 1e-6;

/// What the pipeline's own rendering of a shaped `H(f)` costs, measured on the
/// shapes below. Two orders of magnitude under the coupler's 2.0 dB gate, so a
/// real accounting error cannot hide inside it.
const SHAPE_TOLERANCE_DB: f64 = 0.05;

/// One +6 dB boost — the level book's worked example, and the shape that makes
/// `preamp_db` nonzero so every preamp term in the accounting is load-bearing.
fn boost_6db() -> Vec<EQBand> {
    vec![EQBand {
        filter_type: FilterType::Peaking,
        fc: 120.0,
        gain_db: 6.0,
        q: 1.5,
    }]
}

/// A pure cut: `preamp_db` is exactly 0.0, so a test can separate "the preamp
/// term is wrong" from "the prediction shape is wrong".
fn cut_4db() -> Vec<EQBand> {
    vec![EQBand {
        filter_type: FilterType::Peaking,
        fc: 120.0,
        gain_db: -4.0,
        q: 1.5,
    }]
}

fn residual_rms(bundle: &MeasurementBundle) -> f64 {
    graded_residual(&decide(bundle))
}

/// The graded residual of a pass that HAS one.
///
/// Since ruling R-A12 `residual_rms_db` is `None` on every path that refuses
/// before a residual exists, so a test that wants the number says so and fails
/// loudly rather than reading a `0.0` that means "nothing was measured".
fn graded_residual(set: &paraeq_decide::DecisionSet) -> f64 {
    set.verification
        .as_ref()
        .expect("the bundle carries a verification")
        .residual_rms_db
        .expect("this pass computed a residual")
}

// ---------------------------------------------------------------------------
// The accounting book
// ---------------------------------------------------------------------------

/// The gate's reason to exist: an engine that installed what we designed leaves
/// nothing behind.
#[test]
fn residual_vs_prediction_is_zero_when_the_engine_did_what_we_designed() {
    let bundle = verified_bundle(&VerifiedSpec::default());
    let set = decide(&bundle);
    let report = set.verification.as_ref().expect("carried");
    let rms = report.residual_rms_db.expect("a graded pass");
    assert!(
        rms < SHAPE_TOLERANCE_DB,
        "residual {rms} dB on a correct engine"
    );
    assert_eq!(set.verdict, Verdict::Proceed);
    assert!(!has_code(&set, DiagnosticCode::VerificationResidual));
}

/// **T1.** The residual does not move when `preamp_db` does.
///
/// `preamp_db` appears three times in the accounting — inside `D`, inside the
/// measured `C`, and inside `K` — and the three cancel. A surviving `k·preamp_db`
/// term with any `k != 0` shows up here as a boost-proportional difference that
/// a single-case "is it zero" assertion could miss.
#[test]
fn residual_is_invariant_to_preamp_db() {
    let mut residuals = Vec::new();
    for boost_db in [0.0, 3.0, 6.0, 12.0] {
        let bands = vec![vec![EQBand {
            filter_type: FilterType::Peaking,
            fc: 120.0,
            gain_db: boost_db,
            q: 1.5,
        }]];
        let bundle = verified_bundle(&VerifiedSpec {
            bands,
            ..VerifiedSpec::default()
        });
        let armed = eq_at(
            &bundle
                .verification
                .as_ref()
                .unwrap()
                .installed
                .bands
                .as_slice()[0],
            48_000.0,
        )
        .preamp_db();
        assert!(
            (armed + boost_db.max(0.0)).abs() < 1e-9,
            "the fixture's armed preamp must be -max(0, peak): {armed} for {boost_db}"
        );
        let set = decide(&bundle);
        assert_eq!(set.verdict, Verdict::Proceed, "boost {boost_db} dB refused");
        residuals.push(graded_residual(&set));
    }
    for (i, residual) in residuals.iter().enumerate() {
        assert!(
            (residual - residuals[0]).abs() < SHAPE_TOLERANCE_DB,
            "residual {residual} at case {i} moved from {}",
            residuals[0]
        );
    }
}

/// The task the level book was written for: a +6 dB-boost correction with
/// `preamp_lin` actually applied verifies clean, at a level MS-19 put 6 dB below
/// the baseline and another 6 dB below that at the boost peak.
#[test]
fn a_six_db_boost_with_the_preamp_applied_leaves_no_residual() {
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![boost_6db()],
        ..VerifiedSpec::default()
    });
    let verification = bundle.verification.as_ref().expect("carried");
    assert!(
        (verification.installed_preamp_db + 6.0).abs() < 0.2,
        "the fixture must actually carry a -6 dB preamp, got {}",
        verification.installed_preamp_db
    );
    // MS-19: `L_verify = L_measure + preamp_db`, so `K = -preamp_db = +6 dB`.
    assert!(
        (bundle.capture.sweep.level_dbfs - verification.level_dbfs
            + verification.installed_preamp_db)
            .abs()
            < 1e-12
    );
    assert!(residual_rms(&bundle) < SHAPE_TOLERANCE_DB);
    assert_eq!(decide(&bundle).verdict, Verdict::Proceed);
}

/// **T3.** The falsifier for `D = H + preamp_db`, kept in-tree so nobody
/// simplifies the term away.
///
/// The bands-only reading leaves a CONSTANT residual of `preamp_db`. Here that
/// is −6 dB against the coupler's 2.0 dB gate — and it refuses the engine that
/// is behaving, while passing the engine that dropped the preamp. This test
/// computes the wrong prediction's residual the only way a black-box test can:
/// by handing the gate a pass whose measured curve is missing the preamp term,
/// which is arithmetically the same displacement.
#[test]
fn omitting_the_preamp_from_the_prediction_refuses_a_correct_engine() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![boost_6db()],
        ..VerifiedSpec::default()
    });
    let verification = bundle.verification.as_mut().expect("carried");
    let preamp = verification.installed_preamp_db;
    // Undo exactly one of the two factors: the chain did NOT apply `preamp_lin`.
    let undo = 10f64.powf(-preamp / 20.0);
    for channel in verification.ir.samples.iter_mut() {
        for sample in channel.iter_mut() {
            *sample *= undo;
        }
    }
    let set = decide(&bundle);
    let residual = graded_residual(&set);
    assert!(
        (residual - preamp.abs()).abs() < SHAPE_TOLERANCE_DB,
        "a dropped preamp must leave exactly |preamp_db| = {} dB, got {residual}",
        preamp.abs()
    );
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::VerificationResidual).severity,
        Severity::Refuse
    );
    assert_eq!(set.verdict, Verdict::Refuse);
    assert!(set.correction.is_none());
}

/// **T5 / R6.** `K` is read off the two carried levels and nothing else.
///
/// A bundle whose `capture.sweep.level_dbfs` disagrees with `−preamp_db` still
/// produces a zero residual for a correct engine, because `K` is a difference of
/// two measured levels rather than a re-derivation from the plan. An
/// implementation that "knew" `K = −preamp_db` would report the whole
/// disagreement as a residual.
#[test]
fn k_is_read_from_capture_sweep_level_not_from_a_re_derived_preamp() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![boost_6db()],
        ..VerifiedSpec::default()
    });
    // Both levels move together by the same 3 dB: the file was quieter than the
    // nominal rung on both passes, so `K` is unchanged and the residual is too.
    bundle.capture.sweep.level_dbfs -= 3.0;
    bundle.verification.as_mut().expect("carried").level_dbfs -= 3.0;
    assert!(residual_rms(&bundle) < SHAPE_TOLERANCE_DB);

    // Now move ONLY the verification level. `K` changes by the same 2 dB and the
    // residual must follow it exactly — a constant, so this one is float-tight.
    let mut skewed = bundle.clone();
    skewed.verification.as_mut().expect("carried").level_dbfs -= 2.0;
    let residual = residual_rms(&skewed);
    assert!(
        (residual - 2.0).abs() < SHAPE_TOLERANCE_DB,
        "a 2 dB level move must appear as a 2 dB residual, got {residual}"
    );
}

/// **R14.** Both terms of `K` are sweep-span RMS, and a whole-file RMS on one
/// side is worth `10·log10(5.5/7.7) = 1.46 dB` against a 2.0 dB gate.
///
/// The convention has no runtime witness — both numbers are `f64` dBFS either
/// way — so this test states the arithmetic that would follow from getting it
/// wrong, and pins that the gate has no compensation for it.
#[test]
fn both_level_terms_are_sweep_span_rms() {
    let bundle = verified_bundle(&VerifiedSpec::default());
    let whole_file_error_db = 10.0 * (5.5f64 / 7.7).log10();
    let mut wrong = bundle.clone();
    wrong.verification.as_mut().expect("carried").level_dbfs += whole_file_error_db;
    let residual = residual_rms(&wrong);
    assert!(
        (residual - whole_file_error_db.abs()).abs() < SHAPE_TOLERANCE_DB,
        "a whole-file RMS on the verification side must move the residual by \
         {} dB, got {residual}",
        whole_file_error_db.abs()
    );
    assert!(
        residual > 1.4 && residual < 1.5,
        "the 1.46 dB figure: {residual}"
    );
}

// ---------------------------------------------------------------------------
// The prediction
// ---------------------------------------------------------------------------

/// **R16 b3.** The prediction uses the engine's OWN armed preamp, carried on the
/// bundle, not the plan's number at `design_rate`.
///
/// A plan fitted at 44.1 kHz and run at 48 kHz has two legitimately different
/// preamps. Predicting with `installed.preamp_db` would manufacture the
/// difference as a residual.
#[test]
fn the_prediction_uses_the_carried_installed_preamp_not_the_plans() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![boost_6db()],
        design_rate: 44_100.0,
        running_rate_hz: 48_000.0,
        ..VerifiedSpec::default()
    });
    // The plan's own number, displaced by 1.5 dB. It is provenance — "never the
    // rate the engine designs at" — and a prediction that read it would report
    // the whole 1.5 dB as a residual. The engine's armed number is untouched, so
    // the preamp re-check still passes and only the prediction's SOURCE is under
    // test here.
    let verification = bundle.verification.as_mut().expect("carried");
    verification.installed.preamp_db = verification.installed_preamp_db - 1.5;
    assert_eq!(
        bundle.verification.as_ref().unwrap().installed.design_rate,
        44_100.0
    );
    let set = decide(&bundle);
    assert!(
        graded_residual(&set) < SHAPE_TOLERANCE_DB,
        "the prediction must use the carried preamp: {:?}",
        set.verification.as_ref().expect("carried").residual_rms_db
    );
    assert_eq!(set.verdict, Verdict::Proceed);
}

/// **R16 b3, the other half.** Two independent computations of one number, in
/// two crates — and when they disagree, `decide()` refuses rather than builds a
/// prediction on a preamp nothing confirmed.
///
/// This test names [`DiagnosticCode::VerificationPreampMismatch`] by hand: it is
/// the tripwire that does not compile if the variant is missing.
#[test]
fn a_carried_preamp_that_disagrees_with_its_own_bands_refuses() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![boost_6db()],
        ..VerifiedSpec::default()
    });
    bundle
        .verification
        .as_mut()
        .expect("carried")
        .installed_preamp_db -= 0.5;
    let set = decide(&bundle);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::VerificationPreampMismatch);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert_eq!(set.verdict, Verdict::Refuse);
    assert_refusal_is_consistent(&set);

    // And the tolerance is a float guard, not a modelling allowance: half a
    // micro-dB is still the same number.
    let mut inside = bundle.clone();
    inside
        .verification
        .as_mut()
        .expect("carried")
        .installed_preamp_db += 0.5 - 5e-7;
    assert!(!has_code(
        &decide(&inside),
        DiagnosticCode::VerificationPreampMismatch
    ));
}

/// **R6 / §5.3.** `H(f)` is the REALIZED cascade — rows failing the Jury
/// stability test evaluated as identity, exactly as the engine installs them —
/// not the designed one.
///
/// A `q = 0` row is unstable at any rate. The engine substitutes identity, so
/// the measured capture carries no trace of it, and a prediction built from
/// `frequency_response` would report a prediction fault as an engine fault.
#[test]
fn the_prediction_uses_the_realized_cascade_not_the_designed_one() {
    let mut bands = boost_6db();
    bands.push(EQBand {
        filter_type: FilterType::Peaking,
        fc: 1000.0,
        gain_db: 9.0,
        q: 0.0,
    });
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![bands],
        ..VerifiedSpec::default()
    });
    let set = decide(&bundle);
    assert!(
        graded_residual(&set) < SHAPE_TOLERANCE_DB,
        "an identity-substituted row must leave no residual"
    );
    assert_eq!(set.verdict, Verdict::Proceed);
    // And the substitution is visible rather than silent.
    let substituted = set
        .verification
        .as_ref()
        .expect("carried")
        .evidence
        .iter()
        .find_map(|e| match e {
            Evidence::Scalar {
                label: EvidenceLabel::SectionsSubstituted,
                value,
                ..
            } => Some(*value),
            _ => None,
        })
        .expect("SectionsSubstituted is always attached");
    assert_eq!(substituted, 1.0);
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// The threshold is `VERIFICATION_RESIDUAL_MULTIPLE · flatness_target_db`, per
/// class — 2.0 dB on both coupler paths and 6.0 dB on both room paths — and it
/// is read off the DECISION, so an override moves it.
///
/// Both sides of the boundary are asserted, because "greater than" is the whole
/// specification of a gate.
#[test]
fn the_gate_is_twice_the_flatness_target_per_class() {
    for (class, expected_gate) in [
        (TransducerClass::OverEar, 2.0),
        (TransducerClass::InEar, 2.0),
        (TransducerClass::Bookshelf, 6.0),
        (TransducerClass::Floorstander, 6.0),
    ] {
        let bundle = verified_bundle(&VerifiedSpec {
            bands: vec![cut_4db()],
            class,
            positions: paraeq_decide::profile_for(class).positions_default,
            ..VerifiedSpec::default()
        });
        let set = decide(&bundle);
        let gate = set.verification.as_ref().expect("carried").gate_db;
        assert_eq!(gate, expected_gate, "{class:?}");
        assert_eq!(
            gate,
            VERIFICATION_RESIDUAL_MULTIPLE * set.decisions.flatness_target_db.value
        );

        // A pure level offset is a flat residual of exactly that many dB, so the
        // boundary is testable to float precision.
        let under = offset_by(&bundle, gate - 1e-3);
        assert!(!has_code(
            &decide(&under),
            DiagnosticCode::VerificationResidual
        ));
        let over = offset_by(&bundle, gate + 1e-3);
        assert_eq!(
            only_diagnostic(&decide(&over), DiagnosticCode::VerificationResidual).severity,
            Severity::Refuse
        );
    }
}

/// P9: one source for the number. Overriding `flatness_target_db` moves the gate
/// with it, because the gate reads the decision rather than the profile.
#[test]
fn overriding_the_flatness_target_moves_the_gate() {
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db()],
        ..VerifiedSpec::default()
    });
    let baseline = decide(&bundle).verification.expect("carried").gate_db;
    assert_eq!(baseline, 2.0);

    let mut widened = offset_by(&bundle, 3.0);
    widened.overrides.flatness_target_db = Some(2.0);
    let set = decide(&widened);
    assert_eq!(set.verification.as_ref().expect("carried").gate_db, 4.0);
    assert!(!has_code(&set, DiagnosticCode::VerificationResidual));

    let mut tightened = offset_by(&bundle, 3.0);
    tightened.overrides.flatness_target_db = Some(1.0);
    assert!(has_code(
        &decide(&tightened),
        DiagnosticCode::VerificationResidual
    ));
}

/// The band is `correction_range ∩ { f : authority allows any correction }`, on
/// `Analysis::freqs_hz`. A displacement that lives entirely OUTSIDE it is not
/// graded — which is what stops the ultrasonic edge of a bass correction's
/// measurement deciding whether the bass correction shipped.
#[test]
fn residual_outside_the_authority_band_does_not_refuse() {
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db()],
        ..VerifiedSpec::default()
    });
    let range = decide(&bundle).decisions.correction_range.value;
    // A 12 dB step, an octave above the band the correction claims. Applied to
    // the CAPTURED verification IR through a high shelf, so it is a real feature
    // of a real curve rather than an edit to an asserted number.
    let mut outside = bundle.clone();
    let shelf = eq_at(
        &[EQBand {
            filter_type: FilterType::HighShelf,
            fc: (range.1 * 1.5).min(19_000.0),
            gain_db: 12.0,
            q: std::f64::consts::FRAC_1_SQRT_2,
        }],
        48_000.0,
    )
    .realized_sos(48_000.0);
    let verification = outside.verification.as_mut().expect("carried");
    for channel in verification.ir.samples.iter_mut() {
        *channel = paraeq_dsp::peq::sosfilt(&shelf, channel);
    }
    let set = decide(&outside);
    assert!(
        !has_code(&set, DiagnosticCode::VerificationResidual),
        "a displacement above the correction range must not be graded"
    );
    assert_eq!(set.verdict, Verdict::Proceed);
}

/// An EMPTY authority band refuses. It does not pass with a residual of zero.
///
/// Ruling R-A12. `masked_rms` answers `None` when the mask keeps no bin, the
/// per-channel loop `continue`d, `worst` stayed at its `0.0` initializer and the
/// gate compared `0.0 > gate_db` — false. So a verification that checked
/// NOTHING reported "0.0 dB RMS, within the 2.0 dB limit" and proceeded. The
/// number was not a measurement and the pass was not a pass.
///
/// A band with no authority in it means the correction claims nothing we can
/// check there, which is a reason to refuse to confirm — not a reason to
/// confirm.
#[test]
fn an_empty_authority_band_refuses_instead_of_passing_with_a_zero_residual() {
    let mut bundle = verified_bundle(&VerifiedSpec::default());
    // The coupler excursion envelope is zeroed at 10 kHz, so a correction range
    // sitting entirely above it carries no boost and no cut authority anywhere:
    // the mask keeps nothing. The value is inside `correction_range`'s own
    // domain, so § D-N's clamp leaves it alone.
    bundle.overrides.correction_range = Some((19_000.0, 20_000.0));

    let set = decide(&bundle);
    let refusal = only_diagnostic(&set, DiagnosticCode::VerificationResidual);
    assert_eq!(refusal.severity, Severity::Refuse);
    assert_eq!(refusal.position, Some(0));
    assert_eq!(set.verdict, Verdict::Refuse);

    let report = set.verification.as_ref().expect("carried");
    assert_eq!(
        report.residual_rms_db, None,
        "nothing was measured, so there is no number to report"
    );
}

/// A structural refusal carries no residual: `None`, not `0.0`.
///
/// Ruling R-A12. `VerificationReport::residual_rms_db` used to be an `f64` that
/// the two early-return paths filled with `0.0` — a value the module's own
/// comment called "not a measurement" — and the desktop then rendered
/// "Residual 0.0 dB RMS — within the 2.0 dB limit" beside a refusal. The type
/// now says what the comment said.
#[test]
fn a_structural_refusal_carries_no_residual_number() {
    let mut bundle = verified_bundle(&VerifiedSpec::default());
    // The routing fence: a per-ear routing against a `Both` baseline. No
    // prediction exists, so no residual does either.
    bundle.verification.as_mut().expect("carried").routing = CaptureRouting::Only(0);

    let set = decide(&bundle);
    assert!(has_code(&set, DiagnosticCode::VerificationRoutingMismatch));
    let report = set.verification.as_ref().expect("carried");
    assert_eq!(report.residual_rms_db, None);
    assert!(
        !report.evidence.iter().any(|e| matches!(
            e,
            Evidence::Scalar {
                label: EvidenceLabel::ResidualVsPrediction,
                ..
            }
        )),
        "no residual evidence either — the absence is the witness"
    );

    // And a pass that DOES compute one still reports it.
    let graded = decide(&verified_bundle(&VerifiedSpec::default()));
    assert!(graded
        .verification
        .as_ref()
        .expect("carried")
        .residual_rms_db
        .is_some());
}

/// `residual_vs_target` is computed, attached and **never** gated: "Report it,
/// plot it, never gate on it."
#[test]
fn residual_vs_target_is_reported_but_never_gates() {
    // A DEEP, WIDE cut, not the 4 dB one the other tests use: the point of the
    // second half below is that `residual_vs_target` sits past the gate and is
    // still not compared to it, and `flatness_target_db`'s domain floor is 0.5
    // (gate 1.0) — so the number that has to clear the gate is this one, and it
    // has to be made to.
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![vec![EQBand {
            filter_type: FilterType::Peaking,
            fc: 500.0,
            gain_db: -12.0,
            q: 0.5,
        }]],
        ..VerifiedSpec::default()
    });
    let set = decide(&bundle);
    let report = set.verification.as_ref().expect("carried");
    let vs_target = report
        .evidence
        .iter()
        .find_map(|e| match e {
            Evidence::Scalar {
                label: EvidenceLabel::ResidualVsTarget,
                unit,
                value,
            } => Some((*unit, *value)),
            _ => None,
        })
        .expect("attached");
    assert_eq!(vs_target.0, Unit::Db);
    // It is a DIFFERENT quantity from the gated one, and on this rig it is two
    // orders of magnitude larger — the capture is flat and the decided target is
    // not. The verdict is Proceed anyway. That is the whole point: it is
    // rig-dependent, and it is not the claim we make.
    assert!(vs_target.1.is_finite());
    assert!(
        vs_target.1 > 10.0 * report.residual_rms_db.expect("a graded pass"),
        "vs_target {} against the gated {:?}",
        vs_target.1,
        report.residual_rms_db
    );
    assert_eq!(set.verdict, Verdict::Proceed);
    assert!(!has_code(&set, DiagnosticCode::VerificationResidual));

    // And it does not gate even when it is well past the threshold: tightening
    // the flatness target moves the GATE and therefore the prediction residual's
    // verdict, never this one.
    //
    // 0.5 is the BOTTOM of `flatness_target_db`'s own domain, not an arbitrary
    // small number: since ruling R-A6 an out-of-domain override is clamped, so
    // asking for 0.2 would silently be answered with 0.5 anyway and the test
    // would be asserting against a number it did not choose.
    let mut tightened = bundle.clone();
    tightened.overrides.flatness_target_db = Some(0.5);
    let tight = decide(&tightened);
    let tight_gate = tight.verification.as_ref().expect("carried").gate_db;
    assert_eq!(tight_gate, 2.0 * 0.5, "the gate followed the override down");
    assert!(
        vs_target.1 > tight_gate,
        "vs_target {} against the tightened gate {tight_gate}",
        vs_target.1
    );
    assert!(!has_code(&tight, DiagnosticCode::VerificationResidual));
}

// ---------------------------------------------------------------------------
// The channel rule (R20)
// ---------------------------------------------------------------------------

/// The fence: a verification that played to both speakers cannot be differenced
/// against a per-ear baseline. Refuse, never a computed number.
#[test]
fn a_routing_mismatch_refuses_rather_than_differencing_a_sum() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db(), cut_4db()],
        routing: CaptureRouting::Only(0),
        ..VerifiedSpec::default()
    });
    bundle.verification.as_mut().expect("carried").routing = CaptureRouting::Both;
    let set = decide(&bundle);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::VerificationRoutingMismatch).severity,
        Severity::Refuse
    );
    assert_eq!(set.verdict, Verdict::Refuse);
    let report = set.verification.as_ref().expect("carried");
    assert!(
        !report.evidence.iter().any(|e| matches!(
            e,
            Evidence::Scalar {
                label: EvidenceLabel::ResidualVsPrediction,
                ..
            }
        )),
        "a refused routing must produce no residual at all"
    );
}

/// `Both` heard the SUM of two differently-EQ'd channels, which is the response
/// of neither. Refuse rather than model it.
#[test]
fn a_both_routing_over_divergent_per_channel_bands_refuses() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db(), cut_4db()],
        ..VerifiedSpec::default()
    });
    bundle
        .verification
        .as_mut()
        .expect("carried")
        .installed
        .bands = paraeq_dsp::PerChannel::new(vec![cut_4db(), boost_6db()]).expect("two channels");
    let set = decide(&bundle);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::VerificationRoutingMismatch).severity,
        Severity::Refuse
    );
}

/// The other half, so the refusal is not over-broad: a room run corrected as one
/// still verifies.
#[test]
fn a_both_routing_over_identical_per_channel_bands_predicts_normally() {
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db(), cut_4db()],
        ..VerifiedSpec::default()
    });
    let set = decide(&bundle);
    assert!(!has_code(&set, DiagnosticCode::VerificationRoutingMismatch));
    assert!(graded_residual(&set) < SHAPE_TOLERANCE_DB);
}

/// A routing naming a channel the plan never corrected cannot be predicted at
/// all.
#[test]
fn an_only_routing_naming_a_channel_the_plan_never_corrected_refuses() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db()],
        routing: CaptureRouting::Only(0),
        ..VerifiedSpec::default()
    });
    bundle.positions[0].routing = CaptureRouting::Only(1);
    let verification = bundle.verification.as_mut().expect("carried");
    verification.routing = CaptureRouting::Only(1);
    verification.position_index = 0;
    let set = decide(&bundle);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::VerificationRoutingMismatch).severity,
        Severity::Refuse
    );
}

/// Differencing captures of different width is not a residual.
#[test]
fn a_capture_channel_count_mismatch_between_baseline_and_verification_refuses() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db(), cut_4db()],
        ..VerifiedSpec::default()
    });
    bundle
        .verification
        .as_mut()
        .expect("carried")
        .ir
        .samples
        .truncate(1);
    let set = decide(&bundle);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::VerificationRoutingMismatch).severity,
        Severity::Refuse
    );
}

/// The gate is the WORST channel, never the mean. One clean ear and one 3 dB out
/// must refuse; a mean-over-channels implementation would report 1.5 dB against
/// a 2.0 dB gate and pass.
#[test]
fn the_residual_is_computed_per_capture_channel_and_the_gate_is_the_worst_one() {
    let mut bundle = verified_bundle(&VerifiedSpec {
        bands: vec![cut_4db(), cut_4db()],
        ..VerifiedSpec::default()
    });
    let displacement = 10f64.powf(3.0 / 20.0);
    for sample in bundle.verification.as_mut().expect("carried").ir.samples[1].iter_mut() {
        *sample *= displacement;
    }
    let set = decide(&bundle);
    let report = set.verification.as_ref().expect("carried");
    assert!(
        (report.residual_rms_db.expect("a graded pass") - 3.0).abs() < SHAPE_TOLERANCE_DB,
        "the worst channel is 3 dB out, got {:?}",
        report.residual_rms_db
    );
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::VerificationResidual).severity,
        Severity::Refuse
    );

    // The per-channel values are attached individually, so the drawer can say
    // WHICH ear failed — and the mean of them (2 dB) is under the gate, which is
    // the implementation this test exists to falsify.
    let per_channel: Vec<f64> = report
        .evidence
        .iter()
        .filter_map(|e| match e {
            Evidence::Scalar {
                label: EvidenceLabel::ResidualVsPrediction,
                value,
                ..
            } => Some(*value),
            _ => None,
        })
        .collect();
    assert_eq!(per_channel.len(), 2);
    assert!(per_channel[0] < SHAPE_TOLERANCE_DB, "{:?}", per_channel);
    let mean = (per_channel[0] + per_channel[1]) / 2.0;
    assert!(mean < report.gate_db, "the mean would have passed: {mean}");
}

// ---------------------------------------------------------------------------
// The pre-checks that are not about the residual's shape
// ---------------------------------------------------------------------------

/// **T5.** The two facts the pass carries so `decide()` can refuse rather than
/// trust: the user's trim was pinned, and the check sweep was not louder than
/// the measurement.
#[test]
fn decide_refuses_a_verification_that_did_not_carry_its_level() {
    let bundle = verified_bundle(&VerifiedSpec::default());

    // The trim displaces the capture by exactly its own value, so this is what a
    // real un-pinned pass looks like: a 10 dB residual with a known cause.
    let mut trimmed = offset_by(&bundle, 10.0);
    trimmed.verification.as_mut().expect("carried").gain_db = 10.0;
    let set = decide(&trimmed);
    // ONE row, not two: the residual is off by exactly the trim, so naming it a
    // residual failure as well would send the user to fix the correction.
    let diagnostic = only_diagnostic(&set, DiagnosticCode::VerificationResidual);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert!(diagnostic.remedy.contains("trim"), "{}", diagnostic.remedy);
    // And the residual is still attached as evidence, because "the level is off
    // by 10 dB" is what confirms the diagnosis.
    assert!((graded_residual(&set) - 10.0).abs() < SHAPE_TOLERANCE_DB);

    let mut louder = bundle.clone();
    louder.verification.as_mut().expect("carried").level_dbfs =
        bundle.capture.sweep.level_dbfs + 0.1;
    assert_eq!(
        only_diagnostic(&decide(&louder), DiagnosticCode::VerificationResidual).severity,
        Severity::Refuse
    );
}

/// Every verification remedy renders without a run of spaces.
///
/// Ruling R-A11: two of the level-book refusals lost their `\` line
/// continuations, so the string literal carried the source file's own 18-space
/// indentation into copy a user reads ("…still at +10.0 dB,                  so
/// we cannot…"). The check is over a run of spaces rather than over an exact
/// count, because the indentation depth is not the bug.
#[test]
fn every_verification_remedy_renders_without_a_run_of_spaces() {
    let base = verified_bundle(&VerifiedSpec::default());

    let mut trimmed = offset_by(&base, 10.0);
    trimmed.verification.as_mut().expect("carried").gain_db = 10.0;

    let mut louder = base.clone();
    louder.verification.as_mut().expect("carried").level_dbfs = base.capture.sweep.level_dbfs + 0.1;

    let mut railed = offset_by(&base, 9.0);
    railed
        .verification
        .as_mut()
        .expect("carried")
        .capture
        .peak_dbfs = -0.1;

    let mut mismatched = base.clone();
    mismatched.verification.as_mut().expect("carried").routing = CaptureRouting::Only(0);

    let mut off = offset_by(&base, 9.0);
    off.verification
        .as_mut()
        .expect("carried")
        .capture
        .peak_dbfs = -12.0;

    let mut seen = 0usize;
    for bundle in [&trimmed, &louder, &railed, &mismatched, &off] {
        let set = decide(bundle);
        assert!(!set.diagnostics.is_empty());
        for diagnostic in &set.diagnostics {
            seen += 1;
            assert!(!diagnostic.remedy.is_empty(), "{diagnostic:?}");
            assert!(
                !diagnostic.remedy.contains("  "),
                "a run of spaces from a missing line continuation in {diagnostic:?}"
            );
        }
    }
    assert!(seen >= 5, "only {seen} remedies were graded");
}

/// **R23 / MS-21.** A railed verification capture is a refusal about the
/// MICROPHONE, not about the correction. Blaming the correction for a clipped
/// ADC is exactly the silent failure this row exists to prevent.
#[test]
fn a_railed_verification_capture_refuses_as_clipping_not_as_a_residual() {
    // A capture that is BOTH railed and far off the prediction: without the
    // suppression this would earn a `VerificationResidual` beside the clipping
    // row and send the user to fix a correction that is fine.
    let mut bundle = offset_by(&verified_bundle(&VerifiedSpec::default()), 9.0);
    bundle
        .verification
        .as_mut()
        .expect("carried")
        .capture
        .peak_dbfs = -0.1;
    let set = decide(&bundle);
    let clipped = diagnostics_with(&set, DiagnosticCode::ClippingPosition);
    assert_eq!(clipped.len(), 1);
    assert_eq!(clipped[0].severity, Severity::Refuse);
    assert_eq!(clipped[0].position, Some(0));
    assert!(
        !has_code(&set, DiagnosticCode::VerificationResidual),
        "R23: report the clipping, NOT the residual"
    );
    assert!(graded_residual(&set) > 8.0);
    assert_eq!(set.verdict, Verdict::Refuse);
}

// ---------------------------------------------------------------------------
// The wiring
// ---------------------------------------------------------------------------

/// Without this the module compiles and is never invoked.
#[test]
fn the_gate_is_called_from_decide_and_populates_decision_set() {
    let bundle = verified_bundle(&VerifiedSpec::default());
    let set = decide(&bundle);
    let report = set
        .verification
        .expect("a verification bundle produces a report");
    assert!(report.gate_db > 0.0);
    for label in [
        EvidenceLabel::ResidualMean,
        EvidenceLabel::ResidualScatter,
        EvidenceLabel::ResidualVsPrediction,
        EvidenceLabel::ResidualVsTarget,
        EvidenceLabel::SectionsSubstituted,
        EvidenceLabel::TwoClockResidual,
        EvidenceLabel::TwoClockSkewPpm,
    ] {
        assert!(
            report.evidence.iter().any(|e| match e {
                Evidence::Scalar { label: l, .. } => *l == label,
                _ => false,
            }),
            "{label:?} is not attached"
        );
    }
}

/// `Option` is what keeps every golden case that carries no verification
/// byte-stable, and the gate must be invisible to them.
#[test]
fn the_verification_report_is_absent_when_the_bundle_carries_no_verification() {
    let mut bundle = verified_bundle(&VerifiedSpec::default());
    let with = decide(&bundle);
    bundle.verification = None;
    let without = decide(&bundle);
    assert!(without.verification.is_none());
    assert!(with.verification.is_some());
    // The gate ADDS rows and changes nothing else.
    assert_eq!(with.decisions, without.decisions);
    assert_eq!(with.analysis, without.analysis);
    assert_eq!(with.diagnostics, without.diagnostics);
}

/// A verification block only ever ADDS refusals: it cannot clear a row the
/// refusal table earned, and it cannot turn a Refuse into a Proceed.
#[test]
fn a_verification_block_only_ever_adds_refusals() {
    let mut bundle = verified_bundle(&VerifiedSpec::default());
    bundle.capture.self_excluded = false;
    let refused = decide(&bundle);
    assert!(has_code(&refused, DiagnosticCode::SelfExclusionUnavailable));
    assert_eq!(refused.verdict, Verdict::Refuse);

    let mut without = bundle.clone();
    without.verification = None;
    let baseline = decide(&without);
    assert!(
        baseline
            .diagnostics
            .iter()
            .all(|d| refused.diagnostics.contains(d)),
        "the table's rows survive the gate"
    );
    assert_refusal_is_consistent(&refused);
}

/// Determinism, with the heaviest input the gate takes.
#[test]
fn decide_is_idempotent_with_a_verification_present() {
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![boost_6db(), boost_6db()],
        ..VerifiedSpec::default()
    });
    assert_eq!(decide(&bundle), decide(&bundle));
}

/// **T6.** The residual is taken from the RAW position curves, never from the
/// aligned ones the analysis publishes.
///
/// Plan B8's falsifier, named. `curve_db` deliberately omits `align_spl`: the
/// verification capture is not a member of the position cohort, so aligning the
/// baseline to the ensemble mean would remove a level difference that the
/// verification side never had — and re-aligning the pair would absorb a real
/// `preamp_lin`-not-applied failure into the alignment, which is the one
/// failure this module exists to catch.
///
/// The falsifier: scale the BASELINE position the pass verifies by +3 dB and
/// leave the verification capture alone. `U` rises by 3 dB, so the residual
/// mean must be exactly −3 dB. An implementation that aligned the positions
/// first would spread that offset across the five-position cohort and leave a
/// mean near −0.6 dB, which is why the assertion is a two-sided window on 3.0
/// rather than "the mean moved".
#[test]
fn the_residual_uses_raw_position_curves_not_aligned_ones() {
    let bundle = verified_bundle(&VerifiedSpec::default());
    let verified_index = bundle
        .verification
        .as_ref()
        .expect("carried")
        .position_index;

    let mut offset = bundle.clone();
    let scale = 10f64.powf(3.0 / 20.0);
    for channel in offset.positions[verified_index].ir.samples.iter_mut() {
        for sample in channel.iter_mut() {
            *sample *= scale;
        }
    }

    let set = decide(&offset);
    let report = set.verification.as_ref().expect("carried");
    let mean = report
        .evidence
        .iter()
        .find_map(|e| match e {
            Evidence::Scalar {
                label: EvidenceLabel::ResidualMean,
                value,
                ..
            } => Some(*value),
            _ => None,
        })
        .expect("the residual mean is attached per capture channel");
    assert!(
        (mean + 3.0).abs() < SHAPE_TOLERANCE_DB,
        "a +3 dB baseline offset must reach the residual as −3 dB, got {mean};          a mean near −0.6 dB is the signature of align_spl having run over the          five-position cohort first"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The same bundle with the verification capture scaled by `db`, which displaces
/// the residual by exactly `db` at every bin — a constant, so the gate boundary
/// is testable to float precision.
fn offset_by(bundle: &MeasurementBundle, db: f64) -> MeasurementBundle {
    let mut out = bundle.clone();
    let scale = 10f64.powf(db / 20.0);
    for channel in out
        .verification
        .as_mut()
        .expect("carried")
        .ir
        .samples
        .iter_mut()
    {
        for sample in channel.iter_mut() {
            *sample *= scale;
        }
    }
    out
}

/// `CONSTANT_TOLERANCE_DB` has one user: the assertion that the two preamp
/// computations agree to a float guard rather than to a modelling allowance.
#[test]
fn the_preamp_tolerance_is_a_float_guard() {
    let bundle = verified_bundle(&VerifiedSpec {
        bands: vec![boost_6db()],
        ..VerifiedSpec::default()
    });
    let verification = bundle.verification.as_ref().expect("carried");
    let recomputed = eq_at(
        &verification.installed.bands.as_slice()[0],
        verification.running_rate_hz,
    )
    .preamp_db();
    assert!((verification.installed_preamp_db - recomputed).abs() < CONSTANT_TOLERANCE_DB);
}
