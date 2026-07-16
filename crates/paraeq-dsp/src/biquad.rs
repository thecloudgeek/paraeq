//! RBJ Audio EQ Cookbook biquad designers + cascade response.
//! Oracle: prototype/paraeq/correction/biquad.py. SOS rows: [b0,b1,b2,1,a1,a2]/a0.

use std::f64::consts::PI;

fn wa(fc: f64, q: f64, sample_rate: f64) -> (f64, f64) {
    let w0 = 2.0 * PI * fc / sample_rate;
    (w0, w0.sin() / (2.0 * q))
}

fn row(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> [f64; 6] {
    [b0 / a0, b1 / a0, b2 / a0, 1.0, a1 / a0, a2 / a0]
}

pub fn peaking(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let a = 10f64.powf(gain_db / 40.0);
    let (w0, alpha) = wa(fc, q, sample_rate);
    row(
        1.0 + alpha * a,
        -2.0 * w0.cos(),
        1.0 - alpha * a,
        1.0 + alpha / a,
        -2.0 * w0.cos(),
        1.0 - alpha / a,
    )
}

pub fn low_shelf(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let a = 10f64.powf(gain_db / 40.0);
    let (w0, alpha) = wa(fc, q, sample_rate);
    let (c, s2a) = (w0.cos(), 2.0 * a.sqrt() * alpha);
    row(
        a * ((a + 1.0) - (a - 1.0) * c + s2a),
        2.0 * a * ((a - 1.0) - (a + 1.0) * c),
        a * ((a + 1.0) - (a - 1.0) * c - s2a),
        (a + 1.0) + (a - 1.0) * c + s2a,
        -2.0 * ((a - 1.0) + (a + 1.0) * c),
        (a + 1.0) + (a - 1.0) * c - s2a,
    )
}

pub fn high_shelf(fc: f64, gain_db: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let a = 10f64.powf(gain_db / 40.0);
    let (w0, alpha) = wa(fc, q, sample_rate);
    let (c, s2a) = (w0.cos(), 2.0 * a.sqrt() * alpha);
    row(
        a * ((a + 1.0) + (a - 1.0) * c + s2a),
        -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
        a * ((a + 1.0) + (a - 1.0) * c - s2a),
        (a + 1.0) - (a - 1.0) * c + s2a,
        2.0 * ((a - 1.0) - (a + 1.0) * c),
        (a + 1.0) - (a - 1.0) * c - s2a,
    )
}

pub fn notch(fc: f64, q: f64, sample_rate: f64) -> [f64; 6] {
    let (w0, alpha) = wa(fc, q, sample_rate);
    row(
        1.0,
        -2.0 * w0.cos(),
        1.0,
        1.0 + alpha,
        -2.0 * w0.cos(),
        1.0 - alpha,
    )
}

/// Jury stability conditions for a real-coefficient second-order section:
/// both poles strictly inside the unit circle.
pub fn is_stable(sos: &[f64; 6]) -> bool {
    let (a1, a2) = (sos[4], sos[5]);
    sos.iter().all(|c| c.is_finite()) && a2.abs() < 1.0 && a1.abs() < a2 + 1.0
}

/// scipy.signal.sosfreqz equivalent at worN = freqs·2π/sr, returned as
/// 20·log10(|H| + 1e-10)  — NOTE the +1e-10 is ADDED (biquad.py convention),
/// not a max() floor (frequency_response.py uses max(); do not mix them up).
pub fn sos_frequency_response_db(sos: &[[f64; 6]], freqs: &[f64], sample_rate: f64) -> Vec<f64> {
    freqs
        .iter()
        .map(|f| {
            let w = f * 2.0 * PI / sample_rate;
            let (c1, s1) = (w.cos(), w.sin());
            let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
            let mut mag = 1.0f64;
            for r in sos {
                let nr = r[0] + r[1] * c1 + r[2] * c2;
                let ni = -(r[1] * s1 + r[2] * s2);
                let dr = r[3] + r[4] * c1 + r[5] * c2;
                let di = -(r[4] * s1 + r[5] * s2);
                mag *= ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt();
            }
            20.0 * (mag + 1e-10).log10()
        })
        .collect()
}
