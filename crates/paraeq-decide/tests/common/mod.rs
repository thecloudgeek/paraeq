//! Fixture values for the type-contract tests. These are NOT policy: nothing
//! here is what `decide()` chooses, and no test asserts they are. They exist
//! to give the wire types a populated value to round-trip and to give
//! `Decisions::iter()` something to enumerate. Stage 6 owns the rules.

#![allow(dead_code)]

use paraeq_decide::{
    Analysis, AuthorityCurve, AuthorityPreset, AveragingMode, CalFile, CalVariant, CapturePlan,
    CaptureRouting, CaptureStats, CorrectionForm, CorrectionPlan, CouplingPath, Decision,
    DecisionSet, Decisions, Diagnostic, DiagnosticCode, Domain, Evidence, EvidenceLabel,
    ImpulseResponse, Invalidation, MeasurementBundle, NoiseFloor, Overrides, Position, Rationale,
    RationaleKey, Severity, SmoothingMode, Source, SweepPlan, TargetChoice, TransducerClass,
    TwoClockFit, Unit, Verdict, Verification, VerificationReport, WindowType,
};
use paraeq_dsp::authority::{
    build_authority, AuthorityPolicy, Clamp, COUPLER_Q_CEILING, ROOM_Q_CEILING,
};
use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::targets::TargetCurve;
use paraeq_dsp::PerChannel;

pub fn decision<T>(
    value: T,
    domain: Domain<T>,
    key: RationaleKey,
    invalidates: Invalidation,
) -> Decision<T> {
    Decision {
        domain,
        evidence: Vec::new(),
        invalidates,
        rationale: Rationale {
            key,
            text: String::new(),
        },
        source: Source::Default,
        value,
    }
}

/// Built through `authority::build_authority` rather than a struct literal,
/// because as of Stage 5 that is the only way to build one: `AuthorityCurve`
/// is a re-export of the sealed `paraeq_dsp::authority` type, so `decide()` and
/// its tests cannot fabricate a ceiling that skipped validation. A small grid
/// keeps the round-trip fixtures readable.
pub fn authority_curve() -> AuthorityCurve {
    let grid = LogGrid::new(20.0, 20_000.0, 1).expect("valid grid");
    let sigma = vec![1.5; grid.len()];
    build_authority(&grid, &sigma, &AuthorityPolicy::default()).expect("valid authority inputs")
}

