mod common;
use common::{assert_allclose, Case};
use paraeq_dsp::deconvolution::deconvolve;

#[test]
fn wiener_deconvolution_matches_oracle() {
    let c = Case::load("deconvolution", "delta_plus_tail");
    let ir = deconvolve(
        &c.array("recorded"),
        &c.array("sweep"),
        c.param_u64("sample_rate") as u32,
    );
    assert_allclose(&ir, &c.array("ir_out"), 1e-9, 1e-9, "deconvolved IR");
}

#[test]
fn recovers_the_true_delay() {
    let c = Case::load("deconvolution", "delta_plus_tail");
    let ir = deconvolve(&c.array("recorded"), &c.array("sweep"), 48000);
    let argmax = ir
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    assert!(
        (argmax as i64 - 32).unsigned_abs() <= 10,
        "peak at {argmax}, true delay 32"
    );
}
