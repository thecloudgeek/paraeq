//! The decision RULES — a module per decision-table row, plus the four policy
//! scans, the fit, the cascade and the copy.
//!
//! **What makes a test here load-bearing: it fails if the rule is replaced by
//! the default B7a shipped.** Each module below names that default in its own
//! doc comment, so a reader can see what the test is falsifying rather than
//! having to diff two branches. The B7a skeleton answered `transition_hz` with
//! a flat 200.0, `low_corner_hz` with the sweep's start frequency,
//! `correction_range` with the whole analysis grid, `target` with the first
//! class-legal candidate, and shipped no bands at all — so every assertion
//! about those five is a falsifier by construction.
//!
//! **The oracle is a filter's closed form, never a second copy of the rule.**
//! `common::shaped_bundle` builds an impulse response by filtering a unit
//! impulse through a biquad cascade the test names, so "the low corner is near
//! 150 Hz" is checked against a capture whose low corner IS near 150 Hz. Where
//! an exact number would be a re-derivation of the rule, the assertion is an
//! ORDERING or a BRACKET instead — a corner that moves the right way when the
//! shelf moves, a crossing that lands above the dip and below the grid top.
//!
//! Tier: **3 (analytic)** — `decide()` is policy, not DSP, and there is no
//! prototype decision engine to port; writing one would launder a design bug
//! into a fixture rather than provide an independent oracle. The delegate for
//! the numbers these rules consume is `paraeq-dsp`, which is fixture-pinned;
//! what is tested here is the POLICY laid over them.

mod common;

use common::{
    authority_policy_for, cal_with_curve, minimal_bundle, shaped_bundle, sos_impulse,
    synthetic_bundle, synthetic_targets, target_curve, well_formed_bundle, with_override,
    SyntheticSpec, EVERY_CLASS,
};
use paraeq_decide::{
    decide, profile_for, AuthorityPreset, CalVariant, CorrectionForm, CouplingPath, Decisions,
    Evidence, EvidenceLabel, MeasurementBundle, QCapPolicy, RationaleKey, Source, TargetChoice,
    TransducerClass,
};
use paraeq_dsp::authority::{
    build_authority_gated, max_q_for_boost_capped, AuthorityPolicy, DEFAULT_MIN_DIP_WIDTH_OCT,
};
use paraeq_dsp::biquad;
use paraeq_dsp::gating::{resolution_limit_hz, RESOLUTION_FRACTION};
use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::peq::{EQBand, ParametricEQ};

/// Floating-point slack for a comparison against a number `decide()` computed
/// the same way the test does. Not a tolerance on policy — policy assertions
/// below are brackets, not epsilons.
const EPS: f64 = 1e-9;

/// The rate every shaped bundle is built at, so a test can design a filter and
/// evaluate it on the same axis `decide()` does.
const RATE: f64 = 48_000.0;

/// Every band the plan emitted, flattened across channels.
fn bands_of(bundle: &MeasurementBundle) -> Vec<EQBand> {
    decide(bundle)
        .correction
        .map(|c| c.bands.iter().flatten().cloned().collect())
        .unwrap_or_default()
}

// ===========================================================================
// The echoes — `class`, `positions_n`
// ===========================================================================

/// `class`: "Echo the declared class." The copy renders the owner-decided UI
/// string, never the `Debug` spelling.
#[test]
fn class_echoes_the_declared_class_and_renders_its_display_name() {
    for class in EVERY_CLASS {
        let set = decide(&well_formed_bundle(class));
        assert_eq!(set.decisions.class.value, class);
        assert_eq!(set.decisions.class.rationale.key, RationaleKey::Class);
        assert!(
            set.decisions
                .class
                .rationale
                .text
                .contains(class.display_name()),
            "{class:?}: the class copy must name the class in the wizard's own words, got {:?}",
            set.decisions.class.rationale.text
        );
    }
}

/// `positions_n`: "Echo accepted count", verbatim — NOT clamped into its
/// domain. A count below the hard minimum is `TooFewPositions`, and clamping
/// the echo would hide the very number the refusal is about.
#[test]
fn positions_n_echoes_the_accepted_count_and_is_never_clamped() {
    let bundle = synthetic_bundle(SyntheticSpec {
        positions: 2,
        ..SyntheticSpec::default()
    });
    let set = decide(&bundle);
    assert_eq!(set.decisions.positions_n.value, 2);
    assert!(
        !set.decisions
            .positions_n
            .domain
            .contains(&set.decisions.positions_n.value),
        "a two-position room capture is outside `positions_domain` BY DESIGN"
    );
}

// ===========================================================================
// The analysis rows — window, gates, FDW
// ===========================================================================

/// `left_window_ms`: `min(requested, t_peak, 0.5·dt₂)`, where § D-J takes the
/// STRICTER of the spec's half-dt₂ and `apply_gate`'s own full-dt₂ clamp.
///
/// The default capture's Farina bound is ~586 ms, so `t_peak` binds and the
/// half-dt₂ term is invisible. This test shortens the sweep until half-dt₂ is
/// the smaller of the two, which is the only way to tell the two clamps apart.
#[test]
fn left_window_clamps_to_half_farina_and_to_the_peak() {
    let mut bundle = well_formed_bundle(TransducerClass::Bookshelf);
    let peak_ms = bundle.positions[0].ir.peak as f64 * 1000.0 / RATE;

    // Long sweep: dt₂/2 ≈ 293 ms, so `t_peak` (~46 ms) is the binding clamp.
    let long = decide(&bundle);
    assert!(
        (long.decisions.left_window_ms.value - peak_ms).abs() < 1e-6,
        "with a 5.5 s sweep the peak binds, got {}",
        long.decisions.left_window_ms.value
    );

    // Short sweep: dt₂ = T·ln2/ln(f₂/f₁) = 0.5·0.6931/ln(20000/30) ≈ 53.3 ms,
    // so half of it (~26.6 ms) is stricter than the ~46 ms peak.
    bundle.capture.sweep.duration_s = 0.5;
    let sweep = &bundle.capture.sweep;
    let half_dt2_ms =
        0.5 * 1000.0 * sweep.duration_s * 2f64.ln() / (sweep.f_end_hz / sweep.f_start_hz).ln();
    assert!(half_dt2_ms < peak_ms, "the test's own premise");
    let short = decide(&bundle);
    assert!(
        (short.decisions.left_window_ms.value - half_dt2_ms).abs() < 1e-6,
        "with a 0.5 s sweep the HALF Farina bound binds: expected {half_dt2_ms}, got {}",
        short.decisions.left_window_ms.value
    );
    assert!(short.decisions.left_window_ms.value <= peak_ms + EPS);
}

/// `right_window_ms`: "Publish the **stricter** resolution limit
/// `f = (1/T)/(2^(1/2N) − 2^(−1/2N))`, not `1/T`."
///
/// A 400 ms window is 1/6-octave-valid above ~21.6 Hz, not 2.5 Hz. Publishing
/// `1/T` would tell the user the measurement resolves detail an octave and a
/// half below where it actually does.
#[test]
fn right_window_publishes_the_stricter_resolution_limit() {
    let set = decide(&well_formed_bundle(TransducerClass::Bookshelf));
    let window_ms = set.decisions.right_window_ms.value;
    assert_eq!(window_ms, 400.0, "room: 400 ms");
    let strict = resolution_limit_hz(window_ms / 1000.0, RESOLUTION_FRACTION);
    let naive = 1000.0 / window_ms;

    let published = set
        .decisions
        .right_window_ms
        .evidence
        .iter()
        .find_map(|e| match e {
            Evidence::Scalar {
                label: EvidenceLabel::ResolutionLimit,
                value,
                ..
            } => Some(*value),
            _ => None,
        })
        .expect("the resolution limit is published as evidence");
    assert!((published - strict).abs() < 1e-9);
    assert!(
        published > naive * 5.0,
        "the stricter limit ({published}) must not be the 1/T figure ({naive})"
    );
}

/// `fdw_post_cycles`: § D-K's domain, "**derived at runtime** as
/// `STORE_POST_MS/1000 · grid.f_min()` and additionally floored by the grid's
/// `ppo/1.849` limit". On the shipped grid that is `min(1.5·20, 96/1.849) =
/// min(30.0, 51.9) = 30.0` — the store window binds, not the grid. The spec's
/// former 61.0 ceiling was unattainable under either bound.
#[test]
fn fdw_post_cycles_domain_is_what_the_store_and_the_grid_can_honour() {
    let grid = LogGrid::standard();
    let expected =
        (1500.0 / 1000.0 * grid.f_min()).min(f64::from(grid.points_per_octave()) / 1.849);
    assert!(
        (expected - 30.0).abs() < 1e-9,
        "the arithmetic, spelled out"
    );

    for class in EVERY_CLASS {
        let set = decide(&well_formed_bundle(class));
        match set.decisions.fdw_post_cycles.domain {
            paraeq_decide::Domain::Range { max, min, .. } => {
                assert!((max - expected).abs() < 1e-9, "{class:?}: ceiling {max}");
                assert_eq!(min, 3.0, "{class:?}");
            }
            ref other => panic!("{class:?}: fdw_post_cycles domain is a Range, got {other:?}"),
        }
    }
}

