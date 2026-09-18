//! MS-3 (post-fade assertion set) + MS-4 (assembly and the emit guard).
//!
//! Tier 3 (analytic/policy) per the four-tier convention: the pipeline's
//! correctness properties are closed-form invariants — exact-zero endpoints,
//! exact RMS at the solved level, |mean| under the DC gate, bounded peak,
//! all-finite — with no oracle to port (`generate_sweep`'s Tier 1 fixture
//! parity is untouched in `paraeq-dsp`). The headline case is the spec's own
//! measured worst case: the 5 s / 20 Hz–20 kHz / 48 kHz sweep that carries
//! 1.24e-3 of DC after the policy fade (§ Fade and DC, OPEN block), which the
//! envelope-shaped DC-block must bring under 1e-4 at both class levels.

use paraeq_dsp::targets::TransducerClass;
use paraeq_measure::stimulus::{
    DC_GATE_ABS_MEAN, FADE_IN_MS, FADE_OUT_MS, PILOT_FREQ_HZ, PILOT_LEVEL_DBFS_RMS,
    PILOT_MAX_DURATION_S,
};
use paraeq_measure::{
    assemble_pilot, assemble_sweep, emit_guard, verify_stimulus, AssembledStimulus, MeasureError,
    MeasurementDiagnostic as D, StimulusError, StimulusKind, StimulusSink, StreamFormat,
    SweepLevel,
};

fn mean(x: &[f64]) -> f64 {
    x.iter().sum::<f64>() / x.len() as f64
}

fn rms_dbfs(x: &[f64]) -> f64 {
    20.0 * (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64)
        .sqrt()
        .log10()
}

fn peak(x: &[f64]) -> f64 {
    x.iter().fold(0.0f64, |a, v| a.max(v.abs()))
}

/// The spec's measured worst case, at both class levels the caps table
/// carries. Un-DC-blocked, this sweep misses the gate at every level (1.24e-3
/// faded; 1.76e-4 at −20 dBFS, 4.42e-4 at −12 dBFS — spec § Fade and DC);
/// the envelope-shaped DC-block must bring it under 1e-4 at both.
#[test]
fn the_specs_measured_sweep_passes_the_dc_gate_at_both_class_levels() {
    for (class, dbfs) in [
        (TransducerClass::InEar, -20.0),
        (TransducerClass::Bookshelf, -12.0),
    ] {
        let level = SweepLevel::new(dbfs, class).unwrap();
        let stim = assemble_sweep(5.0, 48_000, 20.0, 20_000.0, level).unwrap();
        let m = mean(stim.samples()).abs();
        assert!(
            m < DC_GATE_ABS_MEAN,
            "{class:?} at {dbfs} dBFS: |mean| = {m:e} misses the MS-3 gate"
        );
        // The subtraction is exact algebra, not a tuned filter: the residual
        // is fp roundoff, orders under the gate, not a near miss.
        assert!(
            m < 1e-9,
            "{class:?}: DC residual {m:e} is suspiciously large"
        );
    }
}

/// MS-3, first assertion: first and last samples exactly 0.0 — the raw-mean
/// alternative to the envelope-shaped DC-block would break precisely this.
#[test]
fn sweep_endpoints_are_exactly_zero_after_the_dc_block() {
    let level = SweepLevel::new(-20.0, TransducerClass::OverEar).unwrap();
    let stim = assemble_sweep(5.0, 48_000, 20.0, 20_000.0, level).unwrap();
    let s = stim.samples();
    assert_eq!(s[0], 0.0, "first sample must be exactly zero");
    assert_eq!(s[s.len() - 1], 0.0, "last sample must be exactly zero");
}

/// The emitted RMS is the constructed [`SweepLevel`], exactly: scaling runs
/// after fade and DC-block, so what was solved is what plays.
#[test]
fn sweep_rms_hits_the_constructed_level_exactly() {
    let level = SweepLevel::new(-20.0, TransducerClass::InEar).unwrap();
    let stim = assemble_sweep(5.0, 48_000, 20.0, 20_000.0, level).unwrap();
    assert!((rms_dbfs(stim.samples()) - -20.0).abs() < 1e-9);
}

