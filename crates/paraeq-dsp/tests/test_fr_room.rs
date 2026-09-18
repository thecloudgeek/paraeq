//! `fr.rs` room-path tests — Tier 2 (numpy/scipy-direct fixtures) + Tier 3
//! (analytic physics). Spec: docs/specs/2026-07-15-room-dsp-design.md,
//! "`fr.rs` — rework (strictly additive)".
//!
//! Separate from `test_fr.rs` on purpose: that file is Tier 1 (frozen prototype
//! fixtures) and `averaging_is_in_db_domain` is the contract that stops a future
//! session "unifying" the two averaging paths. It stays untouched.

mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::fr::{self, Smoothing};
use paraeq_dsp::logf::LogGrid;
use paraeq_dsp::Complex;

/// A bin at 60 Hz to null, and four bins inside the 200–2000 Hz alignment band.
const FREQS: [f64; 9] = [
    20.0, 60.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0,
];
/// 60 Hz — deliberately OUTSIDE the alignment band, so `align_spl` cannot move
/// it and the power average is observed uncontaminated.
const NULL_BIN: usize = 1;
const BAND: (f64, f64) = (200.0, 2000.0);

/// `n` curves, flat at 0 dB, with `k` of them nulled by `d` dB at 60 Hz only.
/// Flat inside the alignment band ⇒ every band mean is 0 ⇒ every offset is 0 ⇒
/// `align_spl` is a no-op here by construction.
fn nulled_set(n: usize, k: usize, d: f64) -> Vec<Vec<f64>> {
    (0..n)
        .map(|j| {
            let mut m = vec![0.0; FREQS.len()];
            if j < k {
                m[NULL_BIN] = d;
            }
            m
        })
        .collect()
}

fn power_avg_at_null(n: usize, k: usize, d: f64) -> f64 {
    let set = fr::align_spl(&nulled_set(n, k, d), &FREQS, BAND).unwrap();
    fr::average_measurements_rms(&set)[NULL_BIN]
}

/// The EXACT formula, not the refuted "null-immune" one:
/// `P_dB = 10·log10( ((N−k) + k·10^(d/10)) / N )`.
fn expected_db(n: usize, k: usize, d: f64) -> f64 {
    10.0 * ((((n - k) as f64) + (k as f64) * 10f64.powf(d / 10.0)) / n as f64).log10()
}

/// SplitMix64 — a tiny deterministic PRNG so the property sweep needs no dev
/// dependency and no time-derived seed. The seed is a FIXED literal; the run is
/// bit-reproducible forever.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Acklam's rational approximation to the standard normal inverse CDF
/// (|relative error| < 1.15e-9 — far below the 1% budget it feeds).
fn inv_norm_cdf(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.383_577_518_672_69e2,
        -3.066479806614716e1,
        2.506628277459239e0,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e1,
        1.615858368580409e2,
        -1.556989798598866e2,
        6.680131188771972e1,
        -1.328068155288572e1,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838e0,
        -2.549732539343734e0,
        4.374664141464968e0,
        2.938163982698783e0,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-3,
        3.224671290700398e-1,
        2.445134137142996e0,
        3.754408661907416e0,
    ];
    const P_LOW: f64 = 0.02425;
    assert!(p > 0.0 && p < 1.0);
    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - P_LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

// ---------------------------------------------------------------- Tier 2

#[test]
fn rms_average_and_sigma_match_numpy_fixture() {
    let c = Case::load("fr", "rms_average");
    let meas = c.array2("measurements");
    let freqs = c.array("freqs");
    let rows: Vec<Vec<f64>> = (0..meas.rows)
        .map(|r| meas.data[r * meas.cols..(r + 1) * meas.cols].to_vec())
        .collect();
    // (200.0, 2000.0) == the fixture's params.band_hz. The generator
    // pre-aligned the curves in this band, so align_spl must be a no-op here
    // (offsets at float-noise level) and the expected outputs are numpy's
    // alone — which is what keeps the tier honest.
    let set = fr::align_spl(&rows, &freqs, (200.0, 2000.0)).unwrap();
    for (j, o) in set.offsets_db().iter().enumerate() {
        assert!(o.abs() < 1e-12, "offset[{j}] = {o}, expected float noise");
    }
    assert_allclose(
        &fr::average_measurements_rms(&set),
        &c.array("rms_db"),
        1e-12,
        1e-12,
        "rms_db",
    );
    assert_allclose(
        &fr::sigma_db(&set),
        &c.array("sigma_db"),
        1e-12,
        1e-12,
        "sigma_db",
    );
}

