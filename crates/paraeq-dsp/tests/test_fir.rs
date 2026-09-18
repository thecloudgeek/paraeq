mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::fir::{design_fir_correction, minimum_phase_spectrum, FirPhase};
use paraeq_dsp::Complex;

fn run_case(name: &str, phase: FirPhase, atol: f64) {
    let c = Case::load("fir", name);
    let taps = design_fir_correction(
        &c.array("correction_db"),
        c.param_u64("n_taps") as usize,
        phase,
    );
    assert_allclose(&taps, &c.array("taps"), 0.0, atol, name);
}

#[test]
fn linear_phase_matches_oracle() {
    run_case("linear_512", FirPhase::Linear, 1e-9);
    run_case("linear_4096", FirPhase::Linear, 1e-9);
}

#[test]
fn minimum_phase_taps_match_oracle() {
    // log/exp cepstrum amplifies FFT rounding: 1e-6 abs per spec
    run_case("minimum_512", FirPhase::Minimum, 1e-6);
    run_case("minimum_4096", FirPhase::Minimum, 1e-6);
}

/// The perceptually true criterion (spec): the min-phase filter's magnitude
/// response must match the oracle taps' response within 0.01 dB, 20 Hz-20 kHz.
#[test]
fn minimum_phase_response_within_001_db() {
    for name in ["minimum_512", "minimum_4096"] {
        let c = Case::load("fir", name);
        let ours = design_fir_correction(
            &c.array("correction_db"),
            c.param_u64("n_taps") as usize,
            FirPhase::Minimum,
        );
        let theirs = c.array("taps");
        let sr = 48000.0;
        for k in 0..200 {
            // 200-point log grid, 20 Hz .. 20 kHz
            let f = 20.0 * (20000.0f64 / 20.0).powf(k as f64 / 199.0);
            let a = fir_mag_db(&ours, f, sr);
            let b = fir_mag_db(&theirs, f, sr);
            assert!(
                (a - b).abs() < 0.01,
                "{name} @ {f:.1} Hz: {a:.5} vs {b:.5} dB"
            );
        }
    }
}

fn fir_mag_db(taps: &[f64], f: f64, sr: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f / sr;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, t) in taps.iter().enumerate() {
        re += t * (w * n as f64).cos();
        im -= t * (w * n as f64).sin();
    }
    20.0 * (re * re + im * im).sqrt().max(1e-30).log10()
}

// ---------------------------------------------------------------- Tier 2
// `minimum_phase_spectrum` — scipy.signal.minimum_phase(h, "homomorphic",
// n_fft, half=False), followed by rfft. Fixture: fixtures/fir/min_phase_spectrum.

#[test]
fn minimum_phase_spectrum_matches_the_scipy_half_false_fixture() {
    // The delegate's own arguments — taps in, n_fft, homomorphic, half=False —
    // so the fixture reaches the whole call, including the truncation back to
    // the input order that scipy performs before returning.
    let c = Case::load("fir", "min_phase_spectrum");
    let n_fft = c.param_u64("n_fft") as usize;
    //
    // 1e-12, not the tier table's 1e-9: the cepstrum's log/exp round trip does
    // amplify FFT rounding (which is why the frozen min-phase TAPS above sit at
    // 1e-6), but against the SPECTRUM the measured worst bin here is 1.2e-14
    // absolute on a spectrum peaking at 36.1. Grading at 1e-9 would leave four
    // orders of slack for a real regression to hide in.
    let spectrum = minimum_phase_spectrum(&c.array("h"), n_fft).unwrap();
    let re: Vec<f64> = spectrum.iter().map(|z| z.re).collect();
    let im: Vec<f64> = spectrum.iter().map(|z| z.im).collect();
    assert_allclose(&re, &c.array("spectrum_re"), 1e-12, 1e-12, "min-phase re");
    assert_allclose(&im, &c.array("spectrum_im"), 1e-12, 1e-12, "min-phase im");
}

// ---------------------------------------------------------------- Tier 3

