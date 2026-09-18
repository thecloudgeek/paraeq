//! The averaging-divergence pin, by literal.
//!
//! The spec pins it as a number rather than as a property, deliberately:
//! "The averaging divergence is pinned by literal: a −30 dB null at **one of
//! five** positions gives dB-avg = **−6.0 dB** and power-avg = **−0.97 dB**.
//! (The −5.0/−0.79 pair is the *six*-position result; a −40 dB null at one of
//! three gives −13.3 vs −1.76.)"
//!
//! Why a literal and not a property: the two averaging modes are the room path
//! and the coupler path, and choosing the wrong one is not a crash — it is a
//! correction that tries to fill a seat-specific null with 6 dB of boost the
//! other four seats never needed. A property test ("power is greater than dB
//! mean") passes on an implementation that is wrong by 5 dB. The number does
//! not.
//!
//! Two halves, both here:
//!
//! 1. **The arithmetic**, through `paraeq-dsp`'s own two averaging functions,
//!    on the exact configuration the spec states. Exact for the dB mean, which
//!    is a division; to 0.005 dB for the power mean, which is what the spec's
//!    own two-decimal figure can carry.
//! 2. **The fixture**, `floorstander_null_one_of_five`, which is the case the
//!    literal exists to describe. Asserted against the closed form at the
//!    REALIZED null depth, because the depth that reaches `per_position_db` has
//!    been through the gate, the log resample and the room path's smoothing —
//!    so pinning the realized depth to −30.000 dB would be pinning the
//!    smoothing profile, not the averaging.

mod common;

use common::generator::{NULL_DEPTH_DB, NULL_FC_HZ, NULL_POSITION};
use common::golden::GoldenCase;
use paraeq_decide::decide;
use paraeq_dsp::fr::{align_spl, average_measurements, average_measurements_rms};

/// The frequency axis the literal is evaluated on.
///
/// Two bins, and the second one is load-bearing: `align_spl` runs BEFORE any
/// spatial average and would otherwise remove the very level difference being
/// measured. 80 Hz is the null; 1 kHz sits inside the (500, 2000) alignment
/// band and is identical across positions, so the alignment is a no-op and the
/// null survives to the average — which is exactly the geometry of the fixture
/// this pin describes.
const PIN_FREQS: [f64; 2] = [80.0, 1000.0];
const ALIGN_BAND: (f64, f64) = (500.0, 2000.0);

/// `10·log10( (N − 1 + 10^(depth/10)) / N )` — the power average of `N`
/// positions with one of them nulled by `depth` dB, the closed form the spec's
/// own "null-RESISTANT, not null-immune" note states.
fn power_average_db(positions: usize, depth_db: f64) -> f64 {
    let n = positions as f64;
    10.0 * (((n - 1.0) + 10.0_f64.powf(depth_db / 10.0)) / n).log10()
}

/// One null of `depth_db` among `positions`, run through both averaging modes
/// the way the pipeline runs them: align first, then average.
fn both_averages(positions: usize, depth_db: f64) -> (f64, f64) {
    let curves: Vec<Vec<f64>> = (0..positions)
        .map(|index| {
            if index == positions - 1 {
                vec![depth_db, 0.0]
            } else {
                vec![0.0, 0.0]
            }
        })
        .collect();
    let aligned = align_spl(&curves, &PIN_FREQS, ALIGN_BAND).expect("a well-formed aligned set");
    let db_mean = average_measurements(aligned.measurements_db()).expect("a dB-domain mean");
    let power = average_measurements_rms(&aligned);
    (db_mean[0], power[0])
}