#[test]
fn am_gaussian_tracks_scipy_within_method_accuracy() {
    // METHOD-ACCURACY test, NOT parity: the Alvarez–Mazorra recursion is by
    // construction an approximation to a true Gaussian, so the budget is what
    // the method can do, not float noise.
    //
    // The spec asks for 1e-3 dB here; that is UNATTAINABLE for AM at any K,
    // not just K = 4. The K-pass composite transfer function is
    // (1 + 2λ(1−cos ω))^−K → exp(−q²(1−cos ω)) as K → ∞, while scipy's
    // sampled Gaussian is exp(−σ²ω²/2); the (1−cos ω) vs ω²/2 gap is a
    // K-independent discretization floor of the second-difference operator.
    // At K = 4 the worst transfer-function deviation is ~3.2% (σ = 8),
    // measured on these fixtures as 0.023 / 0.047 / 0.105 dB for
    // σ = 2 / 8 / 32. Budgets pin measured + ~25% headroom, so a regression
    // still fails: dropping Getreuer's q-correction reads 0.045 dB at σ = 2,
    // over its 0.03 budget — the σ = 2 case is that correction's tripwire.
    let grid = LogGrid::standard();
    for (case, sigma_bins, budget_db) in [
        ("gaussian_sigma2", 2.0, 3e-2),
        ("gaussian_sigma8", 8.0, 6e-2),
        ("gaussian_sigma32", 32.0, 1.3e-1),
    ] {
        let c = Case::load("fr", case);
        // The fixture axis is the standard grid itself (bit-identical per
        // test_logf.rs); σ is in BINS on the uniform octave axis, where
        // constant σ IS constant-Q. Invert fr.rs's own (unpinned)
        // fraction ↔ σ mapping to request exactly this σ.
        assert_allclose(grid.freqs(), &c.array("freqs"), 1e-15, 0.0, case);
        let fraction =
            sigma_bins * fr::GAUSSIAN_FWHM_PER_SIGMA / f64::from(grid.points_per_octave());
        let out = fr::smooth(&c.array("mag_db"), &grid, Smoothing::Gaussian { fraction }).unwrap();
        assert_allclose(&out, &c.array("smoothed"), 0.0, budget_db, case);
    }
}

#[test]
fn complex_spectrum_matches_the_numpy_rfft_fixture() {
    // Magnitude AND phase. `compute_frequency_response` pins the magnitude half
    // only; `fdw::apply_fdw` and `logf::resample_complex_to_log_grid` both need
    // the complex spectrum, so the phase half needs its own reference.
    //
    // The fixture's n_samples (1024) is deliberately shorter than its n_fft
    // (4096): np.fft.rfft zero-pads UP TO n_fft and never truncates, so this is
    // the case that exercises realfft's padding rather than the easy path.
    let c = Case::load("fr", "complex_spectrum");
    let n_fft = c.param_u64("n_fft") as usize;
    let sr = c.param_u64("sample_rate") as u32;
    // 1e-12, the `logf/resample_db` bar, not the tier table's looser 1e-9:
    // realfft's packing costs far less than that here. Measured worst bin
    // against numpy, on a spectrum peaking at 21.3: 1.1e-14 absolute (the axis
    // itself is bit-identical).
    let (freqs, spectrum) = fr::complex_spectrum(&c.array("ir"), sr, Some(n_fft)).unwrap();
    assert_allclose(&freqs, &c.array("freqs"), 1e-12, 1e-9, "rfftfreq");
    let re: Vec<f64> = spectrum.iter().map(|z| z.re).collect();
    let im: Vec<f64> = spectrum.iter().map(|z| z.im).collect();
    assert_allclose(&re, &c.array("spectrum_re"), 1e-12, 1e-12, "spectrum re");
    assert_allclose(&im, &c.array("spectrum_im"), 1e-12, 1e-12, "spectrum im");
}

#[test]
fn complex_spectrum_magnitude_equals_compute_frequency_response() {
    // The Tier-1 frozen function is the CONTRACT for the magnitude half. Bit
    // equality, not a tolerance: both go through the same realfft plan, so any
    // daylight between them means one of the two grew its own FFT path — which
    // is exactly the drift this test exists to stop.
    let c = Case::load("fr", "complex_spectrum");
    let ir = c.array("ir");
    let n_fft = c.param_u64("n_fft") as usize;
    let sr = c.param_u64("sample_rate") as u32;
    let (freqs_complex, spectrum) = fr::complex_spectrum(&ir, sr, Some(n_fft)).unwrap();
    let (freqs_mag, mag_db) = fr::compute_frequency_response(&ir, sr, Some(n_fft)).unwrap();
    assert_eq!(freqs_complex, freqs_mag, "the two axes must be identical");
    let from_complex: Vec<f64> = spectrum
        .iter()
        .map(|z| 20.0 * z.norm().max(1e-10).log10())
        .collect();
    assert_eq!(from_complex, mag_db, "magnitude must agree bit for bit");
}

#[test]
fn excess_group_delay_matches_the_scipy_reference_fixture() {
    // The stencil is the contract (see the generator's docstring): unwrap each
    // phase, difference, then np.gradient against the omega COORDINATE ARRAY on
    // the linear rfft axis. omega is not exactly uniform in float, so numpy
    // takes its non-uniform branch and the uniform central difference is NOT a
    // substitute.
    let c = Case::load("fr", "excess_group_delay");
    let n_fft = c.param_u64("n_fft") as usize;
    let sr = c.param_u64("sample_rate") as u32;
    // Measured worst bin against numpy: 1.8e-15 s absolute, against EGD values
    // of 1.7…14.7 ms. The absolute floor is set just above that rather than at
    // it, because the interesting bins are the near-zero ones where the
    // relative term buys nothing.
    let egd = fr::excess_group_delay_s(&c.array("ir"), sr, n_fft).unwrap();
    assert_allclose(&egd, &c.array("egd_s"), 1e-9, 1e-14, "egd_s");
}

