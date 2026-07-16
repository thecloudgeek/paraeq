//! Synthetic-block tests for the realtime chain:
//! bypass? -> correction -> trim gain -> safety clamp(+-1.0).

use std::f64::consts::PI;

use paraeq_dsp::biquad::peaking;
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
        assert_eq!(outcome.nonfinite_outputs, 0, "clean audio must not trip the output guard");

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
        assert_eq!(outcome.nonfinite_outputs, 0, "clean audio must not trip the output guard");

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
fn mono_iir_config_broadcasts_to_both_channels() {
    // ONE SOS set on a 2-channel chain: the last (only) set is broadcast,
    // so both channels are corrected identically.
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![SOS]], 2, BLOCK)));

    let mut direct = IIRProcessor::new();
    direct.set_sos(0, vec![SOS]);
    direct.set_sos(1, vec![SOS]);

    for b in 0..3 {
        let input = make_block(b, 2, BLOCK);
        let (out, outcome) = chain_process(&mut chain, &input, false, 1.0);
        assert!(outcome.corrected);

        let in64: Vec<Vec<f64>> = input
            .iter()
            .map(|v| v.iter().map(|&s| s as f64).collect())
            .collect();
        let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
        let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
        direct.process(&views, &mut out64);

        for ch in 0..2 {
            for i in 0..BLOCK {
                let expected = downcast_gain_clamp(out64[ch][i], 1.0);
                assert_eq!(
                    out[ch][i].to_bits(),
                    expected.to_bits(),
                    "block {b} ch {ch} sample {i}: mono SOS not broadcast"
                );
            }
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
fn frame_mismatch_resets_correction_state() {
    let fir = vec![0.5f64, 0.25, -0.125];
    let gain = 1.0f32;
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_fir(vec![fir.clone(), fir.clone()], 2, BLOCK)));

    // Block 0: conforming -- builds overlap state in the convolver.
    let (_, outcome) = chain_process(&mut chain, &make_block(0, 2, BLOCK), false, gain);
    assert!(outcome.corrected);

    // Mismatch block (half size): passes through AND clears the stale
    // correction state (the correction never saw these samples, so its
    // overlap tail no longer describes the stream).
    let short = vec![vec![0.3f32; BLOCK / 2]; 2];
    let (_, outcome) = chain_process(&mut chain, &short, false, gain);
    assert!(outcome.frame_mismatch);

    // Resumed conforming blocks must equal a FRESH convolver fed ONLY the
    // resumed blocks -- no stale tail from block 0.
    let mut fresh = OverlapAddConvolver::new(vec![fir.clone(), fir], BLOCK);
    for b in 2..4 {
        let input = make_block(b, 2, BLOCK);
        let (out, outcome) = chain_process(&mut chain, &input, false, gain);
        assert!(outcome.corrected);

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
                    "block {b} ch {ch} sample {i}: stale tail after mismatch"
                );
            }
        }
    }
}

// --- swap state transplant (spec R1-7a: click-free coefficient swaps) ---

/// Phase-continuous 100 Hz sine at 48 kHz. 0.1 amplitude keeps the +12 dB
/// Q=10 correction's output well inside the safety clamp.
fn sine100(start_sample: usize, frames: usize, channels: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|_| {
            (start_sample..start_sample + frames)
                .map(|n| ((n as f64 * 2.0 * PI * 100.0 / 48000.0).sin() * 0.1) as f32)
                .collect()
        })
        .collect()
}

