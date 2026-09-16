//! Output-device enumeration + the system-default-output setter.
//!
//! New unsafe FFI for the desktop shell's output-device picker (spec's "output
//! pick", spec:89). Every selector/function below is verified against the
//! objc2-core-audio 0.3.2 generated bindings; line numbers cite
//! `~/.cargo/registry/src/*/objc2-core-audio-0.3.2/src/generated/AudioHardware.rs`:
//!   - `kAudioHardwarePropertyDevices`            AudioHardware.rs:1030
//!   - `kAudioHardwarePropertyDefaultOutputDevice` AudioHardware.rs:1034
//!   - `kAudioDevicePropertyStreamConfiguration`  AudioHardware.rs:1319
//!   - `kAudioObjectPropertyName`                 AudioHardware.rs:256
//!   - `kAudioObjectPropertyScopeOutput`          AudioHardware.rs:224
//!   - `AudioObjectGetPropertyDataSize`           AudioHardware.rs:776
//!   - `AudioObjectGetPropertyData`               AudioHardware.rs:815
//!   - `AudioObjectSetPropertyData`               AudioHardware.rs:856
//!
//! `AudioBuffer`/`AudioBufferList` layout is objc2-core-audio-types 0.3.2
//! CoreAudioBaseTypes.rs:104/132 (C variable-length array: `mNumberBuffers`
//! `AudioBuffer` entries follow contiguously).

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioDevicePropertyStreamConfiguration, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectSetPropertyData,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::{CFRetained, CFString};

use crate::error::{check, CaError};
use crate::properties::device_uid;

/// UID prefix of ParaEQ's own private aggregate (`create_aggregate`,
/// tap.rs:76 builds `com.paraeq.engine.<pid>`). Defensive: never offer our own
/// tap aggregate as a user-selectable output target.
const OWN_AGGREGATE_UID_PREFIX: &str = "com.paraeq.engine.";

/// A selectable output device: its live HAL object id plus display name and
/// persistent UID. No serde here — src-tauri maps this to its own serializable
/// struct so the crate stays serde-free (crate boundary).
#[derive(Clone, Debug, PartialEq)]
pub struct OutputDevice {
    pub id: AudioObjectID,
    pub name: String,
    pub uid: String,
}

fn addr(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Every audio device known to the HAL (input, output, or both).
fn all_device_ids() -> Result<Vec<AudioObjectID>, CaError> {
    let address = addr(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );

    let mut size: u32 = 0;
    // SAFETY: address/size pointers reference live stack locals; no qualifier
    // (0 / NULL). The call only writes the byte count into `size`.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            kAudioObjectSystemObject as u32,
            (&address).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
        )
    };
    check(status, "get devices list size")?;

    let count = size as usize / size_of::<AudioObjectID>();
    let mut ids: Vec<AudioObjectID> = vec![0; count];
    if count == 0 {
        return Ok(ids);
    }

    let mut io_size = size;
    // SAFETY: address/size pointers reference live stack locals; the out buffer
    // is `ids`, exactly `io_size` bytes of plain-old-data (AudioObjectID = u32)
    // backed by an allocation of `count` elements. `as_mut_slice` carries
    // whole-allocation provenance.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            (&address).into(),
            0,
            std::ptr::null(),
            (&mut io_size).into(),
            NonNull::from(ids.as_mut_slice()).cast::<c_void>(),
        )
    };
    check(status, "get devices list")?;

    // The HAL may report fewer bytes on write than the size query returned.
    ids.truncate(io_size as usize / size_of::<AudioObjectID>());
    Ok(ids)
}