#[test]
fn weighted_rms_average_and_sigma_match_the_numpy_weighted_fixture() {
    // Same construction as `rms_average_and_sigma_match_numpy_fixture` above —
    // same seed, same five pre-aligned curves — plus the weight vector
    // LowSnrSoft produces. Reusing the construction is what makes
    // `weighted_with_equal_weights_equals_the_unweighted_one` a claim about the
    // WEIGHTING rather than about two unrelated data sets.
    let c = Case::load("fr", "rms_average_weighted");
    let meas = c.array2("measurements");
    let freqs = c.array("freqs");
    let weights = c.array("weights");
    let rows: Vec<Vec<f64>> = (0..meas.rows)
        .map(|r| meas.data[r * meas.cols..(r + 1) * meas.cols].to_vec())
        .collect();
    let set = fr::align_spl(&rows, &freqs, (200.0, 2000.0)).unwrap();
    for (j, o) in set.offsets_db().iter().enumerate() {
        assert!(o.abs() < 1e-12, "offset[{j}] = {o}, expected float noise");
    }
    assert_allclose(
        &fr::average_measurements_rms_weighted(&set, &weights).unwrap(),
        &c.array("rms_db"),
        1e-12,
        1e-12,
        "weighted rms_db",
    );
    assert_allclose(
        &fr::sigma_db_weighted(&set, &weights).unwrap(),
        &c.array("sigma_db"),
        1e-12,
        1e-12,
        "weighted sigma_db",
    );
}

// ---------------------------------------------------------------- Tier 3

/// The analytic derotated spectrum of a sparse tap set, evaluated directly on
/// `freqs` — the `test_fdw.rs::exact_derotated_spectrum` oracle, restated here
/// because integration-test binaries do not share helpers. Each tap's phase is
/// referenced to `reference_samples`, so the bulk delay is gone by construction
/// rather than removed by the function under test.
fn exact_derotated_spectrum(
    freqs: &[f64],
    paths: &[(usize, f64)],
    reference_samples: f64,
    sample_rate: f64,
) -> Vec<Complex<f64>> {
    freqs
        .iter()
        .map(|&f| {
            let mut acc = Complex::new(0.0, 0.0);
            for &(d, a) in paths {
                let ph =
                    -2.0 * std::f64::consts::PI * f * (d as f64 - reference_samples) / sample_rate;
                acc += Complex::new(a * ph.cos(), a * ph.sin());
            }
            acc
        })
        .collect()
}

#[test]
fn derotate_matches_the_test_helper_oracle() {
    // logf.rs:122-124, verbatim: "The caller must derotate (remove the bulk
    // delay) BEFORE calling: at 20 kHz a 50 ms delay winds ~1000 full turns and
    // no log grid could sample it." The only implementations before this one
    // were the test helpers in test_fdw.rs; the fdw.rs precedent is that the
    // oracle is written first and the shipping path is graded against it.
    //
    // No fixture: X_k · e^{+j2πf_kτ} has no library delegate, so a numpy case
    // would pin one line of our own algebra against itself.
    const SR: f64 = 48_000.0;
    let paths = [(240usize, 1.0f64), (360, 0.5), (1200, -0.25)];
    let reference = 240.0;
    let freqs: Vec<f64> = (0..512)
        .map(|k| 20.0 * (1000.0f64).powf(k as f64 / 511.0))
        .collect();
    // The RAW spectrum still carries the bulk delay.
    let raw: Vec<Complex<f64>> = exact_derotated_spectrum(&freqs, &paths, 0.0, SR);
    let derotated = fr::derotate(&raw, &freqs, reference / SR);
    let want = exact_derotated_spectrum(&freqs, &paths, reference, SR);
    for (i, (got, exp)) in derotated.iter().zip(&want).enumerate() {
        let d = (got - exp).norm();
        assert!(
            d < 1e-12,
            "bin {i} @ {:.1} Hz: {got:?} vs {exp:?}",
            freqs[i]
        );
    }
}

#[test]
fn derotating_by_zero_is_the_identity() {
    // The falsifier for a sign error that only shows up off zero: a zero delay
    // must return the input untouched, bit for bit.
    let freqs = [20.0, 200.0, 2000.0, 20_000.0];
    let spectrum: Vec<Complex<f64>> = vec![
        Complex::new(1.0, 0.0),
        Complex::new(0.0, -1.0),
        Complex::new(-0.5, 0.25),
        Complex::new(3.0, 4.0),
    ];
    assert_eq!(fr::derotate(&spectrum, &freqs, 0.0), spectrum);
}

