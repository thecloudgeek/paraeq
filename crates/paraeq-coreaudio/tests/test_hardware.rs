//! Hardware integration suite — every test is `#[ignore]` because CI has no
//! audio devices. Run locally (TCC-granted terminal) with:
//! `cargo test -p paraeq-coreaudio -- --ignored`
//!
//! Two tests here need a human at the keyboard and say so on stdout. Their
//! `#[ignore]` reasons both start with `manual:`, but the suite's skip filter
//! matches test NAMES, so an unattended sweep needs both
//! `--skip manual_ --skip rate_change_`; otherwise
//! `rate_change_keeps_the_band_at_its_designed_frequency` sits waiting for a
//! rate change nobody is there to make.
//!
//! # MS-6's witness: only the `true` branch is reachable on hardware
//!
//! [`backend_witness_tracks_start_and_stop`] below, and
//! `test_measure_hardware.rs`'s full-session run, assert that the witness
//! reads `true` over a live tap and `false` with no tap. **The
//! `self_excluded == false`-with-a-tap-up branch — MS-6's actual refusal
//! condition — is mock-only, and permanently so.** Reaching it requires
//! `properties::translate_pid(getpid())` to return 0 on a Mac where this
//! process is registered with the HAL: a launch race that cannot be
//! reproduced on demand, with no API to provoke it and no fault injection
//! short of editing `tap.rs`. No hardware test asserts that branch and none
//! can be written. Its coverage is, and will stay, mock-driven —
//! `crates/paraeq-measure/tests/test_session.rs::ms6_refuses_before_any_sample_is_emitted`
//! and `::ms6_rechecks_at_the_sweep_gate` over a mock `TapStatus`, plus the
//! convention tests in `tests/test_exclusion_witness.rs`. Read a green
//! `--ignored` run as "the witness tracks a real tap", never as "the refusal
//! path has been exercised".

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use objc2_core_audio::{kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectSystemObject};
use paraeq_coreaudio::backend::TapBackend;
use paraeq_coreaudio::devices;
use paraeq_coreaudio::ioproc::{IoCallback, IoProcHandle};
use paraeq_coreaudio::listeners::PropertyListener;
use paraeq_coreaudio::properties;
use paraeq_coreaudio::tap::TapSystem;
use paraeq_coreaudio::volume;
use paraeq_dsp::biquad;
use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};
use paraeq_engine::backend::AudioBackend;
use paraeq_engine::chain::RealtimeChain;
use paraeq_engine::controller::{
    CorrectionConfig, EngineCommand, EngineConfig, EngineHandle, EngineState,
};
use paraeq_engine::shared::{links, RtProcessor, RtShared};
use paraeq_engine::status::{EngineStatus, WatchdogConfig};
use paraeq_measure::TapStatus;

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

/// Enumeration must include the current default output and every entry must
/// carry a non-empty name and uid. Needs no TCC grant (property reads are not
/// capture); `#[ignore]` for CI-safety (no HAL there).
#[test]
#[ignore = "requires audio hardware"]
fn output_enumeration_includes_default() {
    let outputs = devices::list_output_devices().expect("list_output_devices");
    assert!(!outputs.is_empty(), "at least one output device expected");
    for d in &outputs {
        assert!(!d.name.is_empty(), "device name must be non-empty: {d:?}");
        assert!(!d.uid.is_empty(), "device uid must be non-empty: {d:?}");
    }
    let default = properties::default_output_device().expect("default_output_device");
    let default_uid = properties::device_uid(default).expect("device_uid");
    assert!(
        outputs.iter().any(|d| d.uid == default_uid),
        "default output {default_uid:?} not in enumeration {outputs:?}"
    );
}

