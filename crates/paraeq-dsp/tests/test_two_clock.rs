//! Falsifiers for the bracketed-marker half of the two-clock story: the layout,
//! the assembly, the recovery, and the two rules that are easy to get quietly
//! wrong — the marker LEVEL and the matched-filter TEMPLATE.
//!
//! The module's own shipped unit tests (marker chirp shape, `find_marker_train`
//! credibility, `estimate_skew` algebra) moved with the module and still live
//! beside it in `crates/paraeq-dsp/src/two_clock.rs`. These are the
//! integration-level properties the Stage 6 bracket adds on top.
//!
//! Every test here runs a SHORT layout — same shape as the shipped 7.7 s
//! bracket, roughly a tenth the duration — because `find_marker_train` is an
//! O(N·M) naive correlation and the product layout is about 8e8 multiply-adds
//! per call, which is a minute of debug-build test time, not a second.

use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};
use paraeq_dsp::resample::resample_ratio;
use paraeq_dsp::sweep::generate_sweep;
use paraeq_dsp::two_clock::{
    bracket, default_marker, estimate_skew, expected_marker_positions, layout_marker, locate,
    MarkerLayout, DEFAULT_MARKER_LAYOUT, MAX_CLOCK_ADJUST_PPM,
};

const RATE: f64 = 48_000.0;

/// A tenth-scale stand-in for [`DEFAULT_MARKER_LAYOUT`]: same seven regions in
/// the same order, short enough to correlate in a debug build.
fn test_layout() -> MarkerLayout {
    MarkerLayout {
        guard_gap_s: 0.03,
        lead_in_s: 0.05,
        marker_s: 0.01,
        markers_per_end: 2,
        pair_gap_s: 0.05,
        tail_s: 0.05,
    }
}

/// The sweep payload for the LOCATE tests: 100 → 1200 Hz, i.e. deliberately
/// BELOW the marker's 2–8 kHz band.
///
/// These tests grade marker recovery. How well a matched filter rejects a
/// full-band sweep sharing the marker's octaves is a different question, it is
/// answered by rig data (the plan's S6 / S8 spikes), and mixing the two would
/// make a marker failure look like a timing failure.
fn low_band_payload() -> Vec<f64> {
    generate_sweep(0.3, RATE as u32, 100.0, 1_200.0)
        .iter()
        .map(|v| 0.5 * v)
        .collect()
}

/// The sweep payload for the LEVEL test: full band, at the kind of realized
/// peak a levelled verification sweep actually has.
fn full_band_payload() -> Vec<f64> {
    generate_sweep(0.3, RATE as u32, 20.0, 20_000.0)
        .iter()
        .map(|v| 0.2 * v)
        .collect()
}

fn peak(x: &[f64]) -> f64 {
    x.iter().fold(0.0f64, |a, &v| a.max(v.abs()))
}

/// Deterministic LCG noise in `[-amp, amp]`, same shape as the generator the
/// module's own unit tests use (no `rand` dependency, seeded).
fn lcg_noise(len: usize, amp: f64, seed: u64) -> Vec<f64> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
            amp * (2.0 * unit - 1.0)
        })
        .collect()
}

/// What the mic records: a constant transport delay (playback start latency +
/// acoustic propagation) followed by a capture clock running `ppm` parts per
/// million fast.
///
/// The two compose in that order, so a marker played at playback sample `p`
/// is captured at `(delay + p)·ratio`: the fit's slope is `ratio` and its
/// intercept is `delay·ratio`.
fn simulate_capture(playback: &[f64], delay: usize, ppm: f64) -> Vec<f64> {
    let mut delayed = vec![0.0; delay];
    delayed.extend_from_slice(playback);
    resample_ratio(&delayed, 1.0 + ppm * 1e-6, 0.0)
}

