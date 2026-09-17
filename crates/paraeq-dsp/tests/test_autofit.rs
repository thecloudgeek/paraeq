//! Two entry points, two tiers.
//!
//! - [`greedy_fit_matches_oracle_bands`] is **Tier 1**: `auto_fit_parametric_eq`
//!   against the golden fixture, byte-identical to the Python oracle.
//! - Everything below it is **Tier 3** (analytic policy) for `auto_fit_room`'s
//!   B4 additions — the residual-RMS stop, shelf emission and the
//!   `min_gain_db` drop rule. No oracle exists and none is faked: what is
//!   pinned is the spec's own sentences, each asserted as the behaviour it
//!   buys (a band count against a curve with a known residual; the closed-form
//!   half-gain width for the shelf).
//!
//! Spec: docs/specs/2026-07-15-decision-engine-design.md § Decision table,
//! rows `max_filters`, `flatness_target_db` and `shelves`.

mod common;

use common::Case;
use paraeq_dsp::authority::{
    authority_band_mask, build_authority, width_oct_for_q, AuthorityCurve, AuthorityPolicy, Clamp,
    DEFAULT_MIN_DIP_WIDTH_OCT,
};
use paraeq_dsp::autofit::{
    auto_fit_parametric_eq, auto_fit_room, RoomFitPolicy, RoomFitReport, SHELF_Q,
};
use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};
use paraeq_dsp::PerChannel;
use std::f64::consts::TAU;

const SR: f64 = 48_000.0;

#[test]
fn greedy_fit_matches_oracle_bands() {
    let c = Case::load("autofit", "two_peaks");
    let fitted = auto_fit_parametric_eq(
        &c.array("correction_db"),
        &c.array("freqs"),
        c.param_f64("sample_rate"),
        c.param_u64("max_bands") as usize,
        0.5,
    );
    let expected = c.scalar("fitted").as_array().unwrap();
    assert_eq!(fitted.len(), expected.len(), "band count");
    for (b, e) in fitted.iter().zip(expected) {
        assert_eq!(b.filter_type.as_str(), e["filter_type"].as_str().unwrap());
        let (fc, gain, q) = (
            e["fc"].as_f64().unwrap(),
            e["gain_db"].as_f64().unwrap(),
            e["q"].as_f64().unwrap(),
        );
        assert!(
            (b.fc - fc).abs() <= 1e-9 * fc.abs().max(1.0),
            "fc {} vs {fc}",
            b.fc
        );
        assert!(
            (b.gain_db - gain).abs() <= 1e-9 * gain.abs().max(1.0),
            "gain {} vs {gain}",
            b.gain_db
        );
        assert!(
            (b.q - q).abs() <= 1e-9 * q.abs().max(1.0),
            "q {} vs {q}",
            b.q
        );
    }
}

// ───────────────────────────────── helpers ───────────────────────────────────

/// A flat-σ authority curve on the standard grid.
fn curve_at_sigma(sigma: f64) -> AuthorityCurve {
    let grid = LogGrid::standard();
    let sigma_db = vec![sigma; grid.len()];
    build_authority(&grid, &sigma_db, &AuthorityPolicy::default()).expect("valid inputs")
}

/// The grid bin nearest `f`, so a synthetic feature sits exactly on a sample
/// point and the test is not measuring interpolation error.
fn nearest_bin_hz(grid: &LogGrid, f: f64) -> f64 {
    *grid
        .freqs()
        .iter()
        .min_by(|a, b| (*a - f).abs().total_cmp(&(*b - f).abs()))
        .expect("non-empty grid")
}

/// A Gaussian bump in log-frequency of amplitude `amp_db` centred on
/// `centre_hz`, with `width_oct` measured at half amplitude — the same shape
/// `tests/test_authority.rs` uses, so the width under test is exact by
/// construction rather than a property of a biquad.
fn gaussian(grid: &LogGrid, centre_hz: f64, amp_db: f64, width_oct: f64) -> Vec<f64> {
    let s = width_oct / (2.0 * (2.0 * std::f64::consts::LN_2).sqrt());
    grid.freqs()
        .iter()
        .map(|&f| {
            let x = (f / centre_hz).log2() / s;
            amp_db * (-0.5 * x * x).exp()
        })
        .collect()
}

