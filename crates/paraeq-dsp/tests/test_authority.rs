//! Tier 3 (analytic physics/policy) for `authority.rs` and
//! `autofit::auto_fit_room`. No oracle exists and none is faked: what is
//! pinned is the closed form of every composition step, the worked values the
//! spec states, and the invariants the module exists to guarantee.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`authority.rs` — new" and
//! "`autofit.rs` changes (additive)".

use approx::assert_relative_eq;
use paraeq_dsp::authority::{
    build_authority, clamp_band, excursion_db, is_stable, max_q_for_boost, stabilize_band,
    width_oct_for_q, AuthorityCurve, AuthorityPolicy, Clamp, DEFAULT_BOOST_RATIO,
    DEFAULT_EXCURSION_DB, DEFAULT_MIN_DIP_WIDTH_OCT, SIGMA_FULL_DB, SIGMA_NONE_DB,
};
use paraeq_dsp::autofit::{auto_fit_parametric_eq, auto_fit_room, RoomFitReport};
use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::peq::{EQBand, FilterType};
use paraeq_dsp::PerChannel;
use proptest::prelude::*;

const SR: f64 = 48_000.0;

fn policy() -> AuthorityPolicy {
    AuthorityPolicy::default()
}

/// A flat-σ curve on the standard grid — the common fixture.
fn curve_at_sigma(sigma: f64) -> AuthorityCurve {
    let grid = LogGrid::standard();
    let sigma_db = vec![sigma; grid.len()];
    build_authority(&grid, &sigma_db, &policy()).expect("valid inputs")
}

/// The grid bin nearest `f`, so synthetic features sit exactly on a sample
/// point and the tests are not measuring interpolation error.
fn nearest_bin(grid: &LogGrid, f: f64) -> usize {
    grid.freqs()
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (*a - f).abs().total_cmp(&(*b - f).abs()))
        .map(|(i, _)| i)
        .expect("non-empty grid")
}

/// A Gaussian bump in log-frequency of amplitude `amp_db` centred on
/// `centre_hz`, with `width_oct` measured at half amplitude. Gaussian rather
/// than a peaking-filter shape so the width under test is exact by
/// construction rather than a property of a biquad.
fn gaussian_residual(grid: &LogGrid, centre_hz: f64, amp_db: f64, width_oct: f64) -> Vec<f64> {
    // half amplitude at |log2(f/f0)| = s·sqrt(2 ln 2), so full width = 2·that.
    let s = width_oct / (2.0 * (2.0 * std::f64::consts::LN_2).sqrt());
    grid.freqs()
        .iter()
        .map(|&f| {
            let x = (f / centre_hz).log2() / s;
            amp_db * (-0.5 * x * x).exp()
        })
        .collect()
}

fn fit(residual: &[f64], curve: &AuthorityCurve, max_bands: usize) -> RoomFitReport {
    let grid = LogGrid::standard();
    let per = PerChannel::new(vec![residual.to_vec()]).expect("one channel");
    let out = auto_fit_room(&per, &grid, SR, curve, max_bands, 1.0, 0.0).expect("valid fit inputs");
    out.get(0).expect("one channel").clone()
}

// ───────────────────────────── excursion envelope ────────────────────────────

#[test]
fn excursion_hits_the_trinnov_breakpoints_exactly() {
    let bp = &DEFAULT_EXCURSION_DB;
    assert_eq!(excursion_db(bp, 150.0), 10.0);
    assert_eq!(excursion_db(bp, 500.0), 2.0);
    assert_eq!(excursion_db(bp, 1000.0), 2.0);
    // At or below 150 Hz is the flat +-10 dB plateau; above 500 Hz is +-2 dB.
    assert_eq!(excursion_db(bp, 20.0), 10.0);
    assert_eq!(
        excursion_db(bp, 10.0),
        10.0,
        "clamped below the first point"
    );
    assert_eq!(
        excursion_db(bp, 40_000.0),
        2.0,
        "clamped above the last point"
    );
}

#[test]
fn excursion_taper_matches_the_specs_worked_value() {
    // 10 - 8·(log10(300) - log10(150)) / (log10(500) - log10(150)) = 5.394 dB
    assert_relative_eq!(
        excursion_db(&DEFAULT_EXCURSION_DB, 300.0),
        5.394,
        epsilon = 5e-4
    );
}

