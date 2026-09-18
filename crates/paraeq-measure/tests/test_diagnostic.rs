//! MS-20: the diagnostic wire contract; MS-9: the refusal framework.
//!
//! Tier 3 (analytic/policy) per the four-tier convention: codes, severities
//! and user strings are product policy with no oracle. What is pinned: every
//! wire number of the initial set (a log/wire regression — numbers are
//! append-only, never reused, never reordered), the refusal-vs-warn split
//! row-for-row against the safety spec, both user strings non-empty for every
//! variant, and two exhaustive wildcard-free matches so that adding a variant
//! breaks this file deliberately.

use paraeq_measure::{
    CalSensitivity, MeasurementDiagnostic as D, MicSensitivity, Refusal, Severity,
};

/// Every variant of the initial set, constructed once, in declaration order.
/// [`expected`] matches without a wildcard, so a new variant fails
/// compilation there first — the deliberate breakage MS-20's append-only
/// contract wants — and the author extends this list (and the code/severity
/// tables) in the same edit.
fn all_variants() -> Vec<D> {
    vec![
        D::SelfExclusionUnavailable,
        D::SensitivityMissing,
        D::SensitivityUnparseable,
        D::MicUnidentified,
        D::CapExceedsMicFullScale,
        D::SensitivityOutOfEnvelope,
        D::VolumeUncontrollable,
        D::ProjectedSplOverCap,
        D::RungOverCap,
        D::SplProjectionMismatch,
        D::SplOverCap,
        D::InputClipping,
        D::SnrUnachievable,
        D::StimulusEndpointNonzero,
        D::StimulusFadeNotMonotone,
        D::StimulusDcOffset { mean: 1.24e-3 },
        D::StimulusOverFullScale { peak: 1.2 },
        D::StimulusNonFinite { count: 3 },
        D::UserAborted,
        D::MicDisconnected,
        D::OutputDeviceChanged,
        D::EngineFailed,
        D::SolvedLevelIllegal,
        D::EngineNotRunning,
        D::LowSnr,
        D::FixedMaxVolume,
        D::TwoClock,
        D::EmitClamped { count: 1 },
        D::EmitNonFiniteSanitized { count: 2 },
        // ── The verification loop's nineteen (Stage 6) ────────────────────
        D::HelperUnavailable,
        D::HelperFailed { exit_code: 3 },
        D::HelperStalled,
        D::VerificationMarkersNotFound {
            residual_peak_samples: 41.0,
        },
        D::VerificationGainNotPinned { read_back_db: 2.5 },
        D::VerificationLevelBelowSnrBudget {
            projected_snr_db: 12.0,
        },
        D::TapSilentDuringVerification,
        D::SystemAudioNotQuiet,
        D::HelperRoutingMismatch,
        D::OutputRateChangedDuringVerify {
            expected_hz: 48_000.0,
            observed_hz: 44_100.0,
        },
        D::VerificationChainClipped { count: 19 },
        D::VerificationMarkersNotCredible,
        D::ClockAdjustTooLarge { skew_ppm: 900.0 },
        D::RenderDeviceLeaked { pid: 4242 },
        D::VerificationBandsDropped {
            dropped: 2,
            rate_hz: 48_000.0,
        },
        D::VerificationPreampMismatch {
            armed_db: -6.0,
            recomputed_db: -3.0,
        },
        D::HelperKilledAfterRampDeadline,
        D::TwoClockResidualHigh {
            residual_peak_samples: 7.5,
        },
        D::VerificationMarkerSnrLow { margin_db: 9.0 },
    ]
}

