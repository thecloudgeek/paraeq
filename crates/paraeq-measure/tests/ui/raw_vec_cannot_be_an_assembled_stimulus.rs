//! MS-3/MS-4's type-discipline half: holding an `AssembledStimulus` is the
//! proof that the post-fade assertion set ran on these samples at this level.
//! A raw `Vec<f64>` fabricated at a call site — un-faded, un-DC-blocked,
//! unverified — must not be able to masquerade as one. The fields are
//! private and there is no public constructor; this must not compile.

use paraeq_measure::{AssembledStimulus, StimulusKind, SweepLevel, TransducerClass};

fn main() {
    let level = SweepLevel::new(-20.0, TransducerClass::InEar).unwrap();
    let _ = AssembledStimulus {
        kind: StimulusKind::Sweep,
        level,
        sample_rate_hz: 48_000,
        samples: vec![2.0; 48_000],
    };
}
