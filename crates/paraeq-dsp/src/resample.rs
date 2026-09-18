//! Time-domain ratio resampling with a sub-sample slice phase.
//!
//! Why this exists: the adopted two-clock resolution (decision doc
//! `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q6) is REW's
//! bracketed-timing-marker skew estimate **plus a resample** — "compute the
//! clock-rate difference, and **resample** the capture to correct it — default
//! on, with `Warn(TwoClock)` as the fallback only when no estimate can be
//! formed". Nothing in this crate could execute that half: `fir.rs` interpolates
//! *magnitudes* positionally and `logf.rs` resamples a *frequency* axis; neither
//! moves a signal in time. At the ~12 ppm §Q6 measures, a 7.7 s file at 48 kHz
//! drifts 4.4 samples end to end, which is exactly the regime where rounding
//! `t = 0` to an integer sample is wrong.
//!
//! # Index convention — read this before slicing anything
//!
//! Output sample `j` is the input signal at input-time
//! `t_in(j) = (j + frac_offset) / ratio`, in input samples. So
//! `resample_ratio(x, r, φ)[j] ≈ x((j + φ)/r)`, and **an output index is an
//! output-clock index**. A caller holding a `t0` measured in the **input**
//! clock must convert:
//!
//! ```text
//! j0 = floor(t0 · ratio)        // the output index to slice at
//! φ  = frac(t0 · ratio)         // the leftover, handed to resample_ratio
//! ```
//!
//! **not** `floor(t0)`. The verification path measures `t0` as
//! `SkewEstimate::intercept_samples` on the *capture* clock and then slices the
//! *resampled* array, so this conversion is the one place the two axes meet.
//! `resample_ratio_index_convention_is_output_clock` is its falsifier.
//!
//! Positive `frac_offset` advances input time, so it moves a feature **earlier**
//! in output index. Input indices outside `x` contribute zero (the signal is
//! treated as zero-padded), so the first and last ~`HALF_TAPS` output samples
//! see the edge rather than the signal.
//!
//! # The kernel
//!
//! A 32-tap Kaiser-windowed sinc, β = 8.6, cutoff `min(1, ratio)` — the cutoff
//! follows the ratio so that a downsample band-limits before it decimates, and a
//! resample at or above unity is pure interpolation. It is one fixed impulse
//! response evaluated at a fractional argument, **not** a per-output-sample
//! normalised kernel: that is what makes the filter linear phase (a flat group
//! delay, which is what lets a recovered `t = 0` survive the resample) and what
//! makes `ratio = 1, frac_offset = 0` the exact identity — every tap then lands
//! on an integer, where sinc is 1 at the centre and 0 everywhere else. The cost
//! of not normalising is a DC-gain ripple across fractional phases of about
//! 1e-4, which is the kernel's own stopband depth.
//!
//! # Test tiers
//!
//! - [`resample_ratio`] — **Tier 3 + one Tier-2 case.** Tier 3 because there is
//!   no library delegate for arbitrary-ratio fractional-phase resampling, and a
//!   numpy transcription of *our* kernel would launder our own algebra into a
//!   fixture (`prototype/tools/generate_fixtures.py`'s Tier-2 rule: a fixture's
//!   provenance must be the library, not our ported code). The invariants are
//!   analytic: identity at ratio 1, a chirp's group delay preserved to a
//!   hundredth of a sample, an impulse landing at `t0·ratio`. The **one** Tier-2
//!   case is the rational-ratio special case, where `scipy.signal.resample_poly`
//!   **is** a real delegate — fixture `("resample", "poly_rational")`, graded by
//!   `resample_ratio_matches_resample_poly_on_a_rational_ratio`. That grade is
//!   coarse on purpose: scipy's own Kaiser β = 5.0 design ripples about 3e-3 on
//!   the fixture's signal while this kernel sits within 5e-5 of the exact
//!   band-limited answer, so the delegate is the authority on *the convention,
//!   the ratio and the kernel's shape*, not on the last three digits.