/// The (code, severity) contract per variant. Deliberately a second,
/// independent statement of the numbers in `diagnostic.rs` — this is the
/// regression pin, so it must not be derived from the code under test. **No
/// wildcard arm**: adding a variant must break this match.
fn expected(d: &D) -> (u16, Severity) {
    use Severity::{Error, Warning};
    match d {
        D::SelfExclusionUnavailable => (1, Error),
        D::SensitivityMissing => (2, Error),
        D::SensitivityUnparseable => (3, Error),
        D::MicUnidentified => (4, Error),
        D::CapExceedsMicFullScale => (5, Error),
        D::SensitivityOutOfEnvelope => (6, Error),
        D::VolumeUncontrollable => (7, Error),
        D::ProjectedSplOverCap => (8, Error),
        D::RungOverCap => (9, Error),
        D::SplProjectionMismatch => (10, Error),
        D::SplOverCap => (11, Error),
        D::InputClipping => (12, Error),
        D::SnrUnachievable => (13, Error),
        D::StimulusEndpointNonzero => (14, Error),
        D::StimulusFadeNotMonotone => (15, Error),
        D::StimulusDcOffset { .. } => (16, Error),
        D::StimulusOverFullScale { .. } => (17, Error),
        D::StimulusNonFinite { .. } => (18, Error),
        D::UserAborted => (19, Error),
        D::MicDisconnected => (20, Error),
        D::OutputDeviceChanged => (21, Error),
        D::EngineFailed => (22, Error),
        D::SolvedLevelIllegal => (23, Error),
        D::EngineNotRunning => (24, Error),
        D::LowSnr => (100, Warning),
        D::FixedMaxVolume => (101, Warning),
        D::TwoClock => (102, Warning),
        D::EmitClamped { .. } => (103, Warning),
        D::EmitNonFiniteSanitized { .. } => (104, Warning),
        D::HelperUnavailable => (25, Error),
        D::HelperFailed { .. } => (26, Error),
        D::HelperStalled => (27, Error),
        D::VerificationMarkersNotFound { .. } => (28, Error),
        D::VerificationGainNotPinned { .. } => (29, Error),
        D::VerificationLevelBelowSnrBudget { .. } => (30, Error),
        D::TapSilentDuringVerification => (31, Error),
        D::SystemAudioNotQuiet => (32, Error),
        D::HelperRoutingMismatch => (33, Error),
        D::OutputRateChangedDuringVerify { .. } => (34, Error),
        D::VerificationChainClipped { .. } => (35, Error),
        D::VerificationMarkersNotCredible => (36, Error),
        D::ClockAdjustTooLarge { .. } => (37, Error),
        D::RenderDeviceLeaked { .. } => (38, Error),
        D::VerificationBandsDropped { .. } => (39, Error),
        D::VerificationPreampMismatch { .. } => (40, Error),
        D::HelperKilledAfterRampDeadline => (105, Warning),
        D::TwoClockResidualHigh { .. } => (106, Warning),
        D::VerificationMarkerSnrLow { .. } => (107, Warning),
    }
}

/// MS-20: "a stable numbered enum". The numbers are a wire/log contract:
/// errors from 1, warnings from 100, append-only within each block.
#[test]
fn every_wire_code_of_the_initial_set_is_pinned() {
    let variants = all_variants();
    assert_eq!(variants.len(), 48, "all_variants() lags the enum");
    for d in &variants {
        let (code, _) = expected(d);
        assert_eq!(d.code(), code, "{d:?} renumbered — wire contract broken");
    }
    // No number is reused across the whole set.
    let mut codes: Vec<u16> = variants.iter().map(D::code).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), variants.len(), "a wire code is reused");
}

/// MS-20: "blocking Errors vs non-blocking Warnings", per the spec's
/// refusal-vs-warn split — every Hard Caps and Refusals row and every MS-3
/// stimulus defect is blocking; the SNR warn row, `FixedMaxVolume`,
/// `Warn(TwoClock)` and the MS-4 guard counts are not.
#[test]
fn severity_matches_the_spec_split_for_every_variant() {
    for d in all_variants() {
        let (_, severity) = expected(&d);
        assert_eq!(d.severity(), severity, "{d:?} severity");
        assert_eq!(
            d.is_blocking(),
            severity == Severity::Error,
            "{d:?} is_blocking disagrees with severity"
        );
    }
}

/// MS-20: "each mapping to a plain-language fix (easy mode) and an
/// explanation (guided mode)".
#[test]
fn every_variant_has_both_user_strings() {
    for d in all_variants() {
        assert!(!d.fix_easy().is_empty(), "{d:?} has no easy-mode fix");
        assert!(
            !d.explain_guided().is_empty(),
            "{d:?} has no guided-mode explanation"
        );
        assert_ne!(
            d.fix_easy(),
            d.explain_guided(),
            "{d:?} reuses one string for both modes"
        );
    }
}

/// The count-carrying warnings interpolate their counts — a warning that says
/// "some samples" is not a diagnostic.
#[test]
fn count_carrying_warnings_surface_their_counts() {
    for (d, count) in [
        (D::EmitClamped { count: 7 }, "7"),
        (D::EmitNonFiniteSanitized { count: 9 }, "9"),
        (D::StimulusNonFinite { count: 5 }, "5"),
    ] {
        assert!(
            d.explain_guided().contains(count),
            "{d:?} does not surface its count"
        );
    }
}