pub fn decisions() -> Decisions {
    Decisions {
        align_spl_band: decision(
            (500.0, 2000.0),
            Domain::Range {
                max: (8000.0, 8000.0),
                min: (100.0, 100.0),
                step: None,
            },
            RationaleKey::AlignSplBand,
            Invalidation::Reanalyze,
        ),
        authority: decision(
            AuthorityPreset::Standard,
            Domain::Choice(vec![
                AuthorityPreset::Conservative,
                AuthorityPreset::Custom(authority_curve()),
                AuthorityPreset::Standard,
            ]),
            RationaleKey::Authority,
            Invalidation::Redesign,
        ),
        averaging: decision(
            AveragingMode::Power,
            Domain::Choice(vec![AveragingMode::DbMean, AveragingMode::Power]),
            RationaleKey::Averaging,
            Invalidation::Reanalyze,
        ),
        class: decision(
            TransducerClass::Bookshelf,
            Domain::Choice(vec![
                TransducerClass::Bookshelf,
                TransducerClass::Floorstander,
                TransducerClass::InEar,
                TransducerClass::OverEar,
            ]),
            RationaleKey::Class,
            Invalidation::Recapture,
        ),
        clock_adjust: decision(
            true,
            Domain::Choice(vec![false, true]),
            RationaleKey::ClockAdjust,
            Invalidation::Recapture,
        ),
        correction_kind: decision(
            CorrectionForm::Peq,
            Domain::Choice(vec![CorrectionForm::MinPhaseFir, CorrectionForm::Peq]),
            RationaleKey::CorrectionKind,
            Invalidation::Redesign,
        ),
        correction_range: decision(
            (38.0, 20000.0),
            Domain::Range {
                max: (20000.0, 20000.0),
                min: (20.0, 20.0),
                step: None,
            },
            RationaleKey::CorrectionRange,
            Invalidation::Redesign,
        ),
        fdw_post_cycles: decision(
            15.0,
            Domain::Range {
                max: 61.0,
                min: 3.0,
                step: None,
            },
            RationaleKey::FdwPostCycles,
            Invalidation::Reanalyze,
        ),
        fdw_pre_cycles: decision(
            3.0,
            Domain::Range {
                max: 61.0,
                min: 1.0,
                step: None,
            },
            RationaleKey::FdwPreCycles,
            Invalidation::Reanalyze,
        ),
        flatness_target_db: decision(
            3.0,
            Domain::Range {
                max: 6.0,
                min: 0.5,
                step: None,
            },
            RationaleKey::FlatnessTargetDb,
            Invalidation::Redesign,
        ),
        left_window_ms: decision(
            46.0,
            Domain::Range {
                max: 46.0,
                min: 1.0,
                step: None,
            },
            RationaleKey::LeftWindowMs,
            Invalidation::Reanalyze,
        ),
        low_corner_hz: decision(
            38.0,
            Domain::Derived,
            RationaleKey::LowCornerHz,
            Invalidation::Reanalyze,
        ),
        max_filters: decision(
            10,
            Domain::Range {
                max: 20,
                min: 1,
                step: None,
            },
            RationaleKey::MaxFilters,
            Invalidation::Redesign,
        ),
        positions_n: decision(
            9,
            Domain::Range {
                max: 15,
                min: 3,
                step: Some(1),
            },
            RationaleKey::PositionsN,
            Invalidation::Recapture,
        ),
        preamp_db: decision(
            -4.2,
            Domain::Derived,
            RationaleKey::PreampDb,
            Invalidation::Redesign,
        ),
        // D-C: a two-item `Choice` over the two path policies, read from
        // `paraeq_dsp::authority` rather than spelled as literals. The `Range`
        // form this replaced answered `contains == false` for the room path's
        // own value, which made the domain invariant unfalsifiable.
        q_cap: decision(
            ROOM_Q_CEILING,
            Domain::Choice(vec![COUPLER_Q_CEILING, ROOM_Q_CEILING]),
            RationaleKey::QCap,
            Invalidation::Redesign,
        ),
        right_window_ms: decision(
            400.0,
            Domain::Range {
                max: 1000.0,
                min: 100.0,
                step: None,
            },
            RationaleKey::RightWindowMs,
            Invalidation::Reanalyze,
        ),
        shelves: decision(
            true,
            Domain::Choice(vec![false, true]),
            RationaleKey::Shelves,
            Invalidation::Redesign,
        ),
        smoothing: decision(
            SmoothingMode::Variable,
            Domain::Choice(vec![SmoothingMode::Fixed(6), SmoothingMode::Variable]),
            RationaleKey::Smoothing,
            Invalidation::Reanalyze,
        ),
        target: decision(
            TargetChoice::Parametric {
                shelf_db: 3.0,
                shelf_fc: 105.0,
                shelf_q: 0.71,
                tilt_db_per_oct: -0.9,
            },
            Domain::Choice(vec![
                TargetChoice::Curve {
                    name: "bk_1974".to_string(),
                },
                TargetChoice::Parametric {
                    shelf_db: 3.0,
                    shelf_fc: 105.0,
                    shelf_q: 0.71,
                    tilt_db_per_oct: -0.9,
                },
            ]),
            RationaleKey::TargetRoomParametric,
            Invalidation::Reanalyze,
        ),
        transition_hz: decision(
            200.0,
            Domain::Range {
                max: 400.0,
                min: 80.0,
                step: None,
            },
            RationaleKey::TransitionHz,
            Invalidation::Reanalyze,
        ),
        window_type: decision(
            WindowType::Tukey(0.25),
            Domain::Choice(vec![
                WindowType::BlackmanHarris,
                WindowType::Hann,
                WindowType::Rect,
                WindowType::Tukey(0.25),
            ]),
            RationaleKey::WindowType,
            Invalidation::Reanalyze,
        ),
    }
}

