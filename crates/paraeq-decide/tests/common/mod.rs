//! Fixture values for the type-contract tests. These are NOT policy: nothing
//! here is what `decide()` chooses, and no test asserts they are. They exist
//! to give the wire types a populated value to round-trip and to give
//! `Decisions::iter()` something to enumerate. Stage 6 owns the rules.

#![allow(dead_code)]

use paraeq_decide::{
    Analysis, AuthorityCurve, AveragingMode, CalFile, CalVariant, CapturePlan, CorrectionKind,
    CorrectionPlan, Decision, DecisionSet, Decisions, Diagnostic, DiagnosticCode, Domain, Evidence,
    EvidenceLabel, ImpulseResponse, Invalidation, MeasurementBundle, NoiseFloor, Overrides,
    Position, QCapPolicy, Rationale, RationaleKey, Severity, SmoothingMode, Source, SweepPlan,
    TargetChoice, TransducerClass, Unit, Verdict, WindowType,
};
use paraeq_dsp::authority::{build_authority, AuthorityPolicy};
use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::targets::TargetCurve;

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
            authority_curve(),
            Domain::Choice(vec![authority_curve()]),
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
        correction_kind: decision(
            CorrectionKind::Peq,
            Domain::Choice(vec![CorrectionKind::MinPhaseFir, CorrectionKind::Peq]),
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
        q_cap: decision(
            QCapPolicy::LogLinear {
                hi: (10000.0, 3.0),
                lo: (200.0, 10.0),
            },
            Domain::Range {
                max: QCapPolicy::Ceiling(20.0),
                min: QCapPolicy::Ceiling(1.0),
                step: None,
            },
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
            Domain::Choice(vec![TargetChoice::Curve {
                name: "bk_1974".to_string(),
            }]),
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
            captured_at_ms: 1_752_537_600_000,
            index: 0,
            ir: ImpulseResponse {
                peak: 2304,
                sample_rate: 48000,
                samples: vec![vec![0.0, 1.0, 0.0], vec![0.0, 0.9, 0.0]],
            },
            label: "Position 1 (primary seat)".to_string(),
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

pub fn decision_set() -> DecisionSet {
    DecisionSet {
        analysis: Analysis {
            averaged_db: vec![vec![0.0, -1.0], vec![0.1, -1.1]],
            excess_group_delay_s: vec![0.001, 0.0005],
            freqs_hz: vec![20.0, 1000.0],
            per_position_db: vec![vec![0.0, -1.0]],
            sigma_db: vec![0.7, 4.2],
        },
        correction: Some(CorrectionPlan {
            bands: vec![vec![paraeq_dsp::peq::EQBand {
                filter_type: paraeq_dsp::peq::FilterType::Peaking,
                fc: 47.0,
                gain_db: -9.1,
                q: 4.0,
            }]],
            design_rate: 48000.0,
            preamp_db: -4.2,
        }),
        decisions: decisions(),
        diagnostics: vec![Diagnostic {
            code: DiagnosticCode::TwoClock,
            position: None,
            remedy: "Your mic and speakers run on different clocks.".to_string(),
            severity: Severity::Warn,
            value: None,
        }],
        verdict: Verdict::ProceedWithWarnings,
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
    ]
}
