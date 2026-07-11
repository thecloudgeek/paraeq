//! Device/tap property getters + the buffer-frame-size setter.
//! Getters are ports of the validated tap spike (spikes/tap-spike/src/ca.rs);
//! `AudioObjectSetPropertyData` verified against objc2-core-audio 0.3.2
//! (generated/AudioHardware.rs:857).

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioTapPropertyFormat,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectSetPropertyData,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFRetained, CFString};

use crate::error::{check, CaError};

fn addr(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

pub fn default_output_device() -> Result<AudioObjectID, CaError> {
    let mut dev: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes of plain-old-data (AudioObjectID = u32).
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

pub fn device_uid(dev: AudioObjectID) -> Result<String, CaError> {
    // Out-param is a +1 retained CFStringRef.
    let mut uid: *const CFString = std::ptr::null();
    let mut size = size_of::<*const CFString>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes (one CFStringRef pointer slot).
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
    let uid = NonNull::new(uid.cast_mut())
        // SAFETY: the HAL hands back a +1 retained CFStringRef; `from_raw`
        // adopts that reference (balanced by CFRetained's Drop release).
        .map(|p| unsafe { CFRetained::from_raw(p) })
        .ok_or_else(|| CaError {
            status: 0,
            fourcc: "????".into(),
            ctx: "get device UID: HAL returned NULL".into(),
        })?;
    Ok(uid.to_string())
}

pub fn nominal_sample_rate(dev: AudioObjectID) -> Result<f64, CaError> {
    let mut rate: f64 = 0.0;
    let mut size = size_of::<f64>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes of plain-old-data (f64).
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
pub fn translate_pid(pid: i32) -> Result<AudioObjectID, CaError> {
    let mut obj: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let pid: libc::pid_t = pid;
    // SAFETY: qualifier points at a live pid_t of the declared size; the out
    // buffer is exactly `size` bytes of plain-old-data (AudioObjectID = u32).
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

pub fn tap_format(tap: AudioObjectID) -> Result<AudioStreamBasicDescription, CaError> {
    // AudioStreamBasicDescription has no Default impl in objc2-core-audio-types
    // 0.3.2; zero-init explicitly (all fields are plain integer/float, so this
    // is safe and gets fully overwritten by AudioObjectGetPropertyData).
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
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes of plain-old-data (the zero-init ASBD).
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

pub fn buffer_frame_size(dev: AudioObjectID) -> Result<u32, CaError> {
    let mut frames: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes of plain-old-data (u32).
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            (&addr(kAudioDevicePropertyBufferFrameSize)).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut frames).cast::<c_void>(),
        )
    };
    check(status, "get buffer frame size")?;
    Ok(frames)
}

pub fn set_buffer_frame_size(dev: AudioObjectID, frames: u32) -> Result<(), CaError> {
    // SAFETY: address points at a live stack local; in_data points at a live
    // u32 of the declared size (read-only for the callee, plain-old-data).
    let status = unsafe {
        AudioObjectSetPropertyData(
            dev,
            (&addr(kAudioDevicePropertyBufferFrameSize)).into(),
            0,
            std::ptr::null(),
            size_of::<u32>() as u32,
            NonNull::from(&frames).cast::<c_void>(),
        )
    };
    check(status, "set buffer frame size")
}