fn fit(
    correction: &[f64],
    curve: &AuthorityCurve,
    max_bands: usize,
    min_gain_db: f64,
    policy: Option<&RoomFitPolicy>,
) -> RoomFitReport {
    let grid = LogGrid::standard();
    let per = PerChannel::new(vec![correction.to_vec()]).expect("one channel");
    let out = auto_fit_room(&per, &grid, SR, curve, max_bands, min_gain_db, 0.0, policy)
        .expect("valid fit inputs");
    out.get(0).expect("one channel").clone()
}

/// What is left over the authority band after `bands` run, in dB RMS —
/// re-derived here, in the test, from the realized cascade rather than from
/// anything the fit reported about itself.
fn residual_rms_over_band(
    correction: &[f64],
    bands: &[EQBand],
    curve: &AuthorityCurve,
    correction_range: (f64, f64),
) -> f64 {
    let grid = LogGrid::standard();
    let realized = ParametricEQ {
        bands: bands.to_vec(),
        sample_rate: SR,
    }
    .frequency_response(grid.freqs());
    let mask = authority_band_mask(grid.freqs(), curve, correction_range);
    let mut sum_sq = 0.0;
    let mut n = 0usize;
    for ((c, r), in_band) in correction.iter().zip(&realized).zip(&mask) {
        if *in_band {
            let left = c - r;
            sum_sq += left * left;
            n += 1;
        }
    }
    assert!(n > 0, "the authority band must not be empty in this test");
    (sum_sq / n as f64).sqrt()
}

// ──────────────────────── the residual-RMS stop (B4.1) ───────────────────────

#[test]
fn max_filters_stops_at_the_flatness_target_not_at_the_cap() {
    // decision-engine-design.md § Decision table, `max_filters`, verbatim:
    // "Greedy worst-first; stop when residual RMS over the authority band
    // `< flatness_target_db` or the cap is hit."  Both halves are asserted:
    // the target ends this fit, the cap ends the next one.
    const CAP: usize = 10;
    let grid = LogGrid::standard();
    let curve = curve_at_sigma(0.5);
    let range = (40.0, 250.0);
    let policy = RoomFitPolicy {
        correction_range: range,
        flatness_target_db: 3.0,
        shelves: false,
    };

    // Three measured peaks worth removing, plus a ±1.5 dB ripple across the
    // whole grid. Every ripple lobe clears `min_gain_db`, so a fit with no
    // target keeps chasing them to the cap; a fit with a 3 dB target must stop
    // once the peaks are gone, because 1.5 dB of ripple is 1.06 dB RMS and
    // already inside the target.
    // The amplitudes are well inside the ±10 dB excursion envelope on purpose:
    // a curve deeper than the ceiling stops being a test of the STOP and
    // becomes a test of the cascade gate, which `test_authority.rs` owns.
    let mut correction = vec![0.0; grid.len()];
    for centre in [50.0, 100.0, 200.0] {
        let peak = gaussian(&grid, nearest_bin_hz(&grid, centre), -5.0, 0.5);
        for (c, p) in correction.iter_mut().zip(&peak) {
            *c += p;
        }
    }
    for (c, &f) in correction.iter_mut().zip(grid.freqs()) {
        *c += 1.5 * (TAU * f.log2()).sin();
    }

    let stopped = fit(&correction, &curve, CAP, 1.0, Some(&policy));
    let unstopped = fit(&correction, &curve, CAP, 1.0, None);

    assert_eq!(
        unstopped.bands.len(),
        CAP,
        "without a target the cap is what binds — otherwise this test cannot \
         tell the stop from running out of candidates"
    );
    assert!(
        !stopped.bands.is_empty(),
        "the stop must not fire before any work is done"
    );
    assert!(
        stopped.bands.len() < CAP,
        "the target, not the cap, must end this fit: got {} bands",
        stopped.bands.len()
    );

    let left = residual_rms_over_band(&correction, &stopped.bands, &curve, range);
    assert!(
        left < policy.flatness_target_db,
        "and the target must actually be met: {left:.3} dB RMS over the band"
    );

    // One band earlier the target is NOT met — so the stop fired as soon as it
    // could, rather than late. The greedy loop is a prefix, so refitting with
    // one fewer band reproduces the same bands minus the last.
    let one_short = fit(&correction, &curve, stopped.bands.len() - 1, 1.0, None);
    let short_left = residual_rms_over_band(&correction, &one_short.bands, &curve, range);
    assert!(
        short_left >= policy.flatness_target_db,
        "the fit used more bands than the target needed: {short_left:.3} dB \
         RMS with {} bands",
        one_short.bands.len()
    );

    // The other half of the sentence — "or the cap is hit" — on the same
    // curve, with a budget too small to reach a coupler-tight 1 dB: the fit
    // must spend every filter it has and hand back a residual that still
    // misses the target, rather than stopping early or pretending it landed.
    const TIGHT_CAP: usize = 5;
    let tight = RoomFitPolicy {
        correction_range: range,
        flatness_target_db: 1.0,
        shelves: false,
    };
    let capped = fit(&correction, &curve, TIGHT_CAP, 1.0, Some(&tight));
    assert_eq!(
        capped.bands.len(),
        TIGHT_CAP,
        "five filters cannot reach 1 dB here, so the cap must be what binds"
    );
    let capped_left = residual_rms_over_band(&correction, &capped.bands, &curve, range);
    assert!(
        capped_left >= tight.flatness_target_db,
        "and it must stop there with the target unmet, got {capped_left:.3} dB"
    );
}

