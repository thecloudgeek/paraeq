//! One envelope, two processes.

use paraeq_coreaudio::{abort_envelope, abort_ramp_len};
use paraeq_stimulus::ramp;

#[test]
fn the_in_process_and_cross_process_aborts_share_one_envelope() {
    // The property, stated plainly: the fade this process applies IS the fade
    // the in-process abort applies. Not "looks like", not "was copied from" —
    // the same function, reached through the platform crate's re-export, so
    // there is one shape with one test rather than two that drift.
    //
    // `n > ramp_len` on purpose. A device block is longer than the 5 ms ramp,
    // so most of the buffer is TAIL, and the tail is the half a
    // one-argument envelope would have got wrong.
    for rate in [44_100.0f64, 48_000.0] {
        let ramp_len = abort_ramp_len(rate);
        assert_eq!(ramp::ramp_len(rate), ramp_len, "one ramp-length rule");

        let n = ramp_len * 2 + 37;
        let mut block = vec![1.0f32; n];
        assert_eq!(ramp::apply(&mut block, rate), n);

        let env = abort_envelope(ramp_len, n);
        for (i, (&applied, &coefficient)) in block.iter().zip(&env).enumerate() {
            // Cast for cast: the envelope is f64 because the parent's samples
            // are, and this crate's are f32, so the comparison has to apply the
            // same single cast the multiply does.
            assert_eq!(
                applied,
                (1.0f64 * coefficient) as f32,
                "rate {rate} index {i}"
            );
        }
        assert!(
            block[ramp_len..].iter().all(|&v| v == 0.0),
            "the shared tail is silence, and it is the function's contract rather than this \
             caller's arithmetic"
        );
    }
}

#[test]
fn a_fade_over_a_block_shorter_than_the_ramp_is_still_monotone() {
    // A stimulus that runs out mid-fade: the block is shorter than the ramp, so
    // no coefficient reaches zero, and what matters is that nothing rises.
    let rate = 48_000.0;
    let ramp_len = ramp::ramp_len(rate);
    let mut block = vec![1.0f32; ramp_len / 4];
    ramp::apply(&mut block, rate);
    for pair in block.windows(2) {
        assert!(pair[1] <= pair[0], "the fade rose: {pair:?}");
    }
    assert!(block[0] < 1.0, "the fade starts falling immediately");
}

#[test]
fn an_empty_block_fades_to_nothing_without_panicking() {
    let mut block: Vec<f32> = Vec::new();
    assert_eq!(ramp::apply(&mut block, 48_000.0), 0);
}

#[test]
fn the_child_declares_no_envelope_shape_of_its_own() {
    // The share runs one way: the envelope lives in the parent and this crate
    // calls it. A local cosine here would be a second fade shape in a process
    // whose whole job is to fade the same way the parent does.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ramp.rs"))
        .expect("ramp.rs is readable");
    let code: String = src
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in ["cos(", "PI", "const "] {
        assert!(
            !code.contains(forbidden),
            "the child's ramp module spells its own shape: found `{forbidden}`"
        );
    }
}