/// MS-3, peak assertion: in range after scaling — and not marginally, because
/// the class caps put the sweep far below full scale (crest factor ≈ √2).
#[test]
fn sweep_peak_is_far_below_full_scale_at_every_class_cap() {
    for class in [
        TransducerClass::Bookshelf,
        TransducerClass::Floorstander,
        TransducerClass::InEar,
        TransducerClass::OverEar,
    ] {
        let cap = paraeq_measure::caps_for(class).sweep_level_dbfs_rms;
        let level = SweepLevel::new(cap, class).unwrap();
        let stim = assemble_sweep(5.0, 48_000, 20.0, 20_000.0, level).unwrap();
        let p = peak(stim.samples());
        assert!(p <= 1.0, "{class:?}: over full scale");
        assert!(p < 0.5, "{class:?}: peak {p} leaves too little headroom");
    }
}

#[test]
fn sweep_carries_its_provenance() {
    let level = SweepLevel::new(-12.0, TransducerClass::Floorstander).unwrap();
    let stim = assemble_sweep(5.0, 48_000, 20.0, 20_000.0, level).unwrap();
    assert_eq!(stim.kind(), StimulusKind::Sweep);
    assert_eq!(stim.level(), level);
    assert_eq!(stim.sample_rate_hz(), 48_000);
    assert_eq!(stim.len(), 240_000);
    assert!(!stim.is_empty());
    assert!((stim.duration_s() - 5.0).abs() < 1e-12);
}

// ── The pilot ─────────────────────────────────────────────────────────────

/// Spec § The Level Ladder, step 2: "300 Hz sine, −40 dBFS RMS, ≤1 s, faded."
/// The level is exact because scaling runs last.
#[test]
fn pilot_is_exactly_minus_40_dbfs_rms() {
    let stim = assemble_pilot(0.5, 48_000, TransducerClass::InEar).unwrap();
    assert_eq!(stim.kind(), StimulusKind::Pilot);
    assert_eq!(stim.level().dbfs_rms(), PILOT_LEVEL_DBFS_RMS);
    assert!((rms_dbfs(stim.samples()) - -40.0).abs() < 1e-9);
}

#[test]
fn pilot_passes_the_same_assertion_set_as_the_sweep() {
    let stim = assemble_pilot(0.5, 48_000, TransducerClass::OverEar).unwrap();
    let s = stim.samples();
    assert_eq!(s[0], 0.0);
    assert_eq!(s[s.len() - 1], 0.0);
    assert!(mean(s).abs() < DC_GATE_ABS_MEAN);
    assert!(peak(s) <= 1.0);
    assert!(s.iter().all(|v| v.is_finite()));
}

/// The sine is generated locally in the module (assembly, not oracle-bearing
/// DSP), so pin its one number analytically: 300 Hz over 0.5 s crosses zero
/// ≈ 2·f·d = 300 times.
#[test]
fn pilot_is_a_300_hz_sine_by_zero_crossing_count() {
    let stim = assemble_pilot(0.5, 48_000, TransducerClass::InEar).unwrap();
    let s = stim.samples();
    let crossings = s.windows(2).filter(|w| w[0] * w[1] < 0.0).count();
    let expected = (2.0 * PILOT_FREQ_HZ * 0.5) as usize;
    assert!(
        crossings.abs_diff(expected) <= 5,
        "{crossings} zero crossings; a {PILOT_FREQ_HZ} Hz sine over 0.5 s has ~{expected}"
    );
}

/// The duration is a validated parameter: the spec's "≤ 1 s" is an input
/// contract, not a comment.
#[test]
fn pilot_duration_is_validated() {
    assert!(assemble_pilot(1.0, 48_000, TransducerClass::InEar).is_ok());
    assert_eq!(
        assemble_pilot(1.01, 48_000, TransducerClass::InEar),
        Err(StimulusError::PilotTooLong {
            max_s: PILOT_MAX_DURATION_S,
            requested_s: 1.01,
        })
    );
    assert_eq!(
        assemble_pilot(0.0, 48_000, TransducerClass::InEar),
        Err(StimulusError::NonPositiveDuration { requested_s: 0.0 })
    );
}

