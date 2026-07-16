//! Pure `AudioBufferList` view tests — no HAL involved. Buffer lists are
//! hand-built over local `Vec<f32>` storage, mirroring the two layouts the
//! HAL actually delivers (spike main.rs:30-31, DGR Labs gotcha): ONE
//! interleaved stereo buffer, or TWO mono buffers laid out as a C
//! variable-length trailing array.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use paraeq_coreaudio::backend::deinterleave_sanitize_channel;
use paraeq_coreaudio::ioproc::{BufferList, BufferListMut};

/// Two-buffer `AudioBufferList` exactly as the HAL lays it out: the second
/// `AudioBuffer` directly follows `mBuffers[0]` in memory (the C
/// variable-length-array idiom `AudioBufferList` encodes with its
/// 1-element `mBuffers`).
#[repr(C)]
struct TwoBufferList {
    list: AudioBufferList,
    extra: AudioBuffer,
}

fn buffer_over(storage: &mut [f32], channels: u32, byte_size: usize) -> AudioBuffer {
    AudioBuffer {
        mNumberChannels: channels,
        mDataByteSize: byte_size as u32,
        mData: storage.as_mut_ptr().cast::<c_void>(),
    }
}

#[test]
fn interleaved_stereo_is_one_buffer_two_channels() {
    // 4 frames x 2 channels, interleaved: L0 R0 L1 R1 ...
    let mut storage: Vec<f32> = (0..8).map(|i| i as f32).collect();
    let byte_size = storage.len() * 4;
    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [buffer_over(&mut storage, 2, byte_size)],
    };
    // SAFETY: `list` describes live, aligned, exclusively-owned f32 storage
    // that outlives `view`; the single buffer is in bounds of the list.
    let view = unsafe { BufferList::new(NonNull::from(&mut list)) };

    assert_eq!(view.len(), 1);
    let bufs: Vec<(&[f32], usize)> = view.buffers().collect();
    assert_eq!(bufs.len(), 1);
    let (data, channels) = bufs[0];
    assert_eq!(channels, 2, "interleaved buffer reports 2 channels");
    assert_eq!(data, &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
}

#[test]
fn deinterleaved_stereo_is_two_mono_buffers() {
    let mut left = vec![1.0f32; 4];
    let mut right = vec![2.0f32; 4];
    let mut two = TwoBufferList {
        list: AudioBufferList {
            mNumberBuffers: 2,
            mBuffers: [buffer_over(&mut left, 1, 16)],
        },
        extra: buffer_over(&mut right, 1, 16),
    };
    // Pointer derived from the WHOLE TwoBufferList so its provenance covers
    // the trailing `extra` buffer, not just the 1-element `mBuffers` array.
    let list_ptr = NonNull::from(&mut two).cast::<AudioBufferList>();
    // SAFETY: repr(C) puts `list` at offset 0 and `extra` contiguously after
    // `mBuffers[0]` (the HAL's own layout); both buffers reference live,
    // aligned, non-overlapping f32 storage outliving `view`.
    let view = unsafe { BufferList::new(list_ptr) };

    assert_eq!(view.len(), 2);
    let bufs: Vec<(&[f32], usize)> = view.buffers().collect();
    assert_eq!(bufs.len(), 2);
    assert_eq!(bufs[0], (&[1.0f32; 4][..], 1));
    assert_eq!(bufs[1], (&[2.0f32; 4][..], 1));
}

#[test]
fn mut_views_write_through_to_storage() {
    let mut left = vec![0.0f32; 3];
    let mut right = vec![0.0f32; 3];
    let mut two = TwoBufferList {
        list: AudioBufferList {
            mNumberBuffers: 2,
            mBuffers: [buffer_over(&mut left, 1, 12)],
        },
        extra: buffer_over(&mut right, 1, 12),
    };
    let list_ptr = NonNull::from(&mut two).cast::<AudioBufferList>();
    // SAFETY: same layout argument as the shared-view test; additionally the
    // two buffers do not overlap, as BufferListMut requires.
    let mut view = unsafe { BufferListMut::new(list_ptr) };

    assert_eq!(view.len(), 2);
    for (b, (data, channels)) in view.buffers_mut().enumerate() {
        assert_eq!(channels, 1);
        for (i, sample) in data.iter_mut().enumerate() {
            *sample = (10 * (b + 1) + i) as f32;
        }
    }

    assert_eq!(left, vec![10.0, 11.0, 12.0]);
    assert_eq!(right, vec![20.0, 21.0, 22.0]);
}