/// Total output-channel count for a device (sum of `mNumberChannels` over the
/// output-scope stream configuration). `0` means the device has no output —
/// i.e. it is input-only and must be excluded from the picker.
fn output_channel_count(device: AudioObjectID) -> Result<u32, CaError> {
    let address = addr(
        kAudioDevicePropertyStreamConfiguration,
        kAudioObjectPropertyScopeOutput,
    );

    let mut size: u32 = 0;
    // SAFETY: address/size pointers reference live stack locals; no qualifier.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            device,
            (&address).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
        )
    };
    check(status, "get output stream config size")?;
    if size == 0 {
        return Ok(0);
    }

    // Back the AudioBufferList with an 8-byte-aligned buffer (u64 alignment
    // matches the pointer alignment of AudioBuffer.mData); a Vec<u8> would be
    // only 1-byte aligned and mis-align the trailing AudioBuffer entries.
    let words = (size as usize).div_ceil(size_of::<u64>()).max(1);
    let mut buf: Vec<u64> = vec![0; words];
    let list_ptr = buf.as_mut_ptr().cast::<AudioBufferList>();

    let mut io_size = size;
    // SAFETY: address/size pointers reference live stack locals; the out buffer
    // is `buf`, ≥ `io_size` bytes, 8-byte aligned, whole-allocation provenance.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            (&address).into(),
            0,
            std::ptr::null(),
            (&mut io_size).into(),
            NonNull::from(buf.as_mut_slice()).cast::<c_void>(),
        )
    };
    check(status, "get output stream config")?;

    // SAFETY: the HAL wrote a valid AudioBufferList into `buf` — `mNumberBuffers`
    // followed by that many contiguous AudioBuffer entries (C variable-length
    // array). `&raw const` keeps whole-allocation provenance (no intermediate
    // reference to the 1-element `mBuffers` array, which would not cover
    // entries beyond index 0). Every field read is plain-old-data.
    let channels = unsafe {
        let n = (*list_ptr).mNumberBuffers as usize;
        let base = (&raw const (*list_ptr).mBuffers).cast::<AudioBuffer>();
        (0..n).map(|i| base.add(i).read().mNumberChannels).sum()
    };
    Ok(channels)
}

/// Human-readable device name via `kAudioObjectPropertyName` (a +1 retained
/// CFStringRef — same conversion gotcha as `properties::device_uid`).
fn device_name(device: AudioObjectID) -> Result<String, CaError> {
    let address = addr(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal);

    // Out-param is a +1 retained CFStringRef.
    let mut name: *const CFString = std::ptr::null();
    let mut size = size_of::<*const CFString>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes (one CFStringRef pointer slot).
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            (&address).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut name).cast::<c_void>(),
        )
    };
    check(status, "get device name")?;
    let name = NonNull::new(name.cast_mut())
        // SAFETY: the HAL hands back a +1 retained CFStringRef; `from_raw`
        // adopts that reference (balanced by CFRetained's Drop release).
        .map(|p| unsafe { CFRetained::from_raw(p) })
        .ok_or_else(|| CaError {
            status: 0,
            fourcc: "????".into(),
            ctx: "get device name: HAL returned NULL".into(),
        })?;
    Ok(name.to_string())
}

/// All devices with ≥1 output channel, excluding ParaEQ's own private
/// aggregate. Sorted by name.
pub fn list_output_devices() -> Result<Vec<OutputDevice>, CaError> {
    let mut devices = Vec::new();
    for id in all_device_ids()? {
        // Input-only devices have no output channels — skip them.
        if output_channel_count(id)? == 0 {
            continue;
        }
        let uid = device_uid(id)?;
        if uid.starts_with(OWN_AGGREGATE_UID_PREFIX) {
            continue;
        }
        let name = device_name(id)?;
        devices.push(OutputDevice { id, name, uid });
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}

/// Resolve `uid` → `AudioObjectID` (via [`list_output_devices`]) and set the
/// system default output device. `Err` if the uid is unknown.
pub fn set_default_output_device(uid: &str) -> Result<(), CaError> {
    let id = list_output_devices()?
        .into_iter()
        .find(|d| d.uid == uid)
        .map(|d| d.id)
        .ok_or_else(|| CaError {
            status: 0,
            fourcc: "????".into(),
            ctx: format!("set default output device: unknown uid {uid:?}"),
        })?;

    let address = addr(
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
    );
    // SAFETY: address points at a live stack local; in_data points at a live
    // AudioObjectID of the declared size (read-only for the callee, plain-old-
    // data). Target is the system object, as the selector requires.
    let status = unsafe {
        AudioObjectSetPropertyData(
            kAudioObjectSystemObject as u32,
            (&address).into(),
            0,
            std::ptr::null(),
            size_of::<AudioObjectID>() as u32,
            NonNull::from(&id).cast::<c_void>(),
        )
    };
    check(status, "set default output device")
}