fn max_first_diff(y: &[f32]) -> f32 {
    y.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

#[test]
fn identical_coefficient_swap_is_bit_exact_noop() {
    // The spec's sharpest test: swapping to identical coefficients at a
    // block boundary must be sample-for-sample identical to never swapping.
    let sos = peaking(100.0, 12.0, 10.0, 48000.0);
    let mut swapped = RealtimeChain::new(2, BLOCK);
    swapped.set_correction(Some(build_iir(vec![vec![sos]], 2, BLOCK)));
    let mut unswapped = RealtimeChain::new(2, BLOCK);
    unswapped.set_correction(Some(build_iir(vec![vec![sos]], 2, BLOCK)));
    for b in 0..64 {
        if b == 32 {
            swapped.set_correction(Some(build_iir(vec![vec![sos]], 2, BLOCK)));
        }
        let input = sine100(b * BLOCK, BLOCK, 2);
        let (out_s, outcome) = chain_process(&mut swapped, &input, false, 1.0);
        assert!(outcome.corrected);
        let (out_u, _) = chain_process(&mut unswapped, &input, false, 1.0);
        for ch in 0..2 {
            for i in 0..BLOCK {
                assert_eq!(
                    out_s[ch][i].to_bits(),
                    out_u[ch][i].to_bits(),
                    "block {b} ch {ch} sample {i}: a no-op swap must be a no-op"
                );
            }
        }
    }
}

#[test]
fn gain_only_swap_with_transplant_has_no_step() {
    // Gain-only coefficient change (+12 dB -> +11 dB, Q=10 @ 100 Hz): with
    // the transplant, the max first-difference across the swap boundary
    // stays within 1.1x the surrounding steady state's (spec bound). The
    // zero-state control below shows the same swap WITHOUT state is a
    // click, so the bound is discriminating.
    const WARM: usize = 96; // ~4 decay constants of the Q=10 resonance
    const TAIL: usize = 32;
    let sos_a = peaking(100.0, 12.0, 10.0, 48000.0);
    let sos_b = peaking(100.0, 11.0, 10.0, 48000.0);

    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![sos_a]], 2, BLOCK)));
    let mut y = Vec::new();
    for b in 0..WARM {
        let input = sine100(b * BLOCK, BLOCK, 2);
        let (out, _) = chain_process(&mut chain, &input, false, 1.0);
        y.extend_from_slice(&out[0]);
    }
    chain.set_correction(Some(build_iir(vec![vec![sos_b]], 2, BLOCK)));

    // Zero-state control: the same continuation through a fresh chain --
    // exactly what every swap sounded like before the transplant existed.
    let mut fresh = RealtimeChain::new(2, BLOCK);
    fresh.set_correction(Some(build_iir(vec![vec![sos_b]], 2, BLOCK)));
    let mut y_zero_state = y.clone();
    for b in WARM..WARM + TAIL {
        let input = sine100(b * BLOCK, BLOCK, 2);
        let (out, _) = chain_process(&mut chain, &input, false, 1.0);
        y.extend_from_slice(&out[0]);
        let (out_f, _) = chain_process(&mut fresh, &input, false, 1.0);
        y_zero_state.extend_from_slice(&out_f[0]);
    }

    let boundary = WARM * BLOCK;
    let steady = max_first_diff(&y[boundary - 16 * BLOCK..boundary]);
    let across = max_first_diff(&y[boundary - 1..boundary + 4 * BLOCK]);
    assert!(
        across <= 1.1 * steady,
        "transplanted swap stepped: boundary diff {across} > 1.1 x steady {steady}"
    );
    let across_zero = max_first_diff(&y_zero_state[boundary - 1..boundary + 4 * BLOCK]);
    assert!(
        across_zero > 1.1 * steady,
        "zero-state control shows no click ({across_zero} <= 1.1 x {steady}); the bound is not discriminating"
    );
}

#[test]
fn iir_to_fir_swap_is_safe_noop() {
    // Cross-kind swap: no state can carry over (a FIR overlap tail cannot
    // be transplanted). Must not panic; the FIR behaves exactly as fresh.
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![SOS], vec![SOS]], 2, BLOCK)));
    for b in 0..2 {
        chain_process(&mut chain, &make_block(b, 2, BLOCK), false, 1.0);
    }
    let fir = vec![0.5f64, 0.25, -0.125];
    chain.set_correction(Some(build_fir(vec![fir.clone(), fir.clone()], 2, BLOCK)));

    let mut direct = OverlapAddConvolver::new(vec![fir.clone(), fir], BLOCK);
    for b in 2..4 {
        let input = make_block(b, 2, BLOCK);
        let (out, outcome) = chain_process(&mut chain, &input, false, 1.0);
        assert!(outcome.corrected);
        let in64: Vec<Vec<f64>> = input
            .iter()
            .map(|v| v.iter().map(|&s| s as f64).collect())
            .collect();
        let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
        let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
        direct.process(&views, &mut out64);
        for ch in 0..2 {
            for i in 0..BLOCK {
                let expected = downcast_gain_clamp(out64[ch][i], 1.0);
                assert_eq!(
                    out[ch][i].to_bits(),
                    expected.to_bits(),
                    "block {b} ch {ch} sample {i}: FIR after cross-kind swap not fresh"
                );
            }
        }
    }
}

