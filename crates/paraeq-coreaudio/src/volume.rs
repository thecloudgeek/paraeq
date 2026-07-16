//! Output-device volume get/set (MS-5, docs/specs/2026-07-15-measurement-safety-design.md).
//! The level ladder's step 0 requires the ACTUAL hardware gain before it solves
//! a sweep level; a device that cannot report one must be refused, not assumed
//! (spec § Level Ladder, § Hard Caps `VolumeUncontrollable`).
//!
//! Test tier: none of the four oracle tiers
//! (docs/specs/2026-07-15-measurement-suite-design.md § The four tiers) applies —
//! they scope DSP math against a numerical oracle, and this is platform FFI with
//! no oracle to match. Pure scalar/selection logic gets the non-hardware unit
//! tests below; HAL behaviour gets `#[ignore]` hardware tests
//! (tests/test_hardware.rs), per the crate's existing convention.
//!
//! Selectors/entry points verified against objc2-core-audio 0.3.2:
//! kAudioDevicePropertyVolumeScalar (generated/AudioHardware.rs:1334),
//! kAudioDevicePropertyPreferredChannelsForStereo (:406),
//! AudioObjectHasProperty (:715), AudioObjectIsPropertySettable (:745).

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioDevicePropertyPreferredChannelsForStereo, kAudioDevicePropertyVolumeScalar,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeOutput, AudioObjectGetPropertyData,
    AudioObjectHasProperty, AudioObjectID, AudioObjectIsPropertySettable,
    AudioObjectPropertyAddress, AudioObjectSetPropertyData,
};

use crate::error::{check, CaError};

/// Real devices quantize the volume scalar coarsely (1/16 steps are common), so
/// "reads at maximum" is a tolerance, not an equality.
const MAX_SCALAR_TOLERANCE: f32 = 1e-4;

/// One addressable volume control and the scalar the HAL reported for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeElement {
    /// kAudioObjectPropertyElementMain (0) for a master control, else the
    /// 1-based channel number.
    pub element: u32,
    /// HAL volume scalar, 0.0..=1.0. NOT dB, and NOT linear in dB — the ladder
    /// must solve level from a measured SPL, never by reading this as a gain.
    pub scalar: f32,
}

/// A read-back of an output device's volume control, one entry per element the
/// HAL exposes.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputVolume {
    elements: Vec<VolumeElement>,
    settable: bool,
}

impl OutputVolume {
    /// Every element the HAL exposed, in read order. Non-empty by construction.
    pub fn elements(&self) -> &[VolumeElement] {
        &self.elements
    }

    /// The device exposes a single master control rather than per-channel ones.
    pub fn is_master(&self) -> bool {
        self.elements.len() == 1 && self.elements[0].element == kAudioObjectPropertyElementMain
    }

    /// Every element accepts a write. `false` with `is_at_max()` is the spec's
    /// `FixedMaxVolume` case: proceed on the SPL solve and warn — do not set.
    pub fn is_settable(&self) -> bool {
        self.settable
    }

    /// The LOUDEST element's scalar. The ladder needs the gain that can injure,
    /// so an imbalanced device reports its maximum, never a mean.
    pub fn scalar(&self) -> f32 {
        self.elements
            .iter()
            .map(|element| element.scalar)
            .fold(0.0, f32::max)
    }

    /// Reads at (or within HAL quantization of) maximum.
    pub fn is_at_max(&self) -> bool {
        self.scalar() >= 1.0 - MAX_SCALAR_TOLERANCE
    }
}

/// Volume lives on the output scope; element 0 (kAudioObjectPropertyElementMain)
/// is the master control and channel elements are 1-based.
fn output_addr(selector: u32, element: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: element,
    }
}

fn has_property(dev: AudioObjectID, addr: &AudioObjectPropertyAddress) -> bool {
    // SAFETY: address points at a live caller-owned stack local.
    unsafe { AudioObjectHasProperty(dev, addr.into()) }
}

fn is_settable(dev: AudioObjectID, addr: &AudioObjectPropertyAddress) -> Result<bool, CaError> {
    // objc2-core-audio's Boolean is `pub(crate) type Boolean = u8` (lib.rs:25),
    // so the out-param is one u8.
    let mut settable: u8 = 0;
    // SAFETY: address/out pointers reference live stack locals; the out buffer
    // is exactly one Boolean (u8).
    let status = unsafe { AudioObjectIsPropertySettable(dev, addr.into(), (&mut settable).into()) };
    check(status, "query output volume settability")?;
    Ok(settable != 0)
}

