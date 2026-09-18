//! The abort fade, as a thin caller over the one shared envelope.
//!
//! This module declares **no constant and no shape**. Both come through
//! `paraeq_coreaudio`'s re-export of the envelope that the in-process abort
//! already uses, so the two aborts are one fade with one test rather than two
//! that happen to look alike. Two shapes would mean the cross-process abort
//! sounded different from the in-process one, and a fade that is wrong in the
//! wrong direction is the full-scale click the whole fade design exists to
//! prevent.

/// Ramp length in samples at this rate — the parent's rule, called rather than
/// re-derived.
pub fn ramp_len(sample_rate_hz: f64) -> usize {
    paraeq_coreaudio::abort_ramp_len(sample_rate_hz)
}

/// Fade `block` to silence in place.
///
/// The envelope is materialized over `block.len()` coefficients, which is what
/// the two-argument form is for: the block is normally longer than the ramp
/// (it is a whole device block), and every coefficient at or past
/// `ramp_len - 1` is exactly 0.0, so the tail is silence by the function's own
/// contract rather than by this caller's arithmetic.
///
/// Returns the number of samples faded, i.e. `block.len()`.
pub fn apply(block: &mut [f32], sample_rate_hz: f64) -> usize {
    let env = paraeq_coreaudio::abort_envelope(ramp_len(sample_rate_hz), block.len());
    for (sample, coefficient) in block.iter_mut().zip(&env) {
        // The one cast, named: the envelope is f64 because the parent's samples
        // are, and this crate's are f32. One coefficient, one cast, at the
        // multiply — the same treatment the WAV's single f64 -> f32 cast gets
        // on the parent's side.
        *sample = (f64::from(*sample) * coefficient) as f32;
    }
    block.len()
}
