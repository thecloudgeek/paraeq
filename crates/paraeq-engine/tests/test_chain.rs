//! Synthetic-block tests for the realtime chain:
//! bypass? -> correction -> trim gain -> safety clamp(+-1.0).

use paraeq_engine::chain::{build_fir, build_iir, ChainOutcome, RealtimeChain};
use paraeq_engine::convolver::OverlapAddConvolver;
use paraeq_engine::iir::IIRProcessor;

const BLOCK: usize = 64;
/// A stable, stateful biquad section (a1 = -0.5, a2 = 0.25).
const SOS: [f64; 6] = [0.2, 0.3, 0.1, 1.0, -0.5, 0.25];

/// Deterministic non-trivial test signal.
fn sig(block: usize, ch: usize, i: usize) -> f32 {
    (((block * BLOCK + i) as f32) * 0.13 + ch as f32 * 0.7).sin() * 0.4
}

fn make_block(block: usize, channels: usize, frames: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|ch| (0..frames).map(|i| sig(block, ch, i)).collect())
        .collect()
}

/// Drive the chain with owned buffers, returning (output, outcome).
fn chain_process(
    chain: &mut RealtimeChain,
    input: &[Vec<f32>],
    bypass: bool,
    gain: f32,
) -> (Vec<Vec<f32>>, ChainOutcome) {
    let views: Vec<&[f32]> = input.iter().map(|v| v.as_slice()).collect();
    let mut out: Vec<Vec<f32>> = input.iter().map(|v| vec![0.0; v.len()]).collect();
    let outcome = {
        let mut out_views: Vec<&mut [f32]> = out.iter_mut().map(|v| v.as_mut_slice()).collect();
        chain.process(&views, &mut out_views, bypass, gain)
    };
    (out, outcome)
}

/// The chain's exact sample path for a corrected f64 value: down-cast, then
/// gain, then clamp.
fn downcast_gain_clamp(y: f64, gain: f32) -> f32 {
    ((y as f32) * gain).clamp(-1.0, 1.0)
}

#[test]
fn no_correction_is_identity_with_gain_and_clamp() {
    let mut chain = RealtimeChain::new(2, BLOCK);

    let input = vec![vec![0.5f32; BLOCK]; 2];
    let (out, outcome) = chain_process(&mut chain, &input, false, 2.0);
    assert!(!outcome.corrected);
    assert!(!outcome.frame_mismatch);
    for ch in &out {
        for &s in ch {
            assert_eq!(s, 1.0, "0.5 * 2.0 == 1.0 (exactly at the clamp edge)");
        }
    }

    let input = vec![vec![0.9f32; BLOCK]; 2];
    let (out, _) = chain_process(&mut chain, &input, false, 2.0);
    for ch in &out {
        for &s in ch {
            assert_eq!(s, 1.0, "0.9 * 2.0 == 1.8 clamps to 1.0");
        }
    }

    let input = vec![vec![0.5f32; BLOCK]; 2];
    let (out, _) = chain_process(&mut chain, &input, false, 0.5);
    for ch in &out {
        for &s in ch {
            assert_eq!(s, 0.25, "0.5 * 0.5 == 0.25");
        }
    }
}

#[test]
fn iir_path_matches_direct_processor_across_blocks() {
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![SOS], vec![SOS]], 2, BLOCK)));

    let mut direct = IIRProcessor::new();
    direct.set_sos(0, vec![SOS]);
    direct.set_sos(1, vec![SOS]);

    let gain = 0.8f32;
    for b in 0..3 {
        let input = make_block(b, 2, BLOCK);
        let (out, outcome) = chain_process(&mut chain, &input, false, gain);
        assert!(outcome.corrected);
        assert!(!outcome.frame_mismatch);

        // Hand-driven reference with the chain's exact casts.
        let in64: Vec<Vec<f64>> = input
            .iter()
            .map(|v| v.iter().map(|&s| s as f64).collect())
            .collect();
        let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
        let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
        direct.process(&views, &mut out64);

        for ch in 0..2 {
            for i in 0..BLOCK {
                let expected = downcast_gain_clamp(out64[ch][i], gain);
                assert_eq!(
                    out[ch][i].to_bits(),
                    expected.to_bits(),
                    "block {b} ch {ch} sample {i}: bitwise mismatch"
                );
            }
        }
    }
}

