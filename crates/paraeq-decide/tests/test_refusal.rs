//! The refusal table (B7c): every row of § Refusal and Sanity Checks, each with
//! one bundle that trips it and one that does not.
//!
//! Every test goes through the public `decide()` rather than through
//! `refusal::diagnostics` — the module is private, and a refusal that fired but
//! did not reach `DecisionSet::verdict` would be a refusal that does not refuse.
//! The shaped bundles are built by driving a delta through a biquad cascade
//! (`tests/common/mod.rs`), so the curve a row is graded on is produced by the
//! real analysis pipeline and not asserted against itself.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**. The thresholds are
//! product policy, not DSP: the prototype has nothing to say about them.

mod common;

use common::{
    cal_with_curve, capture_stats, diagnostics_with, flat_bundle, has_code, only_diagnostic,
    shaped_positions, verification, well_formed_bundle,
};
use paraeq_decide::{
    decide, CalVariant, CaptureStats, DiagnosticCode, MeasurementBundle, Severity, TransducerClass,
    Verdict,
};
use paraeq_dsp::biquad;

/// RBJ's Butterworth shelf Q — the same 0.71 the room target's shelf uses.
/// Named rather than spelled, because a shaped test is only readable if the
/// number that is NOT the variable says so.
const SHELF_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// The coupler path's own default cohort, every position a bare delta: the
/// bundle every "and one that does not" half of a test starts from.
fn clean() -> MeasurementBundle {
    flat_bundle(TransducerClass::OverEar, 5)
}

/// A cal file sampled on 200 log-spaced points from 20 Hz to 20 kHz, whose
/// gains are `shape(f)`.
///
/// Compensation SUBTRACTS the cal, so the analysed curve of a bare delta is
/// `-shape(f)`: the cal is the sharpest tool for giving the averaged curve an
/// exactly known shape that is identical at every position, which is what the
/// two whole-curve rows are graded on. Dense enough that
/// `apply_compensation`'s linear-in-f interpolation between points is worth
/// less than 0.05 dB against the log-spaced shape.
fn shaped_cal(shape: impl Fn(f64) -> f64) -> paraeq_decide::CalFile {
    let freqs: Vec<f64> = (0..200)
        .map(|i| 20.0 * 1000f64.powf(i as f64 / 199.0))
        .collect();
    let gains = freqs.iter().map(|f| shape(*f)).collect();
    cal_with_curve(freqs, gains)
}

/// A `span_db` rise between 2 kHz and 5 kHz, flat on both sides, so the
/// analysed curve's maximum sits on a plateau where smoothing cannot shave it.
///
/// The rise starts ABOVE the midband on purpose. `low_corner_hz` is derived by
/// the -10 dB rule below 200 Hz (B7b's scan), so a span that began at 20 Hz
/// would move the corner up into its own ramp and shrink the band the row
/// measures; and the tilt half is graded over 200 Hz-2 kHz, which this shape
/// leaves flat. Each half of the row is therefore exercised on its own.
fn span_cal(span_db: f64) -> paraeq_decide::CalFile {
    shaped_cal(move |f| {
        -span_db * ((f / 2000.0).log10() / (5000f64 / 2000.0).log10()).clamp(0.0, 1.0)
    })
}

/// A pure `tilt` dB/decade ramp across 200 Hz–2 kHz, flat outside it.
fn tilt_cal(tilt_db_per_decade: f64) -> paraeq_decide::CalFile {
    shaped_cal(move |f| -tilt_db_per_decade * (f.clamp(200.0, 2000.0) / 200.0).log10())
}

// ---------------------------------------------------------------------------
// The verdict rule, and the invariant it is half of
// ---------------------------------------------------------------------------

/// The three-way verdict, all three ways: no diagnostics ⇒ `Proceed`; a Warn
/// and no Refuse ⇒ `ProceedWithWarnings` with an installable correction; any
/// Refuse ⇒ `Refuse` with none.
///
/// "The distinction is not severity theatre — a `Refuse` produces no
/// installable correction, so the system stays as it was."
#[test]
fn the_verdict_is_the_worst_severity_and_a_refusal_carries_no_correction() {
    let clean = decide(&clean());
    assert_eq!(clean.diagnostics, vec![], "the clean cohort earns no row");
    assert_eq!(clean.verdict, Verdict::Proceed);
    assert!(clean.correction.is_some());

    // Four positions on a path whose default is five: `FewPositions`, Warn.
    let warned = decide(&flat_bundle(TransducerClass::OverEar, 4));
    assert!(has_code(&warned, DiagnosticCode::FewPositions));
    assert!(warned
        .diagnostics
        .iter()
        .all(|d| d.severity == Severity::Warn));
    assert_eq!(warned.verdict, Verdict::ProceedWithWarnings);
    assert!(warned.correction.is_some());

    let refused = decide(&flat_bundle(TransducerClass::OverEar, 2));
    assert!(has_code(&refused, DiagnosticCode::TooFewPositions));
    assert_eq!(refused.verdict, Verdict::Refuse);
    assert!(refused.correction.is_none());
}

/// The three `Verification*` codes belong to B8's gate, not to the refusal
/// table: they are the only rows that postdate the capture, and a bundle that
/// carries a verification block must not acquire one of them from here.
#[test]
fn the_verification_codes_are_not_emitted_by_the_refusal_table() {
    let mut bundle = clean();
    bundle.verification = Some(verification());
    let set = decide(&bundle);
    for code in [
        DiagnosticCode::VerificationPreampMismatch,
        DiagnosticCode::VerificationResidual,
        DiagnosticCode::VerificationRoutingMismatch,
    ] {
        assert!(!has_code(&set, code), "{code:?} is B8's, not B7c's");
    }
}