#[test]
fn excursion_is_monotone_non_increasing_over_the_grid() {
    let grid = LogGrid::standard();
    let e: Vec<f64> = grid
        .freqs()
        .iter()
        .map(|&f| excursion_db(&DEFAULT_EXCURSION_DB, f))
        .collect();
    assert!(
        e.windows(2).all(|w| w[1] <= w[0]),
        "the excursion envelope must never rise with frequency"
    );
}

// ───────────────────────────── the boost-Q cap ───────────────────────────────

#[test]
fn max_q_for_boost_matches_the_specs_worked_values() {
    // A = 10^(6/40) = 1.41254; Q = 0.227·100/1.41254 = 16.07
    assert_relative_eq!(max_q_for_boost(100.0, 6.0), 16.07, epsilon = 5e-3);
    // A = 1
    assert_relative_eq!(max_q_for_boost(100.0, 0.0), 22.7, epsilon = 1e-9);
}

#[test]
fn max_q_for_boost_tightens_as_gain_rises() {
    let q: Vec<f64> = (0..=10)
        .map(|g| max_q_for_boost(1000.0, g as f64))
        .collect();
    assert!(
        q.windows(2).all(|w| w[1] < w[0]),
        "a bigger boost must get a tighter Q ceiling, got {q:?}"
    );
}

#[test]
fn the_q_cap_is_the_stability_guard_it_claims_to_be() {
    // The spec's claim: with the cap applied the Jury test never fires. Sweep
    // the whole design space it can be applied over.
    for &f0 in &[20.0, 60.0, 200.0, 1000.0, 8000.0, 20000.0] {
        for &gain in &[0.5, 2.0, 6.0, 10.0] {
            let q = max_q_for_boost(f0, gain).min(20.0);
            let band = EQBand {
                filter_type: FilterType::Peaking,
                fc: f0,
                gain_db: gain,
                q,
            };
            assert!(
                is_stable(&band.to_sos(SR)),
                "capped band f0={f0} gain={gain} q={q} failed the Jury test"
            );
        }
    }
}

#[test]
fn width_and_q_are_exact_inverses() {
    // The conversion the two specs' narrow-dip thresholds differ across:
    // Q = 3 is 0.479 octave; 1/6 octave is Q = 8.64.
    assert_relative_eq!(width_oct_for_q(3.0), 0.4787, epsilon = 5e-4);
    // 1/6 octave is r = 2^(1/6), so 1/Q = sqrt(r) - 1/sqrt(r) = 0.115589.
    assert_relative_eq!(
        width_oct_for_q(8.651_35),
        DEFAULT_MIN_DIP_WIDTH_OCT,
        epsilon = 1e-6
    );
    assert!(
        width_oct_for_q(3.0) > DEFAULT_MIN_DIP_WIDTH_OCT,
        "engine-hardening's Q>3 veto is the STRICTER of the two — if this \
         flips, the reconciliation note in DEFAULT_MIN_DIP_WIDTH_OCT is wrong"
    );
}

// ─────────────────────────── the composition ─────────────────────────────────

#[test]
fn full_confidence_gives_the_bare_excursion_envelope() {
    let curve = curve_at_sigma(SIGMA_FULL_DB);
    let grid = LogGrid::standard();
    for (i, &f) in grid.freqs().iter().enumerate() {
        let e = excursion_db(&DEFAULT_EXCURSION_DB, f);
        assert_relative_eq!(curve.max_cut_db()[i], e, epsilon = 1e-12);
        assert_relative_eq!(
            curve.max_boost_db()[i],
            DEFAULT_BOOST_RATIO * e,
            epsilon = 1e-12
        );
    }
}

#[test]
fn zero_confidence_gives_zero_authority() {
    let curve = curve_at_sigma(SIGMA_NONE_DB);
    assert!(curve.max_cut_db().iter().all(|&v| v == 0.0));
    assert!(curve.max_boost_db().iter().all(|&v| v == 0.0));
}

#[test]
fn the_confidence_weight_is_linear_between_the_endpoints() {
    // sigma = 3.5 dB is the midpoint of [1, 6], so w = 0.5.
    let curve = curve_at_sigma(3.5);
    let grid = LogGrid::standard();
    for (i, &f) in grid.freqs().iter().enumerate() {
        let e = excursion_db(&DEFAULT_EXCURSION_DB, f);
        assert_relative_eq!(curve.max_cut_db()[i], 0.5 * e, epsilon = 1e-12);
    }
}