/// `fdw_pre_cycles`: "Default 3; clamped `≤ fdw_post_cycles`." The clamp is a
/// rule, not a domain, so it is checked on the VALUE — including when the post
/// lobe is overridden down onto the pre lobe's own default.
#[test]
fn fdw_pre_cycles_never_exceeds_the_post_lobe() {
    for class in EVERY_CLASS {
        let bundle = well_formed_bundle(class);
        let set = decide(&bundle);
        assert!(
            set.decisions.fdw_pre_cycles.value <= set.decisions.fdw_post_cycles.value + EPS,
            "{class:?}: pre {} > post {}",
            set.decisions.fdw_pre_cycles.value,
            set.decisions.fdw_post_cycles.value
        );

        let pinched = decide(&with_override(
            &bundle,
            "fdw_post_cycles",
            serde_json::json!(3.0),
        ));
        assert!(
            pinched.decisions.fdw_pre_cycles.value <= 3.0 + EPS,
            "{class:?}: the pre lobe must follow the post lobe down, got {}",
            pinched.decisions.fdw_pre_cycles.value
        );

        // Ruling R-A11: the row's rule is "clamped `≤ fdw_post_cycles`", and a
        // DRAWER value has to obey it too. 61.0 is the top of the pre lobe's
        // own domain, so it is legal on its own terms and § D-N's out-of-domain
        // clamp never sees it — the clamp that binds here is this row's.
        let over_the_post_lobe = decide(&with_override(
            &bundle,
            "fdw_pre_cycles",
            serde_json::json!(61.0),
        ));
        let post = over_the_post_lobe.decisions.fdw_post_cycles.value;
        assert!(
            over_the_post_lobe.decisions.fdw_pre_cycles.value <= post + EPS,
            "{class:?}: an override of 61.0 with post {post} survived as {}",
            over_the_post_lobe.decisions.fdw_pre_cycles.value
        );
        assert_eq!(
            over_the_post_lobe.decisions.fdw_pre_cycles.source,
            Source::UserOverride,
            "{class:?}: the clamp must not un-record the user's intent"
        );
    }
}

/// The four rows whose Rule column is the path table and nothing else, checked
/// against `PathProfile` so a profile edit cannot silently diverge from what
/// `decide()` publishes.
#[test]
fn the_path_table_rows_echo_the_profile() {
    for class in EVERY_CLASS {
        let profile = profile_for(class);
        let d = decide(&well_formed_bundle(class)).decisions;
        assert_eq!(d.averaging.value, profile.averaging, "{class:?}");
        assert_eq!(d.smoothing.value, profile.smoothing, "{class:?}");
        assert_eq!(
            d.flatness_target_db.value, profile.flatness_target_db,
            "{class:?}"
        );
        assert_eq!(d.q_cap.value, profile.q_cap, "{class:?}");
        assert_eq!(d.align_spl_band.value, (200.0, 2000.0), "{class:?}: § D-B");
        assert_eq!(
            d.window_type.value,
            paraeq_decide::WindowType::Tukey(0.25),
            "{class:?}"
        );
        assert_eq!(d.correction_kind.value, CorrectionForm::Peq, "{class:?}");
        assert_eq!(
            d.right_window_ms.value,
            match profile.coupling {
                CouplingPath::Coupler => 1000.0,
                CouplingPath::Room => 400.0,
            },
            "{class:?}"
        );
    }
}

// ===========================================================================
// Scan 1 — `low_corner_hz`
//
// B7a's default was `bundle.capture.sweep.f_start_hz` (30 Hz bookshelf,
// 20 Hz everywhere else), so every assertion below that puts the corner
// anywhere else falsifies it.
// ===========================================================================

/// A room bundle whose response rolls off below `shelf_hz`, built from a low
/// shelf so the roll-off has a closed form rather than a random tail.
///
/// −20 dB rather than something deeper on purpose: the scan's answer is the
/// −10 dB-relative point, and a −20 dB shelf puts that point near the shelf's
/// own corner, where the shelf's closed form is the clearest oracle. A −36 dB
/// shelf is still 13 dB down an octave above its corner, which is a speaker
/// with NO corner at or below the scan's 200 Hz top — a real answer, but a
/// different test.
fn rolled_off_bundle(shelf_hz: f64) -> MeasurementBundle {
    shaped_bundle(
        SyntheticSpec {
            cal: true,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0001,
        },
        move |_| vec![biquad::low_shelf(shelf_hz, -20.0, 0.707, RATE)],
    )
}

/// `low_corner_hz`: "Lowest `f` such that `mag(f′) ≥ M − 10 dB` for all
/// `f′ ∈ [f, 200]`."
///
/// The oracle is the shelf: move the shelf up and the measured corner must
/// follow it up, because that is what "we measured that; we didn't assume it"
/// means. A rule that echoed the sweep's start frequency would answer 30 Hz for
/// both.
#[test]
fn low_corner_scan_finds_the_first_sustained_minus_ten_db_crossing() {
    let low = decide(&rolled_off_bundle(60.0)).decisions.low_corner_hz;
    let high = decide(&rolled_off_bundle(150.0)).decisions.low_corner_hz;

    assert_eq!(low.source, Source::Auto, "the scan measured it");
    assert_eq!(high.source, Source::Auto);
    assert!(
        high.value > low.value * 1.8,
        "a shelf at 150 Hz must report a much higher corner than one at 60 Hz: \
         {} vs {}",
        high.value,
        low.value
    );
    assert!(
        low.value > 40.0 && low.value < 90.0,
        "a 60 Hz shelf's −10 dB-relative point sits near its own corner, got {}",
        low.value
    );
    assert!(
        high.value > 100.0 && high.value < 200.0,
        "…and a 150 Hz shelf's sits near 150, got {}",
        high.value
    );
    // The falsifier against B7a's default, spelled out: the bookshelf profile
    // sweeps from 30 Hz, and neither answer may be that number.
    assert!(low.value > 30.0 && high.value > 30.0);
}

/// The "**and stays there**" clause, which is the whole rule: a dip deeper than
/// 10 dB below midband breaks the run and the corner cannot be reported below
/// it; a dip inside the 10 dB tolerance does not break it at all.
///
/// Both halves matter. Without the first, an isolated notch is read as part of
/// the passband and a speaker that stops at 60 Hz is reported as reaching 20.
/// Without the second, every small ripple becomes a corner.
#[test]
fn an_isolated_dip_moves_the_low_corner_only_when_it_breaks_the_ten_db_run() {
    let dipped = |depth_db: f64| {
        shaped_bundle(
            SyntheticSpec {
                cal: true,
                channels: 1,
                class: TransducerClass::Bookshelf,
                positions: 5,
                sample_rate: RATE as u32,
                seed: 0x5EED_0002,
            },
            move |_| vec![biquad::peaking(60.0, depth_db, 4.0, RATE)],
        )
    };

    let shallow = decide(&dipped(-8.0)).decisions.low_corner_hz.value;
    let deep = decide(&dipped(-12.0)).decisions.low_corner_hz.value;

    assert!(
        shallow < 30.0,
        "an 8 dB dip stays inside the 10 dB tolerance, so the run is unbroken \
         and the corner is the bottom of the grid, got {shallow}"
    );
    assert!(
        deep >= 60.0 && deep < 60.0 * 2f64.powf(1.0 / 3.0),
        "a 12 dB dip breaks the run at 60 Hz, so the corner is the top edge of \
         that dip and nothing higher, got {deep}"
    );
}

/// `low_corner_hz`'s FALLBACK: no crossing anywhere at or below 200 Hz ⇒ the
/// TOP of the scan, `Source::Default`.
///
/// Ruling R-A5. The number is load-bearing — it is `correction_range`'s low
/// edge and the bottom of the `AbsurdCurve` span band and the
/// `ExcessiveVariance` band — and nothing named it before this test.
///
/// **200 Hz, not 20 Hz, and the direction is the whole reason.** "We never got
/// within 10 dB of midband anywhere below 200 Hz" means nothing below 200 Hz is
/// worth correcting; answering with the bottom of the grid would claim the
/// opposite and spend excursion on a band the measurement says is not there.
/// Narrowing is the safe direction.
#[test]
fn low_corner_falls_back_to_the_scan_top_with_source_default() {
    // A −40 dB low shelf at 400 Hz: an octave below its corner the curve is
    // still tens of dB down, so there is no frequency at or below 200 Hz from
    // which the run to 200 Hz stays within 10 dB of midband.
    let bottomless = shaped_bundle(
        SyntheticSpec {
            cal: true,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0011,
        },
        |_| vec![biquad::low_shelf(400.0, -40.0, 0.707, RATE)],
    );
    let set = decide(&bottomless);
    let corner = &set.decisions.low_corner_hz;
    assert_eq!(
        corner.value, 200.0,
        "the fallback is the TOP of the scan, not the bottom of the grid"
    );
    assert_eq!(
        corner.source,
        Source::Default,
        "the enum's own meaning: evidence absent or inconclusive"
    );
    // And the narrowing really reaches the band that spends excursion.
    assert!(
        set.decisions.correction_range.value.0 >= 200.0,
        "the fallback must narrow `correction_range`, got {:?}",
        set.decisions.correction_range.value
    );

    // The complement, so the test is not vacuous: a bundle that DOES have a
    // corner reports it as measured rather than falling back.
    let measured = decide(&rolled_off_bundle(60.0)).decisions.low_corner_hz;
    assert_eq!(measured.source, Source::Auto);
    assert!(measured.value < 200.0, "got {}", measured.value);
}