/// D-M, written into the source and checked here: every threshold this module
/// carries names the capture-layer gate it is NOT, so the next reader cannot
/// "reconcile" two gates that live in two layers into one number.
#[test]
fn the_thresholds_document_the_capture_layer_gates_they_are_not() {
    let source = include_str!("../src/refusal.rs");
    for name in [
        // This crate's own constants.
        "ABSURD_SPAN_DB",
        "ABSURD_TILT_DB_PER_DECADE",
        "CAL_OUTLIER_DB",
        "CLIP_POSITION_DBFS",
        "CLIP_SESSION_BLOCK_FRACTION",
        "NOISE_FLOOR_MAX_DBFS",
        "NO_SIGNAL_PEAK_DBFS",
        "NO_SIGNAL_RMS_DBFS",
        "OUTLIER_DEVIATION_DB",
        "SIGMA_MEDIAN_REFUSE_DB",
        "SNR_HARD_DB",
        "SNR_SOFT_DB",
        // The capture-layer twins each one must name.
        "capture::CLIP_THRESHOLD",
        "capture::CLIP_BLOCK_FRACTION",
        "ladder::NOISE_FLOOR_MAX_DBFS",
        "ladder::SNR_MEDIAN_ACCEPT_DB",
        "ladder::SNR_MIN_ACCEPT_DB",
        "compensation::DEFAULT_OUTLIER_DB",
        "TransducerCaps::sensitivity_envelope_spl_per_dbfs",
        "MeasurementDiagnostic::InputClipping",
    ] {
        assert!(
            source.contains(name),
            "refusal.rs must name {name}: D-M's layering is documentation, and a doc \
             comment nothing checks is a doc comment the next reader deletes"
        );
    }
}

/// Remedy copy is load-bearing safety text. Every row that fires renders a
/// non-empty string with no unsubstituted placeholder left in it.
#[test]
fn every_remedy_renders_non_empty_with_no_unsubstituted_placeholder() {
    let mut bundle = clean();
    bundle.cal = None;
    bundle.capture.input_present = false;
    bundle.capture.self_excluded = false;
    bundle.capture.chain_sensitivity_spl_per_dbfs = Some(200.0);
    bundle.positions[0].capture = CaptureStats {
        clipped_samples: 200_000,
        peak_dbfs: -0.1,
        rms_dbfs: -80.0,
    };
    let set = decide(&bundle);
    assert!(set.diagnostics.len() >= 6, "{:?}", set.diagnostics);
    for diagnostic in &set.diagnostics {
        assert!(!diagnostic.remedy.is_empty(), "{diagnostic:?}");
        // The spec's own placeholder spellings. Not "contains a brace": a
        // rendered JSON value legitimately carries one, and a test that cannot
        // tell them apart is a test that gets weakened the first time it fires.
        for placeholder in [
            "{a:", "{b:", "{class}", "{d:", "{f:", "{g:", "{i}", "{n}", "{name}", "{noun}", "{snr",
        ] {
            assert!(
                !diagnostic.remedy.contains(placeholder),
                "unsubstituted {placeholder} in {diagnostic:?}"
            );
        }
        // A run of spaces is the signature of a missing `\` line continuation
        // inside a multi-line Rust string literal: the source indentation ends
        // up in the copy the user reads. Ruling R-A11.
        assert!(
            !diagnostic.remedy.contains("  "),
            "a run of spaces from a missing line continuation in {diagnostic:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The capture-identity and topology rows
// ---------------------------------------------------------------------------

/// The simplest whole-pipeline refusal: the deconvolved IR and the sweep that
/// produced it disagree about the rate. "Internal error — this is a bug, not a
/// user condition."
#[test]
fn sweep_rate_mismatch_refuses() {
    let mut bundle = clean();
    bundle.positions[1].ir.sample_rate = 44_100;
    let set = decide(&bundle);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::SweepRateMismatch);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert_eq!(diagnostic.position, Some(1));
    assert_eq!(set.verdict, Verdict::Refuse);
    assert!(set.correction.is_none());

    assert!(!has_code(
        &decide(&clean()),
        DiagnosticCode::SweepRateMismatch
    ));
}

/// MS-6's post-capture half. D-V: verification INVERTS it — the tap must SEE
/// the helper — so a bundle carrying a verification pass earns no second row
/// from here, and the one row it can earn is about the MEASUREMENT capture.
#[test]
fn self_exclusion_false_refuses_a_measurement_capture() {
    let mut bundle = clean();
    bundle.capture.self_excluded = false;
    let set = decide(&bundle);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::SelfExclusionUnavailable).severity,
        Severity::Refuse
    );
    assert_eq!(set.verdict, Verdict::Refuse);

    // A verification pass is exempt: it has no self-exclusion witness of its
    // own, and it must not acquire the measurement capture's a second time.
    let mut verified = clean();
    verified.verification = Some(verification());
    assert!(!has_code(
        &decide(&verified),
        DiagnosticCode::SelfExclusionUnavailable
    ));

    let mut both = bundle.clone();
    both.verification = Some(verification());
    assert_eq!(
        diagnostics_with(&decide(&both), DiagnosticCode::SelfExclusionUnavailable).len(),
        1
    );
}