/// A realistic installed headphone correction: a low-shelf-ish cut, a
/// presence boost, a 5 kHz notch and a top-octave shelf.
///
/// Deliberately a whole cascade rather than one contrived filter. A single
/// high-Q band sitting inside the marker's 2–8 kHz sweep barely moves the
/// matched-filter peak at all (measured: 0.3 samples), because the peak is set
/// by phase coherence across the WHOLE band and one narrow anomaly is outvoted.
/// It takes a correction that shapes the band broadly — which is what a real
/// correction is — to bias t = 0 by samples rather than tenths. Anyone tempted
/// to simplify this helper should check the bias assertion still bites.
fn installed_correction() -> ParametricEQ {
    ParametricEQ {
        bands: vec![
            EQBand {
                fc: 120.0,
                filter_type: FilterType::Peaking,
                gain_db: -4.0,
                q: 1.0,
            },
            EQBand {
                fc: 3_000.0,
                filter_type: FilterType::Peaking,
                gain_db: 6.0,
                q: 2.0,
            },
            EQBand {
                fc: 5_000.0,
                filter_type: FilterType::Peaking,
                gain_db: -8.0,
                q: 3.0,
            },
            EQBand {
                fc: 8_000.0,
                filter_type: FilterType::HighShelf,
                gain_db: 5.0,
                q: 0.7,
            },
        ],
        sample_rate: RATE,
    }
}

/// A correction confined to the room band, where the marker has no energy.
fn room_band_correction() -> ParametricEQ {
    ParametricEQ {
        bands: vec![EQBand {
            fc: 80.0,
            filter_type: FilterType::Peaking,
            gain_db: -6.0,
            q: 2.0,
        }],
        sample_rate: RATE,
    }
}

#[test]
fn markers_survive_the_verification_stimulus_shape() {
    let layout = test_layout();
    let sweep = low_band_payload();
    let (playback, span) = bracket(&sweep, &layout, peak(&sweep), RATE);
    assert_eq!(span.start, layout.sweep_start(RATE));

    let delay = 733usize;
    let inserted_ppm = 120.0;
    let capture = simulate_capture(&playback, delay, inserted_ppm);

    let measured = locate(&capture, &layout_marker(&layout, RATE), &layout, RATE)
        .expect("four markers present in a clean capture");
    assert_eq!(measured.len(), 4, "the whole train, not a subset");

    let expected = expected_marker_positions(&layout, sweep.len(), RATE);
    let fit = estimate_skew(&expected, &measured).expect("a four-point fit exists");

    assert!(
        (fit.skew_ppm - inserted_ppm).abs() < 20.0,
        "inserted {inserted_ppm} ppm, recovered {} ppm",
        fit.skew_ppm
    );
    let truth = delay as f64 * (1.0 + inserted_ppm * 1e-6);
    assert!(
        (fit.intercept_samples - truth).abs() < 0.5,
        "transport delay {truth} samples, recovered {}",
        fit.intercept_samples
    );
}

#[test]
fn four_markers_give_a_nonzero_residual_figure_and_two_do_not() {
    // (a) The algebra, directly: the fit has two parameters, so ANY two points
    // lie exactly on the fitted line and the residual figure is a structural
    // zero rather than a small number. This is why the layout is four.
    let two_point = estimate_skew(&[2_400.0, 25_920.0], &[3_141.0, 26_666.5])
        .expect("two points still fit a line");
    assert_eq!(two_point.residuals_samples.len(), 2);
    assert!(
        two_point.residual_peak_samples < 1e-9,
        "two markers cannot produce a jitter figure, got {}",
        two_point.residual_peak_samples
    );

    // (b) The same claim end to end, on the same capture, with the ONLY
    // difference being markers_per_end. Noise is added so the peak refinement
    // really does scatter — without it the four-marker figure would be small
    // for want of anything to measure, and this test would prove nothing.
    let sweep = low_band_payload();
    let mut degenerate = test_layout();
    degenerate.markers_per_end = 1;

    let mut figures = Vec::new();
    for layout in [degenerate, test_layout()] {
        let (playback, _) = bracket(&sweep, &layout, peak(&sweep), RATE);
        let mut capture = simulate_capture(&playback, 733, 120.0);
        for (sample, noise) in capture.iter_mut().zip(lcg_noise(1 << 20, 0.02, 20_260_917)) {
            *sample += noise;
        }
        let measured = locate(&capture, &layout_marker(&layout, RATE), &layout, RATE)
            .expect("the train is present at this SNR");
        let expected = expected_marker_positions(&layout, sweep.len(), RATE);
        figures.push(
            estimate_skew(&expected, &measured)
                .expect("fit")
                .residual_peak_samples,
        );
    }

    assert!(
        figures[0] < 1e-9,
        "markers_per_end = 1 reports {} samples of 'jitter', which is arithmetic, not measurement",
        figures[0]
    );
    assert!(
        figures[1] > 1e-3,
        "markers_per_end = 2 must report a REAL figure, got {} samples",
        figures[1]
    );
}