#[test]
fn egd_is_zero_for_a_minimum_phase_ir() {
    // h = δ(t₀) + 0.5·δ(t₀+τ). |g| < 1 puts both zeros INSIDE the unit circle,
    // so H is already minimum-phase, the reconstruction returns it, and the
    // excess is identically zero.
    //
    // Budget: the homomorphic chain's `+ 1e-7 · min(|H|)` guard against log(0)
    // perturbs the reconstruction slightly, so "zero" is float-plus-epsilon
    // rather than exact. Measured on this IR: 4.0e-10 s worst bin. 1e-8 s is
    // that with ~25× headroom and is still four orders below the 5 ms the
    // non-minimum-phase case below reports — a half=True regression (which
    // halves the phase, not the excess) would not hide here, but a sign error
    // in the phase difference would.
    const SR: u32 = 48_000;
    let mut ir = vec![0.0; 2048];
    ir[0] = 1.0;
    ir[240] = 0.5;
    let egd = fr::excess_group_delay_s(&ir, SR, 8192).unwrap();
    let worst = egd.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    assert!(
        worst < 1e-8,
        "minimum-phase EGD should vanish, worst {worst:e} s"
    );
}

#[test]
fn egd_mean_is_the_excess_delay_for_a_non_minimum_phase_two_path_ir() {
    // h = δ(t₀) + 2·δ(t₀+τ). |g| > 1 reflects the zeros OUTSIDE the unit
    // circle, so the excess is an all-pass. Its group delay has mean exactly τ,
    // ripples with period 1/τ in frequency, and spans
    // [τ(1−|a|)/(1+|a|), τ(1+|a|)/(1−|a|)] with a = 1/g = 0.5.
    //
    // τ = 240 samples at 48 kHz = 5 ms ⇒ mean 5 ms, ripple period 200 Hz,
    // range 1.67 … 15 ms. The upper end is approached, not attained: the
    // all-pass peak is narrow and the 8192-point rfft axis (5.86 Hz spacing)
    // does not land on it, so the measured max is 14.67 ms.
    const SR: u32 = 48_000;
    const TAU_S: f64 = 240.0 / 48_000.0;
    let mut ir = vec![0.0; 2048];
    ir[0] = 1.0;
    ir[240] = 2.0;
    let n_fft = 8192;
    let egd = fr::excess_group_delay_s(&ir, SR, n_fft).unwrap();

    // Drop the two end bins: np.gradient is one-sided (edge_order=1) there, so
    // they are the one place the stencil is not the interior stencil.
    let interior = &egd[1..egd.len() - 1];
    let mean = interior.iter().sum::<f64>() / interior.len() as f64;
    assert!(
        (mean - TAU_S).abs() < 1e-5,
        "mean EGD {mean:e} s, expected τ = {TAU_S:e} s"
    );

    let lo = interior.iter().fold(f64::INFINITY, |m, v| m.min(*v));
    let hi = interior.iter().fold(f64::NEG_INFINITY, |m, v| m.max(*v));
    let a = 0.5;
    assert!(
        (lo - TAU_S * (1.0 - a) / (1.0 + a)).abs() < 1e-5,
        "min EGD {lo:e} s, expected {:e}",
        TAU_S * (1.0 - a) / (1.0 + a)
    );
    assert!(
        hi > 0.9 * TAU_S * (1.0 + a) / (1.0 - a) && hi <= TAU_S * (1.0 + a) / (1.0 - a),
        "max EGD {hi:e} s, expected just under {:e}",
        TAU_S * (1.0 + a) / (1.0 - a)
    );

    // Ripple period 1/τ = 200 Hz, read off the local maxima's spacing.
    let df = f64::from(SR) / n_fft as f64;
    let peaks: Vec<usize> = (1..egd.len() - 1)
        .filter(|&i| egd[i] > egd[i - 1] && egd[i] > egd[i + 1])
        .collect();
    assert!(peaks.len() > 100, "too few ripple peaks: {}", peaks.len());
    let spacing_hz = (peaks[peaks.len() - 1] - peaks[0]) as f64 * df / (peaks.len() - 1) as f64;
    assert!(
        (spacing_hz - 1.0 / TAU_S).abs() < 1.0,
        "ripple spacing {spacing_hz} Hz, expected {} Hz",
        1.0 / TAU_S
    );
}

#[test]
fn weighted_with_equal_weights_equals_the_unweighted_one() {
    // BIT equality, which is the claim worth making: with wᵢ ≡ 1, `w·x == x`
    // exactly and `Σw == N` exactly, so the weighted accumulation visits the
    // same values in the same order as the unweighted one. Anything looser here
    // would let a reordered summation through, and a reordered summation is how
    // a "harmless" refactor of the frozen unweighted path would first show up.
    let c = Case::load("fr", "rms_average");
    let meas = c.array2("measurements");
    let freqs = c.array("freqs");
    let rows: Vec<Vec<f64>> = (0..meas.rows)
        .map(|r| meas.data[r * meas.cols..(r + 1) * meas.cols].to_vec())
        .collect();
    let set = fr::align_spl(&rows, &freqs, (200.0, 2000.0)).unwrap();
    let ones = vec![1.0; meas.rows];
    assert_eq!(
        fr::average_measurements_rms_weighted(&set, &ones).unwrap(),
        fr::average_measurements_rms(&set),
    );
    assert_eq!(
        fr::sigma_db_weighted(&set, &ones).unwrap(),
        fr::sigma_db(&set)
    );
}

