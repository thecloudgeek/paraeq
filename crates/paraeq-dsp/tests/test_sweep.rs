mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::sweep::{generate_inverse_sweep, generate_sweep};

#[test]
fn sweep_matches_oracle() {
    let c = Case::load("sweep", "basic");
    let sweep = generate_sweep(
        c.param_f64("duration"),
        c.param_u64("sample_rate") as u32,
        c.param_f64("f_start"),
        c.param_f64("f_end"),
    );
    assert_allclose(&sweep, &c.array("sweep"), 1e-12, 1e-12, "sweep");
}

#[test]
fn inverse_sweep_matches_oracle() {
    let c = Case::load("sweep", "basic");
    let sweep = c.array("sweep");
    let inv = generate_inverse_sweep(
        &sweep,
        c.param_u64("sample_rate") as u32,
        c.param_f64("f_start"),
        c.param_f64("f_end"),
    );
    assert_allclose(&inv, &c.array("inverse"), 1e-12, 1e-12, "inverse sweep");
}