#[test]
fn boost_authority_is_asymmetric_by_construction() {
    let curve = curve_at_sigma(2.0);
    for i in 0..curve.len() {
        assert!(
            curve.max_boost_db()[i] <= curve.max_cut_db()[i],
            "boost ceiling must never exceed the cut ceiling at bin {i}"
        );
    }
}

#[test]
fn sigma_reproduces_the_200_hz_rule_without_hardcoding_it() {
    // The headline claim: feed a realistic untreated-room sigma profile (low
    // in the modal region, rising to the 5.57 dB diffuse-field asymptote above
    // Schroeder) and authority should collapse near 200 Hz — with no
    // frequency threshold anywhere in the module.
    let grid = LogGrid::standard();
    let sigma: Vec<f64> = grid
        .freqs()
        .iter()
        .map(|&f| {
            // 0.7 dB below 100 Hz rising to 5.57 dB by 400 Hz, log-f linear.
            let t = ((f / 100.0).log2() / (400f64 / 100.0).log2()).clamp(0.0, 1.0);
            0.7 + t * (5.57 - 0.7)
        })
        .collect();
    let curve = build_authority(&grid, &sigma, &policy()).expect("valid");
    let at = |f: f64| curve.at(f).max_cut_db;
    assert!(at(50.0) > 9.0, "full authority deep in the modal region");
    assert!(
        at(200.0) < 0.55 * at(50.0),
        "authority must be well past half-collapsed by 200 Hz, got {} vs {}",
        at(200.0),
        at(50.0)
    );
    assert!(
        at(1000.0) < 0.2,
        "authority must be essentially gone above the transition, got {}",
        at(1000.0)
    );
}

#[test]
fn a_treated_room_keeps_authority_higher() {
    // Same shape, halved sigma: the ceiling must extend upward, which is the
    // adaptivity a hardcoded 200 Hz threshold cannot express.
    let grid = LogGrid::standard();
    let profile = |scale: f64| -> Vec<f64> {
        grid.freqs()
            .iter()
            .map(|&f| {
                let t = ((f / 100.0).log2() / (400f64 / 100.0).log2()).clamp(0.0, 1.0);
                scale * (0.7 + t * (5.57 - 0.7))
            })
            .collect()
    };
    let untreated = build_authority(&grid, &profile(1.0), &policy()).expect("valid");
    let treated = build_authority(&grid, &profile(0.5), &policy()).expect("valid");
    assert!(treated.at(400.0).max_cut_db > untreated.at(400.0).max_cut_db);
}

#[test]
fn the_curve_interpolates_exactly_at_its_own_grid_points() {
    let curve = curve_at_sigma(2.5);
    for (i, &f) in curve.freqs().iter().enumerate() {
        let at = curve.at(f);
        assert_relative_eq!(at.max_cut_db, curve.max_cut_db()[i], epsilon = 1e-12);
        assert_relative_eq!(at.max_boost_db, curve.max_boost_db()[i], epsilon = 1e-12);
    }
}

#[test]
fn the_curve_clamps_outside_its_grid_rather_than_panicking() {
    let curve = curve_at_sigma(2.0);
    let first = curve.at(curve.freqs()[0]);
    let last = curve.at(*curve.freqs().last().unwrap());
    assert_eq!(curve.at(1.0), first);
    assert_eq!(curve.at(1e9), last);
    // A NaN probe must not reach partition_point (index underflow) — it takes
    // the first bin.
    assert_eq!(curve.at(f64::NAN), first);
}

// ──────────────────────────── build_authority gates ──────────────────────────

#[test]
fn build_authority_refuses_a_sigma_that_does_not_match_the_grid() {
    let grid = LogGrid::standard();
    assert!(build_authority(&grid, &[1.0, 2.0], &policy()).is_err());
}

#[test]
fn build_authority_refuses_non_finite_or_negative_sigma() {
    let grid = LogGrid::new(20.0, 200.0, 12).expect("valid grid");
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        let mut sigma = vec![1.0; grid.len()];
        sigma[3] = bad;
        assert!(
            build_authority(&grid, &sigma, &policy()).is_err(),
            "sigma = {bad} must refuse: an unchecked ceiling compares false \
             against every gain, which reads as no limit"
        );
    }
}

