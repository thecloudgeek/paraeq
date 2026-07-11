//! Process-tap + private-aggregate lifecycle: `create_tap`, `create_aggregate`,
//! and the `TapSystem` RAII wrapper. Ported from the validated tap spike
//! (spikes/tap-spike/src/ca.rs + main.rs). No IOProc here — a `TapSystem`
//! alone captures nothing; the IOProc layer composes on top.

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectID, CATapDescription, CATapMuteBehavior,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};

use crate::error::{check, CaError};
use crate::properties;

/// Global stereo tap excluding `excluded` process objects; MutedWhenTapped;
/// private. First AudioHardwareCreateProcessTap call triggers the TCC prompt.
pub fn create_tap(
    excluded: &[AudioObjectID],
) -> Result<(AudioObjectID, Retained<CATapDescription>), CaError> {
    let nums: Vec<Retained<NSNumber>> = excluded.iter().map(|&o| NSNumber::new_u32(o)).collect();
    let arr = NSArray::from_retained_slice(&nums);
    // SAFETY: `arr` is a valid NSArray of NSNumbers (HAL process object IDs);
    // the init consumes a fresh alloc of the correct class.
    let desc = unsafe {
        CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &arr)
    };
    // SAFETY: `desc` is a valid, retained CATapDescription; the setters take
    // plain enum/bool/NSString arguments.
    unsafe {
        desc.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
        desc.setPrivate(true);
        desc.setName(&NSString::from_str("paraeq-engine"));
    }
    let mut tap: AudioObjectID = 0;
    // SAFETY: `desc` is a valid CATapDescription and `tap` a live out-param.
    let status = unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap) };
    check(status, "AudioHardwareCreateProcessTap (TCC-gated)")?;
    Ok((tap, desc))
}

/// Aggregate with the real output device as sub-device AND the tap in the
/// creation dictionary (iqualize gotcha: adding the tap after creation to an
/// aggregate that has sub-devices delivers zero-filled buffers). IsPrivate is
/// required for TapAutoStart.
pub fn create_aggregate(out_uid: &CFString, tap_uuid: &NSString) -> Result<AudioObjectID, CaError> {
    let key = |c: &std::ffi::CStr| CFString::from_str(c.to_str().unwrap());

    // sub-device entry: { uid: <output device UID> }
    let sub_dev: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[&*key(kAudioSubDeviceUIDKey)],
        &[out_uid.as_ref() as &CFType],
    );
    // sub-tap entry: { uid: <CATapDescription UUID string>, drift: true }
    let tap_uid_cf = CFString::from_str(&tap_uuid.to_string());
    let sub_tap: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioSubTapUIDKey),
            &*key(kAudioSubTapDriftCompensationKey),
        ],
        &[
            tap_uid_cf.as_ref() as &CFType,
            CFBoolean::new(true).as_ref() as &CFType,
        ],
    );
    let sub_devs = CFArray::from_objects(&[sub_dev.as_ref() as &CFType]);
    let taps = CFArray::from_objects(&[sub_tap.as_ref() as &CFType]);
    let agg_uid = CFString::from_str(&format!("com.paraeq.engine.{}", std::process::id()));
    let name = CFString::from_str("paraeq-engine-agg");

    let dict: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioAggregateDeviceNameKey),
            &*key(kAudioAggregateDeviceUIDKey),
            &*key(kAudioAggregateDeviceMainSubDeviceKey),
            &*key(kAudioAggregateDeviceIsPrivateKey),
            &*key(kAudioAggregateDeviceIsStackedKey),
            &*key(kAudioAggregateDeviceTapAutoStartKey),
            &*key(kAudioAggregateDeviceSubDeviceListKey),
            &*key(kAudioAggregateDeviceTapListKey),
        ],
        &[
            name.as_ref() as &CFType,
            agg_uid.as_ref() as &CFType,
            out_uid.as_ref() as &CFType,
            CFBoolean::new(true).as_ref() as &CFType, // private (required for tapautostart)
            CFBoolean::new(false).as_ref() as &CFType, // stacked
            CFBoolean::new(true).as_ref() as &CFType, // tapautostart
            sub_devs.as_ref() as &CFType,
            taps.as_ref() as &CFType,
        ],
    );

    let mut agg: AudioObjectID = 0;
    // AudioHardwareCreateAggregateDevice takes the untyped `&CFDictionary`
    // (default Opaque/Opaque params), not `CFDictionary<CFString, CFType>`.
    let dict_ref: &CFDictionary = dict.as_ref();
    // SAFETY: `dict_ref` is a valid composition dictionary kept alive across
    // the call; `agg` is a live out-param.
    let status = unsafe { AudioHardwareCreateAggregateDevice(dict_ref, (&mut agg).into()) };
    check(status, "AudioHardwareCreateAggregateDevice")?;
    Ok(agg)
}