/// The mic that produced the measurement is gone, so nothing downstream can be
/// re-measured against it. The device UID rides in the copy.
#[test]
fn mic_not_connected_refuses_and_names_the_device() {
    let mut bundle = clean();
    bundle.capture.input_present = false;
    let set = decide(&bundle);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::MicNotConnected);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert!(diagnostic.remedy.contains(&bundle.capture.input_uid));

    assert!(!has_code(
        &decide(&clean()),
        DiagnosticCode::MicNotConnected
    ));
}

/// MS-17's envelope, mirrored onto `PathProfile`. A safety event, not a quality
/// event: an empty jig and a sweep going to the laptop speakers both look like
/// this.
#[test]
fn wrong_transducer_refuses_outside_the_class_envelope() {
    let mut bundle = clean();
    // The coupler envelope is 85..=130 dB SPL per dBFS RMS.
    bundle.capture.chain_sensitivity_spl_per_dbfs = Some(60.0);
    let set = decide(&bundle);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::WrongTransducer);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert_eq!(diagnostic.value, Some(60.0));
    assert!(diagnostic
        .remedy
        .contains(TransducerClass::OverEar.display_name()));

    let mut inside = clean();
    inside.capture.chain_sensitivity_spl_per_dbfs = Some(104.0);
    assert!(!has_code(&decide(&inside), DiagnosticCode::WrongTransducer));

    // No solve, nothing to compare: MS-17's check has no input and must not
    // invent one.
    let mut unsolved = clean();
    unsolved.capture.chain_sensitivity_spl_per_dbfs = None;
    assert!(!has_code(
        &decide(&unsolved),
        DiagnosticCode::WrongTransducer
    ));
}

/// D-Q both ways: the Warn is the FALLBACK for "no skew estimate could be
/// formed", so a bundle that carries one has already had the resample applied
/// and earns no row.
#[test]
fn two_clock_warns_on_a_gated_path_only_when_no_skew_estimate_was_formed() {
    let mut room = flat_bundle(TransducerClass::Bookshelf, 9);
    room.capture.input_rate = 44_100;
    room.capture.clock_skew_ppm = None;
    let set = decide(&room);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::TwoClock);
    assert_eq!(diagnostic.severity, Severity::Warn);
    assert_eq!(set.verdict, Verdict::ProceedWithWarnings);

    let mut estimated = room.clone();
    estimated.capture.clock_skew_ppm = Some(12.5);
    assert!(!has_code(&decide(&estimated), DiagnosticCode::TwoClock));

    // EQUAL NOMINAL RATES ARE NOT ONE CLOCK — ruling R-A3. Two devices both
    // reporting 48 000 Hz is the ordinary two-clock case: a USB mic and a USB
    // DAC each run their own crystal, and the nominal number is a label rather
    // than a measurement. The old rate-equality short-circuit made the row
    // unreachable on exactly the configuration it is about, and D-Q's condition
    // is the SKEW ESTIMATE, not the rates: "fire only when it is `None`".
    let mut same_nominal_rate = room.clone();
    same_nominal_rate.capture.input_rate = 48_000;
    same_nominal_rate.capture.output_rate = 48_000;
    let set = decide(&same_nominal_rate);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::TwoClock).severity,
        Severity::Warn,
        "equal nominal rates with no skew estimate is the two-clock hazard"
    );

    // …and a formed estimate still clears it, on equal rates as on unequal.
    let mut same_rate_estimated = same_nominal_rate.clone();
    same_rate_estimated.capture.clock_skew_ppm = Some(3.1);
    assert!(!has_code(
        &decide(&same_rate_estimated),
        DiagnosticCode::TwoClock
    ));

    // The coupler path does not gate, so it has no trustworthy-t=0 stake.
    let mut coupler = clean();
    coupler.capture.input_rate = 44_100;
    coupler.capture.clock_skew_ppm = None;
    assert!(!has_code(&decide(&coupler), DiagnosticCode::TwoClock));
}

// ---------------------------------------------------------------------------
// The level rows
// ---------------------------------------------------------------------------

/// Either half of the row fires it: the pass was silent in RMS, or it never
/// reached a peak a real sweep reaches.
#[test]
fn no_signal_refuses_on_rms_and_on_peak_independently() {
    let mut quiet_rms = clean();
    quiet_rms.positions[2].capture = CaptureStats {
        rms_dbfs: -61.0,
        ..capture_stats()
    };
    let set = decide(&quiet_rms);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::NoSignal);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert_eq!(diagnostic.position, Some(2));

    let mut quiet_peak = clean();
    quiet_peak.positions[0].capture = CaptureStats {
        peak_dbfs: -51.0,
        ..capture_stats()
    };
    assert!(has_code(&decide(&quiet_peak), DiagnosticCode::NoSignal));

    // Exactly at the two thresholds is not below them.
    let mut at_the_gate = clean();
    at_the_gate.positions[0].capture = CaptureStats {
        clipped_samples: 0,
        peak_dbfs: -50.0,
        rms_dbfs: -60.0,
    };
    assert!(!has_code(&decide(&at_the_gate), DiagnosticCode::NoSignal));
}