/// A buffer shorter than the policy fades has no unity plateau, and the
/// envelope-shaped DC-block would divide by a vanishing mean. Refused, for
/// both kinds.
#[test]
fn a_buffer_too_short_for_the_policy_fades_is_refused() {
    let fade_in = (FADE_IN_MS / 1000.0 * 48_000.0).round() as usize;
    let fade_out = (FADE_OUT_MS / 1000.0 * 48_000.0).round() as usize;
    let err = StimulusError::TooShortForFades {
        fade_in,
        fade_out,
        len: 2_400,
    };
    assert_eq!(
        assemble_pilot(0.05, 48_000, TransducerClass::InEar),
        Err(err.clone())
    );
    let level = SweepLevel::new(-20.0, TransducerClass::InEar).unwrap();
    assert_eq!(
        assemble_sweep(0.05, 48_000, 20.0, 20_000.0, level),
        Err(err)
    );
}

// ── Parameter validation ──────────────────────────────────────────────────

#[test]
fn sweep_band_is_validated_for_well_formedness() {
    let level = SweepLevel::new(-20.0, TransducerClass::InEar).unwrap();
    for (f_start_hz, f_end_hz) in [
        (0.0, 20_000.0),      // f_start must be positive
        (-20.0, 20_000.0),    // …and not negative
        (20_000.0, 20.0),     // must ascend
        (20.0, 20.0),         // …strictly
        (20.0, 24_000.0),     // f_end at Nyquist aliases
        (f64::NAN, 20_000.0), // NaN passes no positive comparison
    ] {
        // `matches!` rather than `assert_eq!`: the NaN row's error carries the
        // NaN back, and NaN != NaN would fail an equality on the right answer.
        assert!(
            matches!(
                assemble_sweep(5.0, 48_000, f_start_hz, f_end_hz, level),
                Err(StimulusError::InvalidBand { .. })
            ),
            "band {f_start_hz}..{f_end_hz} must be refused"
        );
    }
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(
            matches!(
                assemble_sweep(bad, 48_000, 20.0, 20_000.0, level),
                Err(StimulusError::NonPositiveDuration { .. })
            ),
            "duration {bad} must be refused"
        );
    }
}

// ── The MS-4 emit guard ───────────────────────────────────────────────────

/// MS-4's named test: "inject NaN/∞/2.0 into a stimulus buffer; assert output
/// finite and in-range; assert a warn with the count." Non-finites are zeroed
/// (NaN.clamp is NaN — the clamp alone cannot fix them), finite out-of-range
/// samples are clamped; each defect class is counted separately.
#[test]
fn the_emit_guard_contains_nan_inf_and_over_full_scale() {
    let mut block = [0.25, f64::NAN, -0.5, f64::INFINITY, 2.0, -0.75];
    let counts = emit_guard(&mut block);
    assert!(block.iter().all(|v| v.is_finite()), "non-finites survived");
    assert!(
        block.iter().all(|v| v.abs() <= 1.0),
        "out-of-range survived"
    );
    assert_eq!(block, [0.25, 0.0, -0.5, 0.0, 1.0, -0.75]);
    assert_eq!(counts.sanitized, 2, "NaN and +inf");
    assert_eq!(counts.clamped, 1, "the 2.0");
    assert_eq!(
        counts.warnings(),
        vec![
            D::EmitClamped { count: 1 },
            D::EmitNonFiniteSanitized { count: 2 },
        ]
    );
}

#[test]
fn negative_infinity_is_sanitized_not_clamped() {
    let mut block = [f64::NEG_INFINITY, -2.0];
    let counts = emit_guard(&mut block);
    assert_eq!(block, [0.0, -1.0]);
    assert_eq!(counts.sanitized, 1);
    assert_eq!(counts.clamped, 1);
}

