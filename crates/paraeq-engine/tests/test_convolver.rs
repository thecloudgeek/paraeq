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

#[test]
fn ola_equals_direct_convolution() {
    // deterministic pseudo-random input, 3 blocks of 128, FIR len 37
    let mut state = 0x9E3779B97F4A7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let fir: Vec<f64> = (0..37).map(|_| next()).collect();
    let input: Vec<f64> = (0..384).map(|_| next()).collect();
    let mut conv = OverlapAddConvolver::new(vec![fir.clone()], 128);
    let mut got = Vec::new();
    let mut out = vec![vec![0.0; 128]];
    for b in 0..3 {
        conv.process(&[&input[b * 128..(b + 1) * 128]], &mut out);
        got.extend_from_slice(&out[0]);
    }
    // direct convolution, first 384 samples
    for (i, g) in got.iter().enumerate() {
        let mut acc = 0.0;
        for (k, f) in fir.iter().enumerate() {
            if i >= k {
                acc += f * input[i - k];
            }
        }
        assert!((g - acc).abs() < 1e-9, "sample {i}: {g} vs {acc}");
    }
}