/// The position row is a PEAK test — one sample over the line loses that
/// capture — and the session row is REW's sustained-clipping rule.
#[test]
fn clipping_refuses_per_position_on_peak_and_per_session_on_sustain() {
    let mut peaked = clean();
    peaked.positions[3].capture = CaptureStats {
        peak_dbfs: -0.2,
        ..capture_stats()
    };
    let set = decide(&peaked);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::ClippingPosition);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert_eq!(diagnostic.position, Some(3));
    assert!(!has_code(&set, DiagnosticCode::ClippingSession));

    let mut under = clean();
    under.positions[3].capture = CaptureStats {
        peak_dbfs: -0.4,
        ..capture_stats()
    };
    assert!(!has_code(&decide(&under), DiagnosticCode::ClippingPosition));

    // 5.5 s at 48 kHz is 264 000 samples; 100 000 of them clipped is 37.9%,
    // which no distribution over blocks can keep under 30% in every block.
    let mut railed = clean();
    railed.positions[1].capture = CaptureStats {
        clipped_samples: 100_000,
        ..capture_stats()
    };
    let set = decide(&railed);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::ClippingSession).severity,
        Severity::Refuse
    );

    let mut brief = clean();
    brief.positions[1].capture = CaptureStats {
        clipped_samples: 50_000,
        ..capture_stats()
    };
    assert!(!has_code(&decide(&brief), DiagnosticCode::ClippingSession));
}

/// The same bundle with a FLAT silence-capture spectrum at `level_dbfs`.
///
/// Flat on purpose: since ruling R-A8 the two level rows read the SPECTRUM and
/// reduce it to a band RMS, and the band RMS of a flat curve is that curve's own
/// level — so the test can name the floor exactly without re-implementing the
/// reduction. `NoiseFloor::rms_dbfs` is deliberately left where it is: it is the
/// FALLBACK the rows use only when the spectrum cannot be read, and leaving it
/// untouched is what proves the spectrum is what they read.
fn with_flat_floor(level_dbfs: f64) -> MeasurementBundle {
    let mut bundle = clean();
    let bins = bundle.noise_floor.freqs_hz.len();
    for channel in bundle.noise_floor.spectrum_db.iter_mut() {
        *channel = vec![level_dbfs; bins];
    }
    bundle
}

/// Dirac's gate. Above it the room is too noisy to measure, and the remedy
/// explicitly refuses to solve it with output level.
#[test]
fn noise_floor_too_high_refuses() {
    let set = decide(&with_flat_floor(-23.9));
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::NoiseFloorTooHigh).severity,
        Severity::Refuse
    );
    assert!(
        set.diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::NoiseFloorTooHigh)
            .and_then(|d| d.value)
            .is_some_and(|v| (v + 23.9).abs() < 0.05),
        "the row reports the band RMS, which on a flat floor is its own level: {:?}",
        set.diagnostics
    );

    assert!(!has_code(
        &decide(&with_flat_floor(-24.0)),
        DiagnosticCode::NoiseFloorTooHigh
    ));
}

/// The two SNR rows at their boundaries. The soft row is a Warn; the hard row
/// refuses, and its copy forbids the remedy a user reaches for first.
///
/// Since ruling R-A8 the SNR is band-restricted, so the level the rows grade
/// against is the analysed curve's own band RMS rather than
/// `CaptureStats::rms_dbfs`. The floor is FLAT, so its band RMS is its own
/// level whatever the band turns out to be, and the SNR the rule should report
/// is re-derived here from the PUBLISHED curves — an oracle over what
/// `decide()` published, not a second copy of the rule's answer.
///
/// **The cohort is scaled down first, and that is not cosmetic.** The two rows
/// overlap with Dirac's `-24 dBFS` noise-floor gate: at the `clean()` fixture's
/// own level a floor 15 dB under it sits well above `-24`, so
/// `NoiseFloorTooHigh` would fire alongside and the test would be grading two
/// rows at once. Scaling to about `-20 dBFS` — the level the fixture's own
/// `CaptureStats` already claims — puts both floors below the gate, which is the
/// arrangement the pre-R-A8 version of this test had for free.
///
/// **A ladder rather than four hand-placed floors.** The band the RMS is taken
/// over moves with `correction_range`, which moves with the floor, so a floor
/// computed to land on exactly 24.9 dB lands a few tenths away. Walking the
/// floor in 0.25 dB steps and asserting the EQUIVALENCE at every step — the row
/// fires exactly when the re-derived SNR is under its threshold — tests the
/// thresholds without needing to hit them.
#[test]
fn snr_soft_warns_and_hard_refuses_at_the_boundary() {
    const CAPTURE_DBFS: f64 = -20.0;

    let probe = decide(&with_flat_floor(-160.0));
    let probe_level = snr_band_rms(&probe, 0);
    let scale = 10f64.powf((CAPTURE_DBFS - probe_level) / 20.0);

    let at_floor = |floor_db: f64| {
        let mut bundle = with_flat_floor(floor_db);
        for position in &mut bundle.positions {
            for channel in position.ir.samples.iter_mut() {
                for sample in channel.iter_mut() {
                    *sample *= scale;
                }
            }
        }
        decide(&bundle)
    };

    let mut saw_soft = false;
    let mut saw_hard = false;
    let mut saw_clean = false;
    // 12 dB to 28 dB of SNR, which brackets both thresholds with room either
    // side. The floor is what moves; the capture does not.
    for step in 0..=32 {
        let floor_db = CAPTURE_DBFS - 12.0 - 0.5 * f64::from(step);
        let set = at_floor(floor_db);
        // The floor is flat, so its band RMS IS its level, whatever band the
        // run settled on.
        let snr = snr_band_rms(&set, 0) - floor_db;

        assert!(
            !has_code(&set, DiagnosticCode::NoiseFloorTooHigh),
            "floor {floor_db} is below Dirac's gate; this ladder must grade the SNR rows alone"
        );

        let hard = has_code(&set, DiagnosticCode::LowSnrHard);
        let soft = has_code(&set, DiagnosticCode::LowSnrSoft);
        assert_eq!(
            hard,
            snr < 15.0,
            "floor {floor_db}: SNR {snr:.3} against the 15 dB hard gate"
        );
        assert_eq!(
            soft,
            (15.0..25.0).contains(&snr),
            "floor {floor_db}: SNR {snr:.3} against the 25 dB soft gate — hard outranks soft, \
             so a position that tripped 15 must not be reported twice"
        );
        if let Some(reported) = set
            .diagnostics
            .iter()
            .find(|d| {
                matches!(
                    d.code,
                    DiagnosticCode::LowSnrHard | DiagnosticCode::LowSnrSoft
                )
            })
            .and_then(|d| d.value)
        {
            assert!(
                (reported - snr).abs() < 0.05,
                "floor {floor_db}: the row reports {reported:.3}, the curves say {snr:.3}"
            );
        }
        saw_hard |= hard;
        saw_soft |= soft;
        saw_clean |= !hard && !soft;
    }
    assert!(
        saw_hard && saw_soft && saw_clean,
        "the ladder must cross both gates"
    );

    // The severities and the copy, once, on a floor deep in each band.
    let hard = at_floor(CAPTURE_DBFS - 12.0);
    assert_eq!(
        diagnostics_with(&hard, DiagnosticCode::LowSnrHard)[0].severity,
        Severity::Refuse
    );
    assert!(diagnostics_with(&hard, DiagnosticCode::LowSnrHard)[0]
        .remedy
        .contains("do not"));
    assert_eq!(hard.verdict, Verdict::Refuse);

    let soft = at_floor(CAPTURE_DBFS - 20.0);
    assert_eq!(
        diagnostics_with(&soft, DiagnosticCode::LowSnrSoft)[0].severity,
        Severity::Warn
    );
    assert_eq!(soft.verdict, Verdict::ProceedWithWarnings);
}

