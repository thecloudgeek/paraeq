//! Hardware integration suite — every test is `#[ignore]` because CI has no
//! audio devices. Run locally (TCC-granted terminal) with:
//! `cargo test -p paraeq-coreaudio -- --ignored`

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use objc2_core_audio::{kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectSystemObject};
use paraeq_coreaudio::backend::TapBackend;
use paraeq_coreaudio::ioproc::{IoCallback, IoProcHandle};
use paraeq_coreaudio::listeners::PropertyListener;
use paraeq_coreaudio::properties;
use paraeq_coreaudio::tap::TapSystem;
use paraeq_coreaudio::volume;
use paraeq_engine::controller::{EngineCommand, EngineConfig, EngineHandle, EngineState};
use paraeq_engine::status::EngineStatus;

/// Keeps the system rendering audio for the guard's lifetime by looping
/// `afplay` on a builtin sound. The tap aggregate's IOProc delivers NO
/// callbacks while the system is idle (hardware finding 2026-07-11: 0 cb/s
/// idle, ~94 cb/s during playback), so any test that counts callbacks MUST
/// drive playback itself. Does not need the TCC capture grant — callbacks
/// flow (with zero samples) in silent-zeros mode too.
struct Playback {
    playing: Arc<AtomicBool>,
    player: Option<std::thread::JoinHandle<()>>,
}

impl Playback {
    fn start() -> Playback {
        let playing = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&playing);
        let player = std::thread::spawn(move || {
            while flag.load(Ordering::Relaxed) {
                let _ = std::process::Command::new("afplay")
                    .arg("/System/Library/Sounds/Submarine.aiff")
                    .status();
            }
        });
        Playback {
            playing,
            player: Some(player),
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        if let Some(player) = self.player.take() {
            player.join().expect("afplay loop thread");
        }
    }
}

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

/// MS-5: the level ladder reads back the actual hardware gain before it solves
/// a sweep level. A device exposing no volume control at all (HDMI, many USB
/// DACs, aggregates) is legal and common, so this asserts the API is HONEST
/// either way — it does not assert a control exists.
#[test]
#[ignore = "requires audio hardware"]
fn output_volume_reports_a_control_or_honestly_reports_none() {
    let dev = properties::default_output_device().expect("default_output_device");
    match volume::output_volume(dev).expect("output_volume") {
        Some(vol) => {
            eprintln!(
                "VOLUME: scalar {:.3}, master={}, settable={}, elements={:?}",
                vol.scalar(),
                vol.is_master(),
                vol.is_settable(),
                vol.elements()
            );
            assert!(
                !vol.elements().is_empty(),
                "Some(_) must carry at least one element"
            );
            assert!(
                (0.0..=1.0).contains(&vol.scalar()),
                "HAL volume scalar must be 0.0..=1.0, got {}",
                vol.scalar()
            );
        }
        None => eprintln!("VOLUME: default output exposes no volume control (ladder must refuse)"),
    }
}

/// Set + restore round-trip. Skips rather than fails on a device with no
/// settable control, because that is a legal device (the `FixedMaxVolume`
/// case). Restores BEFORE asserting so a failed assert cannot leave the system
/// at the test's volume — the same shape as
/// `buffer_frame_size_set_and_restore_roundtrip`, and the same invariant the
/// measurement session's restore path needs.
#[test]
#[ignore = "requires audio hardware; briefly changes system volume"]
fn output_volume_set_and_restore_roundtrip() {
    let dev = properties::default_output_device().expect("default_output_device");
    let Some(original) = volume::output_volume(dev).expect("output_volume") else {
        eprintln!("SKIP: default output exposes no volume control");
        return;
    };
    if !original.is_settable() {
        eprintln!("SKIP: default output's volume control is read-only (FixedMaxVolume case)");
        return;
    }

    let target = if original.scalar() > 0.5 { 0.25 } else { 0.75 };
    volume::set_output_volume(dev, target).expect("set target");
    let after_set = volume::output_volume(dev)
        .expect("read back target")
        .expect("control still present");

    let errors = volume::restore_output_volume(dev, &original);
    let restored = volume::output_volume(dev)
        .expect("read back original")
        .expect("control still present");

    assert!(errors.is_empty(), "restore errored: {errors:?}");
    // Devices quantize the scalar, so the set lands within a step, not exactly.
    assert!(
        (after_set.scalar() - target).abs() < 0.1,
        "set_output_volume did not take effect: wanted {target}, read {}",
        after_set.scalar()
    );
    assert!(
        (restored.scalar() - original.scalar()).abs() < 0.01,
        "original volume not restored: was {}, now {}",
        original.scalar(),
        restored.scalar()
    );
}