pub fn bundle() -> MeasurementBundle {
    MeasurementBundle {
        cal: Some(CalFile {
            content: "\"Sens Factor =-.9dB\"\n20.000\t-3.13\n".to_string(),
            curve: (vec![20.0, 1000.0], vec![-3.13, 0.0]),
            gain_db: None,
            sensitivity_db: Some(-0.9),
            serial: Some("7005770".to_string()),
            variant: CalVariant::Plain,
        }),
        capture: CapturePlan {
            chain_sensitivity_spl_per_dbfs: Some(104.0),
            clock_adjusted: true,
            clock_skew_ppm: Some(12.5),
            input_present: true,
            input_rate: 48000,
            input_uid: "UMIK-1:7005770".to_string(),
            output_rate: 48000,
            output_uid: "BuiltInSpeakerDevice".to_string(),
            self_excluded: true,
            sweep: SweepPlan {
                duration_s: 5.5,
                f_end_hz: 20000.0,
                f_start_hz: 30.0,
                level_dbfs: -12.0,
            },
            sweep_rate: 48000,
        },
        class: TransducerClass::Bookshelf,
        noise_floor: NoiseFloor {
            freqs_hz: vec![20.0, 1000.0],
            rms_dbfs: vec![-62.0, -61.5],
            spectrum_db: vec![vec![-70.0, -80.0], vec![-71.0, -81.0]],
        },
        overrides: Overrides::default(),
        positions: vec![Position {
            capture: capture_stats(),
            captured_at_ms: 1_752_537_600_000,
            index: 0,
            ir: ImpulseResponse {
                peak: 2304,
                sample_rate: 48000,
                samples: vec![vec![0.0, 1.0, 0.0], vec![0.0, 0.9, 0.0]],
            },
            label: "Position 1 (primary seat)".to_string(),
            routing: CaptureRouting::Both,
        }],
        targets: vec![TargetCurve {
            name: "bk_1974".to_string(),
            frequencies: vec![20.0, 20000.0],
            gains_db: vec![6.0, -6.0],
            category: None,
            classes: vec![TransducerClass::Bookshelf, TransducerClass::Floorstander],
            description: None,
            source: None,
        }],
        verification: None,
    }
}

/// A populated capture meter readout. Finite by construction: a digitally
/// silent pass would carry `f64::NEG_INFINITY`, which serde_json writes as
/// `null` and cannot read back (see [`paraeq_decide::CaptureStats`]).
pub fn capture_stats() -> CaptureStats {
    CaptureStats {
        clipped_samples: 0,
        peak_dbfs: -6.2,
        rms_dbfs: -20.0,
    }
}

/// A bundle carrying a populated verification pass, in the final shape.
pub fn bundle_with_verification() -> MeasurementBundle {
    MeasurementBundle {
        verification: Some(verification()),
        ..bundle()
    }
}

pub fn verification() -> Verification {
    Verification {
        capture: capture_stats(),
        gain_db: 0.0,
        installed: correction_plan(),
        // Differs from `installed.preamp_db` on purpose: the engine computes at
        // the live rate over the surviving bands, folded min across channels.
        installed_preamp_db: -4.1,
        ir: ImpulseResponse {
            peak: 2304,
            sample_rate: 48000,
            samples: vec![vec![0.0, 0.8, 0.0], vec![0.0, 0.7, 0.0]],
        },
        level_dbfs: -16.2,
        position_index: 0,
        routing: CaptureRouting::Both,
        running_rate_hz: 48000.0,
        two_clock: Some(TwoClockFit {
            intercept_samples: 2304.5,
            residual_peak_samples: 0.8,
            residual_rms_samples: 0.31,
            skew_ppm: 12.5,
        }),
    }
}

pub fn correction_plan() -> CorrectionPlan {
    CorrectionPlan {
        bands: PerChannel::new(vec![vec![paraeq_dsp::peq::EQBand {
            filter_type: paraeq_dsp::peq::FilterType::Peaking,
            fc: 47.0,
            gain_db: -9.1,
            q: 4.0,
        }]])
        .expect("one channel is not empty"),
        clamps: vec![vec![
            Clamp::BelowMinGain {
                fc: 120.0,
                gain_db: 1.2,
            },
            Clamp::GainToSigma {
                from: 6.0,
                to: 1.0,
                sigma_db: 4.2,
            },
        ]],
        design_rate: 48000.0,
        dropped: vec![3],
        preamp_db: -4.2,
    }
}

pub fn verification_report() -> VerificationReport {
    VerificationReport {
        evidence: vec![Evidence::Scalar {
            label: EvidenceLabel::ResidualVsPrediction,
            unit: Unit::Db,
            value: 0.4,
        }],
        gate_db: 6.0,
        residual_rms_db: 0.4,
    }
}

pub fn decision_set() -> DecisionSet {
    DecisionSet {
        analysis: Analysis {
            authority: authority_curve(),
            averaged_db: vec![vec![0.0, -1.0], vec![0.1, -1.1]],
            excess_group_delay_s: vec![0.001, 0.0005],
            freqs_hz: vec![20.0, 1000.0],
            per_position_db: vec![vec![0.0, -1.0]],
            sigma_db: vec![0.7, 4.2],
        },
        correction: Some(correction_plan()),
        decisions: decisions(),
        diagnostics: vec![Diagnostic {
            code: DiagnosticCode::TwoClock,
            position: None,
            remedy: "Your mic and speakers run on different clocks.".to_string(),
            severity: Severity::Warn,
            value: None,
        }],
        verdict: Verdict::ProceedWithWarnings,
        verification: Some(verification_report()),
    }
}