/// One position's analysed band RMS, over the band the level rows grade on:
/// the published `correction_range`, or the whole grid when that range has
/// collapsed to fewer than two bins.
///
/// The collapse is not hypothetical — `correction_range`'s low edge is the
/// lowest bin with SNR ≥ 25 dB, so every run the SNR rows fire on has collapsed
/// it — which is why `refusal.rs` carries the same fallback.
fn snr_band_rms(set: &paraeq_decide::DecisionSet, position: usize) -> f64 {
    let freqs = &set.analysis.freqs_hz;
    let (lo, hi) = set.decisions.correction_range.value;
    let kept = freqs.iter().filter(|f| **f >= lo && **f <= hi).count();
    let (lo, hi) = if kept >= 2 {
        (lo, hi)
    } else {
        (f64::NEG_INFINITY, f64::INFINITY)
    };
    band_rms_db(&set.analysis.per_position_db[position], freqs, (lo, hi))
}

/// § D-P, taken honestly: the soft row's copy says the position was used
/// UNWEIGHTED, because nothing de-weights it.
///
/// Ruling R-A7. `fr::average_measurements_rms_weighted` and `fr::sigma_db_weighted`
/// shipped with no non-test caller, so "We used it, but weighted it down" was a
/// sentence about a mechanism that is not wired. D-P's own interim is "emit the
/// Warn and do **not** de-weight, **and say so**".
#[test]
fn the_soft_snr_remedy_does_not_claim_a_de_weighting_that_never_happened() {
    let probe = decide(&with_flat_floor(-160.0));
    let level = band_rms_db(
        &probe.analysis.per_position_db[0],
        &probe.analysis.freqs_hz,
        probe.decisions.correction_range.value,
    );
    let set = decide(&with_flat_floor(level - 20.0));
    let soft = &diagnostics_with(&set, DiagnosticCode::LowSnrSoft)[0];
    assert!(
        !soft.remedy.contains("weighted it down"),
        "the copy claims a de-weighting nothing performs: {}",
        soft.remedy
    );
    assert!(
        soft.remedy.contains("as it is"),
        "…and it has to say what really happened: {}",
        soft.remedy
    );
}

/// The two level rows read the band the correction is placed in, not the whole
/// spectrum.
///
/// Ruling R-A8's falsifier. A floor that is quiet INSIDE `correction_range` and
/// railed above it is the case the two readings disagree on: a broadband RMS is
/// dominated by the loud part and refuses a measurement the noise cannot affect,
/// while the band-restricted read sees the quiet band the filters go in. The
/// spec words both rows against a band — "in `correction_range`", "in the
/// analysis band" — and this is what that costs an implementation that ignores
/// it.
#[test]
fn the_snr_and_floor_rows_are_restricted_to_the_correction_range() {
    let mut bundle = clean();
    let bins = bundle.noise_floor.freqs_hz.len();
    assert!(bins >= 3, "the fixture's floor needs a top bin to rail");
    for channel in bundle.noise_floor.spectrum_db.iter_mut() {
        *channel = vec![-120.0; bins];
        // 20 kHz, well above anything a coupler correction is placed at.
        *channel.last_mut().expect("a top bin") = 0.0;
    }
    // The broadband summary still says the room is unusable. The rows must not
    // be reading it.
    bundle.noise_floor.rms_dbfs = vec![0.0; bundle.noise_floor.spectrum_db.len()];

    let set = decide(&bundle);
    assert!(
        !has_code(&set, DiagnosticCode::NoiseFloorTooHigh),
        "a railed bin outside the corrected band is not a reason to refuse: {:?}",
        set.diagnostics
    );
    assert!(
        !has_code(&set, DiagnosticCode::LowSnrHard) && !has_code(&set, DiagnosticCode::LowSnrSoft),
        "the SNR inside the corrected band is enormous: {:?}",
        set.diagnostics
    );
    assert!(
        set.decisions.correction_range.value.1 < 20_000.0,
        "the railed bin should also have pulled the correction range in, got {:?}",
        set.decisions.correction_range.value
    );
}