// ─────────────────────── the min_gain_db drop rule (B4.3) ────────────────────

#[test]
fn bands_below_min_gain_db_are_dropped_and_reported() {
    // decision-engine-design.md § Decision table, `max_filters`: "Drop any band
    // with |gain| < flatness/2". `min_gain_db` IS that flatness/2 — the binding
    // is `decide()`'s (tested in B7b); this owns the mechanism, and the point
    // of `Clamp::BelowMinGain` is that the drop stops being silent.
    let grid = LogGrid::standard();
    let curve = curve_at_sigma(0.5);
    let small_hz = nearest_bin_hz(&grid, 2000.0);
    let big = gaussian(&grid, nearest_bin_hz(&grid, 60.0), -6.0, 1.0);
    let small = gaussian(&grid, small_hz, -1.2, 1.0);
    let correction: Vec<f64> = big.iter().zip(&small).map(|(b, s)| b + s).collect();

    let min_gain_db = 1.5;
    let report = fit(&correction, &curve, 4, min_gain_db, None);

    assert!(
        report.bands.len() < 4,
        "the floor, not the cap, must end this fit: {:?}",
        report.bands
    );
    assert!(
        report.bands.iter().all(|b| b.gain_db.abs() >= min_gain_db),
        "no band may be emitted below the floor: {:?}",
        report.bands
    );
    assert!(
        report
            .bands
            .iter()
            .all(|b| (b.fc / small_hz).log2().abs() > 0.5),
        "the 1.2 dB feature must be left alone, not corrected: {:?}",
        report.bands
    );

    let dropped: Vec<&Clamp> = report
        .clamps
        .iter()
        .filter(|c| matches!(c, Clamp::BelowMinGain { .. }))
        .collect();
    assert_eq!(
        dropped.len(),
        1,
        "the drop must be reported exactly once, got {:?}",
        report.clamps
    );
    let Clamp::BelowMinGain { fc, gain_db } = dropped[0] else {
        unreachable!("filtered above")
    };
    assert!(
        gain_db.abs() < min_gain_db && gain_db.abs() > 0.0,
        "the report must carry what was found and left alone, got {gain_db} dB"
    );
    assert!(fc.is_finite() && *fc > 0.0, "and where it was: {fc} Hz");
    assert_eq!(
        report.dropped, 0,
        "a band below the floor is a tuning outcome, NOT a stability drop"
    );
}

