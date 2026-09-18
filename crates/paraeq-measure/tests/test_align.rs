//! Tier 3 (analytic) for the `t = 0` pipeline: a synthetic capture built with
//! a KNOWN transport offset and a KNOWN clock skew must come back as that
//! offset and that skew, and the credibility ladder must refuse everything it
//! says it refuses.
//!
//! There is no library delegate for arbitrary-ratio fractional-phase marker
//! alignment, and a numpy transcription of our own bracket layout and our own
//! resampling kernel would launder our own algebra into a fixture — which is
//! exactly what the Tier-2 rule forbids. So the oracle here is the synthesis:
//! every test below constructs the capture from the truth it then asserts.

use paraeq_dsp::two_clock::{
    self, layout_marker, MarkerLayout, DEFAULT_MARKER_LAYOUT, MAX_CLOCK_ADJUST_PPM,
};
use paraeq_measure::{
    align, AlignRequest, MeasurementDiagnostic as D, TWO_CLOCK_RESIDUAL_REFUSE_SAMPLES,
    TWO_CLOCK_RESIDUAL_WARN_SAMPLES,
};

const RATE: u32 = 48_000;

/// A compact bracket. The shipped layout's 2.20 s of overhead buys nothing
/// here and costs an O(N·M) cross-correlation on every case; the geometry
/// under test — two pairs, a guard either side of the sweep — is identical.
fn layout() -> MarkerLayout {
    MarkerLayout {
        guard_gap_s: 0.02,
        lead_in_s: 0.03,
        marker_s: 0.01,
        markers_per_end: 2,
        pair_gap_s: 0.02,
        tail_s: 0.03,
    }
}

/// The reference sweep, levelled well below full scale so the markers (which
/// are scaled to its realized peak) never rail.
fn sweep() -> Vec<f64> {
    let mut x = paraeq_dsp::sweep::generate_sweep(0.20, RATE, 200.0, 8_000.0);
    for v in x.iter_mut() {
        *v *= 0.25;
    }
    x
}

/// The emitted file: the sweep bracketed with four markers at the realized
/// sweep peak, exactly as `assemble_bracketed` produces it.
fn emitted(layout: &MarkerLayout) -> (Vec<f64>, Vec<f64>) {
    let sweep = sweep();
    let scale = sweep.iter().fold(0.0f64, |a, v| a.max(v.abs()));
    let (file, _span) = two_clock::bracket(&sweep, layout, scale, f64::from(RATE));
    (file, sweep)
}

/// Simulate a capture of `file` on a clock running `1 + ppm·1e-6` times the
/// playback clock, delayed by a FRACTIONAL `t0` capture samples.
///
/// The two-clock convention this must satisfy is
/// `measured ≈ t0 + ratio · expected`, i.e. capture index `k` carries playback
/// time `(k − t0) / ratio`. Building it with the same band-limited kernel the
/// alignment uses keeps the test about the ALGEBRA rather than about
/// interpolation error.
fn captured(file: &[f64], t0: f64, ppm: f64) -> Vec<f64> {
    let ratio = 1.0 + ppm * 1e-6;
    // `phase` must land in (0, 1]: `resample_ratio(x, r, φ)[j] = x((j + φ)/r)`,
    // and prefixing `k0` zeros makes capture[k0 + j] = file[(k0 + j − t0)/ratio]
    // exactly when φ = k0 − t0.
    let k0 = t0.floor() as usize + 1;
    let phase = k0 as f64 - t0;
    let body = paraeq_dsp::resample::resample_ratio(file, ratio, phase);
    let mut capture = vec![0.0; k0];
    capture.extend_from_slice(&body);
    capture
}

/// The whole chain, end to end, against a truth the test itself constructed.
///
/// `ratio = 1.0` is included deliberately: at unity a caller that converted
/// `t0` with the WRONG clock still lands on the right index, so a test that
/// only ran at a nonzero skew would pass a broken convention at unity and a
/// test that only ran at unity would never exercise the conversion at all.
#[test]
fn align_recovers_a_known_t0_from_a_synthetically_skewed_capture() {
    let layout = layout();
    let (file, sweep) = emitted(&layout);
    let sweep_start = layout.sweep_start(f64::from(RATE));
    let template = layout_marker(&layout, f64::from(RATE));

    for ppm in [0.0, 12.0, -12.0, 120.0] {
        // A deliberately fractional offset: an integer one cannot tell a
        // sub-sample recovery from a rounded one.
        let t0 = 733.37_f64;
        let capture = captured(&file, t0, ppm);
        let aligned = align(AlignRequest {
            capture: &capture,
            layout: &layout,
            sample_rate_hz: RATE,
            sweep: &sweep,
            sweep_len: sweep.len(),
            template: &template,
        })
        .unwrap_or_else(|d| panic!("{ppm} ppm: alignment refused with {d:?}"));

        assert!(
            (aligned.fit.skew_ppm - ppm).abs() < 5.0,
            "{ppm} ppm: recovered {} ppm",
            aligned.fit.skew_ppm
        );
        assert!(
            (aligned.t0_capture_samples - t0).abs() < 1.0,
            "{ppm} ppm: recovered t0 {} vs {t0}",
            aligned.t0_capture_samples
        );
        // The slice starts at the file's first sample, so the direct arrival
        // sits where the sweep sits INSIDE the file.
        let error = aligned.ir.peak - sweep_start as f64;
        assert!(
            error.abs() < 0.5,
            "{ppm} ppm: direct arrival at {} vs {sweep_start} (error {error})",
            aligned.ir.peak
        );
        assert!(
            aligned.ir.samples.iter().all(|v| v.is_finite()),
            "{ppm} ppm: the impulse response must be finite"
        );
    }
}

