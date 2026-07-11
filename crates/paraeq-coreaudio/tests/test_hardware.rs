//! Hardware integration suite — every test is `#[ignore]` because CI has no
//! audio devices. Run locally (TCC-granted terminal) with:
//! `cargo test -p paraeq-coreaudio -- --ignored`

use paraeq_coreaudio::properties;

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