fn read_scalar(dev: AudioObjectID, element: u32) -> Result<f32, CaError> {
    let mut scalar: f32 = 0.0;
    let mut size = size_of::<f32>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes of plain-old-data (f32).
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            (&output_addr(kAudioDevicePropertyVolumeScalar, element)).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut scalar).cast::<c_void>(),
        )
    };
    check(status, "get output volume scalar")?;
    Ok(scalar)
}

fn write_scalar(dev: AudioObjectID, element: u32, scalar: f32) -> Result<(), CaError> {
    // SAFETY: address points at a live stack local; in_data points at a live
    // f32 of the declared size (read-only for the callee, plain-old-data).
    let status = unsafe {
        AudioObjectSetPropertyData(
            dev,
            (&output_addr(kAudioDevicePropertyVolumeScalar, element)).into(),
            0,
            std::ptr::null(),
            size_of::<f32>() as u32,
            NonNull::from(&scalar).cast::<c_void>(),
        )
    };
    check(status, "set output volume scalar")
}

/// The device's stereo pair, for devices exposing no master control. Falls back
/// to the near-universal (1, 2) when the HAL does not answer.
fn stereo_channels(dev: AudioObjectID) -> [u32; 2] {
    const DEFAULT: [u32; 2] = [1, 2];
    let addr = output_addr(
        kAudioDevicePropertyPreferredChannelsForStereo,
        kAudioObjectPropertyElementMain,
    );
    if !has_property(dev, &addr) {
        return DEFAULT;
    }
    let mut channels: [u32; 2] = DEFAULT;
    let mut size = size_of::<[u32; 2]>() as u32;
    // SAFETY: address/size/out pointers reference live stack locals; the out
    // buffer is exactly `size` bytes of plain-old-data ([u32; 2]).
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            (&addr).into(),
            0,
            std::ptr::null(),
            (&mut size).into(),
            NonNull::from(&mut channels).cast::<c_void>(),
        )
    };
    // A failed read may have partially written the buffer: discard it entirely.
    if check(status, "get preferred stereo channels").is_err() {
        return DEFAULT;
    }
    channels
}

/// Read the volume control of `dev`.
///
/// `Ok(None)` means the device exposes NO volume control on either its master
/// element or its stereo channels — HDMI, many USB DACs and aggregates behave
/// this way. The caller must refuse or fall back to the SPL solve; it must never
/// assume a level (spec § Hard Caps, `VolumeUncontrollable`).
pub fn output_volume(dev: AudioObjectID) -> Result<Option<OutputVolume>, CaError> {
    let master = output_addr(
        kAudioDevicePropertyVolumeScalar,
        kAudioObjectPropertyElementMain,
    );
    if has_property(dev, &master) {
        return Ok(Some(OutputVolume {
            elements: vec![VolumeElement {
                element: kAudioObjectPropertyElementMain,
                scalar: read_scalar(dev, kAudioObjectPropertyElementMain)?,
            }],
            settable: is_settable(dev, &master)?,
        }));
    }

    // No master control: fall back to per-channel elements. A device answering
    // on neither has no volume control at all.
    let mut elements = Vec::new();
    let mut settable = true;
    for channel in stereo_channels(dev) {
        // A mono device can report both halves of its "stereo" pair as the same
        // channel; deduplicate so it is neither read nor restored twice.
        if elements
            .iter()
            .any(|e: &VolumeElement| e.element == channel)
        {
            continue;
        }
        let addr = output_addr(kAudioDevicePropertyVolumeScalar, channel);
        if !has_property(dev, &addr) {
            continue;
        }
        elements.push(VolumeElement {
            element: channel,
            scalar: read_scalar(dev, channel)?,
        });
        // One read-only channel makes the whole control unsettable: a partial
        // set would silently imbalance the device.
        settable &= is_settable(dev, &addr)?;
    }
    if elements.is_empty() {
        return Ok(None);
    }
    Ok(Some(OutputVolume { elements, settable }))
}

