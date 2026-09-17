//! Device/tap property getters + the buffer-frame-size setter.
//! Getters are ports of the validated tap spike (spikes/tap-spike/src/ca.rs);
//! `AudioObjectSetPropertyData` verified against objc2-core-audio 0.3.2
//! (generated/AudioHardware.rs:857).

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioHardwarePropertyTranslateUIDToDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioTapPropertyFormat, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioStreamBasicDescription};
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

pub fn default_input_device() -> Result<AudioObjectID, CaError> {
    let mut dev: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes of plain-old-data (AudioObjectID = u32).
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            (&addr(kAudioHardwarePropertyDefaultInputDevice)).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut dev).cast::<c_void>(),
        )
    };
    check(status, "get default input device")?;
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

/// Device UID -> AudioObjectID. Returns 0 (kAudioObjectUnknown) if no device
/// has that UID — the `translate_pid` convention.
pub fn translate_uid_to_device(uid: &str) -> Result<AudioObjectID, CaError> {
    let mut dev: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let cf = CFString::from_str(uid);
    // The qualifier is the CFStringRef VALUE (one pointer slot), passed by
    // address — the same shape translate_pid uses for its pid_t.
    let cf_ref: *const CFString = &*cf;
    // SAFETY: qualifier points at a live CFStringRef of the declared size
    // (`cf` outlives the call); the out buffer is exactly `size` bytes of
    // plain-old-data (AudioObjectID = u32).
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            (&addr(kAudioHardwarePropertyTranslateUIDToDevice)).into(),
            size_of::<*const CFString>() as u32,
            (&raw const cf_ref).cast(),
            (&mut size).into(),
            NonNull::from(&mut dev).cast::<c_void>(),
        )
    };
    check(status, "translate UID to device")?;
    Ok(dev)
}

/// Total input channels across the device's input streams
/// (kAudioDevicePropertyStreamConfiguration, input scope). 0 means the device
/// captures nothing — it is not usable as a mic.
pub fn input_stream_channel_count(dev: AudioObjectID) -> Result<u32, CaError> {
    stream_channel_count(
        dev,
        kAudioObjectPropertyScopeInput,
        "get input stream configuration",
    )
}

/// Total output channels across the device's output streams
/// (kAudioDevicePropertyStreamConfiguration, output scope). 0 means the device
/// renders nothing — refuse rather than open an IOProc that writes into
/// nowhere.
///
/// The twin of [`input_stream_channel_count`], and deliberately not a
/// convenience: the verification helper echoes this count in its `ready` line
/// and refuses a `--channel N` at or above it BEFORE it plays a sample. Reading
/// the channel count off the first callback instead would arrive after `ready`
/// has been emitted and after the parent has committed, i.e. after the decision
/// it is supposed to inform.
pub fn output_channel_count(dev: AudioObjectID) -> Result<u32, CaError> {
    stream_channel_count(
        dev,
        kAudioObjectPropertyScopeOutput,
        "get output stream configuration",
    )
}

/// The shared walk of `kAudioDevicePropertyStreamConfiguration`'s
/// `AudioBufferList`. The SELECTOR is the part that does not change between the
/// two scopes, which is why one body serves both and why the output twin
/// carries no new HAL risk.
fn stream_channel_count(dev: AudioObjectID, scope: u32, ctx: &str) -> Result<u32, CaError> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyStreamConfiguration,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size: u32 = 0;
    // SAFETY: address/out pointers reference live stack locals.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            dev,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    check(status, &format!("{ctx} size"))?;
    if (size as usize) < size_of::<AudioBufferList>() {
        return Ok(0);
    }
    // 8-byte-aligned backing for the variable-length AudioBufferList (u32
    // fields + a pointer per AudioBuffer).
    let mut backing: Vec<u64> = vec![0; size as usize / 8 + 1];
    let mut got = size;
    // SAFETY: the out buffer is `backing`, which is at least `size` bytes,
    // 8-byte aligned, and outlives the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            (&address).into(),
            0,
            std::ptr::null(),
            (&mut got).into(),
            NonNull::new(backing.as_mut_ptr().cast::<c_void>()).expect("vec ptr"),
        )
    };
    check(status, ctx)?;
    let list = backing.as_ptr().cast::<AudioBufferList>();
    // SAFETY: the HAL wrote a valid AudioBufferList into `backing`.
    let declared = unsafe { (*list).mNumberBuffers } as usize;
    // Trust-but-bound: never read past what the HAL actually wrote (`got`
    // bytes, of which the first AudioBuffer is inside AudioBufferList).
    let fitting =
        (got as usize).saturating_sub(size_of::<AudioBufferList>()) / size_of::<AudioBuffer>() + 1;
    let mut channels: u32 = 0;
    for i in 0..declared.min(fitting) {
        // SAFETY: i is within both the declared entry count and the bytes
        // the HAL wrote; `&raw const` keeps the base pointer's provenance
        // over the trailing entries (the ioproc::raw_buffer idiom).
        let buf = unsafe {
            (&raw const (*list).mBuffers)
                .cast::<AudioBuffer>()
                .add(i)
                .read()
        };
        channels += buf.mNumberChannels;
    }
    Ok(channels)
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
