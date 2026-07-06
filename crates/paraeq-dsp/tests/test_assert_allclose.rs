//! Regression test for the shared `assert_allclose` test helper: a NaN (or
//! infinite) actual value must fail loudly instead of silently comparing
//! false against every tolerance check.
mod common;
use common::assert_allclose;

#[test]
#[should_panic(expected = "non-finite actual")]
fn assert_allclose_rejects_nan_actual() {
    let actual = [1.0, f64::NAN, 3.0];
    let expected = [1.0, 2.0, 3.0];
    assert_allclose(&actual, &expected, 1e-6, 1e-9, "nan guard regression");
}