/// The scalar guard must fire on a REAL device too (the unit tests reach no
/// HAL): a NaN volume is a safety issue, not a numerics nit (spec § Non-Finite).
#[test]
#[ignore = "requires audio hardware"]
fn set_output_volume_refuses_bad_scalars_on_a_real_device() {
    let dev = properties::default_output_device().expect("default_output_device");
    for bad in [f32::NAN, f32::INFINITY, -0.5, 1.5] {
        assert!(
            volume::set_output_volume(dev, bad).is_err(),
            "accepted out-of-range scalar {bad}"
        );
    }
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
    let playback = Playback::start();

    // The tap can also take seconds to engage after AudioDeviceStart (spike
    // doc line 27; Task 6's 5 s engage tolerance encodes the same fact).
    // Wait up to 10 s for the first callback, THEN measure the steady-state
    // rate over 1 s.
    let started_at = Instant::now();
    let engage_deadline = started_at + Duration::from_secs(10);
    while count.load(Ordering::Relaxed) == 0 && Instant::now() < engage_deadline {
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

    drop(playback);

    assert!(engaged > 0, "IOProc never engaged within 10 s");
    assert!(
        n > 50,
        "expected > 50 callbacks in the 1 s after engage, got {n}"
    );

    let errors = sys.teardown();
    assert!(errors.is_empty(), "teardown errored: {errors:?}");
}

/// R1-5 regression: the aggregate composes its sub-device with
/// `kAudioSubDeviceInputChannelsKey: 0` (tap.rs::create_aggregate), so the
/// IOProc's input buffer list must carry the tap's channels and nothing
/// else — no mic buffers from the physical output's own input streams.
/// Trivially green on a mic-less output; the meaningful run needs a
/// mic-capable default output selected (AirPods, USB headset). Does not
/// need the TCC capture grant (buffer layout flows in silent-zeros mode).
#[test]
#[ignore = "requires audio hardware; meaningful only with a mic-capable default output"]
fn aggregate_input_carries_only_tap_channels() {
    let mut sys = TapSystem::create().expect("TapSystem::create");
    let tap_channels = sys.format.mChannelsPerFrame as u64;

    let count = Arc::new(AtomicU64::new(0));
    let max_in_channels = Arc::new(AtomicU64::new(0));
    let cb_count = Arc::clone(&count);
    let cb_max = Arc::clone(&max_in_channels);
    let cb: IoCallback = Box::new(move |block| {
        let mut total: u64 = 0;
        for (_, buf_channels) in block.input.buffers() {
            total += buf_channels.max(1) as u64;
        }
        cb_max.fetch_max(total, Ordering::Relaxed);
        cb_count.fetch_add(1, Ordering::Relaxed);
    });

    let mut io = IoProcHandle::register(sys.aggregate, cb).expect("IoProcHandle::register");
    io.start().expect("IoProcHandle::start");
    let playback = Playback::start();

    // Same engage tolerance as the callback-rate test: up to 10 s for the
    // first callback, then 1 s of steady-state layout observation.
    let engage_deadline = Instant::now() + Duration::from_secs(10);
    while count.load(Ordering::Relaxed) == 0 && Instant::now() < engage_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let engaged = count.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_secs(1));

    io.stop();
    drop(playback);

    assert!(engaged > 0, "IOProc never engaged within 10 s");
    let max_seen = max_in_channels.load(Ordering::Relaxed);
    eprintln!("input layout: max {max_seen} channels/callback (tap format: {tap_channels})");
    assert!(
        max_seen <= tap_channels,
        "aggregate delivered {max_seen} input channels but the tap format has only \
         {tap_channels} — the sub-device's input streams leaked into the aggregate"
    );

    let errors = sys.teardown();
    assert!(errors.is_empty(), "teardown errored: {errors:?}");
}

/// Registration/removal round-trip on the system object. Does not need the
/// TCC grant (property listeners are not capture); `#[ignore]` anyway for
/// CI-safety and parity with the suite — CI has no HAL at all.
#[test]
#[ignore = "requires audio hardware"]
fn property_listener_register_drop_roundtrip() {
    let (tx, rx) = mpsc::channel();
    let listener = PropertyListener::watch(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyDefaultOutputDevice,
        tx,
    )
    .expect("PropertyListener::watch");
    // Drop runs AudioObjectRemovePropertyListener + frees the ctx box; a
    // failure is logged inside Drop, and a broken removal would crash or
    // fire-after-free below.
    drop(listener);

    // A second watch on the same (object, selector) must work after removal.
    let (tx2, rx2) = mpsc::channel();
    let listener2 = PropertyListener::watch(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyDefaultOutputDevice,
        tx2,
    )
    .expect("second PropertyListener::watch");
    drop(listener2);

    drop(rx);
    drop(rx2);
}

/// Manual-interaction variant: with the listener registered on the
/// default-output selector, a HUMAN must switch the output device (System
/// Settings → Sound, or Control Center) within 10 s; the test asserts the
/// event arrives. Run it alone with:
/// `cargo test -p paraeq-coreaudio --test test_hardware -- --ignored manual_default_output_switch --nocapture`
/// It is excluded from the normal ignored sweep via `--skip manual_`.
#[test]
#[ignore = "manual: requires switching output device"]
fn manual_default_output_switch_delivers_event() {
    let (tx, rx) = mpsc::channel();
    let _listener = PropertyListener::watch(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyDefaultOutputDevice,
        tx,
    )
    .expect("PropertyListener::watch");

    println!(">>> switch your output device in System Settings within 10 s...");
    let event = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("no default-output event within 10 s — did you switch devices?");
    assert_eq!(event.selector, kAudioHardwarePropertyDefaultOutputDevice);
    assert_eq!(event.object, kAudioObjectSystemObject as u32);
}