// ===========================================================================
// Scan 2 — `transition_hz`
//
// B7a's default was a flat 200.0 with `Source::Default`.
// ===========================================================================

/// A room bundle whose five positions disagree around `centre_hz` — position
/// `p` carries a peaking filter of `-spread_db + p·spread_db/2` — and agree
/// everywhere else.
///
/// **Two independent knobs, and the separation is the point.** `spread_db` sets
/// how HIGH σ(f) climbs at the centre; `q` sets how WIDE the region where it is
/// high turns out to be. The sustain clause ("stays ≥ 3 dB for ≥ 1/3 octave")
/// grades the width alone, so a fixture meant to fail it must clear the HEIGHT
/// gate first — otherwise the scan never reaches the clause and the test passes
/// for the wrong reason, which is exactly what the Q=20/±6 dB fixture used to
/// do (its σ peaked at 2.60 dB and never touched 3.0).
fn scattered_bundle(centre_hz: f64, q: f64, spread_db: f64) -> MeasurementBundle {
    shaped_bundle(
        SyntheticSpec {
            cal: true,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0003,
        },
        move |index| {
            let gain = -spread_db + 0.5 * spread_db * index as f64;
            vec![biquad::peaking(centre_hz, gain, q, RATE)]
        },
    )
}

/// The highest σ(f) `decide()` published, and the widest contiguous run of bins
/// at or above 3 dB, in octaves.
///
/// An oracle over the PUBLISHED σ curve — `Analysis::sigma_db` is the same
/// array `transition_scan` read — so a test can say "this σ really does cross
/// 3 dB, and the crossing really is too narrow to sustain" without asking the
/// rule to confirm its own answer. The run is measured between the first and
/// last bin at or above the threshold, which is how far the crossing can be
/// SHOWN to hold on this grid.
fn sigma_peak_and_widest_run_oct(set: &paraeq_decide::DecisionSet) -> (f64, f64, f64) {
    let freqs = &set.analysis.freqs_hz;
    let sigma = &set.analysis.sigma_db;
    let peak = sigma.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let (mut start_hz, mut widest_oct) = (f64::NAN, 0.0f64);
    let mut i = 0;
    while i < sigma.len() {
        if sigma[i] < 3.0 {
            i += 1;
            continue;
        }
        let first = i;
        while i < sigma.len() && sigma[i] >= 3.0 {
            i += 1;
        }
        let run_oct = (freqs[i - 1] / freqs[first]).log2();
        if run_oct > widest_oct {
            (start_hz, widest_oct) = (freqs[first], run_oct);
        }
    }
    (peak, start_hz, widest_oct)
}

/// `transition_hz`: "Lowest `f` where `σ(f) ≥ 3.0 dB` and stays ≥ for ≥ 1/3
/// octave. … No crossing ⇒ 200.0, `source: Default`."
///
/// Three cases, and all three are the rule: a broad crossing is reported and
/// measured; a crossing too narrow to sustain is not; and no crossing at all
/// falls back to 200.0 with the σ curve STILL attached as evidence — "we could
/// not measure this" has to be showable, which is the fallback's whole point.
#[test]
fn transition_hz_is_the_sustained_three_db_crossing_and_falls_back_to_200() {
    // Broad: a Q=1 disagreement around 140 Hz keeps σ over 3 dB for about an
    // octave, so the crossing is real and the rule reports where it starts.
    let broad = decide(&scattered_bundle(140.0, 1.0, 6.0))
        .decisions
        .transition_hz;
    assert_eq!(
        broad.source,
        Source::Auto,
        "a measured crossing is not a default"
    );
    assert!(
        broad.value >= 80.0 && broad.value < 200.0,
        "the crossing below a 140 Hz scatter centre, clamped into [80, 400], \
         got {}",
        broad.value
    );

    // Narrow: a Q=20 disagreement is ~0.07 octave wide, well inside the 1/3
    // octave the clause requires, so it is scatter and not a transition.
    //
    // The ±9 dB spread is what makes this case grade the SUSTAIN clause rather
    // than the threshold. At ±6 dB the same Q=20 scatter peaked at 2.60 dB, so
    // σ never reached 3.0 anywhere and the scan fell back without ever
    // consulting the clause — the fallback was right for the wrong reason, and
    // `a_sigma_crossing_too_narrow_to_sustain_is_scatter_not_a_transition`
    // below is the falsifier that found it.
    let narrow_set = decide(&scattered_bundle(140.0, 20.0, 9.0));
    let (narrow_peak, _, narrow_run_oct) = sigma_peak_and_widest_run_oct(&narrow_set);
    assert!(
        narrow_peak > 3.0,
        "precondition: σ must CROSS 3 dB for this case to reach the sustain \
         clause at all; it peaks at {narrow_peak:.3} dB"
    );
    assert!(
        narrow_run_oct < 1.0 / 3.0,
        "precondition: and the crossing must be too narrow to sustain; it holds \
         for {narrow_run_oct:.4} octave"
    );
    let narrow = narrow_set.decisions.transition_hz;
    assert_eq!(narrow.value, 200.0);
    assert_eq!(narrow.source, Source::Default);

    // None: identical positions, σ ≈ 0 everywhere.
    let flat = shaped_bundle(
        SyntheticSpec {
            cal: true,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0004,
        },
        |_| Vec::new(),
    );
    let fallback = decide(&flat).decisions.transition_hz;
    assert_eq!(fallback.value, 200.0, "the fallback constant, not a rule");
    assert_eq!(fallback.source, Source::Default);
    assert!(
        fallback.evidence.iter().any(|e| matches!(
            e,
            Evidence::Curve {
                label: EvidenceLabel::Sigma,
                ..
            }
        )),
        "the σ curve stays attached on the fallback path: the drawer must be \
         able to say WHY it could not measure one"
    );
    assert!(
        fallback.evidence.iter().any(|e| matches!(
            e,
            Evidence::Span {
                label: EvidenceLabel::SchroederRange,
                ..
            }
        )),
        "§ D-L: `room::transition_range` rides along as the cross-check"
    );
}

/// The sustain clause on its own, with the HEIGHT of the crossing held fixed
/// and only its WIDTH moved.
///
/// The rule is two conditions joined by an "and": σ(f) reaches 3 dB, **and** it
/// stays there for a third of an octave. Every earlier fixture varied both at
/// once, so deleting the second condition — `if sustained || true` — changed no
/// test's answer: the narrow case fell back because its σ never reached 3 dB,
/// which the first condition already handles.
///
/// Both bundles here scatter by the same ±12 dB, so both cross 3 dB by a wide
/// margin (the assertions say so). The only thing Q moves is how long the
/// crossing holds: at Q=12 it holds for about a sixth of an octave and at Q=4
/// for about half of one. The rule must answer differently, and what it answers
/// is the difference between "the room stops being correctable everywhere above
/// here" and "one bin was noisy".
///
/// **Why the width matters to a user and not only to the scan.** `transition_hz`
/// is what the Advanced drawer renders as "below {f_t} Hz your room's problems
/// are the same everywhere you sit". Reporting a sixth of an octave of scatter
/// as that frequency puts a sentence about the whole room behind a
/// disagreement that is narrower than the ear's own resolution there.
#[test]
fn a_sigma_crossing_too_narrow_to_sustain_is_scatter_not_a_transition() {
    // ~1/6 octave of crossing: over the 3 dB line, under the 1/3 octave rule.
    let scatter = decide(&scattered_bundle(140.0, 12.0, 12.0));
    let (scatter_peak, _, scatter_run_oct) = sigma_peak_and_widest_run_oct(&scatter);
    assert!(
        scatter_peak > 3.0,
        "σ must clear the 3 dB threshold, or the sustain clause is never \
         reached and this case proves nothing; it peaks at {scatter_peak:.3} dB"
    );
    assert!(
        (0.125..1.0 / 3.0).contains(&scatter_run_oct),
        "and it must hold for roughly a sixth of an octave — over a bin's width \
         and under the rule's third — but it holds for {scatter_run_oct:.4}"
    );
    assert_eq!(
        scatter.decisions.transition_hz.value, 200.0,
        "a crossing that cannot be shown to stay is not a crossing this rule \
         may report: σ peaked at {scatter_peak:.3} dB over {scatter_run_oct:.4} \
         octave"
    );
    assert_eq!(
        scatter.decisions.transition_hz.source,
        Source::Default,
        "and the fallback is labelled a fallback, never a measurement"
    );

    // The same ±12 dB disagreement, spread over ~1/2 octave by dropping Q.
    let transition = decide(&scattered_bundle(140.0, 4.0, 12.0));
    let (wide_peak, wide_start_hz, wide_run_oct) = sigma_peak_and_widest_run_oct(&transition);
    assert!(
        wide_peak > 3.0,
        "the wide case crosses too — only the width changed; σ peaks at \
         {wide_peak:.3} dB"
    );
    assert!(
        wide_run_oct >= 1.0 / 3.0,
        "the wide case must SATISFY the clause the narrow one fails, or the \
         pair is not a controlled comparison; it holds for {wide_run_oct:.4} \
         octave"
    );
    assert_eq!(
        transition.decisions.transition_hz.source,
        Source::Auto,
        "a sustained crossing is a measurement"
    );
    // This σ has one contiguous crossing, so its lowest bin is the lowest
    // frequency at which a sustained crossing can begin — the number the rule
    // reports, read off the published curve rather than off the rule.
    assert!(
        (transition.decisions.transition_hz.value - wide_start_hz).abs() < EPS,
        "the rule reports the crossing's own lowest bin: said {}, the σ curve \
         crosses at {wide_start_hz}",
        transition.decisions.transition_hz.value
    );
}