#[test]
fn markers_never_exceed_the_sweep_peak() {
    let layout = test_layout();
    let sweep = full_band_payload();
    let sweep_peak = peak(&sweep);

    let (playback, span) = bracket(&sweep, &layout, sweep_peak, RATE);

    assert!(
        peak(&playback) <= sweep_peak * (1.0 + 1e-12),
        "the file peaks at {}, above the sweep's own {sweep_peak}",
        peak(&playback)
    );
    // Not vacuous: the markers ARE there and they DO reach the levelled peak,
    // so this is a ceiling that binds rather than one nothing approaches.
    assert!(
        peak(&playback[..span.start]) > 0.9 * sweep_peak,
        "the lead-in pair should sit just under the sweep's peak, got {}",
        peak(&playback[..span.start])
    );
    // And the rule is not free: the raw marker really would have blown past it.
    assert!(
        peak(&default_marker(RATE)) > 4.0 * sweep_peak,
        "a peak-1.0 marker spliced verbatim is the case this rule refuses"
    );
}

#[test]
fn bracket_does_not_change_the_sweep_spans_samples() {
    let layout = test_layout();
    let sweep = full_band_payload();

    let (playback, span) = bracket(&sweep, &layout, peak(&sweep), RATE);

    assert_eq!(span.len, sweep.len());
    // Bit-identical, not close. The verification level book measures RMS over
    // this span on both sides of the comparison, and "approximately the same
    // samples" would make that invariance approximate too.
    assert_eq!(
        &playback[span.start..span.start + span.len],
        &sweep[..],
        "the sweep is copied, never re-levelled or filtered"
    );
}

#[test]
fn the_correction_biases_the_intercept_and_the_filtered_template_removes_it() {
    let layout = test_layout();
    let sweep = low_band_payload();
    let (playback, _) = bracket(&sweep, &layout, peak(&sweep), RATE);
    let expected = expected_marker_positions(&layout, sweep.len(), RATE);
    let delay = 733usize;

    // The helper path: the whole file traverses the installed correction on its
    // way to the DAC, markers included.
    let correction = installed_correction();
    let capture = simulate_capture(&correction.apply_offline(&playback, RATE), delay, 0.0);

    let raw = layout_marker(&layout, RATE);
    let filtered = correction.apply_offline(&raw, RATE);

    let with_raw = estimate_skew(
        &expected,
        &locate(&capture, &raw, &layout, RATE).expect("found with the raw template"),
    )
    .expect("fit");
    let with_filtered = estimate_skew(
        &expected,
        &locate(&capture, &filtered, &layout, RATE).expect("found with the filtered template"),
    )
    .expect("fit");

    let truth = delay as f64;
    let raw_bias = (with_raw.intercept_samples - truth).abs();
    let filtered_bias = (with_filtered.intercept_samples - truth).abs();

    // The bias is common-mode across both ends of the bracket, so it lands
    // entirely in the intercept — which is the number used as t = 0 — and a
    // residual-scatter gate never sees it.
    assert!(
        raw_bias > 2.0,
        "the cascade's group delay must actually bias the raw template's \
         intercept, or this test proves nothing; got {raw_bias} samples"
    );
    assert!(
        filtered_bias < 0.5,
        "the filtered template is unbiased by construction; got {filtered_bias} samples \
         against the raw template's {raw_bias}"
    );
    assert!(
        with_raw.residual_peak_samples < raw_bias,
        "the residual figure does NOT see the bias, which is why this needs its own test"
    );
}

