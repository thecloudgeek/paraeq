//! Frequency-response computation and shaping.
//! Oracle: prototype/paraeq/measurement/frequency_response.py

use realfft::RealFftPlanner;

pub fn compute_frequency_response(
    ir: &[f64],
    sample_rate: u32,
    n_fft: Option<usize>,
) -> Result<(Vec<f64>, Vec<f64>), crate::DspError> {
    let n = n_fft.unwrap_or(ir.len());
    if n == 0 {
        return Err(crate::DspError::InvalidInput(
            "FFT length is 0 (empty impulse response, or explicit n_fft=0)".into(),
        ));
    }
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input = vec![0.0; n];
    let m = ir.len().min(n);
    input[..m].copy_from_slice(&ir[..m]);
    let mut spectrum = fft.make_output_vec();
    fft.process(&mut input, &mut spectrum).unwrap();
    let freqs: Vec<f64> = (0..spectrum.len())
        .map(|i| i as f64 * sample_rate as f64 / n as f64)
        .collect();
    let mag_db: Vec<f64> = spectrum
        .iter()
        .map(|c| 20.0 * c.norm().max(1e-10).log10())
        .collect();
    Ok((freqs, mag_db))
}

pub fn fractional_octave_smooth(magnitude_db: &[f64], freqs: &[f64], fraction: u32) -> Vec<f64> {
    let ratio = 2f64.powf(1.0 / (2.0 * fraction as f64));
    let linear: Vec<f64> = magnitude_db
        .iter()
        .map(|db| 10f64.powf(db / 20.0))
        .collect();
    let mut out = Vec::with_capacity(linear.len());
    for (i, &f) in freqs.iter().enumerate() {
        if f <= 0.0 {
            out.push(linear[i]);
            continue;
        }
        let (lo, hi) = (f / ratio, f * ratio);
        // freqs is monotonically increasing (rfftfreq): contiguous window.
        let mut sum = 0.0;
        let mut count = 0usize;
        for (j, &fj) in freqs.iter().enumerate() {
            if fj >= lo && fj <= hi {
                sum += linear[j];
                count += 1;
            }
        }
        out.push(sum / count as f64);
    }
    out.iter().map(|v| 20.0 * v.max(1e-10).log10()).collect()
}

pub fn average_measurements(measurements_db: &[Vec<f64>]) -> Result<Vec<f64>, crate::DspError> {
    let n = match measurements_db.first() {
        Some(first) => first.len(),
        None => {
            return Err(crate::DspError::InvalidInput(
                "no measurements to average".into(),
            ))
        }
    };
    for (i, m) in measurements_db.iter().enumerate() {
        if m.len() != n {
            return Err(crate::DspError::InvalidInput(format!(
                "measurement {i} has {} points, expected {n} (all measurements must match)",
                m.len()
            )));
        }
    }
    let mut out = vec![0.0; n];
    for m in measurements_db {
        for (o, v) in out.iter_mut().zip(m) {
            *o += v;
        }
    }
    for o in &mut out {
        *o /= measurements_db.len() as f64;
    }
    Ok(out)
}

pub fn normalize_to_reference_band(
    freqs: &[f64],
    magnitude_db: &[f64],
    low_hz: f64,
    high_hz: f64,
) -> Result<Vec<f64>, crate::DspError> {
    let band: Vec<f64> = freqs
        .iter()
        .zip(magnitude_db)
        .filter(|(f, _)| **f >= low_hz && **f <= high_hz)
        .map(|(_, m)| *m)
        .collect();
    if band.is_empty() {
        return Err(crate::DspError::InvalidInput(format!(
            "no bins in reference band {low_hz}-{high_hz} Hz"
        )));
    }
    let mean = band.iter().sum::<f64>() / band.len() as f64;
    Ok(magnitude_db.iter().map(|m| m - mean).collect())
}