/// `transition_hz` "is never an input to the authority weight", and
/// `authority.rs` says the same of `room::TransitionRange`: "display-only and
/// deliberately NOT an authority input".
///
/// This is the seam where the refuted "rooms are minimum-phase below 200 Hz"
/// reasoning would creep back in, so it is pinned mechanically.
///
/// **The rationale TEXT is excluded, and deliberately.** The decision table's
/// own copy for `authority` quotes `f_t` ("Above {f_t:.0} Hz the dips are sound
/// cancelling itself out"), so the prose moves with the override while none of
/// the numbers do. Excluding it is what makes the assertion about the physics
/// rather than about the sentence.
#[test]
fn transition_hz_is_never_an_authority_input() {
    let bundle = well_formed_bundle(TransducerClass::Bookshelf);
    let auto = decide(&bundle);
    let pinned = decide(&with_override(
        &bundle,
        "transition_hz",
        serde_json::json!(137.0),
    ));

    assert_eq!(
        pinned.decisions.transition_hz.value, 137.0,
        "the override arrived"
    );
    assert_eq!(
        auto.analysis.authority, pinned.analysis.authority,
        "the resolved authority CURVE must not move with the transition report"
    );
    assert_eq!(
        auto.decisions.authority.value,
        pinned.decisions.authority.value
    );
    assert_eq!(
        auto.decisions.authority.domain,
        pinned.decisions.authority.domain
    );
    assert_eq!(
        auto.decisions.authority.evidence,
        pinned.decisions.authority.evidence
    );
    assert_eq!(
        auto.correction, pinned.correction,
        "and neither do the bands"
    );
}

/// § D-D's split, enforced: the `authority` DOMAIN carries names, and the
/// resolved curve lives in `Analysis` and nowhere else.
///
/// Ruling R-A4. The domain used to carry `Custom(analysis.authority.clone())` —
/// the run's own resolved curve, byte for byte, 77–89 KB per `expected.json`
/// for a value that is "Custom = whatever Standard produced". A domain is what
/// the drawer DRAWS; it is not a second copy of a product.
///
/// `Custom` is still legal, and legal by CONSTRUCTION rather than by being
/// enumerated: `AuthorityCurve` is sealed, so every representable
/// `AuthorityPreset` has already been validated and a `Choice` list could not
/// enumerate the curves anyway.
#[test]
fn the_authority_domain_lists_the_two_named_presets_and_not_the_resolved_curve() {
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let authority = &set.decisions.authority;
        assert_eq!(
            authority.domain,
            paraeq_decide::Domain::Choice(vec![
                AuthorityPreset::Conservative,
                AuthorityPreset::Standard,
            ]),
            "{class:?}: the domain is the two NAMES"
        );
        assert_eq!(authority.value, AuthorityPreset::Standard, "{class:?}");

        // The curve is still published — as a product, once.
        assert!(
            !set.analysis.authority.freqs().is_empty(),
            "{class:?}: the resolved curve is still an Analysis product"
        );

        // And the wire no longer carries it twice. The whole `Decisions::authority`
        // subtree must be far smaller than the resolved curve's own JSON.
        let domain_json = serde_json::to_string(&authority.domain)
            .expect("a domain is plain derived data")
            .len();
        let curve_json = serde_json::to_string(&set.analysis.authority)
            .expect("a curve is plain derived data")
            .len();
        assert!(
            domain_json * 100 < curve_json,
            "{class:?}: the domain is {domain_json} bytes against the curve's              {curve_json} — it is still carrying a copy"
        );
    }
}

/// A `Custom(curve)` override is in-domain by construction, so it neither warns
/// nor gets clamped away.
///
/// Ruling R-A4, the other half. The `Choice` list cannot enumerate a curve, so
/// a `contains` that only walked the list would call every legitimate custom
/// ceiling illegal — and § D-N's clamp would then replace it with the rule's
/// own value, silently discarding exactly the override the drawer exists to
/// offer.
#[test]
fn a_custom_authority_override_is_in_domain_and_survives() {
    let bundle = lumpy_bundle(TransducerClass::Bookshelf);
    let resolved = decide(&bundle).analysis.authority;
    let custom = serde_json::to_value(AuthorityPreset::Custom(resolved))
        .expect("a preset is plain derived data");

    let set = decide(&with_override(&bundle, "authority", custom));
    assert_eq!(set.decisions.authority.source, Source::UserOverride);
    assert!(
        matches!(set.decisions.authority.value, AuthorityPreset::Custom(_)),
        "the override must survive, got {:?}",
        set.decisions.authority.value
    );
    assert!(
        set.decisions
            .authority
            .domain
            .contains(&set.decisions.authority.value),
        "a sealed custom curve is in-domain by construction"
    );
    assert!(
        !set.diagnostics
            .iter()
            .any(|d| d.code == paraeq_decide::DiagnosticCode::OverrideOutOfDomain),
        "{:?}",
        set.diagnostics
    );
}

// ===========================================================================
// Scans 3 and 4 — `correction_range`
//
// B7a's default was `(sweep.f_start_hz, grid.f_max())` — the whole analysis
// band, uncut.
// ===========================================================================

/// `correction_range` low edge: "`max(low_corner_hz, right-window resolution
/// limit, lowest bin with SNR ≥ 25 dB)`".
///
/// Two of the three are checked here because the third (`low_corner_hz`) has
/// its own module: on a flat capture the room's 400 ms window is the binding
/// term at ~21.6 Hz, and raising the noise floor under 100 Hz must take the
/// SNR term past it.
#[test]
fn correction_range_low_takes_the_resolution_limit_and_the_snr_floor() {
    let mut bundle = shaped_bundle(
        SyntheticSpec {
            cal: true,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0005,
        },
        |_| Vec::new(),
    );

    let quiet = decide(&bundle).decisions.correction_range;
    let limit = resolution_limit_hz(0.4, RESOLUTION_FRACTION);
    assert_eq!(quiet.source, Source::Auto);
    assert!(
        (quiet.value.0 - limit).abs() < 1e-6,
        "a 400 ms window cannot resolve 1/6 octave below {limit} Hz, got {}",
        quiet.value.0
    );

    // A floor that sits ABOVE the measured curve below 100 Hz: SNR there is
    // negative, so no bin under 100 Hz clears the 25 dB gate.
    bundle.noise_floor.freqs_hz = vec![20.0, 100.0, 140.0, 20_000.0];
    bundle.noise_floor.spectrum_db = vec![vec![20.0, 20.0, -90.0, -90.0]];
    let noisy = decide(&bundle).decisions.correction_range;
    assert!(
        noisy.value.0 > 100.0,
        "with the floor above the signal below 100 Hz the low edge must move \
         up past it, got {}",
        noisy.value.0
    );
    assert!(
        noisy.value.0 < 400.0,
        "…but not past where the floor drops again"
    );
}

/// `correction_range` high edge: "no filters above the frequency where measured
/// **last drops below** target", scanned DOWN.
///
/// The direction IS the rule. This capture sits above target through the
/// midband, dips below it at 6 kHz, recovers, and then rolls off for good above
/// ~14 kHz. Scanning UP returns the 6 kHz dip and throws away an octave and a
/// half of working tweeter; scanning DOWN steps over the dip and finds the
/// roll-off knee, which is what the row is about.
#[test]
fn correction_range_high_stops_at_the_last_target_crossing() {
    let spec = SyntheticSpec {
        cal: true,
        channels: 1,
        class: TransducerClass::OverEar,
        positions: 5,
        sample_rate: RATE as u32,
        seed: 0x5EED_0006,
    };
    // A target that falls away above 1 kHz, so a flat measurement sits ABOVE it
    // there and the only crossings are the ones this test builds.
    let falling = || vec![target_curve("falling", [0.0, 0.0, -6.0])];

    let mut rolled = shaped_bundle(spec, |_| {
        vec![
            // The decoy: a 6 kHz suckout that dives under the target and comes
            // back out again.
            biquad::peaking(6000.0, -6.0, 2.0, RATE),
            // The knee: below target for good above ~14 kHz.
            biquad::high_shelf(14_000.0, -20.0, 0.707, RATE),
        ]
    });
    rolled.targets = falling();
    let high = decide(&rolled).decisions.correction_range.value.1;
    assert!(
        high > 7000.0,
        "the scan must step OVER the 6 kHz dip, not stop at it, got {high}"
    );
    assert!(
        high < 17_000.0,
        "…and must stop at the roll-off knee rather than at the top of the \
         grid, got {high}"
    );

    // "A curve never dropping below target yields `min(20000, highest
    // SNR-valid bin)`." Same falling target, no roll-off: a flat measurement is
    // above it everywhere above the midband, so the rule imposes no bound at
    // all and the only one left is the SNR-valid top.
    let mut never = shaped_bundle(spec, |_| Vec::new());
    never.targets = falling();
    let open = decide(&never).decisions.correction_range.value.1;
    // The highest SNR-valid BIN, which is the grid's last point (19896.97 Hz on
    // the shipped 96-ppo grid) rather than the nominal 20 kHz the `min` is
    // taken against.
    let top_bin = *LogGrid::standard()
        .freqs()
        .last()
        .expect("the standard grid is not empty");
    assert!(
        (open - top_bin.min(20_000.0)).abs() < 1e-9,
        "with no crossing the only bound left is the SNR-valid top ({top_bin}), \
         got {open}"
    );
}