/// A verified stimulus never trips the guard: zero counts, no warnings.
#[test]
fn a_clean_buffer_passes_the_guard_untouched() {
    let mut block = [0.0, 0.5, -1.0, 1.0, 0.25];
    let before = block;
    let counts = emit_guard(&mut block);
    assert_eq!(block, before);
    assert_eq!(counts.clamped, 0);
    assert_eq!(counts.sanitized, 0);
    assert!(counts.warnings().is_empty());
}

// ── Emission through the seam ─────────────────────────────────────────────

#[derive(Default)]
struct RecordingSink {
    emitted: Vec<(Vec<f64>, f64)>,
}

impl StimulusSink for RecordingSink {
    fn format(&self) -> StreamFormat {
        StreamFormat {
            channels: 2,
            frames_per_block: 512,
            sample_rate_hz: 48_000.0,
        }
    }

    fn emit(&mut self, block: &[f64], level: SweepLevel) -> Result<(), MeasureError> {
        self.emitted.push((block.to_vec(), level.dbfs_rms()));
        Ok(())
    }

    fn stop(&mut self) -> Result<(), MeasureError> {
        Ok(())
    }
}

/// The pipeline end-to-end: an assembled stimulus reaches a sink through the
/// guard, with its [`SweepLevel`] provenance, and a verified buffer produces
/// no warnings. (The samples arrive already scaled to the level — see
/// `AssembledStimulus::emit_to`'s note for the Stage-4 sink.)
#[test]
fn emit_to_hands_the_sink_the_guarded_buffer_and_the_level() {
    let stim = assemble_pilot(0.25, 48_000, TransducerClass::InEar).unwrap();
    let mut sink = RecordingSink::default();
    let warnings = stim.emit_to(&mut sink).unwrap();
    assert!(
        warnings.is_empty(),
        "a verified stimulus warned: {warnings:?}"
    );
    assert_eq!(sink.emitted.len(), 1);
    let (block, dbfs) = &sink.emitted[0];
    assert_eq!(
        block.as_slice(),
        stim.samples(),
        "guard must be a no-op here"
    );
    assert_eq!(*dbfs, PILOT_LEVEL_DBFS_RMS);
}

/// A sink failure propagates — the session must see it, not a shrug.
#[test]
fn emit_to_propagates_sink_failure() {
    struct FailingSink;
    impl StimulusSink for FailingSink {
        fn format(&self) -> StreamFormat {
            StreamFormat {
                channels: 2,
                frames_per_block: 512,
                sample_rate_hz: 48_000.0,
            }
        }
        fn emit(&mut self, _: &[f64], _: SweepLevel) -> Result<(), MeasureError> {
            Err(MeasureError::Sink("device gone".into()))
        }
        fn stop(&mut self) -> Result<(), MeasureError> {
            Ok(())
        }
    }
    let stim = assemble_pilot(0.25, 48_000, TransducerClass::InEar).unwrap();
    assert!(stim.emit_to(&mut FailingSink).is_err());
}

// ── verify_stimulus: each MS-3 assertion refuses with its diagnostic ──────

#[test]
fn verify_refuses_non_finite_samples_first() {
    let samples = [0.0, f64::NAN, f64::INFINITY, 0.0];
    let envelope = [0.0, 1.0, 1.0, 0.0];
    assert_eq!(
        verify_stimulus(&samples, &envelope, 1),
        Err(D::StimulusNonFinite { count: 2 })
    );
}

#[test]
fn verify_refuses_nonzero_endpoints() {
    // Exactly what a raw-mean DC subtraction would produce: a small but
    // nonzero first sample.
    let samples = [-1.24e-3, 0.5, -0.5, 0.0];
    let envelope = [0.0, 1.0, 1.0, 0.0];
    assert_eq!(
        verify_stimulus(&samples, &envelope, 1),
        Err(D::StimulusEndpointNonzero)
    );
}

