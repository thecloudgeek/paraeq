mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::sweep::{apply_fade, generate_inverse_sweep, generate_sweep};

/// Policy fade at 48 kHz (measurement-safety spec): 10 ms in, 50 ms out.
const FADE_IN: usize = 480;
const FADE_OUT: usize = 2400;

/// The fixture's sweep: 0.25 s / 48 kHz / 20 Hz–20 kHz, 12000 samples.
fn measurement_sweep() -> Vec<f64> {
    generate_sweep(0.25, 48_000, 20.0, 20_000.0)
}

/// The envelope itself, recovered by fading a buffer of ones.
fn envelope(n: usize, fade_in: usize, fade_out: usize) -> Vec<f64> {
    let mut w = vec![1.0; n];
    apply_fade(&mut w, fade_in, fade_out);
    w
}

fn mean(x: &[f64]) -> f64 {
    x.iter().sum::<f64>() / x.len() as f64
}

#[test]
fn sweep_matches_oracle() {
    let c = Case::load("sweep", "basic");
    let sweep = generate_sweep(
        c.param_f64("duration"),
        c.param_u64("sample_rate") as u32,
        c.param_f64("f_start"),
        c.param_f64("f_end"),
    );
    assert_allclose(&sweep, &c.array("sweep"), 1e-12, 1e-12, "sweep");
}

#[test]
fn inverse_sweep_matches_oracle() {
    let c = Case::load("sweep", "basic");
    let sweep = c.array("sweep");
    let inv = generate_inverse_sweep(
        &sweep,
        c.param_u64("sample_rate") as u32,
        c.param_f64("f_start"),
        c.param_f64("f_end"),
    );
    assert_allclose(&inv, &c.array("inverse"), 1e-12, 1e-12, "inverse sweep");
}

/// apply_fade is additive: the fixture above pins generate_sweep, and fading is a
/// separate step the caller applies afterward. This pins the seam itself.
#[test]
fn fade_does_not_touch_generate_sweep() {
    let c = Case::load("sweep", "basic");
    let mut faded = generate_sweep(
        c.param_f64("duration"),
        c.param_u64("sample_rate") as u32,
        c.param_f64("f_start"),
        c.param_f64("f_end"),
    );
    apply_fade(&mut faded, FADE_IN, FADE_OUT);
    assert_allclose(
        &generate_sweep(0.25, 48_000, 20.0, 20_000.0),
        &c.array("sweep"),
        1e-12,
        1e-12,
        "generate_sweep after a fade elsewhere",
    );
}

#[test]
fn fade_kills_the_terminal_click() {
    // The defect apply_fade exists for: the un-faded 0.25 s sweep terminates at
    // -0.9146 — a -0.77 dBFS step, and it lands near 20 kHz where tweeters are
    // most fragile and a sealed IEM is most startling.
    let mut x = measurement_sweep();
    assert!(
        (x[11_999] + 0.914_646_802_317_9).abs() < 1e-9,
        "un-faded terminal sample {}",
        x[11_999]
    );
    apply_fade(&mut x, FADE_IN, FADE_OUT);
    assert_eq!(x[0], 0.0, "first sample must be exactly 0.0");
    assert_eq!(x[11_999], 0.0, "last sample must be exactly 0.0");
}

#[test]
fn fade_preserves_length_and_leaves_a_unity_plateau() {
    let n = 12_000;
    let w = envelope(n, FADE_IN, FADE_OUT);
    assert_eq!(w.len(), n, "length preserved");
    // Exactly FADE_IN + FADE_OUT samples are touched; the plateau is bit-exact unity.
    assert!(
        w[FADE_IN..n - FADE_OUT].iter().all(|&v| v == 1.0),
        "plateau must be exactly 1.0"
    );
    assert!(
        w[FADE_IN - 1] < 1.0 && w[n - FADE_OUT] < 1.0,
        "the ramps' innermost samples sit just under unity"
    );
}

#[test]
fn fade_envelope_is_monotone_in_and_out() {
    let n = 12_000;
    let w = envelope(n, FADE_IN, FADE_OUT);
    assert!(
        w[..=FADE_IN].windows(2).all(|p| p[1] >= p[0]),
        "fade-in envelope must be non-decreasing"
    );
    assert!(
        w[n - FADE_OUT - 1..].windows(2).all(|p| p[1] <= p[0]),
        "fade-out envelope must be non-increasing"
    );
}

#[test]
fn fade_is_a_raised_cosine_half_hann() {
    // w(i) = ½(1 − cos(π·i/N)), pinned at its analytic quarter points. These are
    // what separate it from any other ramp through the same midpoint: a linear
    // ramp yields 0.25/0.5/0.75 here yet satisfies every other invariant in this
    // file, including the symmetry below.
    let (n, fade) = (2_001, 1_000);
    let w = envelope(n, fade, 0);
    let pinned = [
        (fade / 4, (2.0 - std::f64::consts::SQRT_2) / 4.0), // ½(1 − cos π/4) = 0.1464466094067262
        (fade / 2, 0.5),                                    // ½(1 − cos π/2)
        (3 * fade / 4, (2.0 + std::f64::consts::SQRT_2) / 4.0), // ½(1 − cos 3π/4) = 0.8535533905932737
    ];
    for (i, expected) in pinned {
        assert!(
            (w[i] - expected).abs() < 1e-12,
            "w[{i}] = {} want {expected}",
            w[i]
        );
    }
    // The slope vanishes at the endpoint — the C¹ join that makes the fade
    // click-free, and the reason the spec calls for a raised cosine over a linear
    // ramp: w(1) is O(1/N²) (2.47e-6 here) where linear would give 1/N = 1e-3.
    assert!(
        w[1] < 1e-5,
        "fade-in must leave the endpoint with vanishing slope: {}",
        w[1]
    );
    // w(i) + w(N−i) = 1.
    for i in 0..=fade {
        assert!(
            (w[i] + w[fade - i] - 1.0).abs() < 1e-12,
            "half-Hann symmetry at {i}"
        );
    }
}