#[test]
fn minimum_phase_spectrum_preserves_the_input_magnitude() {
    // half=False's defining property, and the cheapest falsifier for shipping
    // the half=True port by mistake: the reconstruction has the SAME magnitude
    // response as its input, not the square root of it. Under half=True this
    // assertion misses by |H|^(1/2) — orders of magnitude, not a tolerance.
    //
    // The residual is the homomorphic chain's `+ 1e-7 · min(|H|)` guard against
    // log(0), plus scipy's truncation back to the input order: measured 7.9e-6
    // absolute on a spectrum peaking at 36.1, budgeted at 5e-5 for ~6× headroom.
    // Discrimination is not close — half=True would report √36.1 ≈ 6.0 where
    // this reports 36.1.
    let c = Case::load("fir", "min_phase_spectrum");
    let n_fft = c.param_u64("n_fft") as usize;
    let h = c.array("h");
    let mp_mag: Vec<f64> = minimum_phase_spectrum(&h, n_fft)
        .unwrap()
        .iter()
        .map(|z| z.norm())
        .collect();
    let h_mag: Vec<f64> = rfft_naive(&h, n_fft).iter().map(|z| z.norm()).collect();
    assert_allclose(&mp_mag, &h_mag, 0.0, 5e-5, "min-phase magnitude");
}

#[test]
fn minimum_phase_spectrum_is_twice_the_phase_of_the_half_true_port() {
    // THE trap this function exists to avoid, pinned as the factor of two.
    //
    // `fir.rs`'s private `minimum_phase_homomorphic` is the half=True call: it
    // halves the log magnitude (`* 0.5` at fir.rs:148), so its output's
    // magnitude is √|H| and its phase is HALF the minimum phase of |H|. The old
    // plan's "just make it pub" would therefore have shipped a factor-of-two
    // error into every excess-group-delay number — plausible-looking and
    // invisible unless a test states the factor.
    //
    // `design_fir_correction(.., FirPhase::Minimum)` is the only PUBLIC reach
    // into that private function. It feeds it a prototype whose magnitude is the
    // target SQUARED (fir.rs:48), so the two halvings cancel and its output is
    // full minimum phase — which is why the shipped FIR path is correct while a
    // direct reach into the same function would not be. Reconstructing that
    // prototype here lets the relationship be asserted end to end: the half=True
    // port's phase on `proto` must be exactly HALF `minimum_phase_spectrum`'s
    // on the same `proto`.
    const N_TAPS: usize = 64;
    const N_PROTO: usize = 2 * N_TAPS - 1; // 127, odd — fir.rs's own choice
    const N_FFT: usize = 512; // 1 << ceil(log2(4 * N_PROTO)), fir.rs:50

    // A smooth, strictly positive correction curve on exactly `bins` points, so
    // `fir.rs`'s positional resample is the identity and `proto` below is
    // reproducible from public information alone.
    let bins = N_PROTO / 2 + 1;
    assert_eq!(bins, N_TAPS);
    let correction_db: Vec<f64> = (0..bins)
        .map(|i| 12.0 * (2.0 * std::f64::consts::PI * i as f64 / bins as f64).sin() - 3.0)
        .collect();

    let shipped = design_fir_correction(&correction_db, N_TAPS, FirPhase::Minimum);
    let proto = fir_prototype(&correction_db, N_PROTO);

    let phase_half_true = unwrapped_phase(&rfft_naive(&shipped, N_FFT));
    let phase_half_false = unwrapped_phase(&minimum_phase_spectrum(&proto, N_FFT).unwrap());

    // Both reconstructions are truncated (to N_TAPS and to N_PROTO taps
    // respectively), and the stopband of a 127-tap prototype sits under the
    // 1e-7 log guard where the recovered phase is guard noise. Assert over the
    // passband half of the axis, which is where the claim has content.
    let hi = N_FFT / 4;
    let mut worst = 0.0f64;
    for k in 1..hi {
        worst = worst.max((phase_half_false[k] - 2.0 * phase_half_true[k]).abs());
    }
    assert!(
        worst < 2e-2,
        "the half=True port must carry HALF the phase; worst deviation {worst:e} rad"
    );

    // ...and the halving is real, not a tolerance artifact: the two curves must
    // be far apart, or an implementation that simply exposed the private
    // half=True function would satisfy the assertion above.
    let gap = (1..hi)
        .map(|k| (phase_half_false[k] - phase_half_true[k]).abs())
        .fold(0.0f64, f64::max);
    assert!(
        gap > 1.0,
        "the factor of two must be observable; gap {gap} rad"
    );
}