// ===========================================================================
// `target` — the three selection cases
// ===========================================================================

/// "A room measurement never matches a coupler curve." The class filter is a
/// type-level constraint, not a UI default: an unfiltered match returns
/// `harman_oe_2018` for a loudspeaker, which is +8.3 dB at 3 kHz against the
/// B&K room curve's −3.0 dB — an 11.3 dB category error at the ear's most
/// sensitive frequency, double-applying ear gain the loudspeaker already
/// delivers acoustically.
#[test]
fn a_room_measurement_never_matches_a_coupler_curve() {
    for class in [TransducerClass::Bookshelf, TransducerClass::Floorstander] {
        let bundle = well_formed_bundle(class);
        assert!(
            bundle.targets.iter().any(|t| t.name == "harman_oe_2018"),
            "the test's own premise: the coupler curve IS in the candidate pool"
        );
        let target = decide(&bundle).decisions.target;
        assert!(
            matches!(target.value, TargetChoice::Parametric { .. }),
            "{class:?}: got {:?}",
            target.value
        );
        assert_eq!(
            target.source,
            Source::Default,
            "{class:?}: the table's own column"
        );
        assert_eq!(target.rationale.key, RationaleKey::TargetRoomParametric);
        assert!(
            !target.domain.contains(&TargetChoice::Curve {
                name: "harman_oe_2018".to_string()
            }),
            "{class:?}: a coupler curve must not even be OFFERED on a room path"
        );
    }
}

/// "Coupler, EARS HEQ/HPN/IDF cal: force `flat.csv` + `Warn(CalHasTargetBakedIn)`."
///
/// The warning is the refusal table's; the forced curve and the copy are this
/// row's. All three EARS variants behave identically, and a plain cal on the
/// same capture must still match.
#[test]
fn an_ears_cal_with_a_target_baked_in_forces_flat() {
    for variant in [
        CalVariant::EarsHeq,
        CalVariant::EarsHpn,
        CalVariant::EarsIdf,
    ] {
        let mut bundle = well_formed_bundle(TransducerClass::OverEar);
        bundle
            .cal
            .as_mut()
            .expect("the fixture carries a cal")
            .variant = variant;
        let target = decide(&bundle).decisions.target;
        assert_eq!(
            target.value,
            TargetChoice::Curve {
                name: "flat".to_string()
            },
            "{variant:?}: applying a second target would apply it twice"
        );
        assert_eq!(target.source, Source::Auto);
        assert_eq!(target.rationale.key, RationaleKey::TargetCalBakedIn);
    }

    let plain = decide(&well_formed_bundle(TransducerClass::OverEar))
        .decisions
        .target;
    assert_eq!(
        plain.rationale.key,
        RationaleKey::TargetMatched,
        "a plain cal matches; only a baked-in one is forced"
    );
}

/// The fallback arm must not claim a match it never made.
///
/// Ruling R-A11. When the bundle is unanalysable there is no curve to compare
/// against, so `target` falls back to the first class-legal candidate — and it
/// used to render `TargetMatched`'s "that's the curve we matched, out of 4 we
/// tried", which is a sentence about a comparison that did not happen.
#[test]
fn the_target_fallback_copy_does_not_claim_a_match() {
    // Zero positions: the analysis publishes no curves, so `match_closest_target`
    // is never reached and the fallback arm is the only one left.
    let bundle = synthetic_bundle(SyntheticSpec {
        class: TransducerClass::OverEar,
        positions: 0,
        ..SyntheticSpec::default()
    });
    let target = decide(&bundle).decisions.target;
    assert!(
        matches!(target.value, TargetChoice::Curve { .. }),
        "the fallback still answers with a legal candidate, got {:?}",
        target.value
    );
    assert_eq!(target.source, Source::Default);
    assert_eq!(target.rationale.key, RationaleKey::TargetFallback);
    assert!(
        !target.rationale.text.contains("matched"),
        "the fallback copy claims a match: {:?}",
        target.rationale.text
    );
    assert!(
        !target.rationale.text.contains("we tried"),
        "the fallback copy counts candidates it never compared: {:?}",
        target.rationale.text
    );
    assert!(
        target.domain.contains(&target.value),
        "the fallback value must stay inside its own domain"
    );
}

/// "Coupler, normal cal: `match_closest_target(class-filtered candidates)`."
/// The matched name must be one the class permits, and the copy names it.
#[test]
fn a_coupler_match_stays_inside_the_class_filtered_set() {
    for class in [TransducerClass::InEar, TransducerClass::OverEar] {
        let bundle = well_formed_bundle(class);
        let target = decide(&bundle).decisions.target;
        let TargetChoice::Curve { ref name } = target.value else {
            panic!(
                "{class:?}: a coupler matches a curve, got {:?}",
                target.value
            );
        };
        let legal = bundle
            .targets
            .iter()
            .find(|t| &t.name == name)
            .unwrap_or_else(|| panic!("{class:?}: matched a curve the bundle does not carry"));
        assert!(
            legal.classes.contains(&class),
            "{class:?}: matched {name}, which is not legal for it"
        );
        assert_eq!(target.source, Source::Auto);
        assert!(
            target.rationale.text.contains(name.as_str()),
            "{class:?}: the copy names the curve it matched, got {:?}",
            target.rationale.text
        );
    }
}

// ===========================================================================
// The fit — `autofit::auto_fit_room` in `decide()`'s own slot
// ===========================================================================

/// A capture with a real, correctable problem: a broad modal peak in the bass
/// and a broad lift up top, big enough that the greedy loop cannot call it flat.
fn lumpy_bundle(class: TransducerClass) -> MeasurementBundle {
    shaped_bundle(
        SyntheticSpec {
            cal: true,
            channels: 2,
            class,
            positions: profile_for(class).positions_default,
            sample_rate: RATE as u32,
            seed: 0x5EED_0007,
        },
        |index| {
            // A little position-to-position scatter so σ(f) is a measurement
            // rather than identically zero, which would make every confidence
            // weight 1 and hide the σ gate entirely.
            let jitter = 0.4 * index as f64;
            vec![
                biquad::peaking(52.0, 11.0 + jitter, 3.0, RATE),
                biquad::peaking(3000.0, 9.0, 1.0, RATE),
            ]
        },
    )
}

/// "Room auto mode ships **cut-only by default** … cuts are free, boosts cost
/// headroom and can damage drivers" (§ D-E, `boost_ratio` 0.0 on the room path
/// and `DEFAULT_BOOST_RATIO` on the coupler).
#[test]
fn the_room_auto_path_ships_cut_only() {
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let curve = &set.analysis.authority;
        match profile_for(class).coupling {
            CouplingPath::Room => {
                assert!(
                    curve.max_boost_db().iter().all(|b| *b == 0.0),
                    "{class:?}: the room ceiling admits no boost anywhere"
                );
                // Non-vacuity: "every band is a cut" is trivially true of no
                // bands, and B7a shipped no bands. A capture with an 11 dB modal
                // peak at 52 Hz must produce filters or this whole module is
                // testing nothing.
                assert!(
                    !bands_of(&lumpy_bundle(class)).is_empty(),
                    "{class:?}: an 11 dB modal peak must produce at least one filter"
                );
                for band in set
                    .correction
                    .as_ref()
                    .expect("a lumpy capture still proceeds")
                    .bands
                    .iter()
                    .flatten()
                {
                    assert!(
                        band.gain_db <= 0.0,
                        "{class:?}: the room auto path emitted a boost: {band:?}"
                    );
                }
            }
            CouplingPath::Coupler => {
                assert!(
                    curve.max_boost_db().iter().any(|b| *b > 0.0),
                    "{class:?}: the coupler ceiling admits boost where confidence allows"
                );
            }
        }
    }
}

/// Every emitted band sits inside the authority envelope at its own `f₀`.
/// `clamp_band` is what enforces it; this is the test that says `decide()`
/// actually routes through it rather than emitting `auto_fit_parametric_eq`'s
/// unclamped bands.
#[test]
fn every_band_is_inside_the_authority_envelope_at_its_f0() {
    let mut seen = 0usize;
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let curve = &set.analysis.authority;
        seen += set
            .correction
            .as_ref()
            .map(|c| c.bands.iter().flatten().count())
            .unwrap_or(0);
        for band in set
            .correction
            .as_ref()
            .expect("proceeds")
            .bands
            .iter()
            .flatten()
        {
            let at = curve.at(band.fc);
            assert!(
                band.gain_db <= at.max_boost_db + 1e-6,
                "{class:?}: {band:?} exceeds the boost ceiling {}",
                at.max_boost_db
            );
            assert!(
                band.gain_db >= -at.max_cut_db - 1e-6,
                "{class:?}: {band:?} exceeds the cut ceiling {}",
                at.max_cut_db
            );
        }
    }
    assert!(
        seen > 0,
        "non-vacuity: no bands were emitted on ANY path, so nothing was checked"
    );
}