#[test]
fn skew_is_invariant_to_the_correction() {
    let layout = test_layout();
    let sweep = low_band_payload();
    let (playback, _) = bracket(&sweep, &layout, peak(&sweep), RATE);
    let expected = expected_marker_positions(&layout, sweep.len(), RATE);
    let inserted_ppm = 120.0;
    let raw = layout_marker(&layout, RATE);

    let uncorrected = simulate_capture(&playback, 733, inserted_ppm);
    let corrected = simulate_capture(
        &installed_correction().apply_offline(&playback, RATE),
        733,
        inserted_ppm,
    );

    let mut skews = Vec::new();
    for capture in [&uncorrected, &corrected] {
        let measured = locate(capture, &raw, &layout, RATE).expect("train present");
        skews.push(estimate_skew(&expected, &measured).expect("fit").skew_ppm);
    }

    // Every fallback rung leans on this: the correction moves the INTERCEPT and
    // leaves the SLOPE alone, because the same group delay is added at both ends
    // of the bracket and a slope is a difference.
    assert!(
        (skews[1] - skews[0]).abs() < 5.0,
        "the correction moved the skew estimate from {} to {} ppm",
        skews[0],
        skews[1]
    );
    assert!(
        (skews[1] - inserted_ppm).abs() < 20.0,
        "even with a raw template the corrected capture reports {} ppm against {inserted_ppm}",
        skews[1]
    );
}

#[test]
fn a_room_band_only_correction_leaves_the_marker_intercept_unmoved() {
    let layout = test_layout();
    let sweep = low_band_payload();
    let (playback, _) = bracket(&sweep, &layout, peak(&sweep), RATE);
    let expected = expected_marker_positions(&layout, sweep.len(), RATE);
    let delay = 733usize;

    let capture = simulate_capture(
        &room_band_correction().apply_offline(&playback, RATE),
        delay,
        0.0,
    );
    let measured =
        locate(&capture, &layout_marker(&layout, RATE), &layout, RATE).expect("train present");
    let fit = estimate_skew(&expected, &measured).expect("fit");

    // A correction that lives at 80 Hz has no phase to contribute at 2–8 kHz,
    // so the RAW template is already unbiased. Filtering the template is
    // therefore a no-op on the common case and a repair on the hard one — which
    // is why one code path can serve both.
    assert!(
        (fit.intercept_samples - delay as f64).abs() < 0.5,
        "a room-band correction moved the intercept to {} against a truth of {delay}",
        fit.intercept_samples
    );
}

#[test]
fn locate_returns_none_when_no_peak_clears_the_credibility_floor() {
    let layout = test_layout();
    let template = layout_marker(&layout, RATE);

    // Noise only: the picked peaks are noise and cannot clear 6x the off-peak
    // correlation RMS, so the train is refused rather than fabricated. The
    // caller maps this branch to its own diagnostic — it is the first thing that
    // fails when the verification sweep is quiet.
    let noise = lcg_noise(40_000, 0.1, 4_242);
    assert!(
        locate(&noise, &template, &layout, RATE).is_none(),
        "a noise-only capture has no credible marker train"
    );

    // Silence, likewise: no peak at all.
    assert!(locate(&vec![0.0; 40_000], &template, &layout, RATE).is_none());

    // A layout that brackets nothing asks for zero markers, which is not a
    // train either.
    let mut unbracketed = layout;
    unbracketed.markers_per_end = 0;
    let sweep = low_band_payload();
    let (playback, _) = bracket(&sweep, &unbracketed, peak(&sweep), RATE);
    assert!(locate(&playback, &template, &unbracketed, RATE).is_none());
}

