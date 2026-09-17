//! The MS-14 abort envelope, in its one home.
//!
//! `abort_envelope` was lifted verbatim out of `session.rs`'s `ramp_down` so
//! the verification helper child process can use the identical shape through
//! `paraeq-coreaudio`'s re-export rather than through a dependency on this
//! crate. The lift is only safe if the tail is part of the function's
//! contract: `ramp_down` evaluates the envelope over `0..padded` where
//! `padded = ramp_len.div_ceil(block) * block`, which is strictly greater
//! than `ramp_len` for every block size that is not an exact divisor. Two of
//! the tests below are that tail.
//!
//! Test tier: 3 — an analytic invariant (monotone, ends at exactly 0.0) with
//! no library delegate. It is our own fade shape; a numpy transcription would
//! launder our own algebra into a fixture.

use paraeq_measure::{abort_envelope, abort_ramp_len, ABORT_RAMP_MS};

#[test]
fn abort_envelope_of_out_len_zero_is_empty() {
    // The degenerate call returns `[]` rather than panicking: a caller with
    // nothing left to fade must not have to special-case the call.
    assert!(abort_envelope(240, 0).is_empty());
}

#[test]
fn abort_ramp_len_is_at_least_one_at_any_rate() {
    // The `.max(1.0)` branch. At 1 Hz the arithmetic rounds to 0 samples, and
    // a zero-length ramp divides by zero inside the cosine.
    assert_eq!(abort_ramp_len(1.0), 1);
    assert_eq!(abort_ramp_len(0.0), 1);
}

#[test]
fn abort_ramp_len_is_five_milliseconds_at_48k() {
    // 240 samples — the number the verification pass's acoustic abort budget
    // is built from, and the number the child's ramp must agree on.
    assert_eq!(abort_ramp_len(48_000.0), 240);
    assert_eq!(abort_ramp_len(44_100.0), 221);
    assert!((ABORT_RAMP_MS - 5.0).abs() < f64::EPSILON);
}

#[test]
fn the_abort_envelope_is_exactly_zero_at_and_past_ramp_len_minus_one() {
    // The padded-tail falsifier, at the definition site. 48 kHz with a
    // 512-frame block gives ramp_len 240 and padded 512, so 272 of the 512
    // indices are tail. A one-argument envelope would be 240 long and
    // `env[i]` would panic on every one of them.
    let env = abort_envelope(240, 512);
    assert_eq!(env.len(), 512);
    assert!(env[238] > 0.0, "the last sounding coefficient is nonzero");
    for (i, &v) in env.iter().enumerate().skip(239) {
        assert_eq!(v, 0.0, "index {i} must be bit-equal to 0.0, got {v}");
    }
}

#[test]
fn the_abort_envelope_is_monotone_and_reaches_exactly_zero() {
    // Non-increasing over the whole buffer, exactly 0.0 at `ramp_len - 1`,
    // and still exactly 0.0 at the very end: the shipped comment's own
    // reason is that `cos(pi)` rounding is not to be trusted with the final
    // sample, and a fade that ends at 1e-17 is a click at -340 dBFS only in
    // theory — in practice it is a value nobody can assert on.
    let ramp_len = abort_ramp_len(48_000.0);
    let out_len = ramp_len * 3;
    let env = abort_envelope(ramp_len, out_len);
    assert_eq!(env.len(), out_len);
    for pair in env.windows(2) {
        assert!(
            pair[1] <= pair[0],
            "envelope rose from {} to {}",
            pair[0],
            pair[1]
        );
    }
    assert!(env[0] > 0.0 && env[0] < 1.0);
    assert_eq!(env[ramp_len - 1], 0.0);
    assert_eq!(env[out_len - 1], 0.0);
}

#[test]
fn the_abort_envelope_is_the_shipped_inline_expression_bit_for_bit() {
    // The lift's own falsifier: the expression below is `session.rs`'s
    // inline envelope, transcribed. If the extracted function ever stops
    // being that expression, Stage-5-validated abort code has silently
    // changed shape.
    for &(ramp_len, out_len) in &[(240usize, 512usize), (221, 221), (1, 8), (3, 2)] {
        let env = abort_envelope(ramp_len, out_len);
        for (i, &got) in env.iter().enumerate() {
            let want = if i + 1 >= ramp_len {
                0.0
            } else {
                0.5 * (1.0 + (std::f64::consts::PI * (i + 1) as f64 / ramp_len as f64).cos())
            };
            assert_eq!(got, want, "ramp_len={ramp_len} out_len={out_len} i={i}");
        }
    }
}
