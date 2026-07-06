//! Farina logarithmic sine sweep + inverse filter.
//! Oracle: prototype/paraeq/measurement/sweep.py

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
