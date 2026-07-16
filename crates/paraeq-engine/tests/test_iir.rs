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
#[should_panic(expected = "output[ch] length")]
fn iir_rejects_mis_sized_output() {
    let mut p = IIRProcessor::new();
    p.set_sos(0, vec![[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]]);
    let input = [1.0f64; 64];
    let mut out = vec![Vec::new()]; // wrong: empty, old contract allowed this
    p.process(&[&input], &mut out);
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

// --- adopt_state_from (spec R1-7a: click-free coefficient swaps) ---

const BLOCK: usize = 64;
/// Distinct stable sections (|poles| < 1) so section identity and order are
/// observable in the transplant tests.
const S1: [f64; 6] = [0.2, 0.3, 0.1, 1.0, -0.5, 0.25];
const S2: [f64; 6] = [0.4, -0.2, 0.05, 1.0, 0.3, 0.2];
const S3: [f64; 6] = [0.9, 0.1, -0.3, 1.0, -0.2, 0.15];
const S4: [f64; 6] = [0.6, 0.2, 0.1, 1.0, 0.1, 0.05];
const S5: [f64; 6] = [0.8, -0.4, 0.2, 1.0, -0.35, 0.3];

/// Phase-continuous deterministic test signal.
fn sine(start: usize, len: usize) -> Vec<f64> {
    (start..start + len)
        .map(|n| (n as f64 * 0.13).sin() * 0.4)
        .collect()
}

fn assert_bits_eq(actual: &[f64], expected: &[f64], ctx: &str) {
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            a.to_bits(),
            e.to_bits(),
            "{ctx}: sample {i} diverged ({a} vs {e})"
        );
    }
}

fn iir(sos: &[[f64; 6]]) -> IIRProcessor {
    let mut p = IIRProcessor::new();
    p.set_sos(0, sos.to_vec());
    p
}

#[test]
fn adopt_state_from_identical_sos_continues_bit_exactly() {
    let sos = [S1, S2, S3];
    let mut reference = iir(&sos);
    let mut a = iir(&sos);
    let mut out_r = vec![vec![0.0; BLOCK]];
    let mut out = vec![vec![0.0; BLOCK]];
    for b in 0..2 {
        let x = sine(b * BLOCK, BLOCK);
        reference.process(&[&x], &mut out_r);
        a.process(&[&x], &mut out);
    }
    // Swap: a freshly built identical-coefficient processor adopts A's
    // state and must continue the stream as if no swap had happened.
    let mut b_new = iir(&sos);
    b_new.adopt_state_from(&a);
    for b in 2..4 {
        let x = sine(b * BLOCK, BLOCK);
        reference.process(&[&x], &mut out_r);
        b_new.process(&[&x], &mut out);
        assert_bits_eq(&out[0], &out_r[0], &format!("block {b}"));
    }
}

#[test]
fn adopt_state_from_carries_per_channel_state() {
    // Different cascades per channel: the transplant must not cross wires.
    let mut reference = IIRProcessor::new();
    reference.set_sos(0, vec![S1, S2]);
    reference.set_sos(1, vec![S3]);
    let mut a = IIRProcessor::new();
    a.set_sos(0, vec![S1, S2]);
    a.set_sos(1, vec![S3]);
    let mut out_r = vec![vec![0.0; BLOCK], vec![0.0; BLOCK]];
    let mut out = vec![vec![0.0; BLOCK], vec![0.0; BLOCK]];
    for b in 0..2 {
        let x0 = sine(b * BLOCK, BLOCK);
        let x1 = sine(b * BLOCK + 17, BLOCK);
        reference.process(&[&x0, &x1], &mut out_r);
        a.process(&[&x0, &x1], &mut out);
    }
    let mut b_new = IIRProcessor::new();
    b_new.set_sos(0, vec![S1, S2]);
    b_new.set_sos(1, vec![S3]);
    b_new.adopt_state_from(&a);
    for b in 2..4 {
        let x0 = sine(b * BLOCK, BLOCK);
        let x1 = sine(b * BLOCK + 17, BLOCK);
        reference.process(&[&x0, &x1], &mut out_r);
        b_new.process(&[&x0, &x1], &mut out);
        assert_bits_eq(&out[0], &out_r[0], &format!("block {b} ch0"));
        assert_bits_eq(&out[1], &out_r[1], &format!("block {b} ch1"));
    }
}

#[test]
fn adopt_state_from_zeroes_sections_beyond_old_cascade() {
    // 3 -> 5 sections. The incoming processor is deliberately dirtied first
    // so the test proves sections beyond the old cascade are ZEROED, not
    // merely left at a fresh processor's zeros.
    let mut a = iir(&[S1, S2, S3]);
    let mut out = vec![vec![0.0; BLOCK]];
    for b in 0..2 {
        a.process(&[&sine(b * BLOCK, BLOCK)], &mut out);
    }
    let mut b_new = iir(&[S1, S2, S3, S4, S5]);
    b_new.process(&[&sine(1000, BLOCK)], &mut out); // dirty all five sections
    b_new.adopt_state_from(&a);
    // Expected: the first 3 sections carry A's history (they saw exactly
    // the signal a standalone [S1,S2,S3] cascade saw); the last 2 start
    // zeroed, i.e. behave as a FRESH [S4,S5] cascade fed the head's output.
    let mut head = iir(&[S1, S2, S3]);
    for b in 0..2 {
        head.process(&[&sine(b * BLOCK, BLOCK)], &mut out);
    }
    let mut tail = iir(&[S4, S5]);
    let mut out_h = vec![vec![0.0; BLOCK]];
    let mut out_t = vec![vec![0.0; BLOCK]];
    for b in 2..4 {
        let x = sine(b * BLOCK, BLOCK);
        b_new.process(&[&x], &mut out);
        head.process(&[&x], &mut out_h);
        let h = out_h[0].clone();
        tail.process(&[&h], &mut out_t);
        assert_bits_eq(&out[0], &out_t[0], &format!("block {b}"));
    }
}

#[test]
fn adopt_state_from_discards_state_beyond_new_cascade() {
    // 5 -> 3 sections: no panic; the surviving first 3 sections continue
    // exactly as a standalone [S1,S2,S3] cascade with the same history.
    let mut a = iir(&[S1, S2, S3, S4, S5]);
    let mut reference = iir(&[S1, S2, S3]);
    let mut out = vec![vec![0.0; BLOCK]];
    let mut out_r = vec![vec![0.0; BLOCK]];
    for b in 0..2 {
        let x = sine(b * BLOCK, BLOCK);
        a.process(&[&x], &mut out);
        reference.process(&[&x], &mut out_r);
    }
    let mut b_new = iir(&[S1, S2, S3]);
    b_new.adopt_state_from(&a);
    for b in 2..4 {
        let x = sine(b * BLOCK, BLOCK);
        b_new.process(&[&x], &mut out);
        reference.process(&[&x], &mut out_r);
        assert_bits_eq(&out[0], &out_r[0], &format!("block {b}"));
    }
}