/// RAII owner of the tap + private aggregate pair. Dropping (or explicitly
/// tearing down) destroys the aggregate then the tap — the tail of the
/// teardown invariant (stop → destroy IOProc → destroy aggregate → destroy
/// tap); the IOProc head belongs to the layer above.
pub struct TapSystem {
    pub tap: AudioObjectID,
    pub aggregate: AudioObjectID,
    pub format: AudioStreamBasicDescription,
    pub device: AudioObjectID,
    pub device_uid: String,
    /// Kept alive for the tap's lifetime (the HAL references its UUID).
    #[allow(dead_code)]
    desc: Retained<CATapDescription>,
    torn_down: bool,
}

impl TapSystem {
    /// The spike-validated setup order: default output → uid → rate →
    /// translate own pid → tap → format → aggregate.
    ///
    /// Self-exclusion: if the HAL has no process object for our pid yet
    /// (`translate_pid` returns 0), sleep 200 ms and retry once; if still 0,
    /// proceed with an empty exclusion list and warn (feedback risk is
    /// spike-documented, non-fatal).
    pub fn create() -> Result<TapSystem, CaError> {
        let device = properties::default_output_device()?;
        let device_uid = properties::device_uid(device)?;
        let rate = properties::nominal_sample_rate(device)?;
        log::debug!("default output: id={device} uid={device_uid} rate={rate}");

        let own = own_process_object()?;
        let excluded = if own != 0 { vec![own] } else { vec![] };
        if excluded.is_empty() {
            log::warn!(
                "own process not in HAL registry after retry — no self-exclusion (watch for feedback)"
            );
        }

        let (tap, desc) = create_tap(&excluded)?;
        // From here on the tap exists: destroy it before surfacing any error,
        // so a failed create never leaks a live tap.
        let format = match properties::tap_format(tap) {
            Ok(f) => f,
            Err(e) => {
                destroy_tap_logged(tap);
                return Err(e);
            }
        };
        // SAFETY: `desc` is a valid, retained CATapDescription; UUID() and
        // UUIDString() return retained Foundation objects.
        let tap_uuid = unsafe { desc.UUID().UUIDString() };
        let out_uid = CFString::from_str(&device_uid);
        let aggregate = match create_aggregate(&out_uid, &tap_uuid) {
            Ok(a) => a,
            Err(e) => {
                destroy_tap_logged(tap);
                return Err(e);
            }
        };
        log::debug!("tap system up: tap={tap} aggregate={aggregate} device={device}");

        Ok(TapSystem {
            tap,
            aggregate,
            format,
            device,
            device_uid,
            desc,
            torn_down: false,
        })
    }

    /// Destroy the aggregate, then the tap. Idempotent; every OSStatus
    /// failure is logged AND collected — never silently discarded. We cannot
    /// recover from a failed destroy, but it must be visible.
    pub fn teardown(&mut self) -> Vec<CaError> {
        if self.torn_down {
            return Vec::new();
        }
        self.torn_down = true;
        let mut errors = Vec::new();

        // SAFETY: `aggregate` was created by AudioHardwareCreateAggregateDevice
        // and is destroyed at most once (guarded by `torn_down`).
        let status = unsafe { AudioHardwareDestroyAggregateDevice(self.aggregate) };
        if let Err(e) = check(status, "AudioHardwareDestroyAggregateDevice") {
            log::error!("teardown: {e}");
            errors.push(e);
        }

        // SAFETY: `tap` was created by AudioHardwareCreateProcessTap and is
        // destroyed at most once (guarded by `torn_down`).
        let status = unsafe { AudioHardwareDestroyProcessTap(self.tap) };
        if let Err(e) = check(status, "AudioHardwareDestroyProcessTap") {
            log::error!("teardown: {e}");
            errors.push(e);
        }

        errors
    }
}

impl Drop for TapSystem {
    fn drop(&mut self) {
        // Errors are already logged inside teardown; nothing more a Drop can do.
        let _ = self.teardown();
    }
}

/// `translate_pid` for our own pid, with the spike-gap retry: the HAL may not
/// have registered a freshly launched process yet.
fn own_process_object() -> Result<AudioObjectID, CaError> {
    let pid = std::process::id() as i32;
    let own = properties::translate_pid(pid)?;
    if own != 0 {
        return Ok(own);
    }
    log::warn!("HAL has no process object for pid {pid} yet; retrying in 200 ms");
    std::thread::sleep(std::time::Duration::from_millis(200));
    properties::translate_pid(pid)
}

/// Best-effort tap destroy on a failed `create` path (logged, not returned —
/// the original error is the one the caller needs).
fn destroy_tap_logged(tap: AudioObjectID) {
    // SAFETY: `tap` was just created by AudioHardwareCreateProcessTap and has
    // not been destroyed.
    let status = unsafe { AudioHardwareDestroyProcessTap(tap) };
    if let Err(e) = check(status, "AudioHardwareDestroyProcessTap (create cleanup)") {
        log::error!("create cleanup: {e}");
    }
}
