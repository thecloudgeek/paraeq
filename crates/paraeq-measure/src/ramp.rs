//! The MS-14 abort envelope, in ONE place.
//!
//! Split out of [`session`](crate::session) so the verification helper child
//! process can use the identical shape through `paraeq-coreaudio`'s re-export
//! without depending on this crate (measurement-suite's helper rule: the child
//! "may depend on `paraeq-coreaudio`, must not depend on `paraeq-measure`,
//! `paraeq-decide` or Tauri"). Two envelopes would mean two fade shapes, and
//! measurement-safety § Abort Guards is blunt about what a wrong one costs:
//! "**A hard stop is itself a full-scale click** — precisely the defect § Fade
//! exists to prevent."
//!
//! [`ABORT_RAMP_MS`](crate::session::ABORT_RAMP_MS) stays where it is, in
//! `session.rs` — moving it would touch a file this module deliberately does
//! not, for no gain.
//!
//! Test tier: 3 — an analytic invariant (monotone, ends at exactly 0.0) with
//! no library delegate. It is our own fade shape, and a numpy transcription of
//! it would launder our own algebra into a fixture.

/// Ramp length in samples at `sample_rate_hz`, never zero.
///
/// Verbatim from `session.rs`'s `ramp_down`. The floor at 1 is not decoration:
/// `ramp_len` is the divisor inside [`abort_envelope`]'s cosine, and a rate
/// low enough to round the 5 ms window to zero samples would divide by zero.
pub fn abort_ramp_len(sample_rate_hz: f64) -> usize {
    ((crate::session::ABORT_RAMP_MS / 1000.0) * sample_rate_hz)
        .round()
        .max(1.0) as usize
}

/// The raised cosine falling from ~1 to EXACTLY 0.0 over `ramp_len` samples,
/// materialized over `out_len` coefficients.
///
/// TWO arguments, and this is the whole of the reason. The shipped `ramp_down`
/// evaluates this expression inline over `0..padded`, where
/// `padded = ramp_len.div_ceil(block) * block` — always `>= ramp_len` and
/// strictly greater for every block size that is not an exact divisor (48 kHz
/// with a 512-frame block: `ramp_len` 240, `padded` 512). A one-argument
/// `abort_envelope(ramp_len)` would return `ramp_len` coefficients and `env[i]`
/// would PANIC on that tail, on the abort path. So the tail is this function's
/// contract: **every index at or past `ramp_len - 1` is exactly `0.0`**, which
/// is precisely what the shipped `if i + 1 >= ramp_len { 0.0 }` branch already
/// produces for those `i`. The lift is therefore sample-identical BY
/// CONSTRUCTION, not by argument.
///
/// `out_len == 0` returns an empty vector; `ramp_len` is never 0 because
/// [`abort_ramp_len`] floors it at 1.
///
/// Verbatim from `session.rs`'s `ramp_down`, including the forced final zero —
/// the shipped comment's own reason: "The envelope's last sample is forced to
/// exactly `0.0` rather than trusting `cos(π)` rounding".
///
/// `f64` because the parent's samples are `f64`
/// ([`AssembledStimulus::samples`](crate::stimulus::AssembledStimulus::samples)).
/// A child holding `f32` blocks casts each COEFFICIENT at the multiply — one
/// cast, in one place, named.
pub fn abort_envelope(ramp_len: usize, out_len: usize) -> Vec<f64> {
    (0..out_len)
        .map(|i| {
            if i + 1 >= ramp_len {
                0.0
            } else {
                0.5 * (1.0 + (std::f64::consts::PI * (i + 1) as f64 / ramp_len as f64).cos())
            }
        })
        .collect()
}