/// `q_cap`: "Boosts only: `Q_max = 0.227·f₀/A`, `A = 10^(G/40)`. **And** the
/// path ceiling: 5.0 coupler; room `10.0 @ 200 Hz → 3.0 @ 10 kHz` log-linear.
/// Take the min."
#[test]
fn every_boost_band_obeys_the_rew_q_cap_and_the_path_ceiling() {
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let q_cap = set.decisions.q_cap.value;
        for band in set
            .correction
            .as_ref()
            .expect("proceeds")
            .bands
            .iter()
            .flatten()
            .filter(|b| b.gain_db > 0.0)
        {
            let ceiling = max_q_for_boost_capped(band.fc, band.gain_db, &q_cap);
            assert!(
                band.q <= ceiling + 1e-6,
                "{class:?}: {band:?} has Q above its cap {ceiling}"
            );
        }
    }
}

/// "The `Q_max` cap doubles as the runtime stability guard, and `decide()` must
/// never emit a band that fails Jury." With the cap applied this must NEVER
/// fire; a failure here is a bug report, not a tuning outcome.
#[test]
fn every_band_passes_jury_at_the_design_rate() {
    let mut seen = 0usize;
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let plan = set.correction.as_ref().expect("proceeds");
        for band in plan.bands.iter().flatten() {
            seen += 1;
            assert!(
                biquad::is_stable(&band.to_sos(plan.design_rate)),
                "{class:?}: {band:?} fails Jury at {} Hz",
                plan.design_rate
            );
        }
        assert_eq!(
            plan.dropped.iter().sum::<usize>(),
            0,
            "{class:?}: the Jury retry funnel dropped a band — with the Q cap \
             applied that is a bug report, not a tuning outcome"
        );
    }
    assert!(seen > 0, "non-vacuity: no bands were emitted on ANY path");
}

/// `preamp_db`: "`-max(0, max_f∈[20,20000] of the REALIZED cascade response)`;
/// no headroom constant."
///
/// Realized, not requested: the number is taken from the cascade that actually
/// runs, after the Q cap, the excursion clamp and band-dropping. A cut-only
/// plan therefore yields exactly 0.0 — not "about zero", and not AutoEQ's
/// 0.2 dB GraphicEQ headroom, which never applies to a PEQ cascade.
#[test]
fn preamp_is_minus_the_realized_cascade_peak_with_no_headroom() {
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let plan = set.correction.as_ref().expect("proceeds");
        let expected = plan
            .bands
            .iter()
            .map(|channel| {
                ParametricEQ {
                    bands: channel.clone(),
                    sample_rate: plan.design_rate,
                }
                .preamp_db()
            })
            .fold(0.0f64, f64::min);
        assert_eq!(
            set.decisions.preamp_db.value, expected,
            "{class:?}: the preamp is the worst channel's realized peak"
        );
        assert_eq!(plan.preamp_db, set.decisions.preamp_db.value);

        if plan.bands.iter().flatten().all(|b| b.gain_db <= 0.0) {
            assert_eq!(
                set.decisions.preamp_db.value, 0.0,
                "{class:?}: a cut-only cascade needs exactly no headroom"
            );
        }
    }
}

/// One channel's bands. `PerChannel` is not indexable — the container exists so
/// a channel index cannot be confused with a band index — so a test that is
/// about ONE channel says which through `get`.
fn channel_bands(plan: &paraeq_decide::CorrectionPlan, channel: usize) -> &[EQBand] {
    plan.bands
        .get(channel)
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("the plan carries no channel {channel}"))
}

/// A bundle whose CHANNEL `c` has the magnitude response of `per_channel[c]`,
/// the same at every position.
///
/// `common::shaped_bundle` and `common::shaped_positions` both vary the shape
/// per POSITION and copy one response across the channels, which is the right
/// axis for σ(f) and the wrong one for anything folded over channels: with
/// identical channels a fold cannot be told apart from its own mirror image.
/// The cal is flattened for the same reason `shaped_bundle` flattens it —
/// compensation subtracts the cal, so a vendor curve would put a second shape
/// on the analysed curve that this test did not ask for.
fn per_channel_bundle(class: TransducerClass, per_channel: &[Vec<[f64; 6]>]) -> MeasurementBundle {
    let mut bundle = synthetic_bundle(SyntheticSpec {
        cal: true,
        channels: per_channel.len(),
        class,
        positions: profile_for(class).positions_default,
        sample_rate: RATE as u32,
        seed: 0x5EED_0011,
    });
    bundle.cal = Some(cal_with_curve(vec![20.0, 20_000.0], vec![0.0, 0.0]));
    for position in &mut bundle.positions {
        let peak = position.ir.peak;
        let len = position.ir.samples[0].len();
        for (channel, sections) in position.ir.samples.iter_mut().zip(per_channel) {
            *channel = sos_impulse(sections, peak, len);
        }
    }
    bundle
}

/// The worst channel wins, with the two channels made to disagree.
///
/// `preamp_is_minus_the_realized_cascade_peak_with_no_headroom` re-runs
/// `fold(0.0, f64::min)` and compares, so it cannot tell `min` from `max`: on
/// every fixture it grades, both channels carry the same peak and both folds
/// give the same answer. This one builds the disagreement — a dip on the left
/// ear, a bump on the right — and compares against a number written out from
/// the authority envelope rather than from the fold.
///
/// **What the fold protects.** The preamp is the headroom the corrected path
/// gives back before the cascade runs. Taking the QUIETER channel's requirement
/// would leave the boosted channel 5 dB into the ceiling, so the ear that
/// needed the boost is the ear that clips — and a listener hears it as
/// distortion on one side only, which reads as a broken headphone rather than a
/// broken correction.
#[test]
fn the_preamp_is_the_worst_channel_not_the_quietest() {
    // A 120 Hz dip on channel 0 and a 120 Hz bump on channel 1, both 8 dB, on
    // the coupler path. The correction inverts each: channel 0 asks for a
    // boost, channel 1 for a cut.
    let dip = vec![biquad::peaking(120.0, -8.0, 1.5, RATE)];
    let bump = vec![biquad::peaking(120.0, 8.0, 1.5, RATE)];
    let set = decide(&per_channel_bundle(
        TransducerClass::OverEar,
        &[dip.clone(), bump.clone()],
    ));
    let plan = set.correction.as_ref().expect("a clean coupler cohort");

    // The case is only a case if the two channels really do differ in sign.
    assert!(
        channel_bands(plan, 0).iter().any(|b| b.gain_db > 0.0),
        "precondition: channel 0 must ask for a boost, got {:?}",
        channel_bands(plan, 0)
    );
    assert!(
        channel_bands(plan, 1).iter().all(|b| b.gain_db <= 0.0),
        "precondition: channel 1 must be cut-only, got {:?}",
        channel_bands(plan, 1)
    );

    // Channel 1's own requirement, pinned by a bundle that carries nothing
    // else: a cut-only cascade never rises above 0 dB, so it needs exactly no
    // headroom. This is the number the `max` fold would have answered with.
    let cut_only = decide(&per_channel_bundle(
        TransducerClass::OverEar,
        &[bump.clone(), bump],
    ));
    assert_eq!(
        cut_only.decisions.preamp_db.value,
        0.0,
        "the quiet side's own preamp: {:?}",
        cut_only.correction.as_ref().map(|p| p.bands.clone())
    );

    // Channel 0's, written out rather than re-folded. `COUPLER_EXCURSION_DB` is
    // 10 dB at and below 150 Hz and `DEFAULT_BOOST_RATIO` halves it for boosts,
    // so +5.0 dB is the most the envelope licenses at 120 Hz — the fit asked
    // for +8 and was clamped there. A cascade whose peak is +5.0 dB needs
    // 5.0 dB of headroom.
    const LICENSED_BOOST_DB: f64 = 5.0;
    assert!(
        (set.decisions.preamp_db.value + LICENSED_BOOST_DB).abs() < 0.01,
        "the preamp must be the BOOSTED channel's −{LICENSED_BOOST_DB:.1} dB, \
         not the cut-only channel's 0.0; it is {} with bands {:?}",
        set.decisions.preamp_db.value,
        plan.bands
    );
}

/// A zero preamp renders as "0.0 dB", never "-0.0 dB".
///
/// Ruling R-A10. `preamp_db` is `≤ 0` and the copy renders its magnitude as
/// `-preamp_db`; IEEE negation turns `0.0` into `-0.0`, which `{:.1}` writes
/// with a minus sign. "We turned everything down -0.0 dB" is user-facing copy
/// with a sign on a number that has none.
#[test]
fn a_zero_preamp_renders_without_a_negative_zero() {
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let text = &set.decisions.preamp_db.rationale.text;
        assert!(
            !text.contains("-0.0") && !text.contains("\u{2212}0.0"),
            "{class:?}: negative zero in {text:?}"
        );
    }
    // And the case the bug is actually about: a cut-only plan, where the
    // preamp IS exactly zero.
    let cut_only = decide(&minimal_bundle(TransducerClass::Bookshelf));
    assert_eq!(cut_only.decisions.preamp_db.value, 0.0);
    assert!(
        cut_only
            .decisions
            .preamp_db
            .rationale
            .text
            .contains("down 0.0 dB"),
        "{:?}",
        cut_only.decisions.preamp_db.rationale.text
    );
}