pub fn evidence() -> Vec<Evidence> {
    vec![
        Evidence::Curve {
            db: vec![0.7, 4.2],
            hz: vec![20.0, 1000.0],
            label: EvidenceLabel::Sigma,
        },
        Evidence::Scalar {
            label: EvidenceLabel::MidbandLevel,
            unit: Unit::Db,
            value: 82.4,
        },
        Evidence::Span {
            hz_hi: 20000.0,
            hz_lo: 200.0,
            label: EvidenceLabel::AuthoritySplit,
        },
        // The two units B6 added, each on the label that needs it.
        Evidence::Scalar {
            label: EvidenceLabel::TwoClockSkewPpm,
            unit: Unit::PartsPerMillion,
            value: 12.5,
        },
        Evidence::Scalar {
            label: EvidenceLabel::TwoClockResidual,
            unit: Unit::Samples,
            value: 0.31,
        },
    ]
}

// ---------------------------------------------------------------------------
// Synthetic bundles for the invariant and property tests (B7a).
//
// Still NOT policy. These build a PLAUSIBLE input — an impulse response whose
// direct arrival sits where this architecture actually puts it (~46 ms: tap
// latency plus propagation), followed by reflections and a decaying tail — so
// that `decide()`'s analysis stage is exercised on data with real structure
// rather than on a three-sample stub. Nothing here asserts what `decide()`
// should choose.
//
// Determinism: the "randomness" is an xorshift64 seeded by the caller, so a
// proptest seed reproduces a bundle exactly. `decide()` itself draws no
// randomness at all; this is the TEST's generator, not the engine's.
// ---------------------------------------------------------------------------

/// The shape of one synthetic bundle. Fields alphabetical, per repo
/// convention.
#[derive(Clone, Copy, Debug)]
pub struct SyntheticSpec {
    /// Whether a mic calibration file is present. `None` is a refusal
    /// condition B7c owns; the analysis stage must handle both.
    pub cal: bool,
    /// 1 (mono) or 2 (stereo). The seam the `PerChannel` container exists for.
    pub channels: usize,
    pub class: TransducerClass,
    pub positions: usize,
    pub sample_rate: u32,
    pub seed: u64,
}

impl Default for SyntheticSpec {
    /// A nine-position stereo bookshelf run at 48 kHz with a cal file: the
    /// room path's own default shape (`PathProfile::positions_default`).
    fn default() -> Self {
        SyntheticSpec {
            cal: true,
            channels: 2,
            class: TransducerClass::Bookshelf,
            positions: 9,
            sample_rate: 48_000,
            seed: 0x5EED_1234_ABCD_0001,
        }
    }
}