#[test]
fn build_authority_refuses_a_malformed_policy() {
    let grid = LogGrid::new(20.0, 200.0, 12).expect("valid grid");
    let sigma = vec![1.0; grid.len()];
    let cases: Vec<(&str, AuthorityPolicy)> = vec![
        (
            "empty excursion",
            AuthorityPolicy {
                excursion: vec![],
                ..policy()
            },
        ),
        (
            "non-increasing breakpoints",
            AuthorityPolicy {
                excursion: vec![(500.0, 2.0), (150.0, 10.0)],
                ..policy()
            },
        ),
        (
            "negative excursion dB",
            AuthorityPolicy {
                excursion: vec![(20.0, -1.0)],
                ..policy()
            },
        ),
        (
            "sigma_none <= sigma_full",
            AuthorityPolicy {
                sigma_none_db: 1.0,
                ..policy()
            },
        ),
        (
            "NaN sigma_full",
            AuthorityPolicy {
                sigma_full_db: f64::NAN,
                ..policy()
            },
        ),
        (
            "boost_ratio above 1",
            AuthorityPolicy {
                boost_ratio: 1.5,
                ..policy()
            },
        ),
        (
            "negative dip width",
            AuthorityPolicy {
                min_dip_width_oct: -0.1,
                ..policy()
            },
        ),
    ];
    for (name, p) in cases {
        assert!(
            build_authority(&grid, &sigma, &p).is_err(),
            "{name} must refuse"
        );
    }
}

// ──────────────────────────────── clamp_band ─────────────────────────────────

#[test]
fn clamp_band_leaves_an_in_authority_band_alone() {
    let curve = curve_at_sigma(0.5);
    let band = EQBand {
        filter_type: FilterType::Peaking,
        fc: 60.0,
        gain_db: -8.0,
        q: 4.0,
    };
    let (out, clamps) = clamp_band(&band, &curve);
    assert_eq!(out, band);
    assert!(clamps.is_empty(), "got {clamps:?}");
}

#[test]
fn clamp_band_attributes_an_excursion_clamp_when_confidence_is_full() {
    let curve = curve_at_sigma(SIGMA_FULL_DB);
    let band = EQBand {
        filter_type: FilterType::Peaking,
        fc: 2000.0,
        gain_db: -8.0,
        q: 2.0,
    };
    let (out, clamps) = clamp_band(&band, &curve);
    assert_relative_eq!(out.gain_db, -2.0, epsilon = 1e-12);
    assert!(
        matches!(clamps[0], Clamp::GainToExcursion { .. }),
        "sigma was full, so the physics envelope is what bit: {clamps:?}"
    );
}

#[test]
fn clamp_band_attributes_a_sigma_clamp_when_the_positions_disagree() {
    let curve = curve_at_sigma(3.5); // w = 0.5
    let band = EQBand {
        filter_type: FilterType::Peaking,
        fc: 60.0,
        gain_db: -8.0,
        q: 2.0,
    };
    let (out, clamps) = clamp_band(&band, &curve);
    assert_relative_eq!(out.gain_db, -5.0, epsilon = 1e-12);
    match clamps[0] {
        Clamp::GainToSigma { sigma_db, .. } => assert_relative_eq!(sigma_db, 3.5),
        ref other => panic!("expected a sigma attribution, got {other:?}"),
    }
}

#[test]
fn clamp_band_caps_boost_q_against_the_clamped_gain_not_the_request() {
    let curve = curve_at_sigma(SIGMA_FULL_DB);
    // 60 Hz: e = 10, boost ceiling 5 dB. Request +12 dB at Q 20.
    let band = EQBand {
        filter_type: FilterType::Peaking,
        fc: 60.0,
        gain_db: 12.0,
        q: 20.0,
    };
    let (out, _) = clamp_band(&band, &curve);
    assert_relative_eq!(out.gain_db, 5.0, epsilon = 1e-12);
    // The realized +5 dB band's cap, not the requested +12 dB band's.
    let expected = max_q_for_boost(60.0, 5.0).min(20.0);
    assert_relative_eq!(out.q, expected, epsilon = 1e-12);
    assert!(
        expected > max_q_for_boost(60.0, 12.0).min(20.0),
        "sanity: capping against the +12 dB request would have been TIGHTER \
         ({} vs {}) — restricting a band the corrector never asked for",
        max_q_for_boost(60.0, 12.0).min(20.0),
        expected
    );
}

#[test]
fn clamp_band_zeroes_a_non_finite_band_rather_than_clamping_it() {
    let curve = curve_at_sigma(1.0);
    for bad in [f64::NAN, f64::INFINITY] {
        let band = EQBand {
            filter_type: FilterType::Peaking,
            fc: 100.0,
            gain_db: bad,
            q: 2.0,
        };
        let (out, clamps) = clamp_band(&band, &curve);
        assert_eq!(
            out.gain_db, 0.0,
            "NaN.clamp() is NaN by documented Rust behaviour"
        );
        assert!(!clamps.is_empty());
    }
}

