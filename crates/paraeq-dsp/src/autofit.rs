//! Greedy parametric-EQ auto-fit. Oracle: prototype/paraeq/correction/auto_fit.py

use crate::peq::{EQBand, FilterType, ParametricEQ};

pub fn auto_fit_parametric_eq(
    correction_db: &[f64],
    freqs: &[f64],
    sample_rate: f64,
    max_bands: usize,
    min_gain_db: f64,
) -> Vec<EQBand> {
    let mut residual = correction_db.to_vec();
    let mask: Vec<bool> = freqs.iter().map(|f| (20.0..=20000.0).contains(f)).collect();
    let mut bands = Vec::new();
    for _ in 0..max_bands {
        let mut peak_idx = None;
        let mut peak_abs = f64::NEG_INFINITY;
        for i in 0..residual.len() {
            if mask[i] && residual[i].abs() > peak_abs {
                peak_abs = residual[i].abs();
                peak_idx = Some(i);
            }
        }
        let Some(idx) = peak_idx else { break };
        let peak_gain = residual[idx];
        if peak_gain.abs() < min_gain_db || freqs[idx] <= 0.0 {
            break;
        }
        let q = estimate_q(&residual, freqs, idx);
        let band = EQBand {
            filter_type: FilterType::Peaking,
            fc: freqs[idx],
            gain_db: peak_gain,
            q,
        };
        let response = ParametricEQ {
            bands: vec![band.clone()],
            sample_rate,
        }
        .frequency_response(freqs);
        for (r, resp) in residual.iter_mut().zip(&response) {
            let v = if resp.is_nan() { 0.0 } else { *resp };
            *r -= v;
        }
        bands.push(band);
    }
    bands
}

fn estimate_q(residual: &[f64], freqs: &[f64], peak_idx: usize) -> f64 {
    let half = 0.5 * residual[peak_idx].abs();
    let f_center = freqs[peak_idx];
    let mut f_low = None;
    let mut i = peak_idx;
    while i > 1 {
        i -= 1;
        if residual[i].abs() < half {
            f_low = Some(freqs[i]);
            break;
        }
    }
    let mut f_high = None;
    for j in peak_idx + 1..residual.len() {
        if residual[j].abs() < half {
            f_high = Some(freqs[j]);
            break;
        }
    }
    match (f_low, f_high) {
        (Some(lo), Some(hi)) if hi > lo && f_center > 0.0 => {
            (f_center / (hi - lo)).clamp(0.5, 20.0)
        }
        _ => 2.0,
    }
}