/// xorshift64*, returning a value in `[-1.0, 1.0)`. Three lines of shift-xor
/// with no dependency: the workspace has no RNG crate and this file must not
/// add one for a test generator.
fn noise(state: &mut u64) -> f64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    // 53 bits is f64's mantissa, so the division is exact.
    ((*state >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
}

/// One channel of a plausible impulse response: a direct arrival at `peak`,
/// eight reflections inside the post-peak window, and an exponentially
/// decaying noise tail. Finite by construction — a non-finite sample is a
/// refusal condition, not an analysis input.
fn impulse(peak: usize, len: usize, state: &mut u64) -> Vec<f64> {
    let mut h = vec![0.0; len];
    h[peak] = 1.0;
    let post = len - peak;
    for k in 1..=8 {
        // Spread the reflections over the first half of the post-peak window.
        let delay = (post * k) / 20;
        if delay == 0 || peak + delay >= len {
            continue;
        }
        let decay = (-(k as f64) / 3.0).exp();
        h[peak + delay] += 0.4 * decay * noise(state);
    }
    for (offset, sample) in h[peak..].iter_mut().enumerate() {
        let t = offset as f64 / post as f64;
        *sample += 0.02 * (-4.0 * t).exp() * noise(state);
    }
    h
}

/// A plausible bundle for [`SyntheticSpec`]. `positions == 0` yields an empty
/// position list, which the analysis stage must survive without panicking.
pub fn synthetic_bundle(spec: SyntheticSpec) -> MeasurementBundle {
    let profile = paraeq_decide::profile_for(spec.class);
    let rate = spec.sample_rate;
    // ~46 ms: "Direct-arrival index … ~46–64 ms into the capture on this
    // architecture: tap latency + propagation" (`ImpulseResponse::peak`).
    let peak = (0.046 * f64::from(rate)).round() as usize;
    // 1100 ms of post-peak data: the decision-engine spec's own derivation of
    // the store window, `[peak − 100 ms, peak + 1100 ms]`, and long enough that
    // the coupler path's ungated 1000 ms right window fits inside the recording
    // rather than being bounded by it. (The SHIPPED store window is +1500 ms,
    // per the Stage-5 amendment; the shorter figure keeps a nine-position
    // stereo 96 kHz fixture from being 40 MB of f64 in a debug test.)
    let len = peak + (1.100 * f64::from(rate)).round() as usize;

    let mut state = spec.seed | 1; // xorshift64 has a fixed point at 0.
    let positions = (0..spec.positions)
        .map(|index| Position {
            capture: capture_stats(),
            captured_at_ms: 1_752_537_600_000 + index as u64 * 30_000,
            index,
            ir: ImpulseResponse {
                peak,
                sample_rate: rate,
                samples: (0..spec.channels)
                    .map(|_| impulse(peak, len, &mut state))
                    .collect(),
            },
            label: format!("Position {}", index + 1),
            routing: CaptureRouting::Both,
        })
        .collect();

    MeasurementBundle {
        cal: spec.cal.then(|| CalFile {
            content: "20.000\t-3.13\n20000.000\t1.20\n".to_string(),
            curve: (vec![20.0, 20_000.0], vec![-3.13, 1.20]),
            gain_db: None,
            sensitivity_db: Some(-0.9),
            serial: Some("7005770".to_string()),
            variant: CalVariant::Plain,
        }),
        capture: CapturePlan {
            chain_sensitivity_spl_per_dbfs: Some(104.0),
            clock_adjusted: true,
            clock_skew_ppm: Some(12.5),
            input_present: true,
            input_rate: rate,
            input_uid: "UMIK-1:7005770".to_string(),
            output_rate: rate,
            output_uid: "BuiltInSpeakerDevice".to_string(),
            self_excluded: true,
            sweep: SweepPlan {
                duration_s: 5.5,
                f_end_hz: 20_000.0,
                f_start_hz: profile.sweep_f_start_hz,
                level_dbfs: -12.0,
            },
            sweep_rate: rate,
        },
        class: spec.class,
        noise_floor: NoiseFloor {
            freqs_hz: vec![20.0, 1000.0, 20_000.0],
            rms_dbfs: vec![-62.0; spec.channels.max(1)],
            spectrum_db: vec![vec![-70.0, -80.0, -85.0]; spec.channels.max(1)],
        },
        overrides: Overrides::default(),
        positions,
        targets: synthetic_targets(),
        verification: None,
    }
}

/// One candidate curve per transducer class, so the `target` decision's
/// class-filtered `Choice` domain is never empty on any of the four paths.
pub fn synthetic_targets() -> Vec<TargetCurve> {
    vec![
        TargetCurve {
            name: "bk_1974".to_string(),
            frequencies: vec![20.0, 1000.0, 20_000.0],
            gains_db: vec![6.0, 0.0, -6.0],
            category: None,
            classes: vec![TransducerClass::Bookshelf, TransducerClass::Floorstander],
            description: None,
            source: None,
        },
        TargetCurve {
            name: "flat".to_string(),
            frequencies: vec![20.0, 1000.0, 20_000.0],
            gains_db: vec![0.0, 0.0, 0.0],
            category: None,
            classes: vec![
                TransducerClass::Bookshelf,
                TransducerClass::Floorstander,
                TransducerClass::InEar,
                TransducerClass::OverEar,
            ],
            description: None,
            source: None,
        },
        TargetCurve {
            name: "harman_oe_2018".to_string(),
            frequencies: vec![20.0, 1000.0, 20_000.0],
            gains_db: vec![4.0, 0.0, -3.0],
            category: None,
            classes: vec![TransducerClass::OverEar],
            description: None,
            source: None,
        },
        TargetCurve {
            name: "harman_ie_2019".to_string(),
            frequencies: vec![20.0, 1000.0, 20_000.0],
            gains_db: vec![5.0, 0.0, -4.0],
            category: None,
            classes: vec![TransducerClass::InEar],
            description: None,
            source: None,
        },
    ]
}

/// The four classes, in the order `TransducerClass` declares them.
pub const EVERY_CLASS: [TransducerClass; 4] = [
    TransducerClass::Bookshelf,
    TransducerClass::Floorstander,
    TransducerClass::InEar,
    TransducerClass::OverEar,
];

/// A well-formed bundle for `class`: the path's own default position count, so
/// `positions_n` echoes a value inside `PathProfile::positions_domain`.
pub fn well_formed_bundle(class: TransducerClass) -> MeasurementBundle {
    synthetic_bundle(SyntheticSpec {
        class,
        positions: paraeq_decide::profile_for(class).positions_default,
        ..SyntheticSpec::default()
    })
}

/// The smallest bundle each path still analyses: three positions (the decision
/// table's own hard minimum, and `positions_domain`'s lower bound on both
/// paths) and one channel.
///
/// For properties that are about the OVERRIDE path rather than about the
/// capture — idempotence above all — the capture's size buys nothing and costs
/// a full analysis pass per assertion. The channel count and the position count
/// are varied in `test_props.rs` instead, where one generated field is
/// overridden per case.
pub fn minimal_bundle(class: TransducerClass) -> MeasurementBundle {
    synthetic_bundle(SyntheticSpec {
        channels: 1,
        class,
        positions: 3,
        ..SyntheticSpec::default()
    })
}

/// Replace `bundle.overrides.<id>` with `value`, through serde rather than 22
/// match arms.
///
/// `DecisionView::id` IS the `Overrides` field name — guaranteed by
/// `overrides_mirrors_decisions_field_for_field` — so this cannot fall behind a
/// 23rd decision, which a hand-written match would.
pub fn with_override(
    bundle: &MeasurementBundle,
    id: &str,
    value: serde_json::Value,
) -> MeasurementBundle {
    let mut raw =
        serde_json::to_value(Overrides::default()).expect("Overrides is plain derived data");
    raw[id] = value;
    let overrides: Overrides =
        serde_json::from_value(raw).expect("an auto value is a legal override value");
    MeasurementBundle {
        overrides,
        ..bundle.clone()
    }
}

// ---------------------------------------------------------------------------
// Shared invariant assertions (B7a). They live here rather than in
// `test_invariants.rs` because every integration test file is its own crate:
// `test_props.rs` cannot call a `pub fn` declared in `test_invariants.rs`.
// ---------------------------------------------------------------------------

fn in_domain<T>(id: &str, class: TransducerClass, d: &Decision<T>)
where
    T: paraeq_decide::decision::InRange + PartialEq + std::fmt::Debug,
{
    assert!(
        d.domain.contains(&d.value),
        "{class:?}: {id} = {:?} is outside its domain {:?}",
        d.value,
        d.domain
    );
}

/// The spec's "every `Decision.value` is inside its `Domain`" invariant,
/// written out field by field.
///
/// Not routed through `Decisions::iter()` on purpose: the erased view carries
/// `domain` and `value` as `serde_json::Value`, and comparing those would
/// re-implement `Domain::contains` in JSON instead of testing it.
pub fn assert_every_value_in_domain(d: &Decisions, class: TransducerClass) {
    in_domain("align_spl_band", class, &d.align_spl_band);
    in_domain("authority", class, &d.authority);
    in_domain("averaging", class, &d.averaging);
    in_domain("class", class, &d.class);
    in_domain("clock_adjust", class, &d.clock_adjust);
    in_domain("correction_kind", class, &d.correction_kind);
    in_domain("correction_range", class, &d.correction_range);
    in_domain("fdw_post_cycles", class, &d.fdw_post_cycles);
    in_domain("fdw_pre_cycles", class, &d.fdw_pre_cycles);
    in_domain("flatness_target_db", class, &d.flatness_target_db);
    in_domain("left_window_ms", class, &d.left_window_ms);
    in_domain("low_corner_hz", class, &d.low_corner_hz);
    in_domain("max_filters", class, &d.max_filters);
    in_domain("positions_n", class, &d.positions_n);
    in_domain("preamp_db", class, &d.preamp_db);
    in_domain("q_cap", class, &d.q_cap);
    in_domain("right_window_ms", class, &d.right_window_ms);
    in_domain("shelves", class, &d.shelves);
    in_domain("smoothing", class, &d.smoothing);
    in_domain("target", class, &d.target);
    in_domain("transition_hz", class, &d.transition_hz);
    in_domain("window_type", class, &d.window_type);
}

/// Both halves of the spec's refusal invariant — "`verdict == Refuse ⟺
/// correction.is_none()`" and "`verdict == Refuse ⟺ any diagnostic has
/// `Severity::Refuse`" — plus the `ProceedWithWarnings` leg that makes the
/// three-way verdict total rather than a two-way one with a spare variant.
pub fn assert_refusal_is_consistent(set: &DecisionSet) {
    let refusing = set
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Refuse);
    let warning = set.diagnostics.iter().any(|d| d.severity == Severity::Warn);
    assert_eq!(
        set.verdict == Verdict::Refuse,
        set.correction.is_none(),
        "verdict {:?} against correction.is_none() = {}",
        set.verdict,
        set.correction.is_none()
    );
    assert_eq!(
        set.verdict == Verdict::Refuse,
        refusing,
        "verdict {:?} against a Refuse-severity diagnostic being present = {refusing}",
        set.verdict
    );
    if !refusing {
        assert_eq!(
            set.verdict == Verdict::ProceedWithWarnings,
            warning,
            "verdict {:?} against a Warn diagnostic being present = {warning}",
            set.verdict
        );
    }
}