#[test]
fn byte_sizes_not_divisible_by_four_truncate() {
    let mut storage = vec![7.0f32; 4];
    // 15 bytes = 3 whole f32s + 3 dangling bytes -> exactly 3 samples exposed
    // (mDataByteSize / 4, spike behavior).
    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [buffer_over(&mut storage, 1, 15)],
    };
    // SAFETY: the first 3 exposed samples are within the live 4-sample storage.
    let view = unsafe { BufferList::new(NonNull::from(&mut list)) };

    let (data, _channels) = view.buffers().next().expect("one buffer");
    assert_eq!(data.len(), 3);
    assert_eq!(data, &[7.0, 7.0, 7.0]);
}

#[test]
fn deinterleave_sanitize_zeroes_non_finite_and_counts() {
    // 4 frames x 2 channels interleaved (L0 R0 L1 R1 ...): a NaN in L, a
    // -inf in R, plus a negative zero and a subnormal to pin bit-exact
    // copying of finite samples (the capture-boundary guard, spec R1-2).
    let mut storage: [f32; 8] = [
        0.5,
        -0.25, // frame 0
        f32::NAN,
        0.125, // frame 1
        1.5,
        f32::NEG_INFINITY, // frame 2
        -0.0,
        2.0e-40, // frame 3 (negative zero, subnormal)
    ];
    let byte_size = storage.len() * 4;
    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [buffer_over(&mut storage, 2, byte_size)],
    };
    // SAFETY: `list` describes live, aligned, exclusively-owned f32 storage
    // that outlives `view`; the single buffer is in bounds of the list.
    let view = unsafe { BufferList::new(NonNull::from(&mut list)) };
    let (data, channels) = view.buffers().next().expect("one buffer");
    assert_eq!(channels, 2);

    let mut dst = [7.7f32; 4]; // sentinel: every slot must be overwritten
    let invalid = deinterleave_sanitize_channel(data, 2, 0, &mut dst);
    assert_eq!(invalid, 1, "one NaN in the left channel");
    for (i, (d, e)) in dst.iter().zip([0.5f32, 0.0, 1.5, -0.0]).enumerate() {
        assert_eq!(
            d.to_bits(),
            e.to_bits(),
            "L frame {i}: finite samples bit-exact, NaN zeroed"
        );
    }

    let mut dst = [7.7f32; 4];
    let invalid = deinterleave_sanitize_channel(data, 2, 1, &mut dst);
    assert_eq!(invalid, 1, "one -inf in the right channel");
    for (i, (d, e)) in dst.iter().zip([-0.25f32, 0.125, 0.0, 2.0e-40]).enumerate() {
        assert_eq!(
            d.to_bits(),
            e.to_bits(),
            "R frame {i}: finite samples bit-exact, -inf zeroed"
        );
    }
}

#[test]
fn null_data_buffer_is_an_empty_slice() {
    // Disabled streams: the HAL hands a NULL mData with a nonzero
    // mDataByteSize (AudioHardware.h doc for AudioDeviceIOProc). The view
    // must expose an empty slice, never dereference NULL.
    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: 2,
            mDataByteSize: 512,
            mData: std::ptr::null_mut(),
        }],
    };
    // SAFETY: a NULL-mData buffer is valid per the view's contract (it maps
    // to an empty slice); the list itself is a live stack local.
    let view = unsafe { BufferList::new(NonNull::from(&mut list)) };

    let (data, channels) = view.buffers().next().expect("one buffer");
    assert!(data.is_empty(), "NULL mData must surface as an empty slice");
    assert_eq!(channels, 2);
}