/// `max_filters`: "Drop any band with `|gain| < flatness/2`", and the binding
/// `min_gain_db = flatness_target_db / 2` is `decide()`'s — the number has ONE
/// source, the `Decision`, not a constant.
///
/// The observable is `auto_fit_room`'s admissibility precompute: a bin whose
/// ceiling cannot reach `min_gain_db` in either direction can never carry a
/// band. On the room path the Trinnov envelope is ±2 dB above 500 Hz, so
/// `min_gain_db = 1.5` (flatness 3.0) leaves that region admissible while
/// `min_gain_db = 3.0` (flatness overridden to 6.0) strikes all of it. If the
/// binding were a constant rather than the decision, the override would change
/// nothing.
#[test]
fn max_filters_binds_min_gain_db_to_half_the_flatness_target() {
    let bundle = lumpy_bundle(TransducerClass::Bookshelf);
    let base = decide(&bundle);
    assert_eq!(base.decisions.flatness_target_db.value, 3.0);
    assert!(
        bands_of(&bundle).iter().any(|b| b.fc > 500.0),
        "the test's own premise: at min_gain 1.5 the ±2 dB region above 500 Hz \
         is admissible and the capture has a 3 kHz lump to correct"
    );

    let raised = with_override(&bundle, "flatness_target_db", serde_json::json!(6.0));
    assert_eq!(decide(&raised).decisions.flatness_target_db.value, 6.0);
    assert!(
        !bands_of(&raised).iter().any(|b| b.fc > 500.0),
        "at min_gain 3.0 no bin above 500 Hz can reach the drop floor, so no \
         band may be placed there"
    );

    let lowered = with_override(&bundle, "flatness_target_db", serde_json::json!(0.5));
    for clamp in decide(&lowered)
        .correction
        .as_ref()
        .expect("proceeds")
        .clamps
        .iter()
        .flatten()
    {
        if let paraeq_dsp::authority::Clamp::BelowMinGain { gain_db, .. } = clamp {
            assert!(
                gain_db.abs() < 0.25 + 1e-9,
                "a feature reported as below the drop floor must be below \
                 flatness/2 = 0.25, got {gain_db}"
            );
        }
    }
}

/// § D-F: the narrow-dip veto is "**nothing to decide** — 1/6 octave already
/// shipped at `authority.rs`; `decide()` just uses
/// `AuthorityCurve::min_dip_width_oct()`".
///
/// Recorded as a test rather than as a comment so that nobody re-opens it as
/// unbuilt, and so a `decide()` that started re-specifying the width fails.
#[test]
fn the_narrow_dip_veto_is_the_shipped_one_sixth_octave() {
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        assert_eq!(
            set.analysis.authority.min_dip_width_oct(),
            DEFAULT_MIN_DIP_WIDTH_OCT,
            "{class:?}"
        );
        assert!((DEFAULT_MIN_DIP_WIDTH_OCT - 1.0 / 6.0).abs() < 1e-12);
    }
}

// ===========================================================================
// The EGD trace — evidence, never authority, in v1
// ===========================================================================