// ---------------------------------------------------------------------------
// Shaped bundles for the RULE tests (B7b) — additive; nothing above changes.
//
// The generator above builds a plausible but arbitrary response, which is what
// the invariants need and exactly what a rule test cannot use: "the low corner
// is 38 Hz" is only checkable against a capture whose low corner IS 38 Hz.
// These build an impulse response with a KNOWN magnitude response, by filtering
// a unit impulse through a biquad cascade the caller names — so the oracle is
// the filter's own closed form, not a second copy of the rule.
// ---------------------------------------------------------------------------

/// [`synthetic_bundle`] with every position's impulse response replaced by a
/// unit impulse filtered through `shape(position.index)`.
///
/// Noiseless and identical across channels on purpose: the only variation
/// between positions is the one `shape` introduces, so σ(f) is a quantity the
/// test chose rather than a property of a random tail. An empty cascade is the
/// identity, which gives every position the same flat response and σ ≈ 0.
///
/// **The cal is FLAT here, where [`synthetic_bundle`]'s is a vendor curve.**
/// Both are well-formed files; the difference is what a shaped bundle promises.
/// Compensation SUBTRACTS the cal from the measured magnitude, so a vendor
/// curve would put a second shape on the analysed curve that no test asked for
/// and that the closed form above would not predict. Zero at every point
/// subtracts exactly zero (`m - 0.0 == m`), so the analysed curve is the
/// cascade's magnitude response and nothing else.
///
/// It is a flat FILE rather than `spec.cal = false`, because a bundle carrying
/// no cal is a refusal — `DiagnosticCode::CalMissing`, `Severity::Refuse` — and
/// a refused bundle carries `correction: None`, so a rule test written against
/// one can assert nothing about the filters the rule emitted. A mic whose
/// calibration file reads 0.00 dB everywhere is a capture the shipped product
/// accepts; a mic with no calibration file at all is one it turns away.
pub fn shaped_bundle(
    spec: SyntheticSpec,
    shape: impl Fn(usize) -> Vec<[f64; 6]>,
) -> MeasurementBundle {
    let mut bundle = synthetic_bundle(spec);
    if bundle.cal.is_some() {
        bundle.cal = Some(cal_with_curve(vec![20.0, 20_000.0], vec![0.0, 0.0]));
    }
    for position in &mut bundle.positions {
        let sos = shape(position.index);
        let peak = position.ir.peak;
        let len = position.ir.samples[0].len();
        let mut stimulus = vec![0.0; len];
        stimulus[peak] = 1.0;
        let filtered = paraeq_dsp::peq::sosfilt(&sos, &stimulus);
        for channel in &mut position.ir.samples {
            channel.clone_from(&filtered);
        }
    }
    bundle
}