#[test]
fn verify_refuses_a_non_monotone_fade_out_envelope() {
    let samples = [0.0, 0.1, 0.05, 0.08, 0.0];
    let envelope = [0.0, 1.0, 0.5, 0.8, 0.0]; // rises inside the fade-out
    assert_eq!(
        verify_stimulus(&samples, &envelope, 3),
        Err(D::StimulusFadeNotMonotone)
    );
}

#[test]
fn verify_refuses_dc_at_the_gate() {
    let samples = [0.0, 0.5, 0.5, 0.0];
    let envelope = [0.0, 1.0, 1.0, 0.0];
    match verify_stimulus(&samples, &envelope, 1) {
        Err(D::StimulusDcOffset { mean }) => assert!((mean - 0.25).abs() < 1e-12),
        other => panic!("wanted StimulusDcOffset, got {other:?}"),
    }
}

/// The clamp-as-defect rule: a sample over full scale after scaling is a
/// wrong level solve, surfaced as a blocking diagnostic — never silently
/// clamped and played.
#[test]
fn verify_refuses_over_full_scale_as_a_defect_rather_than_clamping() {
    let samples = [0.0, 1.5, -1.5, 0.0];
    let envelope = [0.0, 1.0, 1.0, 0.0];
    match verify_stimulus(&samples, &envelope, 1) {
        Err(D::StimulusOverFullScale { peak }) => {
            assert_eq!(peak, 1.5);
            assert!(D::StimulusOverFullScale { peak }.is_blocking());
        }
        other => panic!("wanted StimulusOverFullScale, got {other:?}"),
    }
}

#[test]
fn verify_accepts_a_well_formed_buffer() {
    // Zero endpoints, zero mean, monotone fade-out envelope, in range, finite.
    let samples = [0.0, 0.5, -0.25, -0.25, 0.0];
    let envelope = [0.0, 1.0, 1.0, 0.5, 0.0];
    assert_eq!(verify_stimulus(&samples, &envelope, 2), Ok(()));
}

/// The assembled type is the proof-of-verification: `AssembledStimulus` has
/// private fields and no public constructor, so the only way to hold one is
/// through the pipeline (the compile-fail half is
/// `tests/ui/raw_vec_cannot_be_an_assembled_stimulus.rs`).
#[test]
fn an_assembled_stimulus_only_exists_via_the_pipeline() {
    fn takes_verified(_: &AssembledStimulus) {}
    let stim = assemble_pilot(0.25, 48_000, TransducerClass::InEar).unwrap();
    takes_verified(&stim);
}

// ── The marker bracket (Stage 6) ──────────────────────────────────────────
//
// `assemble_bracketed` is the ONE constructor for both bracketed paths: the
// Direct baseline and the Helper verification file. What is pinned here is the
// order (the MS-3 assertion set runs on the sweep span, never on the spliced
// buffer), the three whole-buffer checks, the marker LEVEL rule, and the fact
// that the level rides through the splice exactly.

/// A 1.0 s sweep, short enough to keep the bracketed buffers cheap and long
/// enough to carry the policy fades and a real span RMS.
fn short_sweep(class: TransducerClass, dbfs: f64) -> AssembledStimulus {
    assemble_sweep(
        1.0,
        48_000,
        20.0,
        20_000.0,
        SweepLevel::new(dbfs, class).expect("a legal level"),
    )
    .expect("a legal sweep assembles")
}

fn layout() -> paraeq_dsp::two_clock::MarkerLayout {
    paraeq_dsp::two_clock::DEFAULT_MARKER_LAYOUT
}

