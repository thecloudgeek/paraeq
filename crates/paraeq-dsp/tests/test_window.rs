//! Tier 2 + 3 for `window.rs` (room-dsp/3). Tier 2 pins the full symmetric
//! windows against `scipy.signal.windows.{blackmanharris, hann, tukey}` at
//! n ∈ {8, 9, 64, 4096} to 1e-12 ABSOLUTE (the generator's measured relative
//! divergence at blackmanharris edges makes relative comparison meaningless;
//! windows are bounded in [0, 1]). Tier 3 pins the analytic identities:
//! symmetry, `Rect` all-ones, `Tukey{0} == Rect`, `Tukey{1} == Hann`, and the
//! `half_taper` inner-edge contract.

mod common;

use common::{assert_allclose, Case};
use paraeq_dsp::window::{WindowKind, WindowSpec};

const ALL_KINDS: [WindowKind; 4] = [
    WindowKind::BlackmanHarris,
    WindowKind::Hann,
    WindowKind::Rect,
    WindowKind::Tukey { alpha: 0.25 },
];

fn kind_of(case: &Case) -> WindowKind {
    match case.param_str("kind") {
        "blackmanharris" => WindowKind::BlackmanHarris,
        "hann" => WindowKind::Hann,
        "rect" => WindowKind::Rect,
        "tukey" => WindowKind::Tukey {
            alpha: case.param_f64("alpha"),
        },
        other => panic!("unknown fixture kind {other}"),
    }
}

// ---- Tier 2: scipy golden fixtures -----------------------------------------

#[test]
fn full_matches_scipy_fixtures() {
    for kind in ["blackmanharris", "hann", "rect", "tukey"] {
        for n in [8usize, 9, 64, 4096] {
            let case = Case::load("window", &format!("{kind}_{n}"));
            assert_eq!(case.param_u64("n") as usize, n);
            let w = kind_of(&case).full(n);
            assert_allclose(
                &w,
                &case.array("window"),
                0.0,
                1e-12,
                &format!("{kind}_{n}"),
            );
        }
    }
}

/// The generator's called-out degenerate: tukey alpha=0.25 at n=9 tapers over
/// exactly one sample each end — [0, 1, 1, 1, 1, 1, 1, 1, 0]. A half-open
/// taper loop gets this wrong.
#[test]
fn tukey_degenerate_single_sample_taper() {
    let w = WindowKind::Tukey { alpha: 0.25 }.full(9);
    let expected = [0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0];
    assert_eq!(w, expected);
}

// ---- Tier 3: analytic identities -------------------------------------------

#[test]
fn full_is_symmetric() {
    for kind in ALL_KINDS {
        for n in [2usize, 3, 8, 9, 64, 101] {
            let w = kind.full(n);
            for i in 0..n {
                assert!(
                    w[i] == w[n - 1 - i],
                    "{kind:?} n={n}: w[{i}]={} != w[{}]={}",
                    w[i],
                    n - 1 - i,
                    w[n - 1 - i]
                );
            }
        }
    }
}

#[test]
fn rect_is_all_ones() {
    for n in [1usize, 2, 8, 9, 64] {
        assert_eq!(WindowKind::Rect.full(n), vec![1.0; n]);
    }
}

#[test]
fn tukey_alpha_zero_is_rect() {
    for n in [1usize, 2, 8, 9, 64] {
        assert_eq!(
            WindowKind::Tukey { alpha: 0.0 }.full(n),
            WindowKind::Rect.full(n),
            "n={n}"
        );
    }
}

#[test]
fn tukey_alpha_one_is_hann() {
    for n in [1usize, 2, 8, 9, 64] {
        assert_eq!(
            WindowKind::Tukey { alpha: 1.0 }.full(n),
            WindowKind::Hann.full(n),
            "n={n}"
        );
    }
}

#[test]
fn full_edge_lengths() {
    for kind in ALL_KINDS {
        assert!(kind.full(0).is_empty(), "{kind:?}: full(0) must be empty");
        assert_eq!(kind.full(1), vec![1.0], "{kind:?}: full(1) must be [1.0]");
    }
}

/// The derivation contract: `half_taper(n)` is the last n samples of the full
/// symmetric window of length 2n−1, so index 0 is the full window's center —
/// exactly 1.0 — declining outward.
#[test]
fn half_taper_is_tail_of_odd_full_window() {
    for kind in ALL_KINDS {
        for n in [1usize, 2, 5, 24, 100] {
            let half = kind.half_taper(n);
            let full = kind.full(2 * n - 1);
            assert_eq!(half.len(), n, "{kind:?} n={n}");
            assert_eq!(half, full[n - 1..], "{kind:?} n={n}");
        }
    }
}

#[test]
fn half_taper_edge_lengths_and_inner_edge() {
    for kind in ALL_KINDS {
        assert!(
            kind.half_taper(0).is_empty(),
            "{kind:?}: half_taper(0) must be empty, not a panic"
        );
        assert_eq!(kind.half_taper(1), vec![1.0], "{kind:?}: half_taper(1)");
        for n in [2usize, 5, 24, 100] {
            let half = kind.half_taper(n);
            // Exactly 1.0, no tolerance: the cosine sums hit their center on
            // cos(kπ) = ±1 exactly and the blackmanharris coefficients sum to
            // 1.0 in f64.
            assert!(
                half[0] == 1.0,
                "{kind:?} n={n}: inner edge {} != 1.0",
                half[0]
            );
        }
    }
}

/// Orientation + shape: the Hann half declines monotonically from 1.0 at the
/// inner edge to exactly 0.0 at the outer end.
#[test]
fn hann_half_taper_declines_outward_to_zero() {
    let half = WindowKind::Hann.half_taper(33);
    assert!(half[0] == 1.0);
    assert!(half[32] == 0.0, "outer end {} != 0.0", half[32]);
    for k in 1..33 {
        assert!(
            half[k] < half[k - 1],
            "not strictly declining at {k}: {} vs {}",
            half[k],
            half[k - 1]
        );
    }
}

#[test]
fn default_spec_is_tukey_quarter_both_sides() {
    let spec = WindowSpec::default();
    assert_eq!(spec.left, WindowKind::Tukey { alpha: 0.25 });
    assert_eq!(spec.right, WindowKind::Tukey { alpha: 0.25 });
    assert_eq!(WindowKind::TUKEY_DEFAULT, WindowKind::Tukey { alpha: 0.25 });
}