#[test]
fn a_minus_30_db_null_at_one_of_five_positions_is_minus_6_0_by_db_mean_and_minus_0_97_by_power() {
    // ---- 1. The literal, in the spec's own configuration. ----
    let (db_mean, power) = both_averages(5, -30.0);
    assert!(
        (db_mean - -6.0).abs() < 1e-12,
        "dB mean of one −30 dB null in five: expected −6.0, got {db_mean}"
    );
    assert!(
        (power - -0.97).abs() < 5e-3,
        "power average of one −30 dB null in five: expected −0.97, got {power}"
    );
    // The same number from the closed form the spec states in prose, so the
    // literal and the formula are pinned against each other rather than each
    // being trusted on its own.
    assert!(
        (power - power_average_db(5, -30.0)).abs() < 1e-12,
        "`average_measurements_rms` must agree with 10·log10(((N−1) + 10^(d/10))/N)"
    );

    // The two companions the spec states in the same breath, so that "fixing"
    // the numbers by quietly changing N or the depth fails here too.
    let (db_mean_six, power_six) = both_averages(6, -30.0);
    assert!(
        (db_mean_six - -5.0).abs() < 1e-12,
        "dB mean of one −30 dB null in six: expected −5.0, got {db_mean_six}"
    );
    assert!(
        (power_six - -0.79).abs() < 5e-3,
        "power average of one −30 dB null in six: expected −0.79, got {power_six}"
    );
    let (db_mean_three, power_three) = both_averages(3, -40.0);
    assert!(
        (db_mean_three - -13.3).abs() < 5e-2,
        "dB mean of one −40 dB null in three: expected −13.3, got {db_mean_three}"
    );
    assert!(
        (power_three - -1.76).abs() < 5e-3,
        "power average of one −40 dB null in three: expected −1.76, got {power_three}"
    );

    // ---- 2. The fixture the literal describes. ----
    let set = decide(&GoldenCase::load("floorstander_null_one_of_five").bundle());
    let freqs = &set.analysis.freqs_hz;
    assert_eq!(
        set.analysis.per_position_db.len(),
        5,
        "floorstander_null_one_of_five is a five-position case"
    );

    let bin = freqs
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - NULL_FC_HZ)
                .abs()
                .partial_cmp(&(*b - NULL_FC_HZ).abs())
                .expect("a log grid carries no NaN")
        })
        .map(|(index, _)| index)
        .expect("the standard log grid is not empty");

    let at_bin: Vec<f64> = set
        .analysis
        .per_position_db
        .iter()
        .map(|curve| curve[bin])
        .collect();
    let clean: Vec<f64> = at_bin
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != NULL_POSITION)
        .map(|(_, value)| *value)
        .collect();
    let reference = clean.iter().sum::<f64>() / clean.len() as f64;
    let realized_depth = at_bin[NULL_POSITION] - reference;

    // The null is really in the fixture, and it is the deepest thing at this
    // bin by a wide margin. The realized depth is shallower than the −30 dB the
    // biquad applies because the room path smooths in the bass; asserting the
    // biquad's number here would pin the smoothing profile instead.
    assert!(
        realized_depth < 0.5 * NULL_DEPTH_DB,
        "floorstander_null_one_of_five: position {NULL_POSITION} is only {realized_depth:.2} dB \
         below the other four at {:.1} Hz — the fixture no longer carries a null",
        freqs[bin]
    );

    // And the two averaging modes diverge on it the way the pin says they must.
    //
    // NOT against the closed form at the realized depth: the four surviving
    // positions are four seats in a room and they scatter by several dB at
    // 80 Hz, which the power average — being convex — reads as extra energy.
    // The closed form assumes the survivors are identical, so pinning against
    // it here would be pinning the fixture's own modal scatter. What the pin is
    // actually about is the DIVERGENCE, and the honest way to assert it is to
    // measure the same bin twice: once as captured, and once with the null
    // filled in.
    let db_mean_and_power = |values: &[f64]| {
        let curves: Vec<Vec<f64>> = values.iter().map(|v| vec![v - reference, 0.0]).collect();
        let aligned = align_spl(&curves, &PIN_FREQS, ALIGN_BAND).expect("a well-formed set");
        (
            average_measurements(aligned.measurements_db()).expect("a dB mean")[0],
            average_measurements_rms(&aligned)[0],
        )
    };

    let (with_null_db, with_null_power) = db_mean_and_power(&at_bin);
    let mut filled = at_bin.clone();
    filled[NULL_POSITION] = reference;
    let (filled_db, filled_power) = db_mean_and_power(&filled);

    // The dB mean is the average of the five values, so with the four
    // survivors summing to zero by construction it IS the depth over five.
    assert!(
        (with_null_db - realized_depth / 5.0).abs() < 1e-9,
        "the dB mean of five positions with one {realized_depth:.2} dB down must be that \
         depth over five, got {with_null_db}"
    );

    // With the null present the two modes disagree by several dB. Without it
    // they agree to within the ordinary scatter. That gap IS the pin: an
    // implementation that quietly used the dB mean on the room path would put
    // the whole of `with_null_db` into the correction as boost.
    let divergence = with_null_power - with_null_db;
    let scatter_divergence = filled_power - filled_db;
    assert!(
        divergence > 3.0,
        "the null must drive the two averaging modes apart: dB mean {with_null_db:.2}, \
         power {with_null_power:.2}"
    );
    assert!(
        scatter_divergence < 1.0,
        "with the null filled in, the two modes must agree to within ordinary seat \
         scatter: dB mean {filled_db:.2}, power {filled_power:.2}"
    );
    assert!(
        divergence > 3.0 * scatter_divergence,
        "the divergence must be caused by the null and not by the room: {divergence:.2} dB \
         with it against {scatter_divergence:.2} dB without it"
    );
    assert!(
        with_null_power > filled_power - 1.5,
        "the power average is null-RESISTANT: one nulled seat of five must barely move it, \
         but it went from {filled_power:.2} to {with_null_power:.2}"
    );
}