/// One assertion per row of the spec's "Hard Caps and Refusals" table (with
/// `NoMicSensitivity` split into its two causes): every row is blocking and
/// forms a [`Refusal`]. This is the MS-9 unit test "per refusal row of the
/// table" for the rows representable at this stage.
#[test]
fn every_spec_refusal_row_is_blocking_and_forms_a_refusal() {
    let rows = [
        D::SensitivityMissing,       // "No cal file, or sensitivity unparseable"
        D::SensitivityUnparseable,   // (same row, second cause)
        D::MicUnidentified,          // "Mic not identified…"
        D::ProjectedSplOverCap,      // "Projected SPL exceeds the class cap…"
        D::SplProjectionMismatch,    // "Measured SPL deviates >6 dB…"
        D::SplOverCap,               // "Measured SPL exceeds cap mid-sweep"
        D::SensitivityOutOfEnvelope, // "Chain sensitivity outside the class envelope"
        D::VolumeUncontrollable,     // "System volume reads at maximum and…"
        D::InputClipping,            // ">30% of samples in an input block clipped"
        D::SnrUnachievable,          // "Noise floor above the SNR gate after ≤2 remedies"
        D::SelfExclusionUnavailable, // "Self-exclusion not established"
        D::CapExceedsMicFullScale,   // "Cap exceeds the mic's full-scale SPL"
    ];
    for d in rows {
        assert_eq!(d.severity(), Severity::Error, "{d:?} must refuse, not warn");
        assert_eq!(Refusal::new(d).diagnostic(), d);
    }
}

/// The escalation-abort conditions of ladder step 5 (rung over cap;
/// measured-vs-projected deviation >6 dB) are refusals too — abort, never an
/// input to a heuristic.
#[test]
fn escalation_aborts_are_blocking() {
    for d in [D::RungOverCap, D::SplProjectionMismatch] {
        assert!(d.is_blocking(), "{d:?} must abort the escalation");
    }
}

/// The § Abort Guards trigger rows appended for the session runtime
/// (codes 19–22, plus `EngineNotRunning = 24` for the engine leaving
/// `Running` by request): each terminates the run, so each is blocking and
/// can travel as the terminating diagnostic of an aborted session.
#[test]
fn abort_triggers_are_blocking() {
    for d in [
        D::UserAborted,
        D::MicDisconnected,
        D::OutputDeviceChanged,
        D::EngineFailed,
        D::EngineNotRunning,
    ] {
        assert!(d.is_blocking(), "{d:?} must terminate the run");
        assert_eq!(Refusal::new(d).diagnostic(), d);
    }
}

/// "The EQ is switched off" is a **different condition** from "the engine
/// broke" and from "self-exclusion could not be established", and the whole
/// point of code 24 is that the user is told something different. Without a
/// distinct diagnostic a stopped engine surfaces as
/// `SelfExclusionUnavailable` (no tap ⇒ no witness), whose remedy is "Restart
/// ParaEQ" — the wrong instruction for a switch the user can flip. MS-20's
/// "plain-language fix (easy mode) and an explanation (guided mode)" is only
/// worth anything if the strings actually differ.
#[test]
fn engine_not_running_is_distinct_from_engine_failed_and_self_exclusion() {
    assert_eq!(D::EngineNotRunning.code(), 24);
    assert!(D::EngineNotRunning.is_blocking());
    for other in [D::EngineFailed, D::SelfExclusionUnavailable] {
        assert_ne!(
            D::EngineNotRunning.fix_easy(),
            other.fix_easy(),
            "{other:?} and EngineNotRunning give the same easy-mode remedy"
        );
        assert_ne!(
            D::EngineNotRunning.explain_guided(),
            other.explain_guided(),
            "{other:?} and EngineNotRunning give the same guided explanation"
        );
    }
    // The specific failure this diagnostic exists to prevent: telling someone
    // whose EQ is merely switched off to restart the app (or their Mac).
    let fix = D::EngineNotRunning.fix_easy();
    assert!(
        !fix.contains("Restart"),
        "a stopped engine must not be given the restart remedy: {fix:?}"
    );
}

// ── Stage 6: the verification loop's nineteen codes ───────────────────────