// Refusal-table helpers (B7c). Additive: nothing above this line moved.
//
// The refusal rows are graded against the ANALYSED curve, not against raw
// samples, so a test that wants "this position's bass is 8 dB low" has to make
// the analysis stage produce that curve. These helpers do it the only honest
// way: drive a unit delta through a biquad cascade whose magnitude response IS
// the shape under test, and let the real gate → spectrum → compensation →
// smoothing → Align SPL → average pipeline carry it through. A hand-written
// `averaged_db` would test the assertion against itself.
// ---------------------------------------------------------------------------

/// The impulse response of a biquad cascade: a unit delta at `peak` driven
/// through `sections` in series, Direct Form I. An empty cascade gives the bare
/// delta, whose magnitude response is flat — the blank canvas every shaped
/// bundle starts from.
///
/// Rows are `[b0, b1, b2, 1, a1, a2]`, already normalized by `a0`
/// (`crates/paraeq-dsp/src/biquad.rs`), so the recurrence is
/// `y[n] = b0·x[n] + b1·x[n−1] + b2·x[n−2] − a1·y[n−1] − a2·y[n−2]`.
pub fn sos_impulse(sections: &[[f64; 6]], peak: usize, len: usize) -> Vec<f64> {
    let mut signal = vec![0.0; len];
    signal[peak] = 1.0;
    for row in sections {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for sample in signal.iter_mut() {
            let x0 = *sample;
            let y0 = row[0] * x0 + row[1] * x1 + row[2] * x2 - row[4] * y1 - row[5] * y2;
            (x2, x1) = (x1, x0);
            (y2, y1) = (y1, y0);
            *sample = y0;
        }
    }
    signal
}

