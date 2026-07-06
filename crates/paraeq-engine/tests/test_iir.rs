mod common;
use common::{assert_allclose, Case};
use paraeq_engine::iir::IIRProcessor;

#[test]
fn cascade_state_carries_across_blocks_matching_oracle() {
    let c = Case::load("iir", "cascade_4_blocks");
    let sos_flat = c.array2("sos"); // [3, 6]
    let sos: Vec<[f64; 6]> = (0..sos_flat.rows)
        .map(|r| {
            let row = &sos_flat.data[r * 6..(r + 1) * 6];
            [row[0], row[1], row[2], row[3], row[4], row[5]]
        })
        .collect();
    let input = c.array2("input");
    let expected = c.array2("output");
    let block = 512usize;
    let mut proc = IIRProcessor::new();
    proc.set_sos(0, sos.clone());
    proc.set_sos(1, sos);
    let (in0, in1) = (input.col(0), input.col(1));
    let (exp0, exp1) = (expected.col(0), expected.col(1));
    let mut out = vec![vec![0.0; block], vec![0.0; block]];
    for b in 0..input.rows / block {
        let r = b * block..(b + 1) * block;
        proc.process(&[&in0[r.clone()], &in1[r.clone()]], &mut out);
        assert_allclose(
            &out[0],
            &exp0[r.clone()],
            1e-9,
            1e-12,
            &format!("block {b} ch0"),
        );
        assert_allclose(
            &out[1],
            &exp1[r.clone()],
            1e-9,
            1e-12,
            &format!("block {b} ch1"),
        );
    }
}

#[test]
fn channel_without_sos_passes_through() {
    let mut proc = IIRProcessor::new();
    proc.set_sos(0, vec![[0.5, 0.0, 0.0, 1.0, 0.0, 0.0]]); // pure -6dB gain section
    let x: Vec<f64> = (0..64).map(|i| (i as f64 * 0.1).sin()).collect();
    let mut out = vec![vec![0.0; 64], vec![0.0; 64]];
    proc.process(&[&x, &x], &mut out);
    assert_allclose(
        &out[0],
        &x.iter().map(|v| v * 0.5).collect::<Vec<_>>(),
        1e-12,
        1e-12,
        "filtered",
    );
    assert_allclose(&out[1], &x, 0.0, 0.0, "passthrough");
}