/// The numbering contract is APPEND-ONLY. The shipped set keeps 1..=24 and
/// 100..=104 (pinned above, row by row); this asserts the new block is exactly
/// the next sixteen error numbers and the next three warning numbers, with no
/// gap and no reuse — a gap would look like a deleted variant to a log reader.
#[test]
fn the_verification_codes_append_without_disturbing_the_shipped_numbering() {
    let errors: Vec<u16> = all_variants()
        .iter()
        .filter(|d| d.severity() == Severity::Error)
        .map(D::code)
        .collect();
    let warnings: Vec<u16> = all_variants()
        .iter()
        .filter(|d| d.severity() == Severity::Warning)
        .map(D::code)
        .collect();

    let mut new_errors: Vec<u16> = errors.iter().copied().filter(|c| *c > 24).collect();
    new_errors.sort_unstable();
    assert_eq!(
        new_errors,
        (25u16..=40).collect::<Vec<u16>>(),
        "the verification errors must be exactly 25..=40, contiguous"
    );

    let mut new_warnings: Vec<u16> = warnings.iter().copied().filter(|c| *c > 104).collect();
    new_warnings.sort_unstable();
    assert_eq!(
        new_warnings,
        (105u16..=107).collect::<Vec<u16>>(),
        "the verification warnings must be exactly 105..=107, contiguous"
    );
    assert_eq!(
        new_errors.len() + new_warnings.len(),
        19,
        "sixteen errors plus three warnings is nineteen"
    );
}

/// The measure-side half of the preamp twin. One condition is named twice, on
/// two sides of the capture: `MeasurementDiagnostic::VerificationPreampMismatch`
/// refuses BEFORE the sweep (no bundle exists yet) and
/// `paraeq_decide::DiagnosticCode::VerificationPreampMismatch` refuses AFTER,
/// on a carried number. Without each doc naming the other, the next reader
/// deletes one as a duplicate.
#[test]
fn the_preamp_mismatch_code_documents_its_paraeq_decide_twin() {
    let source = include_str!("../src/diagnostic.rs");
    let marker = "DiagnosticCode::VerificationPreampMismatch";
    assert!(
        source.contains(marker),
        "diagnostic.rs must name the paraeq-decide twin by its full path"
    );
    assert!(
        source.contains("before the capture"),
        "the doc must say WHICH side of the capture this one fires on"
    );
    // And the decide-side doc names this one back. `paraeq-decide` is a
    // dev-dependency here precisely so a cross-crate documentation contract can
    // be asserted without a production edge.
    let twin = include_str!("../../paraeq-decide/src/outcome.rs");
    assert!(
        twin.contains("MeasurementDiagnostic::VerificationPreampMismatch"),
        "paraeq-decide's variant must name the paraeq-measure twin back"
    );
}

/// Near side versus far side of the transducer: `VerificationChainClipped`
/// reads the ENGINE's ±1.0 output clamp, `InputClipping` reads the MIC's ADC.
/// Both can fire, neither implies the other, and a residual computed over a
/// railed capture is meaningless whichever one is silent — so each doc names
/// the other and the strings differ.
#[test]
fn chain_clipping_and_input_clipping_are_documented_against_each_other() {
    let source = include_str!("../src/diagnostic.rs");
    assert!(
        source.contains("VerificationChainClipped") && source.contains("InputClipping"),
        "both codes must be named in this file"
    );
    let chain = D::VerificationChainClipped { count: 3 };
    let input = D::InputClipping;
    assert_ne!(chain.fix_easy(), input.fix_easy());
    assert_ne!(chain.explain_guided(), input.explain_guided());
    assert_ne!(chain.code(), input.code());
}

/// A refusal that says "the markers were not found" without the number is a
/// bug report nobody can act on. Every payload-carrying verification code
/// interpolates its payload.
#[test]
fn payload_carrying_verification_diagnostics_surface_their_numbers() {
    for (d, needle) in [
        (D::HelperFailed { exit_code: 4 }, "4"),
        (
            D::VerificationMarkersNotFound {
                residual_peak_samples: 41.0,
            },
            "41",
        ),
        (D::VerificationGainNotPinned { read_back_db: 2.5 }, "2.5"),
        (
            D::VerificationLevelBelowSnrBudget {
                projected_snr_db: 12.0,
            },
            "12",
        ),
        (
            D::OutputRateChangedDuringVerify {
                expected_hz: 48_000.0,
                observed_hz: 44_100.0,
            },
            "44100",
        ),
        (D::VerificationChainClipped { count: 19 }, "19"),
        (D::ClockAdjustTooLarge { skew_ppm: 900.0 }, "900"),
        (D::RenderDeviceLeaked { pid: 4242 }, "4242"),
        (
            D::VerificationBandsDropped {
                dropped: 2,
                rate_hz: 48_000.0,
            },
            "2",
        ),
        (
            D::VerificationPreampMismatch {
                armed_db: -6.0,
                recomputed_db: -3.0,
            },
            "-6",
        ),
        (
            D::TwoClockResidualHigh {
                residual_peak_samples: 7.5,
            },
            "7.5",
        ),
        (D::VerificationMarkerSnrLow { margin_db: 9.0 }, "9"),
    ] {
        assert!(
            d.explain_guided().contains(needle),
            "{d:?} does not surface {needle} in its guided explanation"
        );
    }
}