#[test]
fn fade_out_is_the_mirror_of_fade_in() {
    let n = 500;
    let head = envelope(n, 128, 0);
    let tail = envelope(n, 0, 128);
    for i in 0..n {
        assert_eq!(head[i], tail[n - 1 - i], "mirror at {i}");
    }
}

#[test]
fn fade_never_amplifies() {
    // Envelope ⊂ [0,1]. A safety function must never make a sample louder.
    let clean = measurement_sweep();
    let mut faded = clean.clone();
    apply_fade(&mut faded, FADE_IN, FADE_OUT);
    for (f, c) in faded.iter().zip(&clean) {
        assert!(f.abs() <= c.abs(), "fade amplified {c} to {f}");
    }
}

#[test]
fn fading_twice_attenuates_but_never_clicks() {
    // Not idempotent — the envelope squares — but re-fading must stay safe: same
    // length, endpoints still exactly zero, never louder than the first pass.
    let mut once = measurement_sweep();
    apply_fade(&mut once, FADE_IN, FADE_OUT);
    let mut twice = once.clone();
    apply_fade(&mut twice, FADE_IN, FADE_OUT);
    assert_eq!(twice.len(), once.len());
    assert_eq!(twice[0], 0.0);
    assert_eq!(twice[twice.len() - 1], 0.0);
    for (t, o) in twice.iter().zip(&once) {
        assert!(t.abs() <= o.abs(), "re-fading amplified {o} to {t}");
    }
}

#[test]
fn zero_length_fades_are_a_bit_exact_noop() {
    // N = 0 must never reach the π·i/N division: 0/0 would poison the buffer with NaN.
    let clean = measurement_sweep();
    let mut x = clean.clone();
    apply_fade(&mut x, 0, 0);
    assert_eq!(x, clean);
}

#[test]
fn fade_handles_degenerate_buffers() {
    let mut empty: Vec<f64> = Vec::new();
    apply_fade(&mut empty, FADE_IN, FADE_OUT);
    assert!(empty.is_empty());

    let mut single = vec![1.0];
    apply_fade(&mut single, 1, 1);
    assert_eq!(single[0], 0.0);

    // Fades longer than the buffer: the ramps simply never reach unity.
    let w = envelope(100, 1_000, 1_000);
    assert!(w.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
    assert_eq!(w[0], 0.0);
    assert_eq!(w[99], 0.0);
}

#[test]
fn overlapping_fades_stay_within_both_ramps() {
    // fade_in + fade_out > len: the ramps multiply, so the result is never louder
    // than either ramp alone and both endpoints are still exactly zero.
    let (n, fade) = (100, 80);
    let w = envelope(n, fade, fade);
    let head = envelope(n, fade, 0);
    let tail = envelope(n, 0, fade);
    assert_eq!(w[0], 0.0);
    assert_eq!(w[n - 1], 0.0);
    for i in 0..n {
        assert!(
            w[i] <= head[i] && w[i] <= tail[i],
            "overlap exceeded a ramp at {i}"
        );
        assert!(
            (w[i] - head[i] * tail[i]).abs() < 1e-15,
            "overlap must be the product of the ramps at {i}"
        );
    }
}

#[test]
fn fade_ramps_a_dc_offset_to_zero() {
    // Why the fade-in exists at all (spec § Fade and DC): the sweep's own start is
    // continuous (phase(0) = 0), but a DC-offset or partial buffer steps at both
    // edges without it.
    let n = 4_800;
    let mut x = vec![0.5; n];
    apply_fade(&mut x, FADE_IN, FADE_OUT);
    assert_eq!(x[0], 0.0);
    assert_eq!(x[n - 1], 0.0);
    assert!(
        x[FADE_IN..n - FADE_OUT].iter().all(|&v| v == 0.5),
        "the plateau's offset must survive — apply_fade shapes edges, it does not block DC"
    );
}

/// apply_fade shapes edges; it is not a DC blocker, and the sweep's DC is not an
/// edge artefact but the LF stationary-phase residue, so the fade barely moves it
/// (5.0 s / 20 Hz: 1.59e-3 un-faded -> 1.24e-3 faded).
///
/// Consequence for MS-4/paraeq-measure, recorded here rather than papered over:
/// the spec's post-fade `|mean(x)| < 1e-4` gate is NOT reachable by fading. Only a
/// fade-in of ~100 ms brings this sweep under it (measured: -7.6e-5), and the same
/// spec section rejects a long fade-in for costing real LF energy. Resolve the gate
/// in the stimulus assembly (or restate its threshold) — do not inflate this fade.
#[test]
fn fade_is_not_a_dc_blocker() {
    let mut x = generate_sweep(5.0, 48_000, 20.0, 20_000.0);
    let before = mean(&x);
    apply_fade(&mut x, FADE_IN, FADE_OUT);
    let after = mean(&x);
    assert!(
        after.abs() > 0.5 * before.abs(),
        "fade must not be mistaken for a DC blocker: {before} -> {after}"
    );
    assert!(
        after.abs() > 1e-4,
        "spec gap closed upstream? re-read this test's doc comment: DC {after}"
    );
}
