//! FIR correction design: frequency sampling (linear phase) + homomorphic
//! minimum phase. Oracles: prototype/paraeq/correction/fir_filter.py and
//! scipy.signal.minimum_phase (homomorphic, half=True), scipy 1.17.1.

use realfft::RealFftPlanner;
use rustfft::{num_complex::Complex, FftPlanner};

/// Phase behavior for [`design_fir_correction`].
pub enum FirPhase {
    Linear,
    Minimum,
}

/// Design a FIR correction filter from a correction curve, via frequency
/// sampling. Port of `prototype/paraeq/correction/fir_filter.py::design_fir_correction`.
///
/// Divergence from the Python: the Python signature also takes a `freqs`
/// parameter that is documented but never used by the implementation
/// (resampling onto the target bin count is purely positional); the Rust
/// port drops it.
pub fn design_fir_correction(correction_db: &[f64], n_taps: usize, phase: FirPhase) -> Vec<f64> {
    let magnitude: Vec<f64> = correction_db
        .iter()
        .map(|db| 10f64.powf(db / 20.0))
        .collect();
    match phase {
        FirPhase::Linear => {
            let bins = n_taps / 2 + 1;
            let resampled = resample(&magnitude, bins);
            windowed_ir(&resampled, n_taps, true)
        }
        FirPhase::Minimum => {
            let n_proto = 2 * n_taps - 1;
            let bins = n_proto / 2 + 1;
            let resampled = resample(&magnitude, bins);
            // Homomorphic half=True takes the square root of the prototype's
            // magnitude spectrum (via cepstrum halving); square the spectrum
            // so sqrt(mag^2) == mag after conversion. Order matters:
            // `fir_filter.py::_design_minimum_phase` resamples the ORIGINAL
            // magnitude first (`target_mag = np.interp(...)`) and only then
            // squares it (`target_mag_sq = target_mag ** 2`). Squaring before
            // resampling would be wrong — squaring doesn't commute with
            // (linear) interpolation — so the square happens on `resampled`
            // here, not on `magnitude`.
            let squared: Vec<f64> = resampled.iter().map(|m| m * m).collect();
            let proto = windowed_ir(&squared, n_proto, false);
            let n_fft = 1usize << ((4 * n_proto) as f64).log2().ceil() as u32;
            let min_ph = minimum_phase_homomorphic(&proto, n_fft);
            if min_ph.len() >= n_taps {
                min_ph.into_iter().take(n_taps).collect()
            } else {
                let mut out = vec![0.0; n_taps];
                out[..min_ph.len()].copy_from_slice(&min_ph);
                out
            }
        }
    }
}

/// Positional resample onto `bins` points: `np.interp(linspace(0, 1, bins),
/// linspace(0, 1, len(magnitude)), magnitude)`.
fn resample(magnitude: &[f64], bins: usize) -> Vec<f64> {
    (0..bins)
        .map(|i| {
            let pos = i as f64 / (bins - 1) as f64 * (magnitude.len() - 1) as f64;
            let j = pos.floor() as usize;
            if j + 1 >= magnitude.len() {
                magnitude[magnitude.len() - 1]
            } else {
                let t = pos - j as f64;
                magnitude[j] * (1.0 - t) + magnitude[j + 1] * t
            }
        })
        .collect()
}

/// Frequency-sampling impulse response from an already-resampled half-spectrum:
/// irfft, roll, Hann window, and (optionally) peak-normalize + symmetrize.
/// Verified against `fir_filter.py`: `_design_linear_phase` (the public linear
/// path) does peak-normalize and symmetrize; `_design_minimum_phase` builds
/// its prototype inline via the same irfft/roll/hanning steps but WITHOUT
/// normalizing or symmetrizing, feeding the raw windowed prototype straight
/// into `minimum_phase(...)`.
fn windowed_ir(resampled_spectrum: &[f64], n: usize, normalize: bool) -> Vec<f64> {
    // irfft(real spectrum): np.fft.irfft(spectrum, n)
    let mut ir = irfft(resampled_spectrum, n);
    // roll by n/2
    ir.rotate_right(n / 2);
    // symmetric Hann (np.hanning): 0.5 - 0.5*cos(2*pi*i/(n-1))
    for (i, v) in ir.iter_mut().enumerate() {
        *v *= 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
    }
    if normalize {
        let peak = ir.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        if peak > 0.0 {
            for v in &mut ir {
                *v /= peak;
            }
        }
        let rev: Vec<f64> = ir.iter().rev().copied().collect();
        for (v, r) in ir.iter_mut().zip(&rev) {
            *v = (*v + r) / 2.0;
        }
    }
    ir
}