// ────────────────────────── shelf emission (B4.2) ────────────────────────────

#[test]
fn a_broad_one_signed_end_excursion_becomes_a_shelf() {
    // decision-engine-design.md § Decision table, `shelves`, verbatim: "Emit a
    // shelf where a >=0.5-octave one-signed excursion exists at either end of
    // `correction_range`; shelf Q clamped `[0.4, 0.7]`."
    //
    // A Gaussian centred on the low edge of the range falls to half its
    // amplitude `width_oct / 2` octaves in — and half the gain is exactly
    // where a shelf's corner sits — so a 1.5-octave bump presents a 0.75-octave
    // one-signed excursion and a 0.5-octave bump presents 0.25.
    let grid = LogGrid::standard();
    let curve = curve_at_sigma(0.5);
    let edge_hz = grid.freqs()[0];
    let range = (edge_hz, 20_000.0);
    let policy = RoomFitPolicy {
        correction_range: range,
        flatness_target_db: 0.5,
        shelves: true,
    };

    let broad = gaussian(&grid, edge_hz, -6.0, 1.5);
    let report = fit(&broad, &curve, 4, 1.0, Some(&policy));
    let shelves: Vec<&EQBand> = report
        .bands
        .iter()
        .filter(|b| matches!(b.filter_type, FilterType::LowShelf | FilterType::HighShelf))
        .collect();
    assert_eq!(
        shelves.len(),
        1,
        "0.75 octaves of one-signed excursion is a shelf: {:?}",
        report.bands
    );
    assert_eq!(shelves[0].filter_type, FilterType::LowShelf);
    assert!(
        shelves[0].gain_db < 0.0,
        "a measured peak at the low end is a CUT shelf: {:?}",
        shelves[0]
    );
    assert!(
        SHELF_Q.contains(&shelves[0].q),
        "shelf Q must be clamped into {SHELF_Q:?}, got {}",
        shelves[0].q
    );
    assert!(
        (shelves[0].fc / (edge_hz * 2f64.powf(0.75))).log2().abs() < 0.05,
        "the corner is the half-gain point, {} Hz here, got {}",
        edge_hz * 2f64.powf(0.75),
        shelves[0].fc
    );

    let narrow = gaussian(&grid, edge_hz, -6.0, 0.5);
    let report = fit(&narrow, &curve, 4, 1.0, Some(&policy));
    assert!(
        !report.bands.is_empty(),
        "the feature must still be corrected, just not with a shelf"
    );
    assert!(
        report
            .bands
            .iter()
            .all(|b| b.filter_type == FilterType::Peaking),
        "0.25 octaves is a bump, not a tilt: {:?}",
        report.bands
    );
}

#[test]
fn shelves_are_not_emitted_when_the_decision_says_no() {
    // `shelves` is a `Choice: true, false` the owner can turn off; the same
    // curve that produced one above must produce none with the flag down —
    // and must still be corrected, with peaking filters.
    let grid = LogGrid::standard();
    let curve = curve_at_sigma(0.5);
    let edge_hz = grid.freqs()[0];
    let policy = RoomFitPolicy {
        correction_range: (edge_hz, 20_000.0),
        flatness_target_db: 0.5,
        shelves: false,
    };
    let broad = gaussian(&grid, edge_hz, -6.0, 1.5);
    let report = fit(&broad, &curve, 4, 1.0, Some(&policy));
    assert!(
        !report.bands.is_empty(),
        "turning shelves off must not turn the fit off"
    );
    assert!(
        report
            .bands
            .iter()
            .all(|b| b.filter_type == FilterType::Peaking),
        "{:?}",
        report.bands
    );
}