#[test]
fn stabilize_band_backs_q_off_and_gives_up_rather_than_shipping_an_unstable_row() {
    // A well-formed band survives untouched.
    let good = EQBand {
        filter_type: FilterType::Peaking,
        fc: 1000.0,
        gain_db: 3.0,
        q: 2.0,
    };
    let (band, sos) = stabilize_band(&good, SR).expect("a sane band designs");
    assert_eq!(band, good);
    assert!(is_stable(&sos));
    // A band whose Q is not a number cannot be rescued by scaling it.
    let hopeless = EQBand {
        filter_type: FilterType::Peaking,
        fc: 1000.0,
        gain_db: 3.0,
        q: f64::NAN,
    };
    assert!(stabilize_band(&hopeless, SR).is_none());
}

// ──────────────────────────── auto_fit_room ──────────────────────────────────

#[test]
fn the_discriminating_test_room_vetoes_the_null_the_legacy_fit_fills() {
    // The exact regression the module exists to prevent, pinning BOTH
    // behaviours: a measured -20 dB narrow dip at 60 Hz is a +20 dB residual
    // (correction = target - measured).
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 60.0)];
    let residual = gaussian_residual(&grid, f0, 20.0, 0.08);

    let legacy = auto_fit_parametric_eq(&residual, grid.freqs(), SR, 4, 1.0);
    assert!(
        !legacy.is_empty(),
        "the legacy fit must still fill the null"
    );
    assert!(
        legacy[0].gain_db > 15.0,
        "legacy band should be a large boost, got {:?}",
        legacy[0]
    );

    let report = fit(&residual, &curve_at_sigma(0.5), 4);
    assert!(
        report.bands.is_empty(),
        "auto_fit_room must return zero bands, got {:?}",
        report.bands
    );
    assert!(
        report
            .clamps
            .iter()
            .any(|c| matches!(c, Clamp::DipRefused { .. })),
        "and it must say why: {:?}",
        report.clamps
    );
}

#[test]
fn a_wide_dip_is_filled_within_authority() {
    // The other half of the veto's contract: wide positive excursions may be
    // real response, and are corrected — up to the boost ceiling.
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 60.0)];
    let residual = gaussian_residual(&grid, f0, 8.0, 1.0);
    let report = fit(&residual, &curve_at_sigma(0.5), 1);
    assert_eq!(report.bands.len(), 1, "clamps: {:?}", report.clamps);
    let band = &report.bands[0];
    assert!(band.gain_db > 0.0, "a dip must be filled as a boost");
    assert!(
        band.gain_db <= 5.0 + 1e-9,
        "and bounded by boost_ratio·e(60) = 5 dB, got {}",
        band.gain_db
    );
}

#[test]
fn zero_authority_everywhere_emits_nothing() {
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 60.0)];
    let residual: Vec<f64> = gaussian_residual(&grid, f0, 8.0, 1.0)
        .iter()
        .map(|v| -v)
        .collect();
    let report = fit(&residual, &curve_at_sigma(10.0), 8);
    assert!(report.bands.is_empty(), "got {:?}", report.bands);
}

#[test]
fn a_confident_peak_below_150_hz_is_cut_in_full() {
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 60.0)];
    let residual: Vec<f64> = gaussian_residual(&grid, f0, 8.0, 0.5)
        .iter()
        .map(|v| -v)
        .collect();
    let report = fit(&residual, &curve_at_sigma(0.5), 1);
    assert_eq!(report.bands.len(), 1);
    assert_relative_eq!(report.bands[0].gain_db, -8.0, epsilon = 1e-9);
    assert_relative_eq!(report.bands[0].fc, f0, epsilon = 1e-9);
    assert!(
        report.clamps.is_empty(),
        "-8 dB is inside the +-10 dB excursion at 60 Hz: {:?}",
        report.clamps
    );
}

#[test]
fn the_same_peak_at_2_khz_is_clamped_to_the_excursion_envelope() {
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 2000.0)];
    let residual: Vec<f64> = gaussian_residual(&grid, f0, 8.0, 0.5)
        .iter()
        .map(|v| -v)
        .collect();
    let report = fit(&residual, &curve_at_sigma(0.5), 1);
    assert_eq!(report.bands.len(), 1);
    assert_relative_eq!(report.bands[0].gain_db, -2.0, epsilon = 1e-9);
    assert!(matches!(report.clamps[0], Clamp::GainToExcursion { .. }));
}