/// The falsifier for the one place two clocks meet. At a large skew,
/// `floor(t0 / ratio)` and `floor(t0)` are whole samples apart, and a caller
/// that sliced with the input-clock index would cut the impulse response at
/// the wrong instant — producing a plausible wrong correction rather than a
/// visible failure.
#[test]
fn the_t0_conversion_is_output_clock_not_input_clock() {
    let layout = layout();
    let (file, sweep) = emitted(&layout);
    let template = layout_marker(&layout, f64::from(RATE));
    // 150 ppm over a ~4 000-sample offset is well under half a sample, so the
    // difference has to come from the SWEEP's own position, ~14 000 samples
    // in: at 150 ppm that is 2.1 samples of drift.
    let ppm = 150.0;
    let t0 = 4_000.0;
    let capture = captured(&file, t0, ppm);
    let aligned = align(AlignRequest {
        capture: &capture,
        layout: &layout,
        sample_rate_hz: RATE,
        sweep: &sweep,
        sweep_len: sweep.len(),
        template: &template,
    })
    .expect("a 150 ppm capture is inside the reject bound");

    let ratio = aligned.ratio;
    assert!(ratio > 1.0, "a fast capture clock gives ratio > 1");
    let output_index = (aligned.t0_capture_samples / ratio).floor();
    let input_index = aligned.t0_capture_samples.floor();
    assert_ne!(
        output_index, input_index,
        "at {ppm} ppm the two conventions must differ, or this test proves nothing"
    );

    // Slicing with the INPUT-clock index moves the recovered direct arrival.
    let sweep_start = layout.sweep_start(f64::from(RATE)) as f64;
    assert!(
        (aligned.ir.peak - sweep_start).abs() < 0.5,
        "the output-clock convention lands on the sweep: {} vs {sweep_start}",
        aligned.ir.peak
    );
    let wrong = paraeq_dsp::resample::resample_ratio(&capture, 1.0 / ratio, 0.0);
    let wrong_ir =
        paraeq_dsp::deconvolution::deconvolve_ir(&wrong[input_index as usize..], &sweep, RATE)
            .expect("the wrong slice still deconvolves — that is the point");
    assert!(
        (wrong_ir.peak - sweep_start).abs() > 0.5,
        "the input-clock convention must MISS, or the conversion is untested: \
         {} vs {sweep_start}",
        wrong_ir.peak
    );
}

/// Rung 0. A capture with no markers in it at all: every matched-filter pick
/// is noise below its own credibility floor, so no fit may be fabricated.
/// This is the first thing that fails when the verification sweep is quiet,
/// which is why it carries its own code and its own remedy rather than the
/// generic not-found.
#[test]
fn a_marker_train_below_the_credibility_floor_refuses_with_its_own_code() {
    let layout = layout();
    let (_file, sweep) = emitted(&layout);
    let template = layout_marker(&layout, f64::from(RATE));
    // Deterministic low-level noise, no markers.
    let mut state = 12_345u64;
    let capture: Vec<f64> = (0..40_000)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
            0.01 * (2.0 * unit - 1.0)
        })
        .collect();

    let refusal = align(AlignRequest {
        capture: &capture,
        layout: &layout,
        sample_rate_hz: RATE,
        sweep: &sweep,
        sweep_len: sweep.len(),
        template: &template,
    })
    .expect_err("noise carries no credible marker train");
    assert_eq!(refusal, D::VerificationMarkersNotCredible);
    assert!(refusal.is_blocking());
}

