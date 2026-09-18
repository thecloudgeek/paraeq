//! Farina logarithmic sine sweep + inverse filter, and the playback fade.
//!
//! Tier 1 (frozen prototype fixtures, 1e-12) for `generate_sweep` /
//! `generate_inverse_sweep`. Oracle: prototype/paraeq/measurement/sweep.py.
//!
//! Tier 3 (analytic invariants) for `apply_fade`: the prototype fades nowhere, so
//! there is no oracle to port, and a tier-2 scipy window fixture would pin one
//! arbitrary length instead of the properties the safety argument rests on —
//! exact-zero endpoints, a monotone fade-out, no amplification. The closed form is
//! itself the specification. Precedent: spline.rs:1-5. See
//! docs/specs/2026-07-15-measurement-safety-design.md § Fade and DC.

/// Log sine sweep. n_samples = trunc(duration * sample_rate);
/// phase = 2π·f_start·duration/ln(rate) · (rate^(t/duration) − 1), rate = f_end/f_start.
pub fn generate_sweep(duration: f64, sample_rate: u32, f_start: f64, f_end: f64) -> Vec<f64> {
    let n = (duration * sample_rate as f64) as usize;
    let rate = f_end / f_start;
    let k = 2.0 * std::f64::consts::PI * f_start * duration / rate.ln();
    (0..n)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            (k * (rate.powf(t / duration) - 1.0)).sin()
        })
        .collect()
}

/// Raised-cosine (half-Hann) fade applied in place: envelope ½(1 − cos(π·i/N))
/// over the first `fade_in` samples and its mirror over the last `fade_out`.
/// Additive — callers fade after `generate_sweep`, which stays bit-exact against
/// its oracle; durations are policy and live in paraeq-measure (10 ms in, 50 ms
/// out). An un-faded sweep ends in a broadband click at up to full scale
/// (−0.5336 at 5.0 s, −0.9146 at 0.25 s / 48 kHz).
///
/// Constraints: the envelope is ⊂ [0,1], so a fade never amplifies; sample 0 and
/// sample len−1 are exactly 0.0 whenever the matching fade is non-zero; a
/// zero-length fade touches no samples, so the π·i/N division never sees N = 0;
/// over-long or overlapping fades multiply, landing under both ramps rather than
/// clicking. Edge shaping only — this is not a DC blocker.
pub fn apply_fade(x: &mut [f64], fade_in: usize, fade_out: usize) {
    for (i, v) in x.iter_mut().enumerate().take(fade_in) {
        *v *= half_hann(i, fade_in);
    }
    let tail = x.len().saturating_sub(fade_out);
    for (j, v) in x[tail..].iter_mut().rev().enumerate() {
        *v *= half_hann(j, fade_out);
    }
}

/// Rising half of a symmetric Hann window of length 2N+1: ½(1 − cos(π·i/N)).
/// Callers must not reach this with n == 0 (see `apply_fade`'s constraints).
fn half_hann(i: usize, n: usize) -> f64 {
    0.5 * (1.0 - (std::f64::consts::PI * i as f64 / n as f64).cos())
}

/// Time-reversed sweep with 6 dB/oct decay envelope and scalar normalization:
/// envelope[i] = rate^(−t_rev[i]/duration), t_rev = reversed arange(n)/sr;
/// inverse /= Σ(sweep · reversed(sweep) · envelope) / n.
pub fn generate_inverse_sweep(
    sweep: &[f64],
    sample_rate: u32,
    f_start: f64,
    f_end: f64,
) -> Vec<f64> {
    let n = sweep.len();
    let duration = n as f64 / sample_rate as f64;
    let rate = f_end / f_start;
    let mut inverse: Vec<f64> = sweep.iter().rev().copied().collect();
    let envelope: Vec<f64> = (0..n)
        .map(|i| {
            let t = (n - 1 - i) as f64 / sample_rate as f64;
            rate.powf(-t / duration)
        })
        .collect();
    let norm: f64 = sweep
        .iter()
        .zip(sweep.iter().rev())
        .zip(&envelope)
        .map(|((s, srev), e)| s * srev * e)
        .sum::<f64>()
        / n as f64;
    for (v, e) in inverse.iter_mut().zip(&envelope) {
        *v *= e / norm;
    }
    inverse
}
