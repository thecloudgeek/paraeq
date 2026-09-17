//! MS-6's producer half (measurement-safety `MS-6`)
//! — the tap self-exclusion witness `paraeq-measure` polls before it emits a
//! single sample.
//!
//! What is pinned here is the *convention*, not the HAL: a witness reads
//! `false` whenever no tap is live (before start, after stop, mid-rebuild), and
//! every clone of a witness is the same cell, so the handle taken before
//! `TapBackend` is moved into `EngineHandle::spawn` keeps tracking the backend
//! afterwards. Those two properties are what make `TapStatus`'s "current state,
//! polled at every gate" contract (`crates/paraeq-measure/src/seam.rs:96-98`)
//! true of this implementation.
//!
//! Test tier: none of the four oracle tiers
//! (docs/specs/2026-07-15-measurement-suite-design.md § The four tiers) applies —
//! they scope DSP math against a numerical oracle, and this is platform FFI with
//! no oracle to match. The convention tests below need no hardware; the one test
//! that asserts what the HAL actually reported is `#[ignore]`, per the crate's
//! existing convention (tests/test_hardware.rs).

use paraeq_coreaudio::backend::{ExclusionWitness, TapBackend};
use paraeq_coreaudio::properties;
use paraeq_coreaudio::tap::TapSystem;
use paraeq_engine::backend::AudioBackend;
use paraeq_measure::TapStatus;

/// No tap, no witness: the invariant is not *in force*, so it is not reported
/// as being in force. `stop()` on a never-started backend is a documented
/// no-op and must leave the witness where it found it — that same `stop()` is
/// what unwinds a failed `start`, so this also pins the failed-start path.
#[test]
fn exclusion_witness_is_false_before_start_and_after_stop() {
    let mut backend = TapBackend::new();
    let witness = backend.exclusion_witness();
    assert!(
        !witness.self_excluded(),
        "a backend that has never started witnesses nothing"
    );

    backend
        .stop()
        .expect("stop on a never-started backend is a no-op");
    assert!(
        !witness.self_excluded(),
        "after teardown there is no tap, so nothing is excluded"
    );
}

/// The witness must survive the move of `TapBackend` into
/// `EngineHandle::spawn`: the caller keeps a clone, the backend keeps its own,
/// and both are the same cell. Written through the backend's own publish point
/// and through `stop()` so the test cannot pass on a per-clone copy.
#[test]
fn exclusion_witness_clones_share_state() {
    let mut backend = TapBackend::new();
    let taken_before_the_move = backend.exclusion_witness();

    // Stands in for `start_inner`'s publish; a separately-taken handle.
    backend.exclusion_witness().set(true);
    assert!(
        taken_before_the_move.self_excluded(),
        "a clone must observe a write made through another clone"
    );

    backend.stop().expect("stop is idempotent");
    assert!(
        !taken_before_the_move.self_excluded(),
        "the backend's own clear must be visible through the clone the caller kept"
    );
}

/// Default is the safe direction, and `TapBackend` gets its witness from it.
#[test]
fn a_default_witness_is_false() {
    assert!(!ExclusionWitness::default().self_excluded());
}

/// The one assertion about what the HAL actually did. On a rig where the app is
/// registered, `translate_pid(getpid())` is non-zero and that object id went
/// into the tap description's exclusion list; the witness must agree with the
/// lookup it was derived from, and teardown must report no error.
#[test]
#[ignore = "requires audio hardware"]
fn tap_system_reports_self_exclusion() {
    let own = properties::translate_pid(std::process::id() as i32).expect("translate_pid");
    let mut system = TapSystem::create().expect("tap system");

    assert_eq!(
        system.self_excluded,
        own != 0,
        "the witness records exactly the own-process lookup that fed the tap description"
    );
    assert!(
        system.self_excluded,
        "a registered process must be excluded from its own tap; \
         false here is MS-6's refusal condition (the fail-open path fired)"
    );

    let errors = system.teardown();
    assert!(errors.is_empty(), "teardown: {errors:?}");
}