#[test]
fn weighted_estimators_reject_a_bad_weight_vector() {
    // A weight vector that does not index the positions, a negative weight, a
    // non-finite weight, and an all-zero vector (Σw = 0 ⇒ a division by zero
    // that would surface as NaN curves deep in `authority.rs`).
    let c = Case::load("fr", "rms_average");
    let meas = c.array2("measurements");
    let freqs = c.array("freqs");
    let rows: Vec<Vec<f64>> = (0..meas.rows)
        .map(|r| meas.data[r * meas.cols..(r + 1) * meas.cols].to_vec())
        .collect();
    let set = fr::align_spl(&rows, &freqs, (200.0, 2000.0)).unwrap();
    for bad in [
        vec![1.0; meas.rows - 1],
        vec![1.0, 1.0, -0.5, 1.0, 1.0],
        vec![1.0, 1.0, f64::NAN, 1.0, 1.0],
        vec![0.0; meas.rows],
    ] {
        assert!(fr::average_measurements_rms_weighted(&set, &bad).is_err());
        assert!(fr::sigma_db_weighted(&set, &bad).is_err());
    }
}

#[test]
fn power_averaging_floors_correctly() {
    // Power averaging is null-RESISTANT, not null-immune. Each assertion below
    // is one clause of that claim.
    let floor = 10.0 * (4.0f64 / 5.0).log10(); // 10·log10((N−k)/N), N=5, k=1
    assert!((floor - (-0.969)).abs() < 5e-4, "floor pin: {floor}");

    // 1. The value at a FINITE depth. N=5, k=1, d=−6 dB is −0.705 dB — NOT the
    //    −0.969 floor. The floor is an asymptote, and at shallow depths the
    //    difference is most of the answer.
    let v = power_avg_at_null(5, 1, -6.0);
    assert!((v - (-0.705)).abs() < 5e-4, "N=5 k=1 d=−6 dB: {v}");
    assert!((v - expected_db(5, 1, -6.0)).abs() < 1e-12);
    assert!(v > floor, "a −6 dB null must sit well above the floor");

    // 2. Monotone approach to the floor, never attaining it.
    let mut prev = f64::INFINITY;
    for d in [-1.0, -3.0, -6.0, -10.0, -15.0, -20.0, -30.0, -60.0, -120.0] {
        let p = power_avg_at_null(5, 1, d);
        assert!(
            (p - expected_db(5, 1, d)).abs() < 1e-12,
            "closed form at d={d}"
        );
        assert!(p < prev, "must decrease monotonically with depth; d={d}");
        assert!(p > floor, "must never attain the floor; d={d} gave {p}");
        prev = p;
    }

    // 3. The floor is reached within 0.01 dB by ≈ −20 dB depth. Exactly −20 dB
    //    leaves 0.0108 dB, so the spec's "≈" is doing real work — −21 dB is the
    //    first integer depth strictly inside 0.01 dB.
    assert!(power_avg_at_null(5, 1, -20.0) - floor < 0.02);
    assert!(power_avg_at_null(5, 1, -21.0) - floor < 0.01);

    // 4. k = N — a null common to EVERY position (floor bounce, SBIR, a driver
    //    notch) passes through exactly. This is correct and desirable: a null at
    //    every seat is a real feature, not spatial scatter. The bound above is
    //    benign only because k/N is normally small.
    for d in [-6.0, -20.0, -40.0] {
        let p = power_avg_at_null(5, 5, d);
        assert!(
            (p - d).abs() < 1e-12,
            "k=N must pass {d} dB through exactly; got {p}"
        );
    }
}

#[test]
fn null_at_one_of_five_pins_both_estimators() {
    // The spec's corrected numbers: a −30 dB null at ONE of five positions →
    // dB-avg = −6.0 dB, power-avg = −0.97 dB. This pins BOTH estimators — the
    // divergence is the entire reason the room path is power-domain while the
    // coupler path stays dB-domain (test_fr.rs::averaging_is_in_db_domain).
    let curves = nulled_set(5, 1, -30.0);

    // dB averaging tracks the null linearly: mean(−30, 0, 0, 0, 0) = −6.0.
    let db_avg = fr::average_measurements(&curves).unwrap();
    assert!(
        (db_avg[NULL_BIN] - (-6.0)).abs() < 1e-12,
        "dB-avg: {}",
        db_avg[NULL_BIN]
    );

    // Power averaging resists it: 10·log10((4 + 10^−3)/5) = −0.968 ≈ −0.97.
    let set = fr::align_spl(&curves, &FREQS, BAND).unwrap();
    let rms = fr::average_measurements_rms(&set);
    assert!(
        (rms[NULL_BIN] - (-0.97)).abs() < 5e-3,
        "power-avg: {}",
        rms[NULL_BIN]
    );
    assert!((rms[NULL_BIN] - expected_db(5, 1, -30.0)).abs() < 1e-12);

    // And σ(f) flags the bin as position-dependent junk: population std of
    // (−30, 0, 0, 0, 0) is 12 dB, far above the ~0.6–0.8 dB of a feature
    // present at every position.
    let sigma = fr::sigma_db(&set);
    assert!(
        (sigma[NULL_BIN] - 12.0).abs() < 1e-12,
        "{}",
        sigma[NULL_BIN]
    );
    assert!(sigma[0].abs() < 1e-12, "un-nulled bins have zero spread");
}

