//! Tier 3 (analytic physics/policy) for `authority.rs` and
//! `autofit::auto_fit_room`. No oracle exists and none is faked: what is
//! pinned is the closed form of every composition step, the worked values the
//! spec states, and the invariants the module exists to guarantee.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`authority.rs` — new" and
//! "`autofit.rs` changes (additive)".

use approx::assert_relative_eq;
use paraeq_dsp::authority::{
    authority_band_mask, build_authority, clamp_band, excursion_db, is_stable, max_q_for_boost,
    max_q_for_boost_capped, stabilize_band, width_oct_for_q, AuthorityCurve, AuthorityPolicy,
    Clamp, QCapPolicy, COUPLER_CUTOFF_HZ, COUPLER_EXCURSION_DB, COUPLER_Q_CEILING,
    DEFAULT_BOOST_RATIO, DEFAULT_EXCURSION_DB, DEFAULT_MIN_DIP_WIDTH_OCT, Q_CLAMP, ROOM_Q_CEILING,
    SIGMA_FULL_DB, SIGMA_NONE_DB,
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

// ─────────────────────── the coupler excursion envelope ──────────────────────

#[test]
fn the_coupler_envelope_is_zero_above_ten_kilohertz() {
    // decision-engine-design.md § Authority 1, verbatim: "On the coupler
    // path, `A_base(f) = 0` above 10 kHz." Above means above: at the cutoff
    // itself the envelope is still the room vector's +-2 dB.
    assert_eq!(excursion_db(&COUPLER_EXCURSION_DB, COUPLER_CUTOFF_HZ), 2.0);
    for f in [10_001.0, 12_000.0, 16_000.0, 20_000.0, 40_000.0] {
        assert_eq!(
            excursion_db(&COUPLER_EXCURSION_DB, f),
            0.0,
            "the coupler envelope must be exactly zero at {f} Hz, not tapering \
             toward it — a taper licenses correction the spec licenses none of"
        );
    }

    // ... and stays at the room vector's shape below it, bin for bin.
    let grid = LogGrid::standard();
    for &f in grid.freqs() {
        if f <= COUPLER_CUTOFF_HZ {
            assert_relative_eq!(
                excursion_db(&COUPLER_EXCURSION_DB, f),
                excursion_db(&DEFAULT_EXCURSION_DB, f),
                epsilon = 1e-12
            );
        }
    }

    // The two path constructors carry their own vectors, so `decide()` picks a
    // path rather than re-specifying numbers.
    assert_eq!(
        AuthorityPolicy::coupler().excursion,
        COUPLER_EXCURSION_DB.to_vec()
    );
    assert_eq!(
        AuthorityPolicy::room().excursion,
        DEFAULT_EXCURSION_DB.to_vec()
    );
    assert_eq!(AuthorityPolicy::room(), AuthorityPolicy::default());

    // And the coupler vector is a policy `build_authority` accepts: strictly
    // increasing, finite, non-negative.
    let sigma = vec![SIGMA_FULL_DB; grid.len()];
    let curve = build_authority(&grid, &sigma, &AuthorityPolicy::coupler())
        .expect("the coupler policy must be well-formed");
    for (i, &f) in grid.freqs().iter().enumerate() {
        if f > COUPLER_CUTOFF_HZ {
            assert_eq!(
                (curve.max_cut_db()[i], curve.max_boost_db()[i]),
                (0.0, 0.0),
                "no authority of either sign above {COUPLER_CUTOFF_HZ} Hz on \
                 the coupler path, at {f} Hz"
            );
        }
    }
}

// ───────────────────────────── the path Q ceiling ────────────────────────────

#[test]
fn the_room_q_ceiling_is_log_linear_between_its_breakpoints() {
    // decision-engine-design.md § Decision table, `q_cap`: room
    // `10.0 @ 200 Hz -> 3.0 @ 10 kHz` log-linear.
    let room = ROOM_Q_CEILING;
    assert_eq!(
        room,
        QCapPolicy::LogLinear {
            hi: (10_000.0, 3.0),
            lo: (200.0, 10.0),
        }
    );
    assert_relative_eq!(room.ceiling_at(200.0), 10.0, epsilon = 1e-12);
    assert_relative_eq!(room.ceiling_at(10_000.0), 3.0, epsilon = 1e-12);
    // Clamped to the end values outside the breakpoints, like every other
    // interpolation in this module.
    assert_relative_eq!(room.ceiling_at(20.0), 10.0, epsilon = 1e-12);
    assert_relative_eq!(room.ceiling_at(20_000.0), 3.0, epsilon = 1e-12);

    // The midpoint in LOG f is the geometric mean, sqrt(200 * 10000) =
    // 1414.21 Hz, where a log-linear ramp is exactly halfway: 6.5.
    let mid_log_f = (200.0f64 * 10_000.0).sqrt();
    assert_relative_eq!(room.ceiling_at(mid_log_f), 6.5, epsilon = 1e-9);

    // The falsifier this test exists for: a linear-f implementation passes
    // both endpoints and is wrong everywhere between. At the log midpoint it
    // answers 9.13, which is 2.6 Q looser than the spec's ramp.
    let linear_f = 10.0 + (mid_log_f - 200.0) / (10_000.0 - 200.0) * (3.0 - 10.0);
    assert!(
        (linear_f - room.ceiling_at(mid_log_f)).abs() > 2.0,
        "a linear-f ramp would answer {linear_f:.3} here; if this assertion \
         stops discriminating, the test has stopped testing anything"
    );

    // A flat ceiling is flat everywhere, including outside any grid.
    let coupler = COUPLER_Q_CEILING;
    assert_eq!(coupler, QCapPolicy::Ceiling(5.0));
    for f in [1.0, 200.0, 10_000.0, 1e9] {
        assert_eq!(coupler.ceiling_at(f), 5.0);
    }

    // Fail-safe on a frequency that cannot be placed: the TIGHTEST breakpoint,
    // never an unbounded ceiling. A NaN ceiling compares false against every Q,
    // which reads as "no limit" rather than "no authority".
    assert_eq!(room.ceiling_at(f64::NAN), 3.0);
    assert_eq!(coupler.ceiling_at(f64::NAN), 5.0);
}

#[test]
fn every_boost_bands_q_is_at_most_the_gain_dependent_cap_and_the_path_ceiling() {
    // The gain-dependent cap alone, at the spec's two worked values.
    assert_relative_eq!(max_q_for_boost(100.0, 6.0), 16.07, epsilon = 5e-3);
    assert_relative_eq!(max_q_for_boost(100.0, 0.0), 22.7, epsilon = 1e-9);

    // Coupler: `Ceiling(5.0)` is far below both, so it is what binds.
    let coupler = AuthorityPolicy::coupler().q_ceiling;
    assert_relative_eq!(
        max_q_for_boost_capped(100.0, 6.0, &coupler),
        5.0,
        epsilon = 1e-12
    );
    assert_relative_eq!(
        max_q_for_boost_capped(100.0, 0.0, &coupler),
        5.0,
        epsilon = 1e-12
    );
    // ... but the gain cap still wins where it is tighter than the path: at
    // 20 Hz a +6 dB boost is capped at 0.227*20/1.41254 = 3.21.
    assert_relative_eq!(
        max_q_for_boost_capped(20.0, 6.0, &coupler),
        max_q_for_boost(20.0, 6.0),
        epsilon = 1e-12
    );

    // Room: `LogLinear` clamps to 10.0 at and below 200 Hz, so at 100 Hz it
    // binds against both worked values; at 10 kHz the ceiling is 3.0 while the
    // gain cap is three orders of magnitude looser.
    let room = AuthorityPolicy::room().q_ceiling;
    assert_relative_eq!(
        max_q_for_boost_capped(100.0, 6.0, &room),
        10.0,
        epsilon = 1e-12
    );
    assert_relative_eq!(
        max_q_for_boost_capped(100.0, 0.0, &room),
        10.0,
        epsilon = 1e-12
    );
    assert_relative_eq!(
        max_q_for_boost_capped(10_000.0, 6.0, &room),
        3.0,
        epsilon = 1e-12
    );

    // The composition is the min of three terms, never above any of them, and
    // never outside the global clamp — swept over the whole design space.
    for policy in [&coupler, &room] {
        for f0 in [20.0, 60.0, 200.0, 1000.0, 8000.0, 20000.0] {
            for gain in [0.0, 0.5, 2.0, 6.0, 10.0] {
                let q = max_q_for_boost_capped(f0, gain, policy);
                assert!(q <= max_q_for_boost(f0, gain) + 1e-12);
                assert!(q <= policy.ceiling_at(f0) + 1e-12);
                assert!(
                    Q_CLAMP.contains(&q),
                    "q={q} outside the global clamp at f0={f0} gain={gain}"
                );
                let band = EQBand {
                    filter_type: FilterType::Peaking,
                    fc: f0,
                    gain_db: gain,
                    q,
                };
                assert!(
                    is_stable(&band.to_sos(SR)),
                    "capped band {band:?} failed the Jury test"
                );
            }
        }
    }

    // One composition, not two: with a path ceiling that cannot bind, the new
    // function must reproduce `clamp_band`'s realized Q exactly. If these ever
    // drift, the Q ceiling has become a second answer.
    let curve = curve_at_sigma(SIGMA_FULL_DB);
    let unbinding = QCapPolicy::Ceiling(*Q_CLAMP.end());
    for fc in [30.0, 60.0, 120.0, 400.0] {
        let band = EQBand {
            filter_type: FilterType::Peaking,
            fc,
            gain_db: 12.0,
            q: *Q_CLAMP.end(),
        };
        let (out, _) = clamp_band(&band, &curve);
        assert_relative_eq!(
            out.q,
            max_q_for_boost_capped(fc, out.gain_db, &unbinding),
            epsilon = 1e-12
        );
    }
}

// ─────────────────────────── the authority band ──────────────────────────────

#[test]
fn the_authority_band_mask_is_the_same_function_the_gate_and_the_stop_both_use() {
    // decision-engine-design.md § Refusal table, verbatim: the band is
    // `correction_range ∩ { f : authority.at(f).max_boost_db > 0 || max_cut_db > 0 }`.
    let grid = LogGrid::standard();
    // sigma below sigma_none in the modal region, at/above it further up, so
    // authority genuinely collapses partway along the grid rather than the
    // range alone doing all the masking.
    let sigma: Vec<f64> = grid
        .freqs()
        .iter()
        .map(|&f| if f < 300.0 { 0.5 } else { SIGMA_NONE_DB })
        .collect();
    let curve = build_authority(&grid, &sigma, &policy()).expect("valid");
    let range = (40.0, 1000.0);
    let mask = authority_band_mask(grid.freqs(), &curve, range);

    assert_eq!(mask.len(), grid.len());
    // The second, open-coded filter the plan exists to prevent — written HERE,
    // in the test, so that the crate's `src/` holds exactly one copy.
    for (i, &f) in grid.freqs().iter().enumerate() {
        let at = curve.at(f);
        let expected =
            f >= range.0 && f <= range.1 && (at.max_boost_db > 0.0 || at.max_cut_db > 0.0);
        assert_eq!(mask[i], expected, "bin {i} at {f:.2} Hz");
    }
    assert!(mask.iter().any(|&m| m), "the band must not be empty here");
    assert!(
        mask.iter().any(|&m| !m),
        "and it must not be the whole grid, or this asserts nothing"
    );

    // Both edges are inclusive. `narrow` ends inside the confident region, so
    // the upper edge tests the RANGE rather than the authority collapse.
    let narrow = (40.0, 250.0);
    assert!(!authority_band_mask(&[39.9], &curve, narrow)[0]);
    assert!(authority_band_mask(&[40.0], &curve, narrow)[0]);
    assert!(authority_band_mask(&[250.0], &curve, narrow)[0]);
    assert!(!authority_band_mask(&[250.1], &curve, narrow)[0]);
    // Zero authority inside the range is still out of band.
    assert!(!authority_band_mask(&[900.0], &curve, range)[0]);

    // Degenerate inputs are empty, not panics and not everything: a band that
    // cannot be placed must grade nothing rather than grade everything.
    assert_eq!(
        authority_band_mask(&[100.0], &curve, (1000.0, 40.0)),
        [false]
    );
    assert_eq!(
        authority_band_mask(&[100.0], &curve, (f64::NAN, 1000.0)),
        [false]
    );
    assert_eq!(authority_band_mask(&[f64::NAN], &curve, range), [false]);

    // The guard that makes the name true: the predicate is spelled in exactly
    // one place in the workspace's library code. B4's residual-RMS stop and
    // B8's verification gate must CALL `authority_band_mask`; re-deriving the
    // definition in either of them fails here.
    //
    // The needles are the DISJUNCTION, in either order — the trailing `||` is
    // what makes them the band predicate and not `autofit`'s legitimate
    // `if at.max_cut_db > 0.0` divide-by-zero guard.
    let needles = ["max_boost_db>0.0||", "max_cut_db>0.0||"];
    let mut hits: Vec<(String, usize)> = Vec::new();
    for path in workspace_library_sources() {
        let squashed = code_without_comments(&path);
        let n: usize = needles
            .iter()
            .map(|needle| squashed.matches(needle).count())
            .sum();
        if n > 0 {
            hits.push((path, n));
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "the authority-band predicate must live in exactly one file, found {hits:?}"
    );
    assert!(
        hits[0].0.ends_with("paraeq-dsp/src/authority.rs"),
        "and that file must be authority.rs, found {hits:?}"
    );
    assert_eq!(
        hits[0].1, 1,
        "and be spelled exactly once there, found {hits:?}"
    );
}

/// Every `.rs` file under a workspace member's `src/`, absolute, sorted —
/// the five crates and the desktop, which is where a second copy of a DSP
/// predicate would most plausibly appear.
///
/// Library code only: a test is allowed — and in the case above, required — to
/// re-derive a definition independently in order to check it.
fn workspace_library_sources() -> Vec<String> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/paraeq-dsp has a parent")
        .to_path_buf();
    let workspace = crates.parent().expect("crates/ has a parent").to_path_buf();
    let mut stack: Vec<std::path::PathBuf> = std::fs::read_dir(&crates)
        .expect("the crates directory is readable")
        .filter_map(|e| e.ok().map(|e| e.path().join("src")))
        .filter(|p| p.is_dir())
        .collect();
    stack.push(workspace.join("desktop/src-tauri/src"));
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a src directory is readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    assert!(
        out.len() > 10,
        "the source scan found only {} files — it is not looking where it \
         thinks it is",
        out.len()
    );
    out.sort();
    out
}

/// A file's code with line comments and all whitespace removed, so the scan
/// above matches a rustfmt line break and does not match prose that quotes the
/// predicate.
fn code_without_comments(path: &str) -> String {
    let text = std::fs::read_to_string(path).expect("a source file is readable");
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let code = match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        };
        out.extend(code.chars().filter(|c| !c.is_whitespace()));
    }
    out
}