/// A band RMS in the power domain — the same reduction `refusal.rs` applies,
/// re-derived here from the PUBLISHED curves so the assertion has an oracle
/// rather than a copy of the rule's own answer.
fn band_rms_db(curve: &[f64], freqs_hz: &[f64], (lo, hi): (f64, f64)) -> f64 {
    let power: Vec<f64> = freqs_hz
        .iter()
        .zip(curve)
        .filter(|(f, _)| **f >= lo && **f <= hi)
        .map(|(_, db)| 10f64.powf(db / 10.0))
        .collect();
    assert!(!power.is_empty(), "the correction range keeps no bin");
    10.0 * (power.iter().sum::<f64>() / power.len() as f64).log10()
}

// ---------------------------------------------------------------------------
// The cohort rows
// ---------------------------------------------------------------------------

/// `< 3` cannot produce a σ(f) anyone should believe; below the path's default
/// is a warning, not a refusal; at the default, neither row fires.
#[test]
fn too_few_positions_refuses_and_below_default_warns() {
    let refused = decide(&flat_bundle(TransducerClass::OverEar, 2));
    let diagnostic = only_diagnostic(&refused, DiagnosticCode::TooFewPositions);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert_eq!(diagnostic.value, Some(2.0));
    assert!(
        diagnostic.remedy.contains("3 reseats"),
        "{}",
        diagnostic.remedy
    );
    assert_eq!(refused.verdict, Verdict::Refuse);

    let warned = decide(&flat_bundle(TransducerClass::Bookshelf, 5));
    assert_eq!(
        only_diagnostic(&warned, DiagnosticCode::FewPositions).severity,
        Severity::Warn
    );
    assert!(!has_code(&warned, DiagnosticCode::TooFewPositions));
    assert_eq!(warned.verdict, Verdict::ProceedWithWarnings);

    let full = decide(&flat_bundle(TransducerClass::Bookshelf, 9));
    assert!(!has_code(&full, DiagnosticCode::FewPositions));
    assert!(!has_code(&full, DiagnosticCode::TooFewPositions));
}

/// `{noun}` is a NOUN that can take an "s", on both rows and both paths.
///
/// Ruling R-A11. `PathProfile::reposition_noun` is the imperative retry phrase
/// ("move the mic ~30 cm", "reseat the tip"), so interpolating it rendered
/// "We need at least 3 move the mic ~30 cms". `rules::position_noun` is the
/// word the decision table's `{noun}s` is written for.
#[test]
fn the_position_count_rows_interpolate_a_noun_not_the_retry_phrase() {
    let coupler = decide(&flat_bundle(TransducerClass::OverEar, 2));
    let hard = only_diagnostic(&coupler, DiagnosticCode::TooFewPositions);
    assert!(
        hard.remedy.contains("at least 3 reseats to tell"),
        "{}",
        hard.remedy
    );

    let room = decide(&flat_bundle(TransducerClass::Bookshelf, 5));
    let soft = only_diagnostic(&room, DiagnosticCode::FewPositions);
    assert!(
        soft.remedy.contains("We averaged 5 positions."),
        "{}",
        soft.remedy
    );

    // The retry phrase must not reach either row.
    for remedy in [&hard.remedy, &soft.remedy] {
        assert!(!remedy.contains("move the mic"), "{remedy}");
        assert!(!remedy.contains("reseat the headphone"), "{remedy}");
    }
}

/// σ(f) below the transition is where the positions are supposed to AGREE, so
/// a high median there says the cohort is not measuring one system.
#[test]
fn excessive_variance_refuses_above_the_median_sigma_gate() {
    let spread = |gains: [f64; 5]| {
        let shapes: Vec<Vec<[f64; 6]>> = gains
            .iter()
            .map(|g| vec![biquad::low_shelf(300.0, *g, SHELF_Q, 48_000.0)])
            .collect();
        decide(&shaped_positions(TransducerClass::OverEar, &shapes))
    };

    let refused = spread([-12.0, -6.0, 0.0, 6.0, 12.0]);
    let diagnostic = only_diagnostic(&refused, DiagnosticCode::ExcessiveVariance);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert!(diagnostic.value.is_some_and(|v| v > 6.0));
    assert_eq!(refused.verdict, Verdict::Refuse);

    let tolerated = spread([-4.0, -2.0, 0.0, 2.0, 4.0]);
    assert!(!has_code(&tolerated, DiagnosticCode::ExcessiveVariance));
}