#[test]
fn fir_to_iir_swap_starts_fresh() {
    // Cross-kind swap the other way: the incoming IIR must start from zero
    // state (fresh), not from anything scavenged off the FIR.
    let fir = vec![0.5f64, 0.25, -0.125];
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_fir(vec![fir.clone(), fir], 2, BLOCK)));
    for b in 0..2 {
        chain_process(&mut chain, &make_block(b, 2, BLOCK), false, 1.0);
    }
    chain.set_correction(Some(build_iir(vec![vec![SOS], vec![SOS]], 2, BLOCK)));

    let mut fresh = IIRProcessor::new();
    fresh.set_sos(0, vec![SOS]);
    fresh.set_sos(1, vec![SOS]);
    for b in 2..4 {
        let input = make_block(b, 2, BLOCK);
        let (out, outcome) = chain_process(&mut chain, &input, false, 1.0);
        assert!(outcome.corrected);
        let in64: Vec<Vec<f64>> = input
            .iter()
            .map(|v| v.iter().map(|&s| s as f64).collect())
            .collect();
        let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
        let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
        fresh.process(&views, &mut out64);
        for ch in 0..2 {
            for i in 0..BLOCK {
                let expected = downcast_gain_clamp(out64[ch][i], 1.0);
                assert_eq!(
                    out[ch][i].to_bits(),
                    expected.to_bits(),
                    "block {b} ch {ch} sample {i}: IIR after cross-kind swap not fresh"
                );
            }
        }
    }
}

#[test]
fn channel_count_change_swap_starts_fresh() {
    // Stereo correction -> mono-built correction: the transplant is a
    // no-op across a channel-count change (spec R1-7a), so the incoming
    // processor behaves exactly as fresh and nothing panics.
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![SOS], vec![SOS]], 2, BLOCK)));
    for b in 0..2 {
        chain_process(&mut chain, &make_block(b, 2, BLOCK), false, 1.0);
    }
    chain.set_correction(Some(build_iir(vec![vec![SOS]], 1, BLOCK)));

    let mut fresh = IIRProcessor::new();
    fresh.set_sos(0, vec![SOS]);
    for b in 2..4 {
        let input = make_block(b, 2, BLOCK);
        let (out, outcome) = chain_process(&mut chain, &input, false, 1.0);
        assert!(outcome.corrected);
        let in64: Vec<f64> = input[0].iter().map(|&s| f64::from(s)).collect();
        let mut out64 = vec![vec![0.0f64; BLOCK]];
        fresh.process(&[&in64], &mut out64);
        for i in 0..BLOCK {
            let expected = downcast_gain_clamp(out64[0][i], 1.0);
            assert_eq!(
                out[0][i].to_bits(),
                expected.to_bits(),
                "block {b} ch 0 sample {i}: transplant crossed a channel-count change"
            );
            // The mono processor has no ch1 state: pass-through (the f64
            // round-trip is exact, gain 1.0, well inside the clamp).
            assert_eq!(
                out[1][i].to_bits(),
                input[1][i].to_bits(),
                "block {b} ch 1 sample {i}: expected pass-through"
            );
        }
    }
}