/// A mono bundle whose position `p` has the magnitude response of `shapes[p]`.
///
/// Everything except the impulse responses comes from [`synthetic_bundle`], so
/// the capture plan, the cal and the noise floor are the same nominal values
/// every other test uses and only the curve under test differs.
pub fn shaped_positions(class: TransducerClass, shapes: &[Vec<[f64; 6]>]) -> MeasurementBundle {
    let mut bundle = synthetic_bundle(SyntheticSpec {
        channels: 1,
        class,
        positions: shapes.len(),
        ..SyntheticSpec::default()
    });
    for (position, sections) in bundle.positions.iter_mut().zip(shapes) {
        let peak = position.ir.peak;
        let len = position.ir.samples[0].len();
        for channel in position.ir.samples.iter_mut() {
            *channel = sos_impulse(sections, peak, len);
        }
    }
    bundle
}

/// One target curve, on the three-point grid [`synthetic_targets`] uses, legal
/// for every class — so a test can name the exact shape the `target` rows and
/// `correction_range`'s high edge are graded against.
pub fn target_curve(name: &str, gains_db: [f64; 3]) -> TargetCurve {
    TargetCurve {
        name: name.to_string(),
        frequencies: vec![20.0, 1000.0, 20_000.0],
        gains_db: gains_db.to_vec(),
        category: None,
        classes: EVERY_CLASS.to_vec(),
        description: None,
        source: None,
    }
}

/// The `AuthorityPolicy` `decide()` composes for `class`, rebuilt from the
/// public `PathProfile` fields.
///
/// A test-side mirror of `analysis::authority_policy`, which is `pub(crate)`.
/// It exists so a test can re-derive the authority curve from σ(f) ALONE and
/// compare — which is how "the EGD trace changes nothing" is checked without an
/// injection point the public API does not have.
pub fn authority_policy_for(class: TransducerClass) -> AuthorityPolicy {
    let profile = paraeq_decide::profile_for(class);
    let base = match profile.coupling {
        CouplingPath::Coupler => AuthorityPolicy::coupler(),
        CouplingPath::Room => AuthorityPolicy::room(),
    };
    AuthorityPolicy {
        boost_ratio: profile.boost_ratio,
        excursion: profile.excursion_breakpoints_db.to_vec(),
        q_ceiling: profile.q_cap,
        ..base
    }
}

/// `n` positions of bare delta: the flattest analysed curve this pipeline can
/// produce, and therefore the one a shaped test perturbs from.
pub fn flat_bundle(class: TransducerClass, positions: usize) -> MeasurementBundle {
    shaped_positions(class, &vec![Vec::new(); positions])
}

/// A cal file carrying `curve` verbatim. The compensation stage subtracts it
/// from every position, so a cal is also the cheapest way to give the averaged
/// curve a known shape that is the SAME at every position — which is what the
/// two whole-curve rows (`AbsurdCurve`) are graded on.
pub fn cal_with_curve(freqs_hz: Vec<f64>, gains_db: Vec<f64>) -> CalFile {
    CalFile {
        content: String::new(),
        curve: (freqs_hz, gains_db),
        gain_db: None,
        sensitivity_db: Some(-0.9),
        serial: Some("7005770".to_string()),
        variant: CalVariant::Plain,
    }
}

/// Every diagnostic carrying `code`, in the order `decide()` emitted them.
pub fn diagnostics_with(set: &DecisionSet, code: DiagnosticCode) -> Vec<&Diagnostic> {
    set.diagnostics.iter().filter(|d| d.code == code).collect()
}

/// Whether `code` fired at all.
pub fn has_code(set: &DecisionSet, code: DiagnosticCode) -> bool {
    !diagnostics_with(set, code).is_empty()
}

/// The one diagnostic carrying `code`. Panics with the whole diagnostic list
/// when there is not exactly one, because "which codes actually fired" is the
/// thing a failing refusal test needs to see.
pub fn only_diagnostic(set: &DecisionSet, code: DiagnosticCode) -> &Diagnostic {
    let found = diagnostics_with(set, code);
    assert_eq!(
        found.len(),
        1,
        "expected exactly one {code:?}; diagnostics were {:?}",
        set.diagnostics
    );
    found[0]
}
