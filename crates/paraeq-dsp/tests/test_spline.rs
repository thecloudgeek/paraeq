mod common;
use paraeq_dsp::spline::NakSpline;

#[test]
fn passes_through_knots() {
    let x = [1.0, 2.0, 3.5, 5.0, 8.0, 13.0];
    let y = [0.0, 3.0, -1.0, 2.5, 2.5, -4.0];
    let s = NakSpline::new(&x, &y).unwrap();
    for (xi, yi) in x.iter().zip(&y) {
        assert!((s.eval(*xi) - yi).abs() < 1e-12, "knot {xi}");
    }
}

/// scipy oracle values, pinned by running the script in this task's Step 1 notes.
#[test]
fn matches_scipy_at_pinned_points() {
    let x = [0.0, 1.0, 2.0, 3.0, 4.0];
    let y = [0.0, 1.0, 0.0, 1.0, 0.0];
    let s = NakSpline::new(&x, &y).unwrap();
    let queries = [0.5, 1.5, 2.5, 3.5, -0.5, 4.5]; // incl. extrapolation
    let expected: [f64; 6] = [1.125, 0.375, 0.375, 1.125, -3.125, -3.125]; // pinned via scipy CubicSpline oracle
    for (q, e) in queries.iter().zip(&expected) {
        assert!(
            (s.eval(*q) - e).abs() < 1e-10,
            "q={q}: {} vs {e}",
            s.eval(*q)
        );
    }
}

#[test]
fn n2_is_linear_and_n3_is_parabola() {
    let s2 = NakSpline::new(&[0.0, 2.0], &[1.0, 5.0]).unwrap();
    assert!((s2.eval(1.0) - 3.0).abs() < 1e-12);
    // parabola y = x^2 through (0,0),(1,1),(2,4) must be reproduced exactly
    let s3 = NakSpline::new(&[0.0, 1.0, 2.0], &[0.0, 1.0, 4.0]).unwrap();
    assert!((s3.eval(0.5) - 0.25).abs() < 1e-12);
    assert!((s3.eval(1.7) - 2.89).abs() < 1e-12);
}