#[test]
fn fir_path_matches_direct_convolver() {
    let fir = vec![0.5f64, 0.25, -0.125];
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_fir(vec![fir.clone(), fir.clone()], 2, BLOCK)));

    let mut direct = OverlapAddConvolver::new(vec![fir.clone(), fir], BLOCK);

    let gain = 0.9f32;
    for b in 0..3 {
        let input = make_block(b, 2, BLOCK);
        let (out, outcome) = chain_process(&mut chain, &input, false, gain);
        assert!(outcome.corrected);
        assert!(!outcome.frame_mismatch);

        let in64: Vec<Vec<f64>> = input
            .iter()
            .map(|v| v.iter().map(|&s| s as f64).collect())
            .collect();
        let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
        let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
        direct.process(&views, &mut out64);

        for ch in 0..2 {
            for i in 0..BLOCK {
                let expected = downcast_gain_clamp(out64[ch][i], gain);
                assert_eq!(
                    out[ch][i].to_bits(),
                    expected.to_bits(),
                    "block {b} ch {ch} sample {i}: bitwise mismatch"
                );
            }
        }
    }
}

#[test]
fn bypass_passes_through_and_edge_resets_state() {
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![SOS], vec![SOS]], 2, BLOCK)));
    let gain = 0.5f32;

    // Block 0: loud, correction active -- builds internal filter state.
    let loud = make_block(0, 2, BLOCK);
    let (_, outcome) = chain_process(&mut chain, &loud, false, gain);
    assert!(outcome.corrected);

    // Block 1: bypass on -- correction skipped, gain and clamp still apply.
    let input = make_block(1, 2, BLOCK);
    let (out, outcome) = chain_process(&mut chain, &input, true, gain);
    assert!(!outcome.corrected);
    assert!(!outcome.frame_mismatch);
    for ch in 0..2 {
        for i in 0..BLOCK {
            let expected = (input[ch][i] * gain).clamp(-1.0, 1.0);
            assert_eq!(out[ch][i].to_bits(), expected.to_bits());
        }
    }

    // Block 2: bypass off again -- state must have been reset on the edge,
    // so the output equals a FRESH processor's output for this block.
    let input = make_block(2, 2, BLOCK);
    let (out, outcome) = chain_process(&mut chain, &input, false, gain);
    assert!(outcome.corrected);

    let mut fresh = IIRProcessor::new();
    fresh.set_sos(0, vec![SOS]);
    fresh.set_sos(1, vec![SOS]);
    let in64: Vec<Vec<f64>> = input
        .iter()
        .map(|v| v.iter().map(|&s| s as f64).collect())
        .collect();
    let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
    let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
    fresh.process(&views, &mut out64);

    for ch in 0..2 {
        for i in 0..BLOCK {
            let expected = downcast_gain_clamp(out64[ch][i], gain);
            assert_eq!(
                out[ch][i].to_bits(),
                expected.to_bits(),
                "ch {ch} sample {i}: stale state survived the bypass edge"
            );
        }
    }
}

#[test]
fn fir_frame_mismatch_passes_through() {
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_fir(vec![vec![0.5f64, 0.25]], 2, BLOCK)));

    // The convolver requires exactly BLOCK frames; half a block must NOT
    // panic -- pass through with gain + clamp instead.
    let input = vec![vec![0.6f32; BLOCK / 2]; 2];
    let (out, outcome) = chain_process(&mut chain, &input, false, 0.5);
    assert!(!outcome.corrected);
    assert!(outcome.frame_mismatch);
    for ch in &out {
        assert_eq!(ch.len(), BLOCK / 2);
        for &s in ch {
            assert_eq!(s, 0.3, "pass-through must still apply gain");
        }
    }
}

#[test]
fn oversized_frames_pass_through_for_any_correction() {
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![SOS], vec![SOS]], 2, BLOCK)));

    // 2x block_size exceeds the preallocated scratch; must NOT panic (the
    // HAL occasionally resizes) -- pass through with gain + clamp.
    let input = vec![vec![0.9f32; 2 * BLOCK]; 2];
    let (out, outcome) = chain_process(&mut chain, &input, false, 2.0);
    assert!(!outcome.corrected);
    assert!(outcome.frame_mismatch);
    for ch in &out {
        assert_eq!(ch.len(), 2 * BLOCK);
        for &s in ch {
            assert_eq!(s, 1.0, "0.9 * 2.0 clamps to 1.0");
        }
    }
}