#[test]
fn a_peak_always_outranks_a_dip_of_the_same_size() {
    // Asymmetric picking, stated as the behaviour it buys: with one band to
    // spend on a curve holding an equal peak and dip, the peak wins.
    let grid = LogGrid::standard();
    let peak_f = grid.freqs()[nearest_bin(&grid, 60.0)];
    let dip_f = grid.freqs()[nearest_bin(&grid, 120.0)];
    let peak = gaussian_residual(&grid, peak_f, 8.0, 0.5);
    let dip = gaussian_residual(&grid, dip_f, 8.0, 0.5);
    let residual: Vec<f64> = peak.iter().zip(&dip).map(|(p, d)| -p + d).collect();
    let report = fit(&residual, &curve_at_sigma(0.5), 1);
    assert_eq!(report.bands.len(), 1);
    assert!(
        report.bands[0].gain_db < 0.0,
        "the cut must be picked first, got {:?}",
        report.bands[0]
    );
    assert_relative_eq!(report.bands[0].fc, peak_f, epsilon = 1e-9);
}

#[test]
fn no_band_is_emitted_below_the_gates_own_resolution_limit() {
    // The link that makes "gated + spatially averaged" honest.
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 40.0)];
    let residual: Vec<f64> = gaussian_residual(&grid, f0, 8.0, 0.5)
        .iter()
        .map(|v| -v)
        .collect();
    let per = PerChannel::new(vec![residual]).expect("one channel");
    let curve = curve_at_sigma(0.5);
    let out = auto_fit_room(&per, &grid, SR, &curve, 8, 1.0, 200.0).expect("valid");
    let report = out.get(0).expect("one channel");
    assert!(
        report.bands.iter().all(|b| b.fc >= 200.0),
        "bands below min_valid_freq_hz: {:?}",
        report.bands
    );
}

#[test]
fn a_narrow_spike_on_a_broad_dip_loses_only_the_spike() {
    // The veto must refuse the interference null without discarding the broad,
    // genuinely correctable region it sits on.
    let grid = LogGrid::standard();
    let broad_f = grid.freqs()[nearest_bin(&grid, 300.0)];
    let spike_f = grid.freqs()[nearest_bin(&grid, 300.0)];
    let broad = gaussian_residual(&grid, broad_f, 3.0, 2.0);
    // Put the spike off-centre so the broad region survives on both sides.
    let spike = gaussian_residual(&grid, spike_f * 1.6, 12.0, 0.05);
    let residual: Vec<f64> = broad.iter().zip(&spike).map(|(b, s)| b + s).collect();
    let report = fit(&residual, &curve_at_sigma(0.5), 4);
    assert!(
        report
            .clamps
            .iter()
            .any(|c| matches!(c, Clamp::DipRefused { .. })),
        "the spike must be refused"
    );
    assert!(
        !report.bands.is_empty(),
        "but the broad region must still be corrected"
    );
    for b in &report.bands {
        assert!(
            (b.fc / (spike_f * 1.6)).log2().abs() > 0.05,
            "no band may land on the refused spike: {b:?}"
        );
    }
}

#[test]
fn every_channel_is_fitted_independently() {
    let grid = LogGrid::standard();
    let left_f = grid.freqs()[nearest_bin(&grid, 60.0)];
    let right_f = grid.freqs()[nearest_bin(&grid, 90.0)];
    let left: Vec<f64> = gaussian_residual(&grid, left_f, 6.0, 0.5)
        .iter()
        .map(|v| -v)
        .collect();
    let right: Vec<f64> = gaussian_residual(&grid, right_f, 6.0, 0.5)
        .iter()
        .map(|v| -v)
        .collect();
    let per = PerChannel::new(vec![left, right]).expect("two channels");
    let curve = curve_at_sigma(0.5);
    let out = auto_fit_room(&per, &grid, SR, &curve, 1, 1.0, 0.0).expect("valid");
    assert_eq!(out.channels(), 2);
    assert_relative_eq!(out.get(0).unwrap().bands[0].fc, left_f, epsilon = 1e-9);
    assert_relative_eq!(out.get(1).unwrap().bands[0].fc, right_f, epsilon = 1e-9);
}

