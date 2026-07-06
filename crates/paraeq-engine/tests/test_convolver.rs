mod common;
use common::{assert_allclose, Case};
use paraeq_engine::convolver::OverlapAddConvolver;

#[test]
fn stereo_blocks_match_oracle_block_by_block() {
    let c = Case::load("convolver", "stereo_8_blocks");
    let block = c.param_u64("block_size") as usize;
    let input = c.array2("input");
    let expected = c.array2("output");
    let mut conv = OverlapAddConvolver::new(vec![c.array("fir_l"), c.array("fir_r")], block);
    let mut out = vec![vec![0.0; block], vec![0.0; block]];
    let n_blocks = input.rows / block;
    assert_eq!(n_blocks, 8);
    let (in0, in1) = (input.col(0), input.col(1));
    let (exp0, exp1) = (expected.col(0), expected.col(1));
    for b in 0..n_blocks {
        let r = b * block..(b + 1) * block;
        conv.process(&[&in0[r.clone()], &in1[r.clone()]], &mut out);
        assert_allclose(
            &out[0],
            &exp0[r.clone()],
            1e-9,
            1e-9,
            &format!("block {b} ch0"),
        );
        assert_allclose(
            &out[1],
            &exp1[r.clone()],
            1e-9,
            1e-9,
            &format!("block {b} ch1"),
        );
    }
}

#[test]
fn mono_fir_broadcasts_to_stereo_without_panicking() {
    // regression for the prototype's mono-FIR IndexError (CONTEXT.md)
    let mut conv = OverlapAddConvolver::new(vec![vec![1.0, 0.5, 0.25]], 64);
    let silence = vec![0.0f64; 64];
    let mut out = vec![vec![0.0; 64], vec![0.0; 64]];
    conv.process(&[&silence, &silence], &mut out);
    assert!(out.iter().flatten().all(|v| v.abs() < 1e-10));
}
