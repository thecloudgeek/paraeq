//! Thin unsafe wrappers over the CoreAudio HAL + process-tap API.
//! Symbols verified against objc2-core-audio 0.3.2 (2026-07-02).

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::AnyThread; // provides ::alloc(); named AllocAnyThread in some 0.6.x — adjust import if needed
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};

fn addr(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Decode an OSStatus into Ok/Err with the fourcc shown (e.g. '!obj', 'who?').
pub fn check(status: i32, ctx: &str) -> Result<(), String> {
    if status == 0 {
        return Ok(());
    }
    let b = status.to_be_bytes();
    let fourcc: String = b
        .iter()
        .map(|&c| if c.is_ascii_graphic() { c as char } else { '?' })
        .collect();
    Err(format!("{ctx}: OSStatus {status} ('{fourcc}')"))
}

pub fn default_output_device() -> Result<AudioObjectID, String> {
    let mut dev: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            (&addr(kAudioHardwarePropertyDefaultOutputDevice)).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut dev).cast::<c_void>(),
        )
    };
    check(status, "get default output device")?;
    Ok(dev)
}

pub fn device_uid(dev: AudioObjectID) -> Result<CFRetained<CFString>, String> {
    // Out-param is a +1 retained CFStringRef.
    let mut uid: *const CFString = std::ptr::null();
    let mut size = size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            (&addr(kAudioDevicePropertyDeviceUID)).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut uid).cast::<c_void>(),
        )
    };
    check(status, "get device UID")?;
    NonNull::new(uid.cast_mut())
        .map(|p| unsafe { CFRetained::from_raw(p) })
        .ok_or_else(|| "device UID was NULL".into())
}

pub fn nominal_sample_rate(dev: AudioObjectID) -> Result<f64, String> {
    let mut rate: f64 = 0.0;
    let mut size = size_of::<f64>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            (&addr(kAudioDevicePropertyNominalSampleRate)).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut rate).cast::<c_void>(),
        )
    };
    check(status, "get nominal sample rate")?;
    Ok(rate)
}

/// PID -> HAL process object. Returns 0 (kAudioObjectUnknown) if not found.
pub fn translate_pid(pid: i32) -> Result<AudioObjectID, String> {
    let mut obj: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let pid: libc::pid_t = pid;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            (&addr(kAudioHardwarePropertyTranslatePIDToProcessObject)).into(),
            size_of::<libc::pid_t>() as u32,
            (&raw const pid).cast(),
            (&mut size).into(),
            NonNull::from(&mut obj).cast::<c_void>(),
        )
    };
    check(status, "translate PID to process object")?;
    Ok(obj)
}

/// Global stereo tap excluding `excluded` process objects; MutedWhenTapped; private.
/// First AudioHardwareCreateProcessTap call triggers the TCC prompt.
pub fn create_tap(
    excluded: &[AudioObjectID],
) -> Result<(AudioObjectID, Retained<CATapDescription>), String> {
    let nums: Vec<Retained<NSNumber>> =
        excluded.iter().map(|&o| NSNumber::new_u32(o)).collect();
    let arr = NSArray::from_retained_slice(&nums);
    let desc = unsafe {
        CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &arr)
    };
    unsafe {
        desc.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
        desc.setPrivate(true);
        desc.setName(&NSString::from_str("paraeq-tap-spike"));
    }
    let mut tap: AudioObjectID = 0;
    let status = unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap) };
    check(status, "AudioHardwareCreateProcessTap (TCC-gated)")?;
    Ok((tap, desc))
}

pub fn tap_format(tap: AudioObjectID) -> Result<AudioStreamBasicDescription, String> {
    // AudioStreamBasicDescription has no Default impl in objc2-core-audio-types 0.3.2;
    // zero-init explicitly (all fields are plain integer/float, so this is safe and
    // gets fully overwritten by AudioObjectGetPropertyData on success).
    let mut asbd = AudioStreamBasicDescription {
        mSampleRate: 0.0,
        mFormatID: 0,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: 0,
        mBytesPerFrame: 0,
        mChannelsPerFrame: 0,
        mBitsPerChannel: 0,
        mReserved: 0,
    };
    let mut size = size_of::<AudioStreamBasicDescription>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            (&addr(kAudioTapPropertyFormat)).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut asbd).cast::<c_void>(),
        )
    };
    check(status, "get tap format")?;
    Ok(asbd)
}

/// Aggregate with the real output device as sub-device AND the tap in the
/// creation dictionary (iqualize gotcha: adding the tap after creation to an
/// aggregate that has sub-devices delivers zero-filled buffers).
pub fn create_aggregate(
    out_uid: &CFString,
    tap_uuid: &NSString,
) -> Result<AudioObjectID, String> {
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
    let agg_uid = CFString::from_str(&format!("com.paraeq.tap-spike.{}", std::process::id()));
    let name = CFString::from_str("paraeq-tap-spike-agg");

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
            CFBoolean::new(true).as_ref() as &CFType,  // private (required for tapautostart)
            CFBoolean::new(false).as_ref() as &CFType, // stacked
            CFBoolean::new(true).as_ref() as &CFType,  // tapautostart
            sub_devs.as_ref() as &CFType,
            taps.as_ref() as &CFType,
        ],
    );

    let mut agg: AudioObjectID = 0;
    // AudioHardwareCreateAggregateDevice takes the untyped `&CFDictionary` (default
    // Opaque/Opaque params), not the statically-typed `CFDictionary<CFString, CFType>`.
    let dict_ref: &CFDictionary = dict.as_ref();
    let status = unsafe { AudioHardwareCreateAggregateDevice(dict_ref, (&mut agg).into()) };
    check(status, "AudioHardwareCreateAggregateDevice")?;
    Ok(agg)
}
