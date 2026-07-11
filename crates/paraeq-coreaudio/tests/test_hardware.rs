//! Hardware integration suite — every test is `#[ignore]` because CI has no
//! audio devices. Run locally (TCC-granted terminal) with:
//! `cargo test -p paraeq-coreaudio -- --ignored`

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use paraeq_coreaudio::ioproc::{IoCallback, IoProcHandle};
use paraeq_coreaudio::properties;
use paraeq_coreaudio::tap::TapSystem;

#[test]
#[ignore = "requires audio hardware + TCC grant"]
fn default_output_device_is_nonzero() {
    let dev = properties::default_output_device().expect("default_output_device");
    assert_ne!(dev, 0, "default output device must be a real AudioObjectID");
}

#[test]
#[ignore = "requires audio hardware + TCC grant"]
fn device_uid_is_non_empty() {
    let dev = properties::default_output_device().expect("default_output_device");
    let uid = properties::device_uid(dev).expect("device_uid");
    assert!(!uid.is_empty(), "device UID must be non-empty");
}

#[test]
#[ignore = "requires audio hardware + TCC grant"]
fn nominal_sample_rate_is_positive() {
    let dev = properties::default_output_device().expect("default_output_device");
    let rate = properties::nominal_sample_rate(dev).expect("nominal_sample_rate");
    assert!(
        rate > 0.0,
        "nominal sample rate must be positive, got {rate}"
    );
}

#[test]
#[ignore = "requires audio hardware + TCC grant"]
fn buffer_frame_size_set_and_restore_roundtrip() {
    let dev = properties::default_output_device().expect("default_output_device");
    let original = properties::buffer_frame_size(dev).expect("read original");
    let target = if original == 512 { 256 } else { 512 };

    properties::set_buffer_frame_size(dev, target).expect("set target");
    let after_set = properties::buffer_frame_size(dev).expect("read back target");

    // Restore BEFORE asserting so a failed assert doesn't leave the device
    // configured with the test value.
    properties::set_buffer_frame_size(dev, original).expect("restore original");
    let restored = properties::buffer_frame_size(dev).expect("read back original");

    assert_eq!(
        after_set, target,
        "set_buffer_frame_size did not take effect"
    );
    assert_eq!(
        restored, original,
        "original buffer frame size not restored"
    );
}

/// Lifecycle roundtrip for the tap + private aggregate. Briefly mutes system
/// audio (MutedWhenTapped) — a sub-second blip is expected and acceptable.
/// Without the TCC grant creation still succeeds (silent-zeros mode), so this
/// validates lifecycle, not capture.
#[test]
#[ignore = "requires audio hardware + TCC grant"]
fn tap_system_create_teardown_roundtrip() {
    let mut sys = TapSystem::create().expect("TapSystem::create");
    assert!(
        sys.format.mSampleRate > 0.0,
        "tap format must report a sample rate, got {}",
        sys.format.mSampleRate
    );
    assert!(
        sys.format.mChannelsPerFrame >= 1,
        "tap format must report at least one channel"
    );
    assert_ne!(sys.tap, 0, "tap object id must be nonzero");
    assert_ne!(sys.aggregate, 0, "aggregate object id must be nonzero");
    assert!(!sys.device_uid.is_empty(), "device UID must be captured");

    let errors = sys.teardown();
    assert!(errors.is_empty(), "explicit teardown errored: {errors:?}");

    let again = sys.teardown();
    assert!(again.is_empty(), "second teardown must be a no-op");
    drop(sys);

    // A fresh create → drop cycle must also work (Drop runs teardown).
    let sys2 = TapSystem::create().expect("second TapSystem::create");
    assert!(sys2.format.mSampleRate > 0.0);
    drop(sys2);
}

/// Exercises the real IOProc trampoline: a counting closure on a TapSystem
/// aggregate must be invoked continuously while started (94 cb/s expected at
/// 512 frames / 48 kHz — spike doc line 16; > 50 per second is the loose
/// gate). Teardown runs in the invariant order: stop IOProc BEFORE TapSystem
/// teardown (stop → destroy IOProc → destroy aggregate → destroy tap).
/// Briefly mutes system audio (MutedWhenTapped) — a blip is expected.
///
/// IO cycles on the aggregate only run while the output device is actually
/// doing IO: on an idle system (nothing playing) the HAL delivers NO
/// callbacks at all (verified against the unmodified spike, which also gets
/// 0 cb/s idle and 94 cb/s during playback). The test therefore drives its
/// own playback via `afplay` — it does NOT need the TCC capture grant
/// (callbacks flow with zero samples in silent-zeros mode too).
#[test]
#[ignore = "requires audio hardware"]
fn ioproc_on_tap_aggregate_delivers_callbacks() {
    let mut sys = TapSystem::create().expect("TapSystem::create");

    let count = Arc::new(AtomicU64::new(0));
    let cb_count = Arc::clone(&count);
    let cb: IoCallback = Box::new(move |block| {
        // Touch the views so the whole IoBlock path is exercised, but do no
        // work: this is the counting closure, not a DSP test.
        let _ = block.input.len();
        let _ = block.output.len();
        cb_count.fetch_add(1, Ordering::Relaxed);
    });

    let mut io = IoProcHandle::register(sys.aggregate, cb).expect("IoProcHandle::register");
    io.start().expect("IoProcHandle::start");

    // Keep the output device doing IO for the duration of the measurement.
    let playing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let playing_flag = Arc::clone(&playing);
    let player = std::thread::spawn(move || {
        while playing_flag.load(Ordering::Relaxed) {
            let _ = std::process::Command::new("afplay")
                .arg("/System/Library/Sounds/Submarine.aiff")
                .status();
        }
    });

    // The tap can also take seconds to engage after AudioDeviceStart (spike
    // doc line 27; Task 6's 5 s engage tolerance encodes the same fact).
    // Wait up to 10 s for the first callback, THEN measure the steady-state
    // rate over 1 s.
    let started_at = std::time::Instant::now();
    let engage_deadline = started_at + Duration::from_secs(10);
    while count.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < engage_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let engaged = count.load(Ordering::Relaxed);
    eprintln!(
        "ioproc engaged after {:?} ({engaged} callbacks seen)",
        started_at.elapsed()
    );
    std::thread::sleep(Duration::from_secs(1));
    let n = count.load(Ordering::Relaxed) - engaged;

    // Invariant order head: stop the IOProc before tearing the TapSystem down.
    io.stop();
    io.stop(); // idempotent: second stop must be a no-op

    playing.store(false, Ordering::Relaxed);
    player.join().expect("afplay loop thread");

    assert!(engaged > 0, "IOProc never engaged within 10 s");
    assert!(
        n > 50,
        "expected > 50 callbacks in the 1 s after engage, got {n}"
    );

    let errors = sys.teardown();
    assert!(errors.is_empty(), "teardown errored: {errors:?}");
}