/// R14's falsifier, and the second half is what makes it one. `scale_to` set
/// the SWEEP's RMS to exactly the solved level; splicing markers and silence
/// around it must not move that number, and the whole FILE's RMS must be
/// strictly lower — otherwise the two readings are indistinguishable and the
/// −1.46 dB the bracket costs a whole-file RMS would sit inside a 2.0 dB gate.
#[test]
fn the_bracketed_buffers_sweep_span_rms_is_exactly_its_level() {
    let sweep = short_sweep(TransducerClass::OverEar, -20.0);
    let (bracketed, span) =
        paraeq_measure::assemble_bracketed(sweep, &layout(), StimulusKind::VerificationSweep)
            .expect("a verified sweep brackets");

    assert_eq!(
        bracketed.level().dbfs_rms(),
        -20.0,
        "the level rides through the splice unchanged"
    );
    let samples = bracketed.samples();
    let span_rms = rms_dbfs(&samples[span.start..span.start + span.len]);
    assert!(
        (span_rms - bracketed.level().dbfs_rms()).abs() < 1e-12,
        "span RMS {span_rms} must equal the level {} exactly",
        bracketed.level().dbfs_rms()
    );
    let whole = rms_dbfs(samples);
    assert!(
        whole < span_rms - 1.0,
        "the whole-file RMS ({whole}) must be strictly lower than the span's \
         ({span_rms}) — the bracket is mostly silence"
    );
}

/// The MS-3 assertion set must NOT be re-run on the spliced buffer. This test
/// is the falsifier for the order: `verify_stimulus` asserts
/// `samples.len() == envelope.len()` and checks fade monotonicity over the
/// envelope's tail, so calling it with the bracketed buffer and the sweep's
/// own envelope panics — which is exactly why `assemble_bracketed` calls
/// `finish()` on the sweep first and checks the assembled file differently.
#[test]
fn ms3_assertions_run_on_the_sweep_span_not_the_whole_buffer() {
    let sweep = short_sweep(TransducerClass::OverEar, -20.0);
    let sweep_len = sweep.len();
    let (bracketed, span) =
        paraeq_measure::assemble_bracketed(sweep, &layout(), StimulusKind::BracketedSweep)
            .expect("a verified sweep brackets");

    assert_eq!(span.len, sweep_len, "the span is the sweep, unresampled");
    assert!(
        bracketed.len() > sweep_len,
        "the bracketed file is longer than the sweep it carries"
    );

    // The span still satisfies MS-3 on its own terms: it is bit-identical to
    // the buffer `verify_stimulus` already passed.
    let span_samples = &bracketed.samples()[span.start..span.start + span.len];
    let fade_out = (FADE_OUT_MS / 1000.0 * 48_000.0).round() as usize;
    let mut envelope = vec![1.0; span_samples.len()];
    paraeq_dsp::sweep::apply_fade(
        &mut envelope,
        (FADE_IN_MS / 1000.0 * 48_000.0).round() as usize,
        fade_out,
    );
    verify_stimulus(span_samples, &envelope, fade_out)
        .expect("the span is the verified sweep, bit for bit");

    // And the whole buffer is NOT a legal `verify_stimulus` input: its length
    // does not match the sweep's envelope. A caller that "just re-verified"
    // the file would be asserting a property of a different buffer.
    assert_ne!(
        bracketed.len(),
        envelope.len(),
        "the whole-buffer length must differ from the sweep envelope's, which \
         is why the assertion set cannot simply be re-run"
    );
}

/// Asserted on the assembled file rather than argued from the Hann window:
/// the third whole-buffer check exists because an argument by construction is
/// not a check.
#[test]
fn the_bracketed_buffer_starts_and_ends_at_exactly_zero() {
    for kind in [
        StimulusKind::BracketedSweep,
        StimulusKind::VerificationSweep,
    ] {
        let sweep = short_sweep(TransducerClass::InEar, -24.0);
        let (bracketed, _) = paraeq_measure::assemble_bracketed(sweep, &layout(), kind)
            .expect("a verified sweep brackets");
        let samples = bracketed.samples();
        assert_eq!(samples[0], 0.0, "{kind:?} first sample");
        assert_eq!(samples[samples.len() - 1], 0.0, "{kind:?} last sample");
        assert!(peak(samples) <= 1.0, "{kind:?} peak");
        assert!(samples.iter().all(|v| v.is_finite()), "{kind:?} finiteness");
    }
}