/// Set every element of `dev`'s volume control to `scalar` (0.0..=1.0).
///
/// Refuses a non-finite or out-of-range scalar before touching the HAL, and
/// refuses a device whose control is absent or read-only rather than returning
/// Ok: a caller that believes it set a level it did not set is exactly the
/// misconfiguration the ladder exists to prevent.
pub fn set_output_volume(dev: AudioObjectID, scalar: f32) -> Result<(), CaError> {
    // Ordered before any HAL call so it holds regardless of the device. Every
    // NaN comparison is false, so `contains` rejects NaN and both infinities
    // too — a NaN volume is a safety issue, not a numerics nit (§ Non-Finite).
    if !(0.0..=1.0).contains(&scalar) {
        return Err(refusal(format!(
            "set output volume: scalar {scalar} outside 0.0..=1.0"
        )));
    }
    let volume = output_volume(dev)?
        .ok_or_else(|| refusal("set output volume: device exposes no volume control".into()))?;
    if !volume.is_settable() {
        return Err(refusal(
            "set output volume: device's volume control is read-only".into(),
        ));
    }
    for element in volume.elements() {
        write_scalar(dev, element.element, scalar)?;
    }
    Ok(())
}

/// A refusal this crate raises itself rather than one the HAL reported. Uses the
/// `status: 0` / `"????"` shape properties.rs already uses for its NULL-UID case.
fn refusal(ctx: String) -> CaError {
    CaError {
        status: 0,
        fourcc: "????".into(),
        ctx,
    }
}

/// Restore a snapshot taken by `output_volume`, element by element, preserving
/// any channel imbalance the user had set.
///
/// Collects rather than short-circuits: a failure on one element must not skip
/// the others, because a half-restored device is left at measurement volume on
/// every channel that got skipped. Mirrors `TapSystem::teardown`'s
/// error-collecting shape (tap.rs) for the same reason — the system must never
/// be left at measurement volume (spec § Abort Guards).
pub fn restore_output_volume(dev: AudioObjectID, volume: &OutputVolume) -> Vec<CaError> {
    let mut errors = Vec::new();
    for element in &volume.elements {
        if let Err(err) = write_scalar(dev, element.element, element.scalar) {
            errors.push(err);
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(elements: &[(u32, f32)], settable: bool) -> OutputVolume {
        OutputVolume {
            elements: elements
                .iter()
                .map(|&(element, scalar)| VolumeElement { element, scalar })
                .collect(),
            settable,
        }
    }

    #[test]
    fn master_element_is_recognized_as_master() {
        let vol = volume(&[(kAudioObjectPropertyElementMain, 0.5)], true);
        assert!(vol.is_master());
        assert_eq!(vol.scalar(), 0.5);
    }

    #[test]
    fn per_channel_elements_are_not_master() {
        assert!(!volume(&[(1, 0.5), (2, 0.5)], true).is_master());
    }

    #[test]
    fn scalar_reports_the_loudest_channel_never_a_mean() {
        // The ladder must read the gain that can injure: 0.9, not the 0.5 mean.
        assert_eq!(volume(&[(1, 0.1), (2, 0.9)], true).scalar(), 0.9);
    }

    #[test]
    fn is_at_max_tolerates_hal_quantization() {
        assert!(volume(&[(0, 1.0)], false).is_at_max());
        assert!(volume(&[(0, 1.0 - 1e-5)], false).is_at_max());
        assert!(!volume(&[(0, 0.99)], false).is_at_max());
    }

    #[test]
    fn one_channel_at_max_reads_at_max() {
        assert!(volume(&[(1, 0.2), (2, 1.0)], true).is_at_max());
    }

    // Scalar validation runs before any FFI, so these reach no HAL and pass on
    // a CI box with no audio devices (device 0 is kAudioObjectUnknown).
    #[test]
    fn set_refuses_non_finite_scalar_before_touching_the_hal() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let err = set_output_volume(0, bad).unwrap_err();
            assert!(err.ctx.contains("outside 0.0..=1.0"), "got {}", err.ctx);
        }
    }

    #[test]
    fn set_refuses_out_of_range_scalar_before_touching_the_hal() {
        for bad in [-0.1, 1.1, 2.0] {
            let err = set_output_volume(0, bad).unwrap_err();
            assert!(err.ctx.contains("outside 0.0..=1.0"), "got {}", err.ctx);
        }
    }
}