#[test]
fn auto_fit_room_refuses_a_malformed_fit_policy() {
    // A stop that cannot be graded must refuse, not run silently forever:
    // `authority_band_mask` returns an empty band for an inverted or
    // non-finite range, and an empty band can never say "flat enough".
    let grid = LogGrid::standard();
    let curve = curve_at_sigma(1.0);
    let flat = PerChannel::new(vec![vec![0.0; grid.len()]]).expect("one channel");
    let bad = [
        RoomFitPolicy {
            correction_range: (1000.0, 40.0),
            flatness_target_db: 3.0,
            shelves: true,
        },
        RoomFitPolicy {
            correction_range: (f64::NAN, 1000.0),
            flatness_target_db: 3.0,
            shelves: true,
        },
        RoomFitPolicy {
            correction_range: (0.0, 1000.0),
            flatness_target_db: 3.0,
            shelves: true,
        },
        RoomFitPolicy {
            correction_range: (40.0, 1000.0),
            flatness_target_db: 0.0,
            shelves: true,
        },
        RoomFitPolicy {
            correction_range: (40.0, 1000.0),
            flatness_target_db: f64::NAN,
            shelves: true,
        },
    ];
    for policy in bad {
        assert!(
            auto_fit_room(&flat, &grid, SR, &curve, 4, 1.0, 0.0, Some(&policy)).is_err(),
            "{policy:?} must be refused"
        );
    }
}

// ───────────────────────────── the narrow-dip veto ───────────────────────────

#[test]
fn a_narrow_dip_is_never_filled() {
    // The two widths under test, placed against the threshold in BOTH of the
    // spellings the specs use: room-dsp states it as a width (1/6 octave,
    // `DEFAULT_MIN_DIP_WIDTH_OCT`) and engine-hardening as a Q (>3 is narrow).
    // `width_oct_for_q` is the exact conversion between them — the equality is
    // pinned in `test_authority.rs`; what is pinned HERE is which rule the
    // shipped fit actually obeys.
    let narrow_oct = 1.0 / 12.0;
    let wide_oct = 1.0 / 3.0;
    let q_rule_oct = width_oct_for_q(3.0);
    assert!(
        narrow_oct < DEFAULT_MIN_DIP_WIDTH_OCT && wide_oct > DEFAULT_MIN_DIP_WIDTH_OCT,
        "the two cases must straddle the shipped threshold of \
         {DEFAULT_MIN_DIP_WIDTH_OCT:.4} octaves"
    );
    assert!(
        wide_oct < q_rule_oct,
        "the 1/3-octave case is the DISCRIMINATING one: Q>3 is anything under \
         {q_rule_oct:.4} octaves, so engine-hardening's spelling would veto it \
         while room-dsp's 1/6-octave spelling fills it. The shipped rule is \
         the width, so it is filled."
    );

    let grid = LogGrid::standard();
    let curve = curve_at_sigma(0.5);
    let f0 = nearest_bin_hz(&grid, 60.0);

    // Positive residual = a measured dip = something to FILL.
    let narrow = fit(&gaussian(&grid, f0, 8.0, narrow_oct), &curve, 4, 1.0, None);
    assert!(
        narrow.bands.is_empty(),
        "1/12 octave is destructive interference, not response: {:?}",
        narrow.bands
    );
    assert!(
        narrow
            .clamps
            .iter()
            .any(|c| matches!(c, Clamp::DipRefused { .. })),
        "and the refusal must say so: {:?}",
        narrow.clamps
    );

    let wide = fit(&gaussian(&grid, f0, 8.0, wide_oct), &curve, 4, 1.0, None);
    assert!(
        !wide.bands.is_empty(),
        "1/3 octave is response, and is filled"
    );
    assert!(
        wide.bands.iter().all(|b| b.gain_db > 0.0),
        "filling a dip is a boost: {:?}",
        wide.bands
    );
}