#[test]
fn auto_fit_room_refuses_malformed_input() {
    let grid = LogGrid::standard();
    let curve = curve_at_sigma(1.0);
    let short = PerChannel::new(vec![vec![0.0; 10]]).expect("one channel");
    assert!(auto_fit_room(&short, &grid, SR, &curve, 4, 1.0, 0.0).is_err());

    let mut nan = vec![0.0; grid.len()];
    nan[5] = f64::NAN;
    let nan = PerChannel::new(vec![nan]).expect("one channel");
    assert!(auto_fit_room(&nan, &grid, SR, &curve, 4, 1.0, 0.0).is_err());

    let ok = PerChannel::new(vec![vec![0.0; grid.len()]]).expect("one channel");
    assert!(auto_fit_room(&ok, &grid, 0.0, &curve, 4, 1.0, 0.0).is_err());
    assert!(auto_fit_room(&ok, &grid, SR, &curve, 4, 1.0, f64::NAN).is_err());
    assert!(auto_fit_room(&ok, &grid, SR, &curve, 4, f64::NAN, 0.0).is_err());
}

#[test]
fn the_legacy_coupler_fit_is_untouched() {
    // "auto_fit_parametric_eq stays byte-identical" is a claim about this
    // file, so assert the two entry points disagree on the case that matters
    // rather than trusting the fixture alone to notice a refactor.
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 60.0)];
    let residual = gaussian_residual(&grid, f0, 20.0, 0.08);
    let legacy = auto_fit_parametric_eq(&residual, grid.freqs(), SR, 1, 1.0);
    assert_eq!(legacy.len(), 1);
    assert_relative_eq!(legacy[0].gain_db, 20.0, epsilon = 1e-9);
    assert_eq!(legacy[0].filter_type, FilterType::Peaking);
}

/// The realized cascade at every grid bin, in dB.
fn cascade_db(bands: &[EQBand], grid: &LogGrid) -> Vec<f64> {
    paraeq_dsp::peq::ParametricEQ {
        bands: bands.to_vec(),
        sample_rate: SR,
    }
    .frequency_response(grid.freqs())
}

/// Assert the whole fit stays inside the curve everywhere — not band by band.
fn assert_cascade_inside(report: &RoomFitReport, curve: &AuthorityCurve, grid: &LogGrid) {
    let cascade = cascade_db(&report.bands, grid);
    for (&f, &total) in grid.freqs().iter().zip(&cascade) {
        let at = curve.at(f);
        assert!(
            total <= at.max_boost_db + 0.05 && total >= -at.max_cut_db - 0.05,
            "cascade is {total:.3} dB at {f:.1} Hz, outside \
             [{:.3}, {:.3}] — the excursion envelope bounds what the DRIVER \
             sees, which is the sum, not any one band",
            -at.max_cut_db,
            at.max_boost_db
        );
    }
}

#[test]
fn the_cascade_stays_inside_the_ceiling_not_just_each_band() {
    // Review regression. A residual deeper than the ceiling stays the
    // top-scoring candidate after being partially corrected, so a per-band
    // clamp lets the greedy loop stack legal bands into an illegal cascade.
    // Measured before the fix: -22.9 dB realized against a +-10 dB envelope,
    // plus two spurious boosts on the shoulders it over-cut. Bounding only
    // each band's own centre bin still gave -14.6 dB (neighbouring skirts).
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 60.0)];
    let residual: Vec<f64> = gaussian_residual(&grid, f0, 24.0, 0.5)
        .iter()
        .map(|v| -v)
        .collect();
    let curve = curve_at_sigma(0.5);
    let report = fit(&residual, &curve, 6);

    assert_cascade_inside(&report, &curve, &grid);
    let cascade = cascade_db(&report.bands, &grid);
    let deepest = cascade.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(
        deepest < -9.0,
        "and it must still USE the authority it has, got {deepest:.3} dB"
    );
    assert!(
        report.bands.iter().all(|b| b.gain_db < 0.0),
        "a pure measured peak must not produce boosts: {:?}",
        report.bands
    );
}

#[test]
fn a_spent_ceiling_does_not_burn_the_band_budget() {
    // The other half: once the ceiling is used up at a feature, the loop must
    // strike it rather than emitting near-zero-gain filters until max_bands
    // runs out.
    let grid = LogGrid::standard();
    let f0 = grid.freqs()[nearest_bin(&grid, 60.0)];
    let residual: Vec<f64> = gaussian_residual(&grid, f0, 30.0, 0.4)
        .iter()
        .map(|v| -v)
        .collect();
    let report = fit(&residual, &curve_at_sigma(0.5), 8);
    assert!(
        report.bands.len() <= 3,
        "one feature at a spent ceiling should not consume 8 bands: {:?}",
        report.bands
    );
    assert!(
        report.bands.iter().all(|b| b.gain_db.abs() >= 1.0),
        "no band may be emitted below min_gain_db: {:?}",
        report.bands
    );
}

