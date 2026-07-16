//! `SweepLevel::new` is the sole constructor (MS-2). The newtype's field is
//! private, so a caller cannot mint a level that skipped the caps table — the
//! "runtime `if` at the playback site" the decisions log rejected as bypassable
//! is bypassable precisely because nothing stops the next caller from building
//! the value another way. Here, something does.

use paraeq_dsp::targets::TransducerClass;
use paraeq_measure::SweepLevel;

fn main() {
    let _wrapped = SweepLevel(-3.01);
    // The sole legal path, for contrast: it refuses this level for every class.
    let _checked = SweepLevel::new(-3.01, TransducerClass::InEar);
}