#[test]
fn nan_input_zeroes_poisoned_output_and_self_heals() {
    let mut chain = RealtimeChain::new(2, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![SOS], vec![SOS]], 2, BLOCK)));

    let mut direct = IIRProcessor::new();
    direct.set_sos(0, vec![SOS]);
    direct.set_sos(1, vec![SOS]);
    let gain = 1.0f32;

    // Block 0: clean -- builds real filter state.
    let input = make_block(0, 2, BLOCK);
    let (_, outcome) = chain_process(&mut chain, &input, false, gain);
    assert!(outcome.corrected);
    assert_eq!(outcome.nonfinite_outputs, 0);
    let in64: Vec<Vec<f64>> = input
        .iter()
        .map(|v| v.iter().map(|&s| s as f64).collect())
        .collect();
    let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
    let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
    direct.process(&views, &mut out64);

    // Block 1: one NaN mid-block on ch 0. The DF2T section is poisoned
    // from that sample on; the output backstop must zero exactly the
    // poisoned tail, leave every finite sample bit-identical, and reset
    // the correction state.
    const NAN_AT: usize = 10;
    let mut input = make_block(1, 2, BLOCK);
    input[0][NAN_AT] = f32::NAN;
    let (out, outcome) = chain_process(&mut chain, &input, false, gain);
    assert!(outcome.corrected);
    assert_eq!(
        outcome.nonfinite_outputs as usize,
        BLOCK - NAN_AT,
        "exactly the poisoned tail is sanitized"
    );
    for ch in &out {
        for &s in ch {
            assert!(s.is_finite(), "output must never carry non-finite samples");
        }
    }
    let in64: Vec<Vec<f64>> = input
        .iter()
        .map(|v| v.iter().map(|&s| s as f64).collect())
        .collect();
    let views: Vec<&[f64]> = in64.iter().map(|v| v.as_slice()).collect();
    let mut out64 = vec![vec![0.0f64; BLOCK]; 2];
    direct.process(&views, &mut out64);
    for i in 0..BLOCK {
        if i < NAN_AT {
            let expected = downcast_gain_clamp(out64[0][i], gain);
            assert_eq!(
                out[0][i].to_bits(),
                expected.to_bits(),
                "ch 0 sample {i}: pre-NaN samples must be bit-identical"
            );
        } else {
            assert_eq!(out[0][i].to_bits(), 0.0f32.to_bits(), "ch 0 sample {i}");
        }
        // The clean channel is untouched bit-for-bit.
        let expected = downcast_gain_clamp(out64[1][i], gain);
        assert_eq!(out[1][i].to_bits(), expected.to_bits(), "ch 1 sample {i}");
    }

    // Block 2: clean again -- the backstop's reset self-heals within one
    // block, so output equals a FRESH processor fed only this block
    // (without the reset, the DF2T state would stay NaN forever).
    let input = make_block(2, 2, BLOCK);
    let (out, outcome) = chain_process(&mut chain, &input, false, gain);
    assert!(outcome.corrected);
    assert_eq!(outcome.nonfinite_outputs, 0);
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
                "ch {ch} sample {i}: state not healed after NaN block"
            );
        }
    }
}

#[test]
fn inf_input_on_pass_through_clamps_to_unity() {
    // No correction installed: the pass-through path's clamp already
    // handles +-inf correctly (f32::clamp only fails to sanitize NaN) --
    // pinned so it cannot regress.
    let mut chain = RealtimeChain::new(2, BLOCK);
    let mut input = vec![vec![0.25f32; BLOCK]; 2];
    input[0][3] = f32::INFINITY;
    input[1][7] = f32::NEG_INFINITY;
    let (out, outcome) = chain_process(&mut chain, &input, false, 1.0);
    assert!(!outcome.corrected);
    assert_eq!(
        outcome.nonfinite_outputs, 0,
        "the output guard lives on the corrected path only"
    );
    assert_eq!(out[0][3], 1.0, "+inf clamps to exactly +1.0");
    assert_eq!(out[1][7], -1.0, "-inf clamps to exactly -1.0");
    assert_eq!(out[0][0], 0.25);
    assert_eq!(out[1][0], 0.25);
}

#[test]
fn nonfinite_correction_output_fires_backstop_and_resets() {
    // Fabricated unstable SOS (pole at 10 -- bypasses the design-side
    // guard in-test): the output overflows f32 range within one block.
    // The backstop must keep the block finite and reset the filter so an
    // identical next block repeats identically instead of compounding.
    const UNSTABLE: [f64; 6] = [1.0, 0.0, 0.0, 1.0, -10.0, 0.0];
    let mut chain = RealtimeChain::new(1, BLOCK);
    chain.set_correction(Some(build_iir(vec![vec![UNSTABLE]], 1, BLOCK)));

    let input = vec![vec![0.9f32; BLOCK]];
    let (out0, outcome0) = chain_process(&mut chain, &input, false, 1.0);
    assert!(outcome0.corrected);
    assert!(
        outcome0.nonfinite_outputs > 0,
        "a divergent filter must trip the backstop"
    );
    for &s in &out0[0] {
        assert!(s.is_finite(), "output must never carry non-finite samples");
    }

    // Same block again, post-reset: bit-identical output (state healed,
    // not left saturated at inf).
    let (out1, outcome1) = chain_process(&mut chain, &input, false, 1.0);
    assert_eq!(outcome1.nonfinite_outputs, outcome0.nonfinite_outputs);
    for (i, (a, b)) in out0[0].iter().zip(out1[0].iter()).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "sample {i}: reset did not heal");
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
