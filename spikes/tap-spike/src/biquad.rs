//! RBJ Audio EQ Cookbook peaking biquad — mirrors prototype/paraeq/correction/biquad.py.
//! Coefficients f64, samples f32, filter state f64 (spec: Sample formats).

#[derive(Clone, Copy, Debug)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

/// RBJ peaking EQ. Matches biquad_peaking() in the Python prototype:
/// a_lin = 10^(gain/40), w0 = 2*pi*fc/fs, alpha = sin(w0)/(2q), normalized by a0.
pub fn peaking(fs: f64, fc: f64, gain_db: f64, q: f64) -> Coeffs {
    let a_lin = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * fc / fs;
    let alpha = w0.sin() / (2.0 * q);
    let a0 = 1.0 + alpha / a_lin;
    Coeffs {
        b0: (1.0 + alpha * a_lin) / a0,
        b1: (-2.0 * w0.cos()) / a0,
        b2: (1.0 - alpha * a_lin) / a0,
        a1: (-2.0 * w0.cos()) / a0,
        a2: (1.0 - alpha / a_lin) / a0,
    }
}

/// Direct Form II Transposed runner — f64 state, f32 samples.
pub struct Df2t {
    c: Coeffs,
    z1: f64,
    z2: f64,
}

impl Df2t {
    pub fn new(c: Coeffs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        y as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// |H(e^{jw})| in dB, evaluated directly from the transfer function.
    fn mag_db(c: &Coeffs, f: f64, fs: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * f / fs;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let nr = c.b0 + c.b1 * c1 + c.b2 * c2;
        let ni = -(c.b1 * s1 + c.b2 * s2);
        let dr = 1.0 + c.a1 * c1 + c.a2 * c2;
        let di = -(c.a1 * s1 + c.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }

    #[test]
    fn peaking_gain_at_fc_is_exact() {
        // RBJ property: |H(fc)| in dB == gain_db exactly.
        let c = peaking(48000.0, 1000.0, 6.0, 1.0);
        assert!((mag_db(&c, 1000.0, 48000.0) - 6.0).abs() < 1e-9);
        let cut = peaking(48000.0, 250.0, -4.5, 2.0);
        assert!((mag_db(&cut, 250.0, 48000.0) + 4.5).abs() < 1e-9);
    }

    #[test]
    fn peaking_is_flat_far_from_fc() {
        let c = peaking(48000.0, 1000.0, 6.0, 1.0);
        assert!(mag_db(&c, 20.0, 48000.0).abs() < 0.1);
        assert!(mag_db(&c, 20000.0, 48000.0).abs() < 0.5);
    }

    #[test]
    fn df2t_amplifies_sine_at_fc_by_gain() {
        let c = peaking(48000.0, 1000.0, 6.0, 1.0);
        let mut f = Df2t::new(c);
        let n = 48000;
        let mut in_rms = 0.0f64;
        let mut out_rms = 0.0f64;
        for i in 0..n {
            let x = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48000.0).sin() as f32;
            let y = f.process(x);
            if i >= 4800 {
                // skip transient
                in_rms += (x as f64) * (x as f64);
                out_rms += (y as f64) * (y as f64);
            }
        }
        let gain_db = 10.0 * (out_rms / in_rms).log10();
        assert!((gain_db - 6.0).abs() < 0.05, "measured {gain_db}");
    }
}