// ───────────────────────── QCapPolicy's move to dsp ──────────────────────────

/// `QCapPolicy`'s wire contract, under the same gate as the rest of this file's
/// serde tests. `cargo test --workspace` runs it — `paraeq-decide`,
/// `paraeq-engine` and the desktop all turn `paraeq-dsp`'s `serde` feature on
/// unconditionally, so feature unification has it on for every command CI and
/// CLAUDE.md name. The gate is what keeps `cargo test -p paraeq-dsp` compiling.
#[cfg(feature = "serde")]
mod q_cap_wire {
    use super::*;

    #[test]
    fn q_cap_policy_round_trips_through_serde_after_the_move() {
        // The move is byte-for-byte invisible on the wire: externally-tagged
        // serde depends only on the variant names, so no fixture and no
        // persisted profile moves with the type.
        let coupler = COUPLER_Q_CEILING;
        assert_eq!(
            serde_json::to_string(&coupler).expect("serializes"),
            r#"{"Ceiling":5.0}"#
        );
        assert_eq!(
            serde_json::from_str::<QCapPolicy>(r#"{"Ceiling":5.0}"#).expect("deserializes"),
            coupler
        );

        let room = ROOM_Q_CEILING;
        let json = serde_json::to_string(&room).expect("serializes");
        assert_eq!(
            json,
            r#"{"LogLinear":{"hi":[10000.0,3.0],"lo":[200.0,10.0]}}"#
        );
        assert_eq!(
            serde_json::from_str::<QCapPolicy>(&json).expect("deserializes"),
            room
        );

        // The two shipped path ceilings are the two variants, so the wire form
        // above is the whole wire form.
        assert!(matches!(coupler, QCapPolicy::Ceiling(_)));
        assert!(matches!(room, QCapPolicy::LogLinear { .. }));
    }
}