/// Poll snapshots until one proves the IOProc has run. Callback flow is
/// visible in the snapshot as `latency_ms > 0` (the rt side stores the
/// out−in sample-time delta on every callback; it is 0 until the first one)
/// or `input_peak > 0` (real capture, only with the TCC grant). This
/// terminal is in TCC silent-zeros mode, so `Running` is NOT reachable —
/// assert on callbacks/lifecycle, never on capture content.
fn wait_for_callbacks(handle: &EngineHandle, timeout: Duration) -> Arc<EngineState> {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = handle.state();
        if let EngineStatus::Failed { reason } = &snap.status {
            panic!("engine failed while waiting for callbacks: {reason}");
        }
        if snap.latency_ms.unwrap_or(0.0) > 0.0 || snap.input_peak > 0.0 {
            return snap;
        }
        assert!(
            Instant::now() < deadline,
            "no callbacks within {timeout:?} (latency/peak never updated): {snap:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll snapshots until the engine reports `Stopped` with the stream gone.
fn wait_for_stopped(handle: &EngineHandle, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = handle.state();
        if snap.status == EngineStatus::Stopped {
            assert!(
                snap.stream.is_none(),
                "stopped engine must drop its stream info: {snap:?}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "engine did not stop within {timeout:?}: {snap:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Full production stack: `EngineHandle::spawn(TapBackend)` boots the tap +
/// aggregate + IOProc + listeners, telemetry reaches the published
/// snapshots, `Disable` tears everything down cleanly. Mutes system audio
/// while running (MutedWhenTapped; silent-zeros mode passes zeros through).
#[test]
#[ignore = "requires audio hardware"]
fn tap_backend_full_engine_boot() {
    let playback = Playback::start();
    let handle = EngineHandle::spawn(TapBackend::new(), EngineConfig::default());

    let snap = wait_for_callbacks(&handle, Duration::from_secs(10));
    eprintln!("boot snapshot: {snap:?}");

    let stream = snap.stream.as_ref().expect("stream info after start");
    assert!(
        stream.sample_rate >= 8000.0,
        "sane sample rate, got {}",
        stream.sample_rate
    );
    assert!(
        (1..=2).contains(&stream.channels),
        "tap channels must be 1 or 2, got {}",
        stream.channels
    );
    assert!(stream.buffer_frames > 0, "effective buffer frames reported");
    assert!(!stream.device_uid.is_empty(), "device UID captured");
    assert!(
        !matches!(
            snap.status,
            EngineStatus::Stopped | EngineStatus::Failed { .. }
        ),
        "live session must not report Stopped/Failed: {:?}",
        snap.status
    );

    handle.send(EngineCommand::Disable);
    wait_for_stopped(&handle, Duration::from_secs(5));
    drop(handle); // Shutdown + join; StopGuard re-runs the idempotent stop
    drop(playback);
}

/// The out−in sample-time delta must surface as `latency_ms` in the
/// snapshots — the number that feeds Task 13's buffer-size tuning table
/// (spike baseline: 62.3 ms at defaults). Printed clearly for the report.
#[test]
#[ignore = "requires audio hardware"]
fn latency_delta_is_reported() {
    let playback = Playback::start();

    // Default buffer frames.
    let handle = EngineHandle::spawn(TapBackend::new(), EngineConfig::default());
    let snap = wait_for_callbacks(&handle, Duration::from_secs(10));
    let latency = snap.latency_ms.expect("latency in live snapshot");
    let stream = snap.stream.as_ref().expect("stream info");
    println!(
        "LATENCY default: {latency:.2} ms ({} frames @ {} Hz)",
        stream.buffer_frames, stream.sample_rate
    );
    assert!(latency > 0.0, "latency_ms must be positive, got {latency}");
    handle.send(EngineCommand::Disable);
    wait_for_stopped(&handle, Duration::from_secs(5));
    drop(handle);

    // Requested 128 frames (feeds the Task 13 tuning table too).
    std::thread::sleep(Duration::from_millis(500)); // let the HAL settle
    let config = EngineConfig {
        requested_buffer_frames: Some(128),
        ..EngineConfig::default()
    };
    let handle = EngineHandle::spawn(TapBackend::new(), config);
    let snap = wait_for_callbacks(&handle, Duration::from_secs(10));
    let latency = snap.latency_ms.expect("latency in live snapshot");
    let stream = snap.stream.as_ref().expect("stream info");
    println!(
        "LATENCY requested-128: {latency:.2} ms (effective {} frames @ {} Hz)",
        stream.buffer_frames, stream.sample_rate
    );
    assert!(latency > 0.0, "latency_ms must be positive, got {latency}");
    handle.send(EngineCommand::Disable);
    wait_for_stopped(&handle, Duration::from_secs(5));
    drop(handle);
    drop(playback);
}