/// Half the kernel length. 32 taps total: `k ∈ [-HALF_TAPS + 1, HALF_TAPS]`.
const HALF_TAPS: i64 = 16;

/// Kaiser shape parameter. β = 8.6 puts the stopband near -85 dB, which is what
/// keeps the images out of a capture that a gated IR will be cut from.
const KAISER_BETA: f64 = 8.6;

/// Resample `x` by `ratio` with a sub-sample slice phase `frac_offset`.
///
/// `resample_ratio(x, r, φ)[j] ≈ x((j + φ)/r)` — see the module header for the
/// index convention, which callers must convert into before slicing.
///
/// `ratio > 1` produces more samples than it consumes (the capture clock ran
/// slow and is being stretched); `ratio < 1` produces fewer and band-limits
/// first. The output holds every `j` whose `t_in(j)` lands inside `x`, i.e.
/// `floor((x.len() - 1)·ratio - frac_offset) + 1` samples.
///
/// Returns an empty vector, never a panic and never a fabricated signal, when
/// the request is degenerate: empty input, or a `ratio` / `frac_offset` that is
/// not a finite positive number. An empty capture is a refusal the caller can
/// see; a NaN one would poison a WAV.
pub fn resample_ratio(x: &[f64], ratio: f64, frac_offset: f64) -> Vec<f64> {
    if x.is_empty() || !ratio.is_finite() || ratio <= 0.0 || !frac_offset.is_finite() {
        return Vec::new();
    }
    let span = (x.len() as f64 - 1.0) * ratio - frac_offset;
    // `span` is an output-clock coordinate; a ratio large enough to overflow
    // usize is a caller error, not something to allocate for.
    if !span.is_finite() || span < 0.0 || span >= usize::MAX as f64 {
        return Vec::new();
    }
    let out_len = span.floor() as usize + 1;
    let cutoff = ratio.min(1.0);
    let n = x.len() as i64;

    let mut out = Vec::with_capacity(out_len);
    for j in 0..out_len {
        let t_in = (j as f64 + frac_offset) / ratio;
        let base = t_in.floor();
        let frac = t_in - base;
        let base = base as i64;
        let mut acc = 0.0;
        for k in (-HALF_TAPS + 1)..=HALF_TAPS {
            let index = base + k;
            if index < 0 || index >= n {
                continue; // zero-padded outside the capture
            }
            // Distance from the tap to the reconstruction point, in input
            // samples: t_in - index = frac - k.
            acc += x[index as usize] * kernel(frac - k as f64, cutoff);
        }
        out.push(acc);
    }
    out
}

/// The interpolation kernel at input-sample distance `u`, band-limited to
/// `cutoff` (1.0 = the input Nyquist).
fn kernel(u: f64, cutoff: f64) -> f64 {
    let v = u / HALF_TAPS as f64;
    if v.abs() > 1.0 {
        return 0.0;
    }
    let window = bessel_i0(KAISER_BETA * (1.0 - v * v).max(0.0).sqrt()) / bessel_i0(KAISER_BETA);
    cutoff * sinc(cutoff * u) * window
}

/// Normalised sinc, `sin(πu)/(πu)`, with the removable singularity filled in.
/// The `u == 0.0` branch is what makes the ratio-1 identity exact rather than
/// 0/0.
fn sinc(u: f64) -> f64 {
    if u == 0.0 {
        return 1.0;
    }
    let pu = std::f64::consts::PI * u;
    pu.sin() / pu
}

/// Modified Bessel function of the first kind, order 0, by its defining series
/// `Σ ((z/2)^m / m!)²`. Converges in well under 40 terms for `z ≤ KAISER_BETA`,
/// and the loop exits on relative term size rather than on a fixed count.
fn bessel_i0(z: f64) -> f64 {
    let half = z / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for m in 1..64 {
        let step = half / f64::from(m);
        term *= step * step;
        sum += term;
        if term < 1e-18 * sum {
            break;
        }
    }
    sum
}