/// **R7: the excess-group-delay gate is v1.1; in v1 the trace is `Evidence` and
/// an input to no decision.**
///
/// The plan's own shape for this test — swap `Analysis.excess_group_delay_s`
/// for zeros and then for a wild ripple and diff the decisions — needs an
/// injection point `decide()` does not expose: the trace is computed from the
/// impulse response inside the pure function, and no all-pass exists in
/// `paraeq_dsp::biquad` to perturb phase while holding magnitude. So the same
/// claim is pinned from the other side, and more strongly: the resolved
/// authority curve is re-derived here from σ(f) and the path policy ALONE, with
/// `None` for the EGD mask, and must equal the one `decide()` published. A
/// curve that is a function of σ and the policy only cannot be a function of the
/// trace — which is what arming the gate would change, and what this test would
/// then catch.
#[test]
fn the_egd_trace_is_evidence_and_never_changes_authority_in_v1() {
    let grid = LogGrid::standard();
    for class in EVERY_CLASS {
        let set = decide(&lumpy_bundle(class));
        let policy = authority_policy_for(class);
        assert_eq!(
            policy.egd_gate,
            paraeq_dsp::authority::EgdGate::Off,
            "{class:?}: the gate ships OFF in v1"
        );
        let from_sigma_alone = build_authority_gated(&grid, &set.analysis.sigma_db, None, &policy)
            .expect("σ from a real analysis is valid authority input");
        assert_eq!(
            set.analysis.authority, from_sigma_alone,
            "{class:?}: the authority curve is a function of σ and the policy, \
             and of nothing else"
        );

        // …and the trace is published, on the decision it would gate.
        let published = set
            .decisions
            .authority
            .evidence
            .iter()
            .find_map(|e| match e {
                Evidence::Curve {
                    db,
                    label: EvidenceLabel::ExcessGroupDelay,
                    ..
                } => Some(db.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{class:?}: the EGD trace is attached as evidence"));
        let expected: Vec<f32> = set
            .analysis
            .excess_group_delay_s
            .iter()
            .map(|v| *v as f32)
            .collect();
        assert_eq!(published, expected, "{class:?}: verbatim, not re-derived");
    }
}

// ===========================================================================
// Cascade and pinning — the Override Semantics invariants
// ===========================================================================

/// The ids of every decision whose VALUE differs between two runs.
fn moved(before: &Decisions, after: &Decisions) -> Vec<&'static str> {
    before
        .iter()
        .zip(after.iter())
        .filter(|(a, b)| a.value != b.value)
        .map(|(a, _)| a.id)
        .collect()
}

/// A capture with something for every link of the spec's own cascade chain to
/// bite on: a roll-off below 60 Hz (so `low_corner_hz` has a real answer that
/// can move), and a position-to-position disagreement at 140 Hz (so σ(f) has a
/// real crossing that can move).
///
/// The scatter sits BELOW the Align-SPL band on purpose. Align SPL levels every
/// position over 200 Hz–2 kHz before averaging, so a disagreement inside that
/// band is largely absorbed by the alignment itself and σ(f) barely sees it —
/// which is the band's whole job, and which would make a cascade test about
/// σ(f) quietly vacuous.
fn cascading_bundle() -> MeasurementBundle {
    shaped_bundle(
        SyntheticSpec {
            cal: true,
            channels: 1,
            class: TransducerClass::Bookshelf,
            positions: 5,
            sample_rate: RATE as u32,
            seed: 0x5EED_0008,
        },
        |index| {
            vec![
                biquad::low_shelf(40.0, -20.0, 0.707, RATE),
                biquad::peaking(140.0, -6.0 + 3.0 * index as f64, 1.0, RATE),
            ]
        },
    )
}

/// Coarse smoothing: 1/3 octave, against the room path's `Variable` (~1/23
/// octave at 140 Hz). Far enough from the default to move the curves every
/// downstream rule reads, which is what a cascade test needs to observe.
fn coarse_smoothing() -> serde_json::Value {
    serde_json::json!({ "Fixed": 3 })
}

/// "An override to a `Reanalyze`-tier decision re-runs `decide()` in full, so
/// downstream `Auto` decisions move. Overriding `smoothing` changes σ(f), which
/// moves `transition_hz`, which moves `authority`, which changes `max_filters`
/// and `preamp_db`. … Guided mode must show the cascade ('changing this also
/// changed 4 other things')."
///
/// The cascade is observable exactly as the drawer will observe it: two
/// `DecisionSet`s, diffed. There is no cascade API because `decide()` is a total
/// recomputation — the tier is advice to the caller about which cached
/// artifacts survive, not a control-flow branch inside the function.
#[test]
fn an_override_cascades_to_the_downstream_auto_decisions() {
    let bundle = cascading_bundle();
    let auto = decide(&bundle);
    let coarse = decide(&with_override(&bundle, "smoothing", coarse_smoothing()));

    let changed = moved(&auto.decisions, &coarse.decisions);
    assert!(
        changed.contains(&"smoothing"),
        "the override itself arrived: {changed:?}"
    );
    let downstream: Vec<&str> = changed
        .iter()
        .copied()
        .filter(|id| *id != "smoothing")
        .collect();
    assert!(
        !downstream.is_empty(),
        "changing the smoothing must change something downstream of σ(f); \
         nothing moved but {changed:?}"
    );
    // "Guided mode must show the cascade ('changing this also changed 4 other
    // things')" — the count is what the caller renders, and it is a diff of two
    // `DecisionSet`s because `decide()` is a total recomputation rather than an
    // incremental one. There is no cascade API to call.
    assert!(
        downstream.contains(&"transition_hz"),
        "the spec's own chain starts here: smoothing changes σ(f), which moves \
         transition_hz. Moved: {downstream:?}"
    );
    assert_eq!(
        coarse.decisions.smoothing.invalidates,
        paraeq_decide::Invalidation::Reanalyze,
        "and the tier the drawer reads off the decision says so"
    );
    for id in &downstream {
        let tier = coarse
            .decisions
            .iter()
            .find(|v| v.id == *id)
            .expect("the id came from this very iterator")
            .invalidates;
        assert_ne!(
            tier,
            paraeq_decide::Invalidation::Recapture,
            "{id} moved on a Reanalyze override, so it cannot be a Recapture-\
             tier decision — the tiers would be lying about what a re-analysis \
             costs"
        );
    }
}

/// "Downstream decisions with `source: UserOverride` are **pinned** and do not
/// move; everything else recomputes."
///
/// The complement of the cascade, and the half that makes the drawer usable: a
/// user who has pinned the transition frequency must not have it silently
/// re-derived out from under them when they touch something upstream.
#[test]
fn a_pinned_downstream_decision_does_not_move_when_its_upstream_does() {
    let bundle = cascading_bundle();
    let auto = decide(&bundle);
    assert_eq!(
        auto.decisions.transition_hz.source,
        Source::Auto,
        "the test's own premise: auto measured a crossing here"
    );
    assert_ne!(
        decide(&with_override(&bundle, "smoothing", coarse_smoothing()))
            .decisions
            .transition_hz
            .value,
        auto.decisions.transition_hz.value,
        "…and the test's second premise: unpinned, this decision DOES move"
    );

    let mut overrides = serde_json::to_value(paraeq_decide::Overrides::default())
        .expect("Overrides is plain derived data");
    overrides["smoothing"] = coarse_smoothing();
    overrides["transition_hz"] = serde_json::json!(137.0);
    let both = MeasurementBundle {
        overrides: serde_json::from_value(overrides).expect("legal override values"),
        ..bundle.clone()
    };
    let pinned = decide(&both);

    assert_eq!(pinned.decisions.transition_hz.value, 137.0);
    assert_eq!(pinned.decisions.transition_hz.source, Source::UserOverride);
    assert!(
        moved(&auto.decisions, &pinned.decisions)
            .iter()
            .any(|id| *id != "smoothing" && *id != "transition_hz"),
        "the rest of the cascade still ran"
    );
}

// ===========================================================================
// The copy
// ===========================================================================

/// Every one of the twenty-two decisions carries the `RationaleKey` named for
/// it, renders non-empty, and contains no unsubstituted `{`.
///
/// `target` may carry any of its three keys because its copy differs materially
/// by which selection rule fired, which is why three exist.
#[test]
fn every_rationale_key_matches_its_field_and_renders_non_empty() {
    for class in EVERY_CLASS {
        for view in decide(&lumpy_bundle(class)).decisions.iter() {
            let key = view.rationale.key;
            let text = &view.rationale.text;
            assert!(
                !text.is_empty(),
                "{class:?}/{}: an empty rationale is an unwritten one",
                view.id
            );
            assert!(
                !text.contains('{'),
                "{class:?}/{}: unsubstituted placeholder in {text:?}",
                view.id
            );
            let expected: &[RationaleKey] = match view.id {
                "align_spl_band" => &[RationaleKey::AlignSplBand],
                "authority" => &[RationaleKey::Authority],
                "averaging" => &[RationaleKey::Averaging],
                "class" => &[RationaleKey::Class],
                "clock_adjust" => &[RationaleKey::ClockAdjust],
                "correction_kind" => &[RationaleKey::CorrectionKind],
                "correction_range" => &[RationaleKey::CorrectionRange],
                "fdw_post_cycles" => &[RationaleKey::FdwPostCycles],
                "fdw_pre_cycles" => &[RationaleKey::FdwPreCycles],
                "flatness_target_db" => &[RationaleKey::FlatnessTargetDb],
                "left_window_ms" => &[RationaleKey::LeftWindowMs],
                "low_corner_hz" => &[RationaleKey::LowCornerHz],
                "max_filters" => &[RationaleKey::MaxFilters],
                "positions_n" => &[RationaleKey::PositionsN],
                "preamp_db" => &[RationaleKey::PreampDb],
                "q_cap" => &[RationaleKey::QCap],
                "right_window_ms" => &[RationaleKey::RightWindowMs],
                "shelves" => &[RationaleKey::Shelves],
                "smoothing" => &[RationaleKey::Smoothing],
                "target" => &[
                    RationaleKey::TargetCalBakedIn,
                    RationaleKey::TargetFallback,
                    RationaleKey::TargetMatched,
                    RationaleKey::TargetRoomParametric,
                ],
                "transition_hz" => &[RationaleKey::TransitionHz],
                "window_type" => &[RationaleKey::WindowType],
                other => panic!("a twenty-third decision, {other}, with no rationale key"),
            };
            assert!(
                expected.contains(&key),
                "{class:?}/{}: carries {key:?}, expected one of {expected:?}",
                view.id
            );
        }
    }
}

/// The copy IS the decision table's, character for character.
///
/// Four uninterpolated rows and two interpolated ones, quoted here from the
/// spec so that a copy edit in `rationale.rs` that drifts from the table fails
/// rather than shipping. The remaining rows are covered structurally above; the
/// full row-by-row review reads `rationale.rs`, which is why the copy lives in
/// one file.
#[test]
fn the_rationale_copy_is_the_decision_tables_own_wording() {
    let set = decide(&lumpy_bundle(TransducerClass::Bookshelf));
    let d = &set.decisions;
    assert_eq!(
        d.window_type.rationale.text,
        "A soft-edged window. A hard cut smears the measurement across frequency."
    );
    assert_eq!(
        d.averaging.rationale.text,
        "One mic position sitting in a cancellation would otherwise drag the \
         average down and make us boost a hole that isn't really there."
    );
    assert_eq!(
        d.q_cap.rationale.text,
        "A boost filter is a resonance. We won't build one that rings longer \
         than the room problem it's fixing."
    );
    assert_eq!(
        d.shelves.rationale.text,
        "This is a broad tilt, not a bump — a shelf fixes it with one filter \
         instead of six."
    );
    assert_eq!(
        d.flatness_target_db.rationale.text,
        "Chasing flatter than 3 dB in a room means fighting your chair, not \
         your speakers."
    );
    assert_eq!(
        d.class.rationale.text,
        "You told us these are Bookshelf speakers. The levels we measured are \
         consistent with that."
    );

    // `{n}` is the number of filters the fit EMITTED, not the cap: "not because
    // {n} is a nice number" is false of a cap, and 10 is exactly a nice number.
    let emitted = set
        .correction
        .as_ref()
        .expect("proceeds")
        .bands
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    assert_eq!(
        d.max_filters.rationale.text,
        format!(
            "We used {emitted} filters because that's what it took to get \
             within 3 dB — not because {emitted} is a nice number."
        )
    );
    assert_eq!(d.max_filters.value, 10, "…while the VALUE stays the cap");
}

// ===========================================================================
// Cross-crate — the numbers `decide()` must not re-specify
// ===========================================================================

/// § P9: `PathProfile.q_cap` is the SAME value as the authority policy's
/// `q_ceiling` for that path, compared field for field and bit for bit.
///
/// The sibling of `the_room_target_default_equals_room_target_spec_default`.
/// With the type move there is one `QCapPolicy`, so this pins the NUMBERS: a
/// ruling that changes the room ceiling's breakpoints in `paraeq-dsp` must not
/// leave `decide()` publishing the old ones.
#[test]
fn the_path_q_ceilings_equal_the_authority_policy_defaults() {
    fn assert_same(class: TransducerClass, decided: &QCapPolicy, owned: &QCapPolicy) {
        match (decided, owned) {
            (QCapPolicy::Ceiling(a), QCapPolicy::Ceiling(b)) => {
                assert_eq!(a.to_bits(), b.to_bits(), "{class:?}: ceiling");
            }
            (
                QCapPolicy::LogLinear { hi: ah, lo: al },
                QCapPolicy::LogLinear { hi: bh, lo: bl },
            ) => {
                assert_eq!(ah.0.to_bits(), bh.0.to_bits(), "{class:?}: hi.0");
                assert_eq!(ah.1.to_bits(), bh.1.to_bits(), "{class:?}: hi.1");
                assert_eq!(al.0.to_bits(), bl.0.to_bits(), "{class:?}: lo.0");
                assert_eq!(al.1.to_bits(), bl.1.to_bits(), "{class:?}: lo.1");
            }
            _ => panic!("{class:?}: {decided:?} is not the same VARIANT as {owned:?}"),
        }
    }

    for class in EVERY_CLASS {
        let profile = profile_for(class);
        let owned = match profile.coupling {
            CouplingPath::Coupler => AuthorityPolicy::coupler().q_ceiling,
            CouplingPath::Room => AuthorityPolicy::room().q_ceiling,
        };
        assert_same(class, &profile.q_cap, &owned);
        // …and the decision publishes the profile's, not a third copy.
        let decided = decide(&well_formed_bundle(class)).decisions.q_cap.value;
        assert_same(class, &decided, &owned);
    }
}

/// The `authority` decision carries the preset NAME, not the curve (§ D-D), and
/// the resolved curve is published in `Analysis` as a product.
#[test]
fn the_authority_decision_carries_a_preset_and_the_curve_is_a_product() {
    let set = decide(&minimal_bundle(TransducerClass::Bookshelf));
    assert_eq!(set.decisions.authority.value, AuthorityPreset::Standard);
    assert!(!set.analysis.authority.is_empty());
    assert_eq!(
        set.analysis.authority.len(),
        set.analysis.freqs_hz.len(),
        "the published curve is on the published grid"
    );
}

/// The candidate pool the class filter is applied to is the bundle's own, so a
/// bundle carrying no class-legal curve still produces a legal decision rather
/// than a wrong one.
#[test]
fn a_coupler_with_no_class_legal_candidate_falls_back_inside_its_own_domain() {
    let mut bundle = well_formed_bundle(TransducerClass::OverEar);
    bundle.targets = synthetic_targets()
        .into_iter()
        .filter(|t| !t.classes.contains(&TransducerClass::OverEar))
        .collect();
    let target = decide(&bundle).decisions.target;
    assert!(
        target.domain.contains(&target.value),
        "the value must stay inside its own domain even with nothing to match"
    );
    assert_eq!(
        target.source,
        Source::Default,
        "nothing was measured to pick it"
    );
}
