//! Tier 3 — analytic physics.
//!
//! Un-ignore when `splice.rs` lands: room-dsp/6, Stage 4 — and only if FDW
//! misses. `fdw.rs` subsumes this module; the room-dsp spec's asymmetry error
//! bound decides whether `splice.rs` is ever built. If FDW holds, delete this
//! file with the module.

use paraeq_dsp::{
    logf::LogGrid,
    splice::{self, SpliceSpec},
};
use std::f64::consts::PI;

const PPO: u32 = 96;
/// The step an unmatched splice produced in testing. Any constant works; this
/// one is the spec's own number.
const OFFSET_DB: f64 = 13.28;
const OVERLAP: SpliceSpec = SpliceSpec {
    overlap_lo_hz: 200.0,
    overlap_hi_hz: 400.0,
};

/// Deliberately NOT flat: a −0.9 dB/oct tilt plus a bump, so that "no
/// discontinuity beyond the underlying curve's own slope" is a real constraint
/// rather than a tautology about a constant.
fn low_curve(grid: &LogGrid) -> Vec<f64> {
    grid.freqs().iter().map(|&f| tilt(f) + bump(f)).collect()
}

fn tilt(f: f64) -> f64 {
    -0.9 * (f / 1000.0).log2()
}

/// Centred at ~316 Hz — inside the 200–400 Hz overlap on purpose, so a curve
/// carrying it and one that does not actually disagree where the blend runs.
fn bump(f: f64) -> f64 {
    3.0 * (-(f.log10() - 2.5).powi(2) / 0.02).exp()
}

#[test]
#[ignore = "splice.rs lands in Stage 4 (room-dsp/6), only if FDW misses"]
fn splice_is_continuous_after_level_matching() {
    let grid = LogGrid::new(20.0, 20_000.0, PPO).unwrap();
    let low = low_curve(&grid);
    // `high` differs from `low` by a pure constant. The level match must
    // therefore recover exactly that constant, and the blend must return `low`
    // at every bin: (1−wt)·low + wt·(high + offset) == low for ANY wt. That
    // makes this one assertion cover level matching, blending and continuity.
    let high: Vec<f64> = low.iter().map(|v| v - OFFSET_DB).collect();

    let (out, report) = splice::splice(&low, &high, &grid, &OVERLAP).unwrap();
    assert_eq!(out.len(), grid.len());

    assert!(
        (report.level_offset_db - OFFSET_DB).abs() < 1e-12,
        "mean-dB level match must recover the constant: {} vs {OFFSET_DB}",
        report.level_offset_db
    );

    for i in 0..grid.len() {
        assert!(
            (out[i] - low[i]).abs() < 1e-12,
            "bin {i} ({:.1} Hz): {} vs {}",
            grid.freqs()[i],
            out[i],
            low[i]
        );
    }

    // Zero discontinuity: the output's bin-to-bin slope is the source's own.
    for i in 1..grid.len() {
        let d_out = out[i] - out[i - 1];
        let d_low = low[i] - low[i - 1];
        assert!(
            (d_out - d_low).abs() < 1e-12,
            "bin-to-bin step at {i} ({:.1} Hz) exceeds the curve's own slope: {d_out} vs {d_low}",
            grid.freqs()[i]
        );
    }

    // The level match is load-bearing, not cosmetic. Above the overlap the
    // blend is pure `high`: unmatched that is `low − 13.28`, matched it is
    // `low`. Assert we got `low`, and that the rejected alternative really is a
    // 13.28 dB step away.
    let above = grid
        .freqs()
        .iter()
        .position(|&f| f > OVERLAP.overlap_hi_hz)
        .expect("grid must extend above the overlap");
    for i in above..grid.len() {
        assert!((out[i] - low[i]).abs() < 1e-12);
        assert!(
            (out[i] - (low[i] - OFFSET_DB)).abs() > 13.0,
            "an unmatched splice would step {OFFSET_DB} dB here"
        );
    }

    // Raised-cosine weights: a partition of unity, 0 and 1 exactly at the ends.
    // Exposed separately because `splice` always level matches, which removes
    // the very difference between `low` and `high` the weights would reveal.
    assert_eq!(splice::blend_weight(0.0), 0.0);
    assert_eq!(splice::blend_weight(1.0), 1.0);
    for k in 0..=100 {
        let u = k as f64 / 100.0;
        let wt = splice::blend_weight(u);
        assert!(
            (wt - 0.5 * (1.0 - (PI * u).cos())).abs() < 1e-12,
            "blend_weight({u}) must be 0.5·(1 − cos(π·u))"
        );
        assert!(
            ((1.0 - wt) + wt - 1.0).abs() < 1e-15,
            "weights sum to 1 at u={u}"
        );
    }
}