#[test]
fn power_mean_inequality_holds_with_zero_violations() {
    // Jensen / power-mean: RMS_dB ≥ dB_avg ALWAYS, with equality iff the
    // magnitude is identical at every position. 10 000 random 5-position
    // vectors, FIXED literal seed (bit-reproducible; never a time-derived
    // seed), zero violations tolerated — not even one-ulp ones, because the
    // uniform [−40, 10) dB spread puts the true gap orders of magnitude above
    // float noise.
    const BINS: usize = 10_000;
    let mut rng = SplitMix64(0x5EED_0001_C0FF_EE00);
    let freqs: Vec<f64> = (0..BINS).map(|i| 100.0 + 0.2 * i as f64).collect();
    let curves: Vec<Vec<f64>> = (0..5)
        .map(|_| (0..BINS).map(|_| -40.0 + 50.0 * rng.next_f64()).collect())
        .collect();
    let set = fr::align_spl(&curves, &freqs, BAND).unwrap();
    // Both estimators on the SAME (aligned) data.
    let rms = fr::average_measurements_rms(&set);
    let db_avg = fr::average_measurements(set.measurements_db()).unwrap();
    let violations = rms.iter().zip(&db_avg).filter(|(r, a)| r < a).count();
    assert_eq!(violations, 0, "power-mean inequality violated");
    // The gap is genuinely large for spatially-scattered data — this is not a
    // both-sides-equal vacuous pass.
    let mean_gap: f64 = rms.iter().zip(&db_avg).map(|(r, a)| r - a).sum::<f64>() / BINS as f64;
    assert!(mean_gap > 1.0, "mean gap {mean_gap} dB suspiciously small");
}

#[test]
fn gap_formula_holds_to_one_percent() {
    // RMS_dB − dB_avg ≈ 0.1151·σ_dB² (the constant is ln 10 / 20 exactly, for
    // normally-distributed dB values). Deterministic construction: N midpoint
    // quantiles of N(0, σ²) via Acklam's inverse CDF — no sampling noise, and
    // N = 20 001 keeps the lognormal tail truncation below 0.05% even at
    // σ = 5 (validated against numpy during development).
    const N: usize = 20_001;
    let c = std::f64::consts::LN_10 / 20.0; // 0.11512925…, the spec's 0.1151
    for sigma in [0.5, 1.0, 2.0, 5.0] {
        let curves: Vec<Vec<f64>> = (0..N)
            .map(|j| {
                let z = inv_norm_cdf((j as f64 + 0.5) / N as f64);
                let mut m = vec![0.0; FREQS.len()];
                m[NULL_BIN] = sigma * z; // outside the band ⇒ align is a no-op
                m
            })
            .collect();
        let set = fr::align_spl(&curves, &FREQS, BAND).unwrap();
        let gap = fr::average_measurements_rms(&set)[NULL_BIN]
            - fr::average_measurements(set.measurements_db()).unwrap()[NULL_BIN];
        let sigma_hat = fr::sigma_db(&set)[NULL_BIN];
        assert!(
            (sigma_hat - sigma).abs() < 0.01 * sigma,
            "σ̂ = {sigma_hat} for nominal {sigma}"
        );
        let predicted = c * sigma_hat * sigma_hat;
        assert!(
            (gap - predicted).abs() <= 0.01 * predicted,
            "σ = {sigma}: gap {gap} vs 0.1151·σ̂² = {predicted}"
        );
    }
}

#[test]
fn vector_average_is_a_tripwire() {
    // Unconditionally Err for n > 1: coherent averaging collapses toward the
    // incoherent floor −10·log10(N) once position spread approaches a
    // wavelength (−10.94 dB at 1.5 kHz for ±40 cm). The function exists so the
    // error is discoverable rather than the operation reinvented.
    let a = vec![Complex::new(1.0, 0.0), Complex::new(0.5, 0.5)];
    let b = vec![Complex::new(1.0, 0.0), Complex::new(0.5, -0.5)];
    assert!(fr::average_measurements_vector(&[a.clone(), b]).is_err());
    // n = 1 is the identity — no averaging happened, nothing to reject.
    let one = fr::average_measurements_vector(std::slice::from_ref(&a)).unwrap();
    assert_eq!(one.len(), a.len());
    assert!(one.iter().zip(&a).all(|(x, y)| x == y));
    // n = 0 has no meaning either.
    assert!(fr::average_measurements_vector(&[]).is_err());
}

#[test]
fn fixed_smoothing_is_byte_identical_to_the_boxcar() {
    // Smoothing::Fixed(n) must ROUTE to the frozen boxcar, not reimplement it:
    // byte-for-byte identical output, so the Tier-1 fixtures keep pinning the
    // coupler path through the new entry point too.
    let grid = LogGrid::standard();
    let mag: Vec<f64> = (0..grid.len())
        .map(|i| (i as f64 * 0.13).sin() * 6.0 + (i as f64 * 0.029).cos() * 3.0)
        .collect();
    for n in [3u32, 6, 12] {
        let via_smooth = fr::smooth(&mag, &grid, Smoothing::Fixed(n)).unwrap();
        let direct = fr::fractional_octave_smooth(&mag, grid.freqs(), n);
        assert_eq!(via_smooth.len(), direct.len());
        for (i, (a, b)) in via_smooth.iter().zip(&direct).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "Fixed({n}) differs at [{i}]");
        }
    }
}