#[test]
fn minimum_phase_spectrum_rejects_degenerate_arguments() {
    // scipy raises on both of its own preconditions — "h must be 1-D and at
    // least 2 samples long" and "n_fft must be at least len(h)" — so these are
    // errors rather than a silently padded or empty spectrum. Non-finite taps
    // are ours: they would sail through the cepstrum as NaN and surface as a
    // NaN EGD trace several stages downstream.
    assert!(minimum_phase_spectrum(&[], 64).is_err());
    assert!(minimum_phase_spectrum(&[1.0, 0.5], 64).is_err());
    assert!(minimum_phase_spectrum(&[1.0, 0.5, 0.25], 2).is_err());
    assert!(minimum_phase_spectrum(&[1.0, 0.5, 0.25], 0).is_err());
    assert!(minimum_phase_spectrum(&[1.0, f64::NAN, 0.25], 64).is_err());
    assert!(minimum_phase_spectrum(&[0.0, 0.0, 0.0], 64).is_err());
}

/// The minimum-phase prototype `design_fir_correction` builds internally
/// (fir.rs:36-49): resample the linear magnitude onto `n/2 + 1` bins (the
/// identity here, by construction of the caller), SQUARE it, then irfft / roll
/// / Hann exactly as `windowed_ir(.., normalize = false)` does. Rebuilt here
/// from public information rather than exposed, because exposing it would make
/// a Tier-1 fixture-pinned path part of this crate's API surface.
fn fir_prototype(correction_db: &[f64], n: usize) -> Vec<f64> {
    let squared: Vec<f64> = correction_db
        .iter()
        .map(|db| {
            let m = 10f64.powf(db / 20.0);
            m * m
        })
        .collect();
    // irfft of a purely real half-spectrum, `n` odd: there is no Nyquist bin to
    // halve, so every k >= 1 is doubled.
    let mut ir: Vec<f64> = (0..n)
        .map(|m| {
            let mut acc = squared[0];
            for (k, x) in squared.iter().enumerate().skip(1) {
                acc += 2.0 * x * (2.0 * std::f64::consts::PI * (k * m) as f64 / n as f64).cos();
            }
            acc / n as f64
        })
        .collect();
    ir.rotate_right(n / 2);
    for (i, v) in ir.iter_mut().enumerate() {
        *v *= 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
    }
    ir
}

/// `np.fft.rfft(x, n)` by direct summation — O(n²), dependency-free, and exact
/// to float noise at the sizes these tests use.
fn rfft_naive(x: &[f64], n: usize) -> Vec<Complex<f64>> {
    (0..n / 2 + 1)
        .map(|k| {
            let mut acc = Complex::new(0.0, 0.0);
            for (i, v) in x.iter().enumerate().take(n) {
                let ph = -2.0 * std::f64::consts::PI * ((k * i) % n) as f64 / n as f64;
                acc += Complex::new(v * ph.cos(), v * ph.sin());
            }
            acc
        })
        .collect()
}

/// `np.unwrap(np.angle(spectrum))` — the running 2π correction is accumulated
/// against the RAW differences, as numpy does, not against already-corrected
/// samples.
fn unwrapped_phase(spectrum: &[Complex<f64>]) -> Vec<f64> {
    let two_pi = 2.0 * std::f64::consts::PI;
    let raw: Vec<f64> = spectrum.iter().map(|z| z.im.atan2(z.re)).collect();
    let mut out = raw.clone();
    let mut cumulative = 0.0;
    for i in 1..raw.len() {
        let dd = raw[i] - raw[i - 1];
        let wrapped = (dd + std::f64::consts::PI).rem_euclid(two_pi) - std::f64::consts::PI;
        cumulative += wrapped - dd;
        out[i] = raw[i] + cumulative;
    }
    out
}