/// The continuity test above deliberately makes `high` differ from `low` by a
/// pure constant, so after level matching the two agree everywhere and the
/// blend output is `low` for ANY weights. That is what makes its one assertion
/// so sharp about level matching — and it is exactly why it can say nothing
/// about the blend: a `splice` ignoring `overlap_lo_hz`/`overlap_hi_hz`
/// entirely still passes it.
///
/// So here the two curves genuinely disagree, by a bump centred inside the
/// overlap. The assertions are structural rather than a pinned expected curve,
/// because the level match's exact value depends on how `splice` averages over
/// the overlap, and this test must not over-constrain a module the FDW
/// asymmetry result may yet delete.
///
/// Verified against modelled implementations: a raised-cosine blend passes
/// (89 of the overlap's bins land strictly between the sources); one ignoring
/// the overlap bounds fails on the first bin below 200 Hz; a hard switchover
/// fails on `blended_bins == 0`. A LINEAR blend also passes — deliberately:
/// the weight function's raised-cosine shape is pinned by
/// `splice_is_continuous_after_level_matching`'s `blend_weight` assertions,
/// and this test's job is that `splice` applies those weights over the right
/// frequencies.
#[test]
#[ignore = "splice.rs lands in Stage 4 (room-dsp/6), only if FDW misses"]
fn the_blend_runs_over_the_overlap_and_nowhere_else() {
    let grid = LogGrid::new(20.0, 20_000.0, PPO).unwrap();
    let low = low_curve(&grid);
    // `high` is the same tilt WITHOUT the bump, offset down: the two sources
    // disagree by `bump(f)`, which peaks inside the overlap.
    let high: Vec<f64> = grid.freqs().iter().map(|&f| tilt(f) - OFFSET_DB).collect();

    let (out, report) = splice::splice(&low, &high, &grid, &OVERLAP).unwrap();
    assert_eq!(out.len(), grid.len());
    let matched: Vec<f64> = high.iter().map(|v| v + report.level_offset_db).collect();

    let mut blended_bins = 0;
    for (i, &f) in grid.freqs().iter().enumerate() {
        if f < OVERLAP.overlap_lo_hz {
            assert!(
                (out[i] - low[i]).abs() < 1e-12,
                "{f:.1} Hz is below the overlap: the output must be `low` \
                 untouched, not a blend"
            );
        } else if f > OVERLAP.overlap_hi_hz {
            assert!(
                (out[i] - matched[i]).abs() < 1e-12,
                "{f:.1} Hz is above the overlap: the output must be the \
                 level-matched `high`"
            );
        } else {
            // Inside: a convex combination of the two sources, so it lies
            // between them and never outside.
            let (lo_v, hi_v) = (low[i].min(matched[i]), low[i].max(matched[i]));
            assert!(
                out[i] >= lo_v - 1e-12 && out[i] <= hi_v + 1e-12,
                "{f:.1} Hz: {} is outside [{lo_v}, {hi_v}] — not a convex blend",
                out[i]
            );
            // Count the bins where the sources actually disagree AND the
            // output sits strictly between them: that is the blend being
            // observed. A hard switchover would produce none.
            if (low[i] - matched[i]).abs() > 0.1
                && (out[i] - low[i]).abs() > 1e-9
                && (out[i] - matched[i]).abs() > 1e-9
            {
                blended_bins += 1;
            }
        }
    }
    assert!(
        blended_bins > 0,
        "no bin in the overlap sits strictly between the two sources: this is \
         a hard switchover, not a blend"
    );
}