/// Setting the default output to the CURRENT default is a no-op switch that
/// never disrupts the dev machine's audio; an unknown uid must be rejected
/// before any HAL call. Needs no TCC grant; `#[ignore]` for CI-safety.
#[test]
#[ignore = "requires audio hardware"]
fn set_default_output_roundtrip_noop() {
    let default = properties::default_output_device().expect("default_output_device");
    let uid = properties::device_uid(default).expect("device_uid");
    devices::set_default_output_device(&uid).expect("set default to the same uid must Ok");
    assert!(
        devices::set_default_output_device("bogus-uid-nope").is_err(),
        "unknown uid must Err"
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

// ───────────── Phase A: the MS-6 witness, the lease, the rate change ─────────
// Three claims the post-merge Phase-A work cannot make for itself, because
// their subjects have no software stand-in: a real `translate_pid` lookup
// feeding a real tap description, a real tap surviving a real silent
// measurement, and a real device changing its sample rate underneath a live
// correction. Each mechanism is already pinned mock-driven in
// `crates/paraeq-engine/tests/test_controller.rs`; what follows is the
// hardware half only, and nothing below is a substitute for those.

/// Poll published snapshots until `pred` holds; `None` on timeout.
///
/// Reads `state()` rather than `subscribe()` because these tests care about
/// the CURRENT state, and a bounded subscriber channel drops updates when the
/// subscriber falls behind ([`EngineHandle::subscribe`]).
fn wait_until(
    handle: &EngineHandle,
    timeout: Duration,
    pred: impl Fn(&EngineState) -> bool,
) -> Option<Arc<EngineState>> {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = handle.state();
        if pred(&snap) {
            return Some(snap);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// MS-6's witness over a REAL tap: `false` before `start`, `true` while the
/// tap is up, `false` again after `stop` — the `AudioBackend::self_excluded`
/// contract ("`false` whenever nothing is running") measured rather than
/// asserted by construction. `tests/test_exclusion_witness.rs` pins the same
/// convention against a hand-written witness; this is the only place the value
/// comes from `TapSystem::create` and a real
/// [`properties::translate_pid`] lookup. See the module header for why the
/// `false`-with-a-tap-up branch is not, and cannot be, covered here.
///
/// Drives [`TapBackend`] directly rather than through [`EngineHandle::spawn`]:
/// `start`/`stop` are the two edges the contract is written about, and the
/// controller puts ticks, renegotiation and a watchdog between them
/// (`tap_backend_full_engine_boot` covers that path).
///
/// Briefly mutes system audio (MutedWhenTapped). Every reading is taken and
/// the backend stopped BEFORE anything is asserted, so a failed assert cannot
/// leave a live tap behind — the same restore-before-assert shape as
/// `buffer_frame_size_set_and_restore_roundtrip`. `TapBackend`'s field
/// declaration order makes its bare `Drop` the backstop if a panic lands
/// earlier anyway.
#[test]
#[ignore = "requires audio hardware"]
fn backend_witness_tracks_start_and_stop() {
    // The minimum a backend can be started with: this test never pumps a
    // block, it only needs something to move into the IOProc closure.
    let (_control, rt) = links(4);
    let processor = RtProcessor::new(
        Arc::new(RtShared::default()),
        rt,
        RealtimeChain::new(2, 512),
    );

    let mut backend = TapBackend::new();
    let witness = backend.exclusion_witness();
    let before = witness.self_excluded();

    let stream = backend.start(processor, None).expect("TapBackend::start");
    let during = witness.self_excluded();
    let during_via_trait = backend.self_excluded();

    backend.stop().expect("TapBackend::stop");
    let after = witness.self_excluded();
    backend.stop().expect("stop is idempotent");
    let after_again = witness.self_excluded();

    eprintln!("WITNESS: before={before} during={during} after={after} (stream {stream:?})");
    assert!(
        !before,
        "a backend that has never started witnesses nothing"
    );
    assert!(
        during,
        "a registered process must be excluded from its own tap; `false` here is MS-6's \
         refusal condition — every measurement would refuse SelfExclusionUnavailable"
    );
    assert_eq!(
        during, during_via_trait,
        "the clone the caller kept before the move and `AudioBackend::self_excluded` must \
         be one fact, not two"
    );
    assert!(
        !after,
        "after teardown there is no tap, so nothing is excluded"
    );
    assert!(
        !after_again,
        "an idempotent second stop must not resurrect the witness"
    );
}

/// **Owner: listen.** The wizard's fail-open scenario against a real tap
/// (wizard § The fail-open watchdog): a measurement plays on the *untapped*
/// measurement route, so ParaEQ's tap — which excludes ParaEQ's own process,
/// which is MS-6's entire point — captures nothing but zeros for the whole
/// run. Without a lease the watchdog reads that as the TCC silent-failure
/// signature and tears the tap down mid-sweep.
///
/// What the owner should hear: the system goes quiet when the engine starts
/// (MutedWhenTapped), STAYS quiet across the whole leased stretch — well past
/// the fail-open window — and comes back by itself a few seconds after the
/// lease is released. The return is fail-open doing its job; the silence
/// before it is the lease doing its job. Nothing is played, deliberately:
/// silence is the signature under test.
///
/// Both windows are shortened from their production defaults (5 s engage,
/// 15 s fail-open), which keeps the system muted for ~15 s instead of the
/// minute-plus the defaults would take.
/// Shortening cannot make this pass falsely: the falsifier is the
/// `AutoDisabledNoInput` wait AFTER the release, which is the same mechanism
/// on the same clock — if the lease had not been suspending anything, the
/// engine would already have disabled during the sleep.
#[test]
#[ignore = "requires audio hardware; mutes system audio for ~15 s"]
fn lease_keeps_the_tap_alive_across_a_silent_measurement() {
    const ENGAGE_MS: u64 = 2_000;
    const FAIL_OPEN_MS: u64 = 3_000;

    let handle = EngineHandle::spawn(
        TapBackend::new(),
        EngineConfig {
            fail_open_after_ms: Some(FAIL_OPEN_MS),
            watchdog: WatchdogConfig {
                engage_tolerance_ms: ENGAGE_MS,
                ..WatchdogConfig::default()
            },
            ..EngineConfig::default()
        },
    );

    // A lease is only meaningful over a live tap, and `self_excluded` is the
    // published field that names the TAP specifically -- it is read straight
    // off the backend, whereas `stream` describes the controller's session.
    let up = wait_until(&handle, Duration::from_secs(10), |s| s.self_excluded)
        .expect("no live tap within 10 s — there is nothing for a lease to keep alive");
    eprintln!("LEASE: tap up: {up:?}");

    let lease = handle
        .acquire_measurement_lease()
        .expect("a fresh engine has no outstanding lease");
    println!(">>> the system is muted now, and must STAY muted while the lease is held.");

    // The lease suspends the auto-disable, never the diagnosis. Waiting for
    // `NoInputDetected` first is what gives the sleep below its teeth:
    // fail-open can fire from no other state, so a run that never reached it
    // would prove nothing at all.
    let silent = wait_until(&handle, Duration::from_secs(10), |s| {
        matches!(s.status, EngineStatus::NoInputDetected { .. })
    })
    .expect(
        "engine never reported NoInputDetected — something is playing into the tap; \
         stop all playback and rerun",
    );
    eprintln!("LEASE: reported silent (the lease must not suppress this): {silent:?}");

    let held_for = Duration::from_millis(FAIL_OPEN_MS * 3);
    std::thread::sleep(held_for);
    let leased = handle.state();

    // Release BEFORE asserting: a failed assert must never leave the
    // fail-open watchdog suspended over a muted system.
    drop(lease);
    println!(">>> lease released — audio should come back on its own within a few seconds.");

    assert!(
        !matches!(leased.status, EngineStatus::AutoDisabledNoInput { .. }),
        "the lease did not suspend fail-open: the engine auto-disabled {held_for:?} into a \
         leased measurement, which is the tap being torn down mid-sweep: {leased:?}"
    );
    assert!(
        leased.stream.is_some(),
        "the session is gone under the lease: {leased:?}"
    );
    assert!(
        leased.self_excluded,
        "the tap is gone under the lease — a session polling MS-6 here would refuse: \
         {leased:?}"
    );

    let disabled = wait_until(&handle, Duration::from_secs(15), |s| {
        matches!(s.status, EngineStatus::AutoDisabledNoInput { .. })
    })
    .expect(
        "fail-open never re-armed after the release — the lease left the system muted with \
         no auto-recovery, which is the failure D-20 calls the highest-severity one",
    );
    eprintln!("LEASE: auto-disabled after release: {disabled:?}");
    assert!(
        !disabled.self_excluded,
        "a torn-down tap witnesses nothing: {disabled:?}"
    );
    assert!(
        disabled.stream.is_none(),
        "the aggregate must be gone with the session: {disabled:?}"
    );

    drop(handle);
}

/// R1-6's hardware checklist item, verbatim (engine-hardening § R1-6, Tests):
/// "With a +12 dB 1 kHz band live, change the device's rate in Audio MIDI
/// Setup; sweep the analyzer and confirm the band is still at 1 kHz, not
/// 919 Hz."
///
/// **Manual.** Run it alone and watch stdout:
///
/// ```sh
/// cargo test -p paraeq-coreaudio --test test_hardware -- --ignored --nocapture \
///     rate_change_keeps_the_band_at_its_designed_frequency
/// ```
///
/// What the software half asserts, once the human has changed the rate: the
/// retained `Peq` intent was RE-DERIVED at the rate the backend just reported
/// — `correction_rate_mismatch` stays `None` and `auto_preamp_db` comes back
/// at −12 dB, which only a rebuilt cascade can produce. The `Iir`/`Fir` arms
/// carry baked coefficients and refuse instead (flat pass-through plus the
/// mismatch flag); that half needs no hardware and is pinned in
/// `crates/paraeq-engine/tests/test_controller.rs`.
///
/// What the owner's analyzer confirms: the two frequencies this prints. The
/// band must still sit at 1 kHz; the detuned frequency printed beside it is
/// where stale coefficients would have put it, and is the number to be sure
/// you do NOT see.
///
/// Fail-open is disabled for this run (`fail_open_after_ms: None`) because
/// nothing is playing while the human works in Audio MIDI Setup, and a tap
/// torn down by the watchdog would end the test before the rate change lands.
/// The test does not restore the device's rate — it has no API to, and the
/// change was the human's; it prints a reminder instead.
#[test]
#[ignore = "manual: requires changing the output device's sample rate in Audio MIDI Setup"]
fn rate_change_keeps_the_band_at_its_designed_frequency() {
    const FC_HZ: f64 = 1_000.0;
    const GAIN_DB: f64 = 12.0;
    const Q: f64 = 1.0;

    let handle = EngineHandle::spawn(
        TapBackend::new(),
        EngineConfig {
            fail_open_after_ms: None,
            ..EngineConfig::default()
        },
    );
    let started = wait_until(&handle, Duration::from_secs(10), |s| s.stream.is_some())
        .expect("engine never reported a stream within 10 s");
    let before = started.stream.clone().expect("just matched");

    let band = EQBand {
        filter_type: FilterType::Peaking,
        fc: FC_HZ,
        gain_db: GAIN_DB,
        q: Q,
    };
    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Peq {
        bands: vec![vec![band.clone()]],
        design_rate: before.sample_rate,
    }));
    let installed = wait_until(&handle, Duration::from_secs(5), |s| {
        s.auto_preamp_db.is_some()
    })
    .expect("the band never installed (no auto_preamp_db published within 5 s)");
    assert_eq!(installed.correction.as_deref(), Some("peq:1-band"));
    assert_eq!(
        installed.correction_rate_mismatch, None,
        "the band was refused at its own design rate: {installed:?}"
    );

    println!(
        ">>> a +{GAIN_DB} dB {FC_HZ} Hz band is live at {} Hz on '{}'.",
        before.sample_rate, before.device_uid
    );
    println!(
        ">>> now change THAT device's sample rate in Audio MIDI Setup (any other rate) \
         within 120 s..."
    );

    let changed = wait_until(&handle, Duration::from_secs(120), |s| {
        s.stream
            .as_ref()
            .is_some_and(|st| st.sample_rate != before.sample_rate)
    })
    .expect(
        "no rate change reached the engine within 120 s. Either nobody changed the rate, \
         or the change was made on a device that is not the current default output, or \
         the tap's reported format did not follow the device — the last of those is a \
         finding, not a mistake, and `properties::nominal_sample_rate` on the device will \
         say which it was",
    );
    let after = changed.stream.clone().expect("just matched");

    // Where the band is now, and where reusing the old rate's coefficients
    // would have put it. A peaking biquad's magnitude peak sits exactly at its
    // designed w0 = 2*pi*fc/rate, so running coefficients designed at
    // `before` through a stream at `after` moves the peak to
    // fc * after/before — the spec's 919 Hz for 48000 -> 44100.
    let freqs: Vec<f64> = (0..=8_000)
        .map(|i| FC_HZ / 8.0 * 64.0f64.powf(f64::from(i) / 8_000.0))
        .collect();
    let redesigned = ParametricEQ {
        bands: vec![band.clone()],
        sample_rate: after.sample_rate,
    }
    .frequency_response(&freqs);
    let stale = biquad::sos_frequency_response_db(
        &[band.to_sos(before.sample_rate)],
        &freqs,
        after.sample_rate,
    );
    let peak_hz = |mag_db: &[f64]| -> f64 {
        let at = mag_db
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .expect("non-empty grid");
        freqs[at]
    };
    let designed_peak_hz = peak_hz(&redesigned);
    let stale_peak_hz = peak_hz(&stale);

    eprintln!(
        "RATE CHANGE: {} Hz -> {} Hz; redesigned band peaks at {designed_peak_hz:.1} Hz, \
         stale coefficients would peak at {stale_peak_hz:.1} Hz",
        before.sample_rate, after.sample_rate
    );
    println!(
        ">>> on the analyzer the band must STILL sit at {FC_HZ:.0} Hz. \
         {stale_peak_hz:.0} Hz is the wrong answer to look for."
    );

    // The published half of "still at 1 kHz": the retained intent was
    // re-derived at the new rate. A refusal would publish the mismatch and
    // clear the preamp (flat pass-through) instead.
    assert_eq!(
        changed.correction_rate_mismatch, None,
        "the Peq intent was refused at the new rate instead of being re-derived: {changed:?}"
    );
    assert_eq!(changed.correction.as_deref(), Some("peq:1-band"));
    let preamp = changed
        .auto_preamp_db
        .expect("a rebuilt correction publishes the preamp it is applying");
    assert!(
        (f64::from(preamp) + GAIN_DB).abs() < 0.5,
        "auto-preamp is {preamp} dB after the rate change; a rebuilt +{GAIN_DB} dB band \
         must come back at -{GAIN_DB} dB, so this cascade is not the one that was designed"
    );
    assert!(
        (designed_peak_hz - FC_HZ).abs() < FC_HZ * 0.01,
        "the band redesigned at {} Hz peaks at {designed_peak_hz} Hz, not {FC_HZ} Hz",
        after.sample_rate
    );

    println!(">>> done — you can put the device's sample rate back.");
    handle.send(EngineCommand::Disable);
    wait_for_stopped(&handle, Duration::from_secs(5));
    drop(handle);
}