/// Inverse real FFT of a purely-real, non-negative half-spectrum of `bins =
/// n/2 + 1` values (`np.fft.irfft(spectrum, n)` with `spectrum.imag == 0`
/// everywhere, which is all this module ever feeds it). `n` may be even
/// (the linear-phase path) or odd (`n_proto = 2*n_taps - 1`, always odd, for
/// the minimum-phase prototype) — `realfft` handles both and matches numpy's
/// irfft to machine epsilon for both parities (verified against the golden
/// fixtures at every tap, not just within the 1e-6 spec tolerance).
fn irfft(spectrum: &[f64], n: usize) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(n);
    let mut spec: Vec<Complex<f64>> = spectrum.iter().map(|m| Complex::new(*m, 0.0)).collect();
    let mut out = ifft.make_output_vec();
    ifft.process(&mut spec, &mut out).unwrap();
    for v in &mut out {
        *v /= n as f64;
    }
    out
}

/// scipy.signal.minimum_phase(h, method="homomorphic", n_fft, half=True), exact port.
fn minimum_phase_homomorphic(h: &[f64], n_fft: usize) -> Vec<f64> {
    let mut planner = FftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(n_fft);
    let inv = planner.plan_fft_inverse(n_fft);

    let mut buf: Vec<Complex<f64>> = h
        .iter()
        .map(|v| Complex::new(*v, 0.0))
        .chain(std::iter::repeat(Complex::new(0.0, 0.0)))
        .take(n_fft)
        .collect();
    fwd.process(&mut buf);
    let mut mag: Vec<f64> = buf.iter().map(|c| c.norm()).collect();
    let min_pos = mag
        .iter()
        .copied()
        .filter(|v| *v > 0.0)
        .fold(f64::INFINITY, f64::min);
    for v in &mut mag {
        *v = (*v + 1e-7 * min_pos).ln() * 0.5; // += eps, log, half=True
    }
    // real cepstrum: Re(IFFT(log-mag))
    let mut cep: Vec<Complex<f64>> = mag.iter().map(|v| Complex::new(*v, 0.0)).collect();
    inv.process(&mut cep);
    for c in &mut cep {
        *c /= n_fft as f64;
    }
    // Homomorphic window (scipy `_fir_filter_design.py`, `minimum_phase`):
    //   win[0] = 1
    //   win[1..stop] = 2                       (stop = n_fft // 2)
    //   win[stop] = 1 + (n_fft % 2)             (1 when n_fft even, 2 when odd)
    //   win[stop+1..] = 0
    // n_fft is always a power of two here (even), so win[stop] == 1.
    let stop = n_fft / 2;
    let mut windowed: Vec<Complex<f64>> = vec![Complex::new(0.0, 0.0); n_fft];
    windowed[0] = Complex::new(cep[0].re, 0.0);
    for i in 1..stop {
        windowed[i] = Complex::new(2.0 * cep[i].re, 0.0);
    }
    let nyquist_weight = 1.0 + (n_fft % 2) as f64;
    windowed[stop] = Complex::new(nyquist_weight * cep[stop].re, 0.0);
    // exp(FFT(windowed)), then IFFT, take real
    fwd.process(&mut windowed);
    for c in &mut windowed {
        *c = c.exp();
    }
    inv.process(&mut windowed);
    let scale = 1.0 / n_fft as f64;
    let n_out = h.len().div_ceil(2);
    windowed.iter().take(n_out).map(|c| c.re * scale).collect()
}