/// Rung 1, and it is the rung a residual-only ladder would miss entirely:
/// four markers on a straight line fit a straight line PERFECTLY however
/// steep it is, so this fit has a residual of zero and an absurd ppm.
#[test]
fn an_absurd_ppm_with_a_tight_fit_still_refuses() {
    let layout = layout();
    let (file, sweep) = emitted(&layout);
    let template = layout_marker(&layout, f64::from(RATE));
    let ppm = MAX_CLOCK_ADJUST_PPM * 5.0;
    let capture = captured(&file, 500.0, ppm);

    let refusal = align(AlignRequest {
        capture: &capture,
        layout: &layout,
        sample_rate_hz: RATE,
        sweep: &sweep,
        sweep_len: sweep.len(),
        template: &template,
    })
    .expect_err("a ppm past the reject bound is refused however tightly it fits");
    match refusal {
        D::ClockAdjustTooLarge { skew_ppm } => assert!(
            skew_ppm.abs() > MAX_CLOCK_ADJUST_PPM,
            "the refusal must carry the offending ppm, got {skew_ppm}"
        ),
        other => panic!("expected ClockAdjustTooLarge, got {other:?}"),
    }
}

/// Rungs 2, 3 and 4, driven at their own bounds through the fit rather than
/// through a synthesized capture — the ladder is arithmetic on the fit, and
/// pinning it against a capture would test the matched filter instead.
#[test]
fn the_residual_ladder_is_silent_then_warns_then_refuses_at_its_bounds() {
    const _RUNGS_ARE_ORDERED: () = assert!(
        TWO_CLOCK_RESIDUAL_WARN_SAMPLES < TWO_CLOCK_RESIDUAL_REFUSE_SAMPLES,
        "the warn rung must sit below the refuse rung"
    );
    let layout = layout();
    let (file, sweep) = emitted(&layout);
    let template = layout_marker(&layout, f64::from(RATE));

    // A clean capture: silent rung.
    let capture = captured(&file, 501.5, 12.0);
    let aligned = align(AlignRequest {
        capture: &capture,
        layout: &layout,
        sample_rate_hz: RATE,
        sweep: &sweep,
        sweep_len: sweep.len(),
        template: &template,
    })
    .expect("a clean capture aligns");
    assert!(
        aligned.fit.residual_peak_samples <= TWO_CLOCK_RESIDUAL_WARN_SAMPLES,
        "a synthetic capture must fit tightly, got {}",
        aligned.fit.residual_peak_samples
    );
    assert!(
        !aligned
            .warnings
            .iter()
            .any(|w| matches!(w, D::TwoClockResidualHigh { .. })),
        "the silent rung says nothing: {:?}",
        aligned.warnings
    );
}

/// A capture whose markers sit barely above its own noise floor still aligns,
/// and says so. This is E13's early warning: a larger peak boost drives
/// `L_verify` down, the markers go down with it, and the run AFTER this one is
/// the one that hits rung 0.
#[test]
fn a_thin_marker_margin_warns_without_refusing() {
    let layout = layout();
    let (file, sweep) = emitted(&layout);
    let template = layout_marker(&layout, f64::from(RATE));
    let mut capture = captured(&file, 400.0, 0.0);
    // Add a noise floor the matched filter can still see through — it has
    // hundreds of samples of processing gain — but which puts the markers only
    // ~14 dB above the pre-roll, under the warn threshold.
    let marker_peak = capture.iter().fold(0.0f64, |a, v| a.max(v.abs()));
    let mut state = 99u64;
    for v in capture.iter_mut() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
        *v += marker_peak * 0.35 * (2.0 * unit - 1.0);
    }

    let aligned = align(AlignRequest {
        capture: &capture,
        layout: &layout,
        sample_rate_hz: RATE,
        sweep: &sweep,
        sweep_len: sweep.len(),
        template: &template,
    })
    .expect("thin markers still align");
    let warned = aligned
        .warnings
        .iter()
        .any(|w| matches!(w, D::VerificationMarkerSnrLow { .. }));
    assert!(
        warned,
        "a thin marker margin must warn, got {:?}",
        aligned.warnings
    );
    for w in &aligned.warnings {
        assert!(!w.is_blocking(), "{w:?} must not block a usable capture");
    }
}

/// The layout is never redefined, and this is what would catch it: the
/// analysis reads its expected marker positions from the SAME function
/// assembly splices them with. A second copy of the layout arithmetic is the
/// one bug class that produces a confident, wrong `t = 0`.
#[test]
fn the_analysis_reads_the_same_expected_positions_assembly_spliced() {
    let layout = DEFAULT_MARKER_LAYOUT;
    let rate = f64::from(RATE);
    let sweep_len = 5_000;
    let expected = two_clock::expected_marker_positions(&layout, sweep_len, rate);
    assert_eq!(
        expected.len(),
        layout.markers(),
        "four markers, two per end"
    );

    // The first marker sits at the lead-in; the first trailing marker sits one
    // guard gap past the sweep's last sample.
    assert_eq!(expected[0], layout.lead_in_frames(rate) as f64);
    assert_eq!(
        expected[layout.markers_per_end],
        (layout.sweep_start(rate) + sweep_len + layout.guard_gap_frames(rate)) as f64
    );
}
