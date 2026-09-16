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
    }
}

/// MS-20: "a stable numbered enum". The numbers are a wire/log contract:
/// errors from 1, warnings from 100, append-only within each block.
#[test]
fn every_wire_code_of_the_initial_set_is_pinned() {
    let variants = all_variants();
    assert_eq!(variants.len(), 29, "all_variants() lags the enum");
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