#[test]
fn an_absurd_ppm_with_a_tight_fit_still_refuses() {
    // 5000 ppm on an exact straight line. The residuals are identically zero, so
    // every residual-based rung of the credibility ladder passes it — and acting
    // on it would stretch the capture by half a percent. MAX_CLOCK_ADJUST_PPM is
    // the only thing that refuses; this is its rung, stated where the constant
    // lives so the constant cannot drift away from its reason.
    let expected = [0.0, 10_000.0, 20_000.0, 30_000.0];
    let measured: Vec<f64> = expected
        .iter()
        .map(|&e| 512.0 + e * (1.0 + 5_000e-6))
        .collect();

    let fit = estimate_skew(&expected, &measured).expect("a four-point fit exists");

    assert!(
        fit.residual_peak_samples < 1e-6,
        "an exact line fits exactly: {} samples of residual",
        fit.residual_peak_samples
    );
    assert!(
        fit.skew_ppm.abs() > MAX_CLOCK_ADJUST_PPM,
        "{} ppm must exceed the reject bound of {MAX_CLOCK_ADJUST_PPM}",
        fit.skew_ppm
    );
    // And the bound is not so tight that the real magnitude trips it: the
    // decision doc's §Q6 measures ~12 ppm across two devices, and the bound is
    // cut an order of magnitude above that.
    // A const block, so the bound is checked when this file COMPILES: lowering
    // the constant below an order of magnitude over the measured ~12 ppm is a
    // build error, not a test failure someone can skip past.
    const {
        assert!(
            MAX_CLOCK_ADJUST_PPM >= 120.0,
            "the bound must leave an order of magnitude over the measured ~12 ppm"
        )
    };
    let plausible = estimate_skew(
        &expected,
        &expected
            .iter()
            .map(|&e| 512.0 + e * (1.0 + 12e-6))
            .collect::<Vec<f64>>(),
    )
    .expect("fit");
    assert!(
        plausible.skew_ppm.abs() < MAX_CLOCK_ADJUST_PPM,
        "a real ~12 ppm rig must not be refused, got {} ppm",
        plausible.skew_ppm
    );
}

#[test]
fn the_default_layouts_marker_is_exactly_default_marker() {
    // The anti-drift device for the one number that appears in two places: the
    // template a caller builds from the layout must be the waveform `bracket`
    // spliced, or the matched filter is correlating against something else.
    assert_eq!(
        layout_marker(&DEFAULT_MARKER_LAYOUT, RATE),
        default_marker(RATE),
        "the shipped layout's marker IS default_marker"
    );
    assert_eq!(
        DEFAULT_MARKER_LAYOUT.marker_frames(RATE),
        default_marker(RATE).len(),
        "marker_frames and the waveform's length are the same rounding"
    );
}

#[test]
fn expected_marker_positions_matches_the_assembled_offsets() {
    // Assembly and analysis must not be able to disagree about where the
    // markers are: a confident, wrong t = 0 is worse than a refusal.
    let layout = test_layout();
    let sweep = low_band_payload();
    let (playback, _) = bracket(&sweep, &layout, peak(&sweep), RATE);
    let expected = expected_marker_positions(&layout, sweep.len(), RATE);

    let measured = locate(&playback, &layout_marker(&layout, RATE), &layout, RATE)
        .expect("the file it just assembled");

    assert_eq!(measured.len(), expected.len());
    for (found, want) in measured.iter().zip(&expected) {
        assert!(
            (found - want).abs() < 0.5,
            "marker expected at {want}, located at {found}"
        );
    }
    // The positions are ordered and inside the file, and the sweep sits between
    // the two pairs rather than on top of one.
    let span_start = layout.sweep_start(RATE) as f64;
    assert!(expected[1] < span_start && expected[2] > span_start + sweep.len() as f64);
    assert!(expected.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn the_default_layout_costs_two_point_two_seconds() {
    // The plan's arithmetic, pinned: 2.20 s of overhead, so a 5.5 s capped sweep
    // produces a 7.70 s file. This is the number owner question E14 is about, so
    // it must not drift silently.
    let sweep_len = (5.5 * RATE) as usize;
    let total = DEFAULT_MARKER_LAYOUT.total_len(sweep_len, RATE);

    assert_eq!(total - sweep_len, (2.20 * RATE) as usize, "overhead frames");
    assert!(
        ((total as f64 / RATE) - 7.70).abs() < 1e-9,
        "a 5.5 s sweep makes a {} s file",
        total as f64 / RATE
    );
    assert_eq!(DEFAULT_MARKER_LAYOUT.markers(), 4);
}