/// The three outlier rows differ in BAND and in SEVERITY, which is the whole
/// point of having three: a coupler's bass outlier is a seal problem and is
/// refused, the same scatter up top is normal and is averaged in.
#[test]
fn position_outliers_fire_at_their_own_band_and_severity() {
    let lf = {
        let mut shapes = vec![Vec::new(); 5];
        shapes[1] = vec![biquad::low_shelf(300.0, -14.0, SHELF_Q, 48_000.0)];
        decide(&shaped_positions(TransducerClass::OverEar, &shapes))
    };
    let diagnostic = only_diagnostic(&lf, DiagnosticCode::PositionOutlierCouplerLf);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert_eq!(diagnostic.position, Some(1));
    assert!(!has_code(&lf, DiagnosticCode::PositionOutlierCouplerHf));

    let hf = {
        let mut shapes = vec![Vec::new(); 5];
        shapes[3] = vec![biquad::high_shelf(3000.0, -16.0, SHELF_Q, 48_000.0)];
        decide(&shaped_positions(TransducerClass::OverEar, &shapes))
    };
    let diagnostic = only_diagnostic(&hf, DiagnosticCode::PositionOutlierCouplerHf);
    assert_eq!(diagnostic.severity, Severity::Warn);
    assert_eq!(diagnostic.position, Some(3));
    assert!(!has_code(&hf, DiagnosticCode::PositionOutlierCouplerLf));

    // The room path has one outlier row, over the WHOLE band, and it warns. Its
    // story is "the mic moved during the sweep, or a door opened", so the
    // outlier has to be broadband: a 14 dB shelf confined to the bottom two
    // octaves averages away to ~3 dB over ten of them and does not fire, which
    // is the row behaving correctly rather than a gap.
    let room = {
        let mut shapes = vec![Vec::new(); 9];
        shapes[5] = vec![biquad::low_shelf(1000.0, -30.0, SHELF_Q, 48_000.0)];
        decide(&shaped_positions(TransducerClass::Bookshelf, &shapes))
    };
    let diagnostic = only_diagnostic(&room, DiagnosticCode::PositionOutlierRoom);
    assert_eq!(diagnostic.severity, Severity::Warn);
    assert_eq!(diagnostic.position, Some(5));
    assert!(!has_code(&room, DiagnosticCode::PositionOutlierCouplerLf));

    let clean = decide(&clean());
    for code in [
        DiagnosticCode::PositionOutlierCouplerHf,
        DiagnosticCode::PositionOutlierCouplerLf,
        DiagnosticCode::PositionOutlierRoom,
    ] {
        assert!(!has_code(&clean, code));
    }
}

// ---------------------------------------------------------------------------
// The curve rows
// ---------------------------------------------------------------------------

/// Two independent halves, one code: a 40 dB span or a 20 dB/decade midband
/// tilt says the thing measured is not a loudspeaker or a headphone.
#[test]
fn absurd_curve_refuses_on_span_and_on_tilt_at_their_thresholds() {
    let with_cal = |cal| {
        let mut bundle = clean();
        bundle.cal = Some(cal);
        decide(&bundle)
    };

    let wide = with_cal(span_cal(41.0));
    assert_eq!(
        only_diagnostic(&wide, DiagnosticCode::AbsurdCurve).severity,
        Severity::Refuse
    );
    assert_eq!(wide.verdict, Verdict::Refuse);
    assert!(!has_code(
        &with_cal(span_cal(39.0)),
        DiagnosticCode::AbsurdCurve
    ));

    let steep = with_cal(tilt_cal(21.0));
    assert_eq!(
        only_diagnostic(&steep, DiagnosticCode::AbsurdCurve).severity,
        Severity::Refuse
    );
    assert!(!has_code(
        &with_cal(tilt_cal(19.0)),
        DiagnosticCode::AbsurdCurve
    ));
}

/// Vector/coherent averaging is not a variant of `AveragingMode`, and no
/// override can make it one: the tripwire guards a door the type already
/// locked, and `fr::average_measurements_vector` is never reached.
#[test]
fn decide_never_reaches_a_coherent_averaging_routine() {
    for class in [
        TransducerClass::Bookshelf,
        TransducerClass::Floorstander,
        TransducerClass::InEar,
        TransducerClass::OverEar,
    ] {
        let set = decide(&well_formed_bundle(class));
        assert!(!has_code(&set, DiagnosticCode::CoherentAveragingRejected));
    }
}

// ---------------------------------------------------------------------------
// The calibration rows
// ---------------------------------------------------------------------------

/// Without a cal we are measuring the microphone, not the system. Malformed is
/// the same consequence with a different remedy, and the neighbour-outlier row
/// catches a file that parses cleanly and is still wrong.
#[test]
fn cal_missing_and_malformed_and_neighbour_outlier_refuse() {
    let mut missing = clean();
    missing.cal = None;
    let set = decide(&missing);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::CalMissing).severity,
        Severity::Refuse
    );
    assert_eq!(set.verdict, Verdict::Refuse);

    let mut non_finite = clean();
    non_finite.cal = Some(cal_with_curve(
        vec![20.0, 1000.0, 20000.0],
        vec![-3.13, f64::NAN, 1.20],
    ));
    assert_eq!(
        only_diagnostic(&decide(&non_finite), DiagnosticCode::CalMalformed).severity,
        Severity::Refuse
    );

    let mut backwards = clean();
    backwards.cal = Some(cal_with_curve(
        vec![20.0, 1000.0, 500.0],
        vec![-3.13, 0.0, 1.20],
    ));
    assert!(has_code(&decide(&backwards), DiagnosticCode::CalMalformed));

    // The shipping 7005770_90deg.txt pattern: an exact 0.0000 at 19.611 Hz
    // between -3.13 and -3.11, a 3.12 dB error inside the full-authority band.
    let mut vendor = clean();
    vendor.cal = Some(cal_with_curve(
        vec![19.361, 19.611, 19.861],
        vec![-3.13, 0.0000, -3.11],
    ));
    let set = decide(&vendor);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::CalNeighbourOutlier);
    assert_eq!(diagnostic.severity, Severity::Refuse);
    assert!(
        diagnostic.value.is_some_and(|v| (v - 3.12).abs() < 0.01),
        "{diagnostic:?}"
    );
    assert!(diagnostic.remedy.contains("19.611"));

    assert!(!has_code(&decide(&clean()), DiagnosticCode::CalMissing));
    assert!(!has_code(&decide(&clean()), DiagnosticCode::CalMalformed));
    assert!(!has_code(
        &decide(&clean()),
        DiagnosticCode::CalNeighbourOutlier
    ));
}