/// The three warnings must NOT be refusals: a run that killed a wedged child
/// after its ramp deadline, or whose marker fit scattered a little, still
/// produced a usable capture. Turning any of them blocking would make a
/// recoverable run unrecoverable.
#[test]
fn the_three_verification_warnings_are_non_blocking() {
    for d in [
        D::HelperKilledAfterRampDeadline,
        D::TwoClockResidualHigh {
            residual_peak_samples: 7.5,
        },
        D::VerificationMarkerSnrLow { margin_db: 9.0 },
    ] {
        assert_eq!(d.severity(), Severity::Warning, "{d:?} must not block");
        assert!(!d.is_blocking());
    }
}

/// `EngineNotRunning`'s own doc must describe the gate that actually reads it.
/// A3b landed the variant under a literal-`Running` reading; the verification
/// gate reads ENGAGED instead, because the watchdog gates `Running` on nonzero
/// input advancing while the verification pre-roll requires it stationary.
/// Without this clause the code and its own diagnostic disagree about what
/// "not running" means.
#[test]
fn engine_not_running_documents_the_engaged_reading() {
    let source = include_str!("../src/diagnostic.rs");
    assert!(
        source.contains("engine_engaged()"),
        "the shipped variant's doc must name the predicate the gate reads"
    );
    assert!(
        source.contains("AutoDisabledNoInput"),
        "the doc must enumerate the three disengaged states"
    );
}

/// A `Refusal` cannot be built from a warning: a refusal that could be waved
/// through would be the degrade-and-continue path MS-9 forbids.
#[test]
#[should_panic(expected = "not a refusal")]
fn a_warning_cannot_become_a_refusal() {
    let _ = Refusal::new(D::LowSnr);
}

// ── MS-9: the missing/unparseable-sensitivity path refuses ────────────────

/// "A missing Sens Factor must never fall back to an uncapped sweep." The
/// `Err` arm carries a `Refusal` and no sensitivity — there is no value for a
/// fallback code path to be written against.
#[test]
fn missing_sensitivity_refuses() {
    let refusal = MicSensitivity::from_cal(CalSensitivity::Missing).unwrap_err();
    assert_eq!(refusal.diagnostic(), D::SensitivityMissing);
    assert!(refusal.diagnostic().is_blocking());
}

#[test]
fn unparseable_sensitivity_refuses() {
    let refusal = MicSensitivity::from_cal(CalSensitivity::Unparseable).unwrap_err();
    assert_eq!(refusal.diagnostic(), D::SensitivityUnparseable);
    assert!(refusal.diagnostic().is_blocking());
}

/// A cal that "parses" to a non-finite value did not meaningfully parse:
/// NaN would slip through every downstream SPL comparison (`NaN > cap` is
/// false — the same NaN-blindness `SweepLevel::new` guards against).
#[test]
fn a_non_finite_sens_factor_is_unparseable() {
    for bad in [f64::INFINITY, f64::NAN, f64::NEG_INFINITY] {
        let refusal = MicSensitivity::from_cal(CalSensitivity::Parsed(bad)).unwrap_err();
        assert_eq!(refusal.diagnostic(), D::SensitivityUnparseable);
    }
}

/// The happy path: a parsed Sens Factor yields a sensitivity, and the SPL
/// conversion is the spec's formula
/// `SPL_measured = 94 + (dBFS_measured − Sens_Factor)`.
#[test]
fn a_parsed_sens_factor_converts_spl_per_the_spec_formula() {
    // A typical UMIK-1 Sens Factor.
    let sens = MicSensitivity::from_cal(CalSensitivity::Parsed(-18.0)).unwrap();
    assert_eq!(sens.sens_factor_dbfs(), -18.0);
    assert_eq!(sens.spl_from_dbfs(-30.0), 94.0 + (-30.0 - -18.0));
    // At the cal reference itself: a mic reading at its Sens Factor is at
    // 94 dB SPL by definition.
    assert_eq!(sens.spl_from_dbfs(-18.0), 94.0);
}