#[test]
fn flat_spectrum_is_preserved_in_every_mode() {
    let grid = LogGrid::standard();
    let modes = [
        Smoothing::Fixed(6),
        Smoothing::Gaussian {
            fraction: 1.0 / 6.0,
        },
        Smoothing::Variable,
        Smoothing::None,
    ];
    // Flat at 0 dB: bit-exact in every mode (all four paths reduce to
    // arithmetic on exact zeros / exact ones).
    let zero = vec![0.0; grid.len()];
    for mode in modes {
        let out = fr::smooth(&zero, &grid, mode).unwrap();
        assert!(
            out.iter().all(|&v| v == 0.0),
            "{mode:?} moved a flat 0 dB spectrum"
        );
    }
    // Flat at a non-zero level: preserved to float noise (the dB↔linear and
    // normalization round-trips each cost ulps, nothing more).
    let flat = vec![7.25; grid.len()];
    for mode in modes {
        let out = fr::smooth(&flat, &grid, mode).unwrap();
        for (i, v) in out.iter().enumerate() {
            assert!((v - 7.25).abs() < 1e-12, "{mode:?} at [{i}]: {v} != 7.25");
        }
    }
}

/// Tier 2: the whole Variable profile pinned bin-for-bin against an independent
/// numpy re-implementation of the exact per-bin truncated-Gaussian convolution
/// (`fixtures/fr/variable_smooth`). This is the spec-mandated Tier-2 for the
/// Variable mode — a scipy single-σ primitive can't express a per-bin σ, so the
/// reference is a hand-rolled loop; the tolerance is parity, not method
/// accuracy, because both sides implement the same closed form.
#[test]
fn variable_smoothing_matches_numpy_reference() {
    let c = Case::load("fr", "variable_smooth");
    let grid = LogGrid::standard();
    let mag_db = c.array("mag_db");
    assert_eq!(
        mag_db.len(),
        grid.len(),
        "fixture built on the standard grid"
    );
    let out = fr::smooth(&mag_db, &grid, Smoothing::Variable).unwrap();
    assert_allclose(&out, &c.array("smoothed"), 0.0, 1e-9, "variable_smooth");
}

#[test]
fn variable_smoothing_is_fine_in_bass_coarse_in_treble() {
    // The Variable profile is 1/48 oct below 100 Hz and 1/3 oct above 10 kHz —
    // deliberately the INVERSE of psychoacoustic smoothing: fine detail where
    // the corrector has authority (modal bass), coarse where detail is
    // position-specific (treble). Observable consequence: an identical spike
    // survives far better at 100 Hz than at 10 kHz.
    let grid = LogGrid::standard();
    let spike_at = |bin: usize| {
        let mut m = vec![0.0; grid.len()];
        m[bin] = 6.0;
        m
    };
    // bin 223 → 100.1 Hz; bin 861 → 10 020 Hz (f = 20·2^(i/96)).
    let (lo_bin, hi_bin) = (223usize, 861usize);
    assert!((grid.freqs()[lo_bin] - 100.0).abs() < 1.0);
    assert!((grid.freqs()[hi_bin] - 10_000.0).abs() < 100.0);
    let lo = fr::smooth(&spike_at(lo_bin), &grid, Smoothing::Variable).unwrap();
    let hi = fr::smooth(&spike_at(hi_bin), &grid, Smoothing::Variable).unwrap();
    assert!(lo[lo_bin] > 2.0, "bass spike flattened: {}", lo[lo_bin]);
    assert!(hi[hi_bin] < 1.0, "treble spike survived: {}", hi[hi_bin]);
    assert!(
        lo[lo_bin] > 4.0 * hi[hi_bin],
        "profile inversion missing: {} vs {}",
        lo[lo_bin],
        hi[hi_bin]
    );
}

