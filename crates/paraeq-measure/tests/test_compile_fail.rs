//! MS-2's other half: "No other type reaches a `StimulusSink`", proven by the
//! compiler rather than by a reviewer.
//!
//! The risk this test exists for is recorded in the spec's risk table: "Stage 6
//! wires playback without `paraeq-measure`, and the -3 dBFS RMS / REW-maximum
//! sweep becomes real". `generate_sweep` is deliberately unscaled (peak 1.0,
//! -3.01 dBFS RMS) and has zero non-test callers today, so the first playback
//! wiring is the moment of exposure. These cases fail to compile, which is the
//! mitigation: the hot path does not typecheck.
//!
//! Maintenance note: the `.stderr` files pin rustc's diagnostics, and CI tracks
//! floating stable. If a compiler release rewords E0308 or E0603 this test goes
//! red on an unrelated PR — regenerate with `TRYBUILD=overwrite cargo test -p
//! paraeq-measure` and check the diff is only wording.

#[test]
fn a_bare_f64_cannot_reach_a_stimulus_sink() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
