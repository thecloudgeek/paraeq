//! Regularized (Wiener) spectral-division deconvolution.
//! Oracle: prototype/paraeq/measurement/deconvolution.py

use realfft::RealFftPlanner;

pub fn deconvolve(recorded: &[f64], sweep: &[f64], _sample_rate: u32) -> Vec<f64> {
    let n = recorded.len();
    let n_fft = (n + sweep.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let inv = planner.plan_fft_inverse(n_fft);

    let mut sweep_padded = vec![0.0; n_fft];
    sweep_padded[..sweep.len()].copy_from_slice(sweep);
    let mut s = fwd.make_output_vec();
    fwd.process(&mut sweep_padded, &mut s).unwrap();

    let power: Vec<f64> = s.iter().map(|c| c.norm_sqr()).collect();
    let eps = 1e-10 * power.iter().fold(0.0f64, |m, v| m.max(*v));

    let mut rec_padded = vec![0.0; n_fft];
    rec_padded[..n].copy_from_slice(recorded);
    let mut r = fwd.make_output_vec();
    fwd.process(&mut rec_padded, &mut r).unwrap();

    for ((ri, si), p) in r.iter_mut().zip(&s).zip(&power) {
        *ri *= si.conj() / (p + eps);
    }
    let mut ir = inv.make_output_vec();
    inv.process(&mut r, &mut ir).unwrap();
    ir.truncate(n);
    for v in &mut ir {
        *v /= n_fft as f64;
    }
    ir
}