// ──────────────────────────────── properties ─────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The two guarantees the whole module exists to provide, over arbitrary
    /// residuals and arbitrary confidence.
    #[test]
    fn every_emitted_band_is_stable_and_inside_its_ceiling(
        centre_hz in 25.0f64..15_000.0,
        amp_db in -30.0f64..30.0,
        width_oct in 0.02f64..3.0,
        sigma in 0.0f64..8.0,
        max_bands in 1usize..6,
    ) {
        let grid = LogGrid::standard();
        let residual = gaussian_residual(&grid, centre_hz, amp_db, width_oct);
        let curve = curve_at_sigma(sigma);
        let report = fit(&residual, &curve, max_bands);
        for band in &report.bands {
            prop_assert!(
                is_stable(&band.to_sos(SR)),
                "unstable band {band:?}"
            );
            let at = curve.at(band.fc);
            prop_assert!(
                band.gain_db >= -at.max_cut_db - 1e-9 && band.gain_db <= at.max_boost_db + 1e-9,
                "gain {} outside [{}, {}] at {} Hz",
                band.gain_db, -at.max_cut_db, at.max_boost_db, band.fc
            );
            prop_assert!(
                (0.5..=20.0).contains(&band.q),
                "Q {} outside the global clamp",
                band.q
            );
        }
        prop_assert_eq!(report.dropped, 0, "the Q cap should make drops impossible");
        // The invariant the per-band check above CANNOT see: the driver sees
        // the sum. This is the property that caught the restacking bug.
        let cascade = cascade_db(&report.bands, &grid);
        for (&f, &total) in grid.freqs().iter().zip(&cascade) {
            let at = curve.at(f);
            prop_assert!(
                total <= at.max_boost_db + 0.05 && total >= -at.max_cut_db - 0.05,
                "cascade {} dB at {} Hz outside [{}, {}]",
                total, f, -at.max_cut_db, at.max_boost_db
            );
        }
    }

    /// Termination and the band budget: vetoes and zero-authority bins must
    /// not consume band slots, and the loop must always halt.
    #[test]
    fn the_fit_always_terminates_within_its_band_budget(
        sigma in 0.0f64..10.0,
        max_bands in 0usize..8,
    ) {
        let grid = LogGrid::standard();
        // A comb of alternating narrow peaks and dips: the adversarial input
        // for a greedy loop that strikes candidates as it goes.
        let residual: Vec<f64> = grid.freqs()
            .iter()
            .enumerate()
            .map(|(i, &f)| 12.0 * (f.log2() * 40.0).sin() * if i % 2 == 0 { 1.0 } else { 0.9 })
            .collect();
        let report = fit(&residual, &curve_at_sigma(sigma), max_bands);
        prop_assert!(report.bands.len() <= max_bands);
    }
}

// ─────────────────────────────── serde contract ──────────────────────────────

#[cfg(feature = "serde")]
mod wire {
    use super::*;

    #[test]
    fn the_curve_round_trips() {
        let curve = curve_at_sigma(2.0);
        let json = serde_json::to_string(&curve).expect("serializes");
        let back: AuthorityCurve = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, curve);
    }

    #[test]
    fn deserialization_is_not_a_second_unvalidated_constructor() {
        let curve = curve_at_sigma(2.0);
        let mut value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&curve).unwrap()).unwrap();
        // A NaN ceiling would compare false against every gain, i.e. read as
        // "no limit" rather than "no authority" — the exact fail-unsafe shape
        // build_authority refuses.
        value["max_cut_db"][0] = serde_json::json!(null);
        assert!(serde_json::from_value::<AuthorityCurve>(value.clone()).is_err());

        let mut ragged: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&curve).unwrap()).unwrap();
        ragged["max_boost_db"] = serde_json::json!([1.0, 2.0]);
        assert!(serde_json::from_value::<AuthorityCurve>(ragged).is_err());

        let mut negative: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&curve).unwrap()).unwrap();
        negative["max_cut_db"][0] = serde_json::json!(-1.0);
        assert!(serde_json::from_value::<AuthorityCurve>(negative).is_err());
    }
}
