//! Not-a-knot cubic spline — direct port of scipy.interpolate.CubicSpline
//! (bc_type='not-a-knot', extrapolate=True), 1-D real case.
//! Oracle: scipy 1.18.0 (the version that generated fixtures/; algorithm
//! unchanged from 1.17.x) _cubic.py:790-958. Parity gate: Task 6 target
//! fixtures.

use crate::DspError;

pub struct NakSpline {
    x: Vec<f64>,
    y: Vec<f64>,
    s: Vec<f64>, // first derivative at each knot
}

impl NakSpline {
    pub fn new(x: &[f64], y: &[f64]) -> Result<Self, DspError> {
        if x.len() != y.len() || x.len() < 2 {
            return Err(DspError::InvalidInput(
                "spline needs >=2 equal-length points".into(),
            ));
        }
        if x.windows(2).any(|w| w[1] <= w[0]) {
            return Err(DspError::InvalidInput(
                "spline x must be strictly increasing".into(),
            ));
        }
        let n = x.len();
        let dx: Vec<f64> = x.windows(2).map(|w| w[1] - w[0]).collect();
        let slope: Vec<f64> = dx
            .iter()
            .enumerate()
            .map(|(i, d)| (y[i + 1] - y[i]) / d)
            .collect();

        let s = if n == 2 {
            vec![slope[0], slope[0]]
        } else if n == 3 {
            // parabola through the three points (scipy special case)
            let a = [
                [1.0, 1.0, 0.0],
                [dx[1], 2.0 * (dx[0] + dx[1]), dx[0]],
                [0.0, 1.0, 1.0],
            ];
            let b = [
                2.0 * slope[0],
                3.0 * (dx[0] * slope[1] + dx[1] * slope[0]),
                2.0 * slope[1],
            ];
            solve3(a, b)
        } else {
            // tridiagonal system in banded form: sub[i]·s[i-1] + diag[i]·s[i] + sup[i]·s[i+1] = b[i]
            let mut sub = vec![0.0; n];
            let mut diag = vec![0.0; n];
            let mut sup = vec![0.0; n];
            let mut b = vec![0.0; n];
            for i in 1..n - 1 {
                sub[i] = dx[i];
                diag[i] = 2.0 * (dx[i - 1] + dx[i]);
                sup[i] = dx[i - 1];
                b[i] = 3.0 * (dx[i] * slope[i - 1] + dx[i - 1] * slope[i]);
            }
            let d0 = x[2] - x[0];
            diag[0] = dx[1];
            sup[0] = d0;
            b[0] = ((dx[0] + 2.0 * d0) * dx[1] * slope[0] + dx[0] * dx[0] * slope[1]) / d0;
            let dn = x[n - 1] - x[n - 3];
            diag[n - 1] = dx[n - 3];
            sub[n - 1] = dn;
            b[n - 1] = (dx[n - 2] * dx[n - 2] * slope[n - 3]
                + (2.0 * dn + dx[n - 2]) * dx[n - 3] * slope[n - 2])
                / dn;
            thomas(&sub, &diag, &sup, &b)
        };

        Ok(NakSpline {
            x: x.to_vec(),
            y: y.to_vec(),
            s,
        })
    }

    pub fn eval(&self, q: f64) -> f64 {
        let n = self.x.len();
        // segment index: clamp to [0, n-2]; out-of-range extends end polynomials
        let i = if q <= self.x[0] {
            0
        } else if q >= self.x[n - 1] {
            n - 2
        } else {
            self.x.partition_point(|&v| v <= q) - 1
        };
        let h = self.x[i + 1] - self.x[i];
        let t = q - self.x[i];
        let slope = (self.y[i + 1] - self.y[i]) / h;
        let c2 = (3.0 * slope - 2.0 * self.s[i] - self.s[i + 1]) / h;
        let c3 = (self.s[i] + self.s[i + 1] - 2.0 * slope) / (h * h);
        self.y[i] + self.s[i] * t + c2 * t * t + c3 * t * t * t
    }
}

/// Thomas algorithm for a tridiagonal system (no pivoting; diagonally dominant here).
fn thomas(sub: &[f64], diag: &[f64], sup: &[f64], b: &[f64]) -> Vec<f64> {
    let n = b.len();
    let mut c = vec![0.0; n];
    let mut d = vec![0.0; n];
    c[0] = sup[0] / diag[0];
    d[0] = b[0] / diag[0];
    for i in 1..n {
        let m = diag[i] - sub[i] * c[i - 1];
        c[i] = if i < n - 1 { sup[i] / m } else { 0.0 };
        d[i] = (b[i] - sub[i] * d[i - 1]) / m;
    }
    let mut xout = vec![0.0; n];
    xout[n - 1] = d[n - 1];
    for i in (0..n - 1).rev() {
        xout[i] = d[i] - c[i] * xout[i + 1];
    }
    xout
}

fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> Vec<f64> {
    // Gaussian elimination on a 3x3 (partial pivoting unnecessary for these systems,
    // but do it anyway for robustness)
    let mut m = [
        [a[0][0], a[0][1], a[0][2], b[0]],
        [a[1][0], a[1][1], a[1][2], b[1]],
        [a[2][0], a[2][1], a[2][2], b[2]],
    ];
    for col in 0..3 {
        let piv = (col..3)
            .max_by(|&r1, &r2| m[r1][col].abs().total_cmp(&m[r2][col].abs()))
            .unwrap();
        m.swap(col, piv);
        let (pivot_rows, rest_rows) = m.split_at_mut(col + 1);
        let pivot = pivot_rows[col];
        for row in rest_rows.iter_mut() {
            let f = row[col] / pivot[col];
            for (val, p) in row.iter_mut().zip(pivot.iter()).skip(col) {
                *val -= f * p;
            }
        }
    }
    let mut s = [0.0; 3];
    for r in (0..3).rev() {
        let mut v = m[r][3];
        for k in r + 1..3 {
            v -= m[r][k] * s[k];
        }
        s[r] = v / m[r][r];
    }
    s.to_vec()
}