#[test]
fn variable_smoothing_width_matches_the_profile_in_the_mid_band() {
    // The endpoint test above only probes the clamped bass floor (100 Hz) and
    // clamped treble (10 kHz), where FRAC_MID and the log-f interpolation law
    // barely move the result. This pins the σ(f) the Variable profile actually
    // applies at 300 Hz / 1 kHz / 3 kHz — the region a FRAC_MID change or a
    // linear-in-f interpolation would silently corrupt. Method: a unit spike's
    // Variable-smoothed response is the (edge-effect-free) normalized Gaussian
    // kernel, so its second moment recovers σ_bins directly.
    let grid = LogGrid::standard();
    // fraction/σ conversion, replicated from fr.rs's public contract so a
    // change to the private curve must move the measured width to disagree.
    let expected_sigma_bins = |f: f64| -> f64 {
        const FRAC_BASS: f64 = 1.0 / 48.0;
        const FRAC_MID: f64 = 1.0 / 6.0;
        const FRAC_TREBLE: f64 = 1.0 / 3.0;
        let fraction = if f <= 100.0 {
            FRAC_BASS
        } else if f <= 1000.0 {
            FRAC_BASS + (FRAC_MID - FRAC_BASS) * (f / 100.0).log10()
        } else if f <= 10_000.0 {
            FRAC_MID + (FRAC_TREBLE - FRAC_MID) * (f / 1000.0).log10()
        } else {
            FRAC_TREBLE
        };
        fraction / fr::GAUSSIAN_FWHM_PER_SIGMA * f64::from(grid.points_per_octave())
    };
    // bin 375 ≈ 300 Hz, 542 ≈ 1 kHz, 694 ≈ 3 kHz (f = 20·2^(i/96)).
    for &bin in &[375usize, 542, 694] {
        let f = grid.freqs()[bin];
        let mut spike = vec![0.0; grid.len()];
        spike[bin] = 6.0;
        let out = fr::smooth(&spike, &grid, Smoothing::Variable).unwrap();
        // Second moment of the (positive) response about the spike bin.
        let (mut m0, mut m2) = (0.0f64, 0.0f64);
        for (j, &v) in out.iter().enumerate() {
            let d = j as f64 - bin as f64;
            m0 += v;
            m2 += d * d * v;
        }
        let measured = (m2 / m0).sqrt();
        let expected = expected_sigma_bins(f);
        assert!(
            (measured / expected - 1.0).abs() < 0.03,
            "σ at {f:.0} Hz (bin {bin}): measured {measured:.3} bins vs profile {expected:.3}"
        );
    }
    // And the profile is strictly monotone increasing across these three — the
    // "fine in bass, coarse in treble" shape, not just three matching numbers.
    assert!(expected_sigma_bins(300.0) < expected_sigma_bins(1000.0));
    assert!(expected_sigma_bins(1000.0) < expected_sigma_bins(3000.0));
}

#[test]
fn align_spl_removes_constant_offsets() {
    // Curves differing by constants must align to IDENTICAL curves, with
    // offsets summing to zero (the ensemble's absolute level is preserved,
    // unlike normalize_to_reference_band, which zeroes it).
    let grid = LogGrid::standard();
    let base: Vec<f64> = (0..grid.len())
        .map(|i| (i as f64 * 0.07).sin() * 4.0 - (i as f64 * 0.011).cos() * 2.0)
        .collect();
    let shifts = [3.5, -1.25, 0.75];
    let curves: Vec<Vec<f64>> = shifts
        .iter()
        .map(|k| base.iter().map(|v| v + k).collect())
        .collect();
    let set = fr::align_spl(&curves, grid.freqs(), BAND).unwrap();
    assert_eq!(set.reference_band(), BAND);

    let offset_sum: f64 = set.offsets_db().iter().sum();
    assert!(offset_sum.abs() < 1e-9, "offsets sum to {offset_sum}");
    // offset_j = k_j − mean(k): 3.5 − 1.0, −1.25 − 1.0, 0.75 − 1.0.
    let mean_shift = shifts.iter().sum::<f64>() / shifts.len() as f64;
    for (o, k) in set.offsets_db().iter().zip(&shifts) {
        assert!((o - (k - mean_shift)).abs() < 1e-12, "offset {o} for {k}");
    }
    // All aligned curves identical: base + mean(k).
    for m in set.measurements_db() {
        for (i, v) in m.iter().enumerate() {
            assert!(
                (v - (base[i] + mean_shift)).abs() < 1e-12,
                "aligned[{i}] = {v}"
            );
        }
    }
}

#[test]
fn align_spl_and_smooth_error_paths() {
    // align_spl: empty set, ragged set, axis mismatch, empty band, non-finite.
    assert!(fr::align_spl(&[], &FREQS, BAND).is_err());
    assert!(fr::align_spl(
        &[vec![0.0; FREQS.len()], vec![0.0; FREQS.len() - 1]],
        &FREQS,
        BAND
    )
    .is_err());
    assert!(fr::align_spl(&[vec![0.0; 4]], &FREQS, BAND).is_err());
    assert!(fr::align_spl(&[vec![0.0; FREQS.len()]], &FREQS, (30_000.0, 40_000.0)).is_err());
    let mut bad = vec![0.0; FREQS.len()];
    bad[3] = f64::NAN;
    assert!(fr::align_spl(&[bad], &FREQS, BAND).is_err());

    // smooth: length mismatch, non-finite input, Fixed(0), bad Gaussian fraction.
    let grid = LogGrid::standard();
    assert!(fr::smooth(&[0.0; 4], &grid, Smoothing::None).is_err());
    let mut nan = vec![0.0; grid.len()];
    nan[10] = f64::NAN;
    assert!(fr::smooth(&nan, &grid, Smoothing::None).is_err());
    let flat = vec![0.0; grid.len()];
    assert!(fr::smooth(&flat, &grid, Smoothing::Fixed(0)).is_err());
    assert!(fr::smooth(&flat, &grid, Smoothing::Gaussian { fraction: 0.0 }).is_err());
    assert!(fr::smooth(&flat, &grid, Smoothing::Gaussian { fraction: -1.0 }).is_err());
    assert!(fr::smooth(&flat, &grid, Smoothing::Gaussian { fraction: f64::NAN }).is_err());
}