/// The neighbour-outlier row does not fire on the three EARS variants, because
/// on those a "neighbour outlier" is the TARGET.
///
/// Ruling R-A2, `OPEN [OWNER]` and reversible. An HEQ/HPN/IDF calibration has
/// the Harman target subtracted into the curve — that is the premise of the
/// `over_ear_ears_heq_cal` case — so the 5 kHz dip the target carries reads as
/// a 4.7 dB neighbour deviation on a twelve-point vendor grid. The rule cannot
/// distinguish that from a defect: both are "one point far from the line
/// through its neighbours", and the variant is the only evidence there is. The
/// `CalHasTargetBakedIn` warning is what the user is told instead, and it says
/// the right thing.
///
/// The exemption is exactly these three variants. A `Plain` cal carrying the
/// identical curve still refuses, which is the second half of this test and the
/// thing that keeps the exemption from being a hole.
#[test]
fn the_neighbour_outlier_row_exempts_the_three_ears_variants() {
    // A shape a target puts in a cal: a deep, narrow dip between two ordinary
    // neighbours, far past the 1.5 dB the row refuses at.
    let curve = || cal_with_curve(vec![2000.0, 5000.0, 10000.0], vec![-1.94, -8.42, -5.11]);

    for variant in [
        CalVariant::EarsHeq,
        CalVariant::EarsHpn,
        CalVariant::EarsIdf,
    ] {
        let mut bundle = clean();
        let mut cal = curve();
        cal.variant = variant;
        bundle.cal = Some(cal);
        let set = decide(&bundle);
        assert!(
            !has_code(&set, DiagnosticCode::CalNeighbourOutlier),
            "{variant:?}: the baked-in target is not a cal defect; {:?}",
            set.diagnostics
        );
        // The user is still told what happened, by the row that is about it.
        assert!(
            has_code(&set, DiagnosticCode::CalHasTargetBakedIn),
            "{variant:?}"
        );
    }

    // …and the same curve on a PLAIN cal is still a defect, because on a plain
    // cal there is no target to explain it.
    let mut plain = clean();
    plain.cal = Some(curve());
    let set = decide(&plain);
    assert_eq!(
        only_diagnostic(&set, DiagnosticCode::CalNeighbourOutlier).severity,
        Severity::Refuse
    );
}

/// An EARS HEQ/HPN/IDF cal already carries a target. Applying another would
/// apply it twice — a Warn, because the measurement is still usable.
#[test]
fn an_ears_cal_variant_warns_that_a_target_is_already_baked_in() {
    for variant in [
        CalVariant::EarsHeq,
        CalVariant::EarsHpn,
        CalVariant::EarsIdf,
    ] {
        let mut bundle = clean();
        bundle
            .cal
            .as_mut()
            .expect("the clean cohort has a cal")
            .variant = variant;
        let set = decide(&bundle);
        let diagnostic = only_diagnostic(&set, DiagnosticCode::CalHasTargetBakedIn);
        assert_eq!(diagnostic.severity, Severity::Warn, "{variant:?}");
        assert_eq!(set.verdict, Verdict::ProceedWithWarnings, "{variant:?}");
    }
    assert!(!has_code(
        &decide(&clean()),
        DiagnosticCode::CalHasTargetBakedIn
    ));
}

// ---------------------------------------------------------------------------
// The override row
// ---------------------------------------------------------------------------

/// D-N: an override outside its decision's `Domain` is CLAMPED into it and the
/// row says so, naming the decision, the number asked for and the number used.
///
/// Ruling R-A6 moved `Diagnostic::value` from the requested number to the
/// clamped one: the drawer's margin display is about what the run did, and the
/// requested number is already in the sentence.
#[test]
fn an_override_outside_its_domain_warns_and_names_both_numbers() {
    let mut bundle = clean();
    bundle.overrides.transition_hz = Some(1000.0);
    let set = decide(&bundle);
    let diagnostic = only_diagnostic(&set, DiagnosticCode::OverrideOutOfDomain);
    assert_eq!(diagnostic.severity, Severity::Warn);
    assert_eq!(
        diagnostic.value,
        Some(400.0),
        "the row carries the number that was USED, not the one that was asked for"
    );
    assert_eq!(set.decisions.transition_hz.value, 400.0);
    assert!(diagnostic.remedy.contains("transition_hz"));
    assert!(diagnostic.remedy.contains("1000"));
    assert!(diagnostic.remedy.contains("400"));
    assert!(
        !diagnostic.remedy.contains("We used it as asked"),
        "{}",
        diagnostic.remedy
    );
    assert_eq!(set.verdict, Verdict::ProceedWithWarnings);

    let mut inside = clean();
    inside.overrides.transition_hz = Some(137.0);
    assert!(!has_code(
        &decide(&inside),
        DiagnosticCode::OverrideOutOfDomain
    ));

    // A `Choice` domain answers the same question: `Gaussian` is representable
    // on the merged smoothing type and is not on the decision table's list. It
    // has no nearest legal value, so the fallback is the rule's own.
    let mut off_list = clean();
    off_list.overrides.smoothing = Some(paraeq_decide::SmoothingMode::Gaussian { fraction: 0.5 });
    let off = decide(&off_list);
    assert!(has_code(&off, DiagnosticCode::OverrideOutOfDomain));
    assert_eq!(
        off.decisions.smoothing.value,
        decide(&clean()).decisions.smoothing.value,
        "an illegal Choice falls back to what the rule decided"
    );
}