/// R8: the markers are scaled to the realized peak of the LEVELLED sweep, not
/// spliced at their documented peak 1.0. A full-scale 50 ms burst into a
/// transducer is louder than anything the caps table validated, and on the
/// Direct path it is tap-excluded — the one signal the engine's clamps never
/// see. `marker_scale` is computed inside `assemble_bracketed` precisely so it
/// cannot be passed the wrong number.
#[test]
fn the_bracketed_markers_never_exceed_the_realized_sweep_peak() {
    // Every level here is at or below the Bookshelf cap of −12 dBFS RMS.
    for dbfs in [-12.0, -20.0, -40.0] {
        let sweep = short_sweep(TransducerClass::Bookshelf, dbfs);
        let sweep_peak = peak(sweep.samples());
        let (bracketed, span) =
            paraeq_measure::assemble_bracketed(sweep, &layout(), StimulusKind::BracketedSweep)
                .expect("a verified sweep brackets");
        let samples = bracketed.samples();
        // Everything OUTSIDE the sweep span is marker or silence.
        let lead = peak(&samples[..span.start]);
        let tail = peak(&samples[span.start + span.len..]);
        assert!(
            lead <= sweep_peak + 1e-12,
            "lead-in markers peak at {lead}, sweep peaks at {sweep_peak}"
        );
        assert!(
            tail <= sweep_peak + 1e-12,
            "trailing markers peak at {tail}, sweep peaks at {sweep_peak}"
        );
        assert!(
            lead > 0.0 && tail > 0.0,
            "the markers must actually be there"
        );
    }
}

/// One order, one function, both paths — item 5b. The Direct baseline and the
/// Helper verification file differ ONLY in the kind they carry (and, outside
/// this function, in the matched-filter template `locate` is handed). A second
/// assembly path would be a second place for the marker level rule, the splice
/// order and the three checks to be got wrong, and on the Direct path a splice
/// defect is a SAFETY defect.
#[test]
fn the_baseline_and_the_verification_buffer_are_assembled_by_one_function() {
    let baseline = short_sweep(TransducerClass::OverEar, -26.0);
    let verification = short_sweep(TransducerClass::OverEar, -26.0);
    let (baseline, base_span) =
        paraeq_measure::assemble_bracketed(baseline, &layout(), StimulusKind::BracketedSweep)
            .expect("baseline brackets");
    let (verification, verify_span) = paraeq_measure::assemble_bracketed(
        verification,
        &layout(),
        StimulusKind::VerificationSweep,
    )
    .expect("verification brackets");

    assert_eq!(base_span, verify_span, "identical layout, identical span");
    assert_eq!(
        baseline.samples(),
        verification.samples(),
        "at the same level the two files are bit-identical; only the kind differs"
    );
    assert_eq!(baseline.kind(), StimulusKind::BracketedSweep);
    assert_eq!(verification.kind(), StimulusKind::VerificationSweep);
}

/// The provenance chain must get longer, not shorter: the only route into a
/// bracketed stimulus runs through `assemble_sweep` → `finish` →
/// `verify_stimulus` and THEN through this function's own three checks.
#[test]
fn assemble_bracketed_refuses_a_pilot_and_refuses_an_unbracketed_kind() {
    let pilot = assemble_pilot(0.5, 48_000, TransducerClass::OverEar).expect("a legal pilot");
    let err = paraeq_measure::assemble_bracketed(pilot, &layout(), StimulusKind::BracketedSweep)
        .expect_err("a pilot is not a sweep");
    assert!(
        matches!(
            err,
            StimulusError::NotASweep {
                kind: StimulusKind::Pilot
            }
        ),
        "got {err:?}"
    );

    let sweep = short_sweep(TransducerClass::OverEar, -20.0);
    let err = paraeq_measure::assemble_bracketed(sweep, &layout(), StimulusKind::Sweep)
        .expect_err("Sweep is not a bracketed kind");
    assert!(
        matches!(
            err,
            StimulusError::NotABracketedKind {
                kind: StimulusKind::Sweep
            }
        ),
        "got {err:?}"
    );
}
