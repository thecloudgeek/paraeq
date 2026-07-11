//! IOProc registration with callback-scoped buffer views.
//!
//! This module replaces the tap spike's two unsound shortcuts
//! (spikes/tap-spike/src/main.rs):
//!
//! 1. `buffers_of` fabricated `&'static mut [AudioBuffer]` from the HAL's
//!    callback pointers (main.rs:32-36). Here every view is [`BufferList`] /
//!    [`BufferListMut`] inside an [`IoBlock<'a>`] whose lifetime is bound to
//!    the trampoline's stack frame — the views cannot escape the callback.
//! 2. The spike aliased `&mut EqState` across threads (main.rs:47 vs
//!    225-237). Here the realtime side exclusively owns its mutable state
//!    inside the [`IoCallback`] closure; cross-thread traffic belongs to
//!    `paraeq-engine`'s atomics + SPSC rings, not this layer.
//!
//! The trampoline does NOTHING but build the views and invoke the closure —
//! no counters, no DSP (those live in the engine's `RtProcessor`).

use std::ffi::c_void;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;

use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioObjectID,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};

use crate::error::{check, CaError};

/// One IO cycle's worth of HAL data, handed to the [`IoCallback`]. The
/// lifetime `'a` is the trampoline's stack frame: the views (and anything
/// borrowed from them) cannot outlive the callback invocation.
pub struct IoBlock<'a> {
    pub input: BufferList<'a>,
    pub output: BufferListMut<'a>,
    pub in_sample_time: f64,
    pub out_sample_time: f64,
}

/// Zero-alloc shared view over an `AudioBufferList`.
///
/// Layout facts (spike main.rs:30-31, 65-93 — the DGR Labs gotcha): a stereo
/// stream may arrive as ONE interleaved 2-channel buffer or as TWO mono
/// buffers. The view stays dumb — it exposes `(samples, channel_count)` per
/// buffer; de/interleaving policy belongs to the caller.
pub struct BufferList<'a> {
    list: NonNull<AudioBufferList>,
    _hal_frame: PhantomData<&'a AudioBufferList>,
}

/// Zero-alloc mutable view over an `AudioBufferList`. Same layout facts as
/// [`BufferList`]; buffers must additionally be pairwise non-overlapping
/// (they are: each HAL buffer owns its own `mData` region).
pub struct BufferListMut<'a> {
    list: NonNull<AudioBufferList>,
    _hal_frame: PhantomData<&'a mut AudioBufferList>,
}

/// Raw per-buffer facts: data pointer, exposed sample count
/// (`mDataByteSize / 4`, truncating — spike behavior), channel count.
///
/// # Safety
///
/// `list` must satisfy the [`BufferList::new`] contract and `i` must be
/// `< mNumberBuffers`.
unsafe fn raw_buffer(list: NonNull<AudioBufferList>, i: usize) -> (*mut f32, usize, usize) {
    // SAFETY: caller guarantees the list is valid with at least i+1 trailing
    // AudioBuffer entries. `&raw const` keeps the original pointer's
    // provenance (no intermediate reference to the 1-element mBuffers array,
    // which would not cover entries beyond index 0).
    let buf = unsafe {
        let base = (&raw const (*list.as_ptr()).mBuffers).cast::<AudioBuffer>();
        base.add(i).read()
    };
    let samples = if buf.mData.is_null() {
        // Disabled streams: NULL mData with a nonzero mDataByteSize
        // (AudioDeviceIOProc doc). Expose nothing.
        0
    } else {
        buf.mDataByteSize as usize / 4
    };
    (
        buf.mData.cast::<f32>(),
        samples,
        buf.mNumberChannels as usize,
    )
}

impl<'a> BufferList<'a> {
    /// Wrap a HAL-supplied `AudioBufferList` pointer.
    ///
    /// # Safety
    ///
    /// For the chosen lifetime `'a`, `list` must point to a valid
    /// `AudioBufferList` whose `mNumberBuffers` `AudioBuffer` entries follow
    /// contiguously in memory (the C variable-length-array layout), each with
    /// `mData` either NULL or pointing to at least `mDataByteSize` bytes of
    /// initialized, f32-aligned data readable for `'a`, with no `&mut` alias.
    pub unsafe fn new(list: NonNull<AudioBufferList>) -> BufferList<'a> {
        BufferList {
            list,
            _hal_frame: PhantomData,
        }
    }

    /// Number of buffers (`mNumberBuffers`).
    pub fn len(&self) -> usize {
        // SAFETY: `list` is valid for reads per the `new` contract.
        unsafe { (*self.list.as_ptr()).mNumberBuffers as usize }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate `(samples, channel_count)` per buffer. Sample count is
    /// `mDataByteSize / 4` (truncating); channel count is `mNumberChannels`
    /// as-is. Allocation-free.
    pub fn buffers(&self) -> impl Iterator<Item = (&'a [f32], usize)> + '_ {
        let list = self.list;
        (0..self.len()).map(move |i| {
            // SAFETY: i < mNumberBuffers; the `new` contract guarantees the
            // data is initialized, aligned, readable and un-aliased by any
            // `&mut` for 'a (NULL mData maps to samples == 0, never deref'd).
            unsafe {
                let (data, samples, channels) = raw_buffer(list, i);
                let slice = if samples == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(data, samples)
                };
                (slice, channels)
            }
        })
    }
}

impl<'a> BufferListMut<'a> {
    /// Wrap a HAL-supplied output `AudioBufferList` pointer.
    ///
    /// # Safety
    ///
    /// Same contract as [`BufferList::new`], strengthened to exclusive
    /// access: for `'a` the buffer data must be writable, free of any other
    /// alias, and the buffers must be pairwise non-overlapping (the mutable
    /// iterator hands out all buffer slices as coexisting `&mut`).
    pub unsafe fn new(list: NonNull<AudioBufferList>) -> BufferListMut<'a> {
        BufferListMut {
            list,
            _hal_frame: PhantomData,
        }
    }

    /// Number of buffers (`mNumberBuffers`).
    pub fn len(&self) -> usize {
        // SAFETY: `list` is valid for reads per the `new` contract.
        unsafe { (*self.list.as_ptr()).mNumberBuffers as usize }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate `(samples, channel_count)` per buffer, mutably. Same exposure
    /// rules as [`BufferList::buffers`]. Allocation-free.
    pub fn buffers_mut(&mut self) -> impl Iterator<Item = (&'a mut [f32], usize)> + '_ {
        let list = self.list;
        (0..self.len()).map(move |i| {
            // SAFETY: i < mNumberBuffers; the `new` contract guarantees
            // exclusive, writable, pairwise non-overlapping buffer data for
            // 'a, so one live `&mut` per buffer cannot alias. The `&mut self`
            // borrow held by `'_` prevents a second iterator meanwhile.
            unsafe {
                let (data, samples, channels) = raw_buffer(list, i);
                let slice = if samples == 0 {
                    &mut []
                } else {
                    std::slice::from_raw_parts_mut(data, samples)
                };
                (slice, channels)
            }
        })
    }
}

/// The realtime callback. `Send` because it is invoked from the HAL's IO
/// thread, not the thread that registered it.
pub type IoCallback = Box<dyn FnMut(IoBlock<'_>) + Send>;

/// The IOProc trampoline handed to `AudioDeviceCreateIOProcID`.
///
/// Panic policy: the closure runs inside `catch_unwind`, and a caught panic
/// calls `std::process::abort()`. This is deliberate and load-bearing: the
/// IOProc ABI is `extern "C-unwind"` (AudioHardware.rs:1185-1195), which
/// PROPAGATES unwinds into CoreAudio's foreign HAL frames — it does NOT
/// abort like plain `extern "C"` would. An unwind escaping into HAL frames
/// could wedge the process with the tap still alive, leaving the system
/// muted. Catch+abort makes the failure mode the safe one: the process dies,
/// and macOS tears the tap down and drops the mute (spike-validated,
/// docs/spikes/2026-07-tap-spike.md line 13).
unsafe extern "C-unwind" fn io_trampoline(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `client` is the `Box::into_raw` pointer from `register`,
        // freed only after `AudioDeviceDestroyIOProcID` returns; the HAL
        // serializes invocations of a single IOProc and the handle never
        // touches the box while registered, so this `&mut` is unique.
        let cb = unsafe { &mut *client.cast::<IoCallback>() };
        // SAFETY: for the duration of this call the HAL guarantees valid
        // input/output AudioBufferLists (input read-only, output exclusively
        // ours to fill) and valid timestamps. The views' lifetime is this
        // stack frame — exactly the region the HAL vouches for.
        let block = unsafe {
            IoBlock {
                input: BufferList::new(input),
                output: BufferListMut::new(output),
                in_sample_time: input_time.as_ref().mSampleTime,
                out_sample_time: output_time.as_ref().mSampleTime,
            }
        };
        cb(block);
    }));
    if result.is_err() {
        // Never unwind into HAL frames (see doc comment above): die loudly so
        // macOS destroys the tap and unmutes the system.
        std::process::abort();
    }
    0
}

/// RAII owner of one registered IOProc on one device.
///
/// # Soundness
///
/// `register` double-boxes the callback (`Box<IoCallback>`) and leaks the
/// outer box's raw pointer to the HAL as the client pointer. This is sound
/// because:
/// - the HAL serializes invocations of a single IOProc, so the trampoline's
///   `&mut IoCallback` is never aliased by another invocation;
/// - the handle never touches the box while the IOProc is registered (no
///   other `&`/`&mut` exists anywhere);
/// - the box is reclaimed and freed only AFTER `AudioDeviceDestroyIOProcID`
///   returns, at which point the HAL guarantees the IOProc is not in flight
///   and will never be invoked again.
pub struct IoProcHandle {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    /// `Box::into_raw` of the double-boxed callback; `None` once `stop` has
    /// reclaimed it (the idempotence flag).
    client: Option<NonNull<IoCallback>>,
}

// SAFETY: `device`/`proc_id` are plain HAL ids; the client pointee is an
// `IoCallback`, which is `Send` by its trait bound, and the handle owns it
// exclusively (the HAL only ever uses it from its own IO thread, which is
// exactly what the `Send` bound on the closure licenses). No shared-state
// `Sync` claim is made.
unsafe impl Send for IoProcHandle {}

impl IoProcHandle {
    /// Register `cb` as an IOProc on `device`. The IOProc exists but is not
    /// running until [`start`](Self::start).
    pub fn register(device: AudioObjectID, cb: IoCallback) -> Result<IoProcHandle, CaError> {
        let client: *mut IoCallback = Box::into_raw(Box::new(cb));
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `io_trampoline` matches AudioDeviceIOProc; `client` is a
        // valid pointer the trampoline knows how to use; `proc_id` is a live
        // out-param.
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                device,
                Some(io_trampoline),
                client.cast::<c_void>(),
                NonNull::from(&mut proc_id),
            )
        };
        if let Err(e) = check(status, "AudioDeviceCreateIOProcID") {
            // SAFETY: registration failed, so the HAL holds no reference to
            // `client`; reclaiming the untouched box is sole ownership.
            drop(unsafe { Box::from_raw(client) });
            return Err(e);
        }
        Ok(IoProcHandle {
            device,
            proc_id,
            // `client` came from Box::into_raw, which never returns null.
            client: NonNull::new(client),
        })
    }

    /// Start IO. The HAL begins invoking the callback on its IO thread.
    pub fn start(&mut self) -> Result<(), CaError> {
        if self.client.is_none() {
            return Err(CaError {
                status: 0,
                fourcc: "????".into(),
                ctx: "AudioDeviceStart: IOProc already stopped and destroyed".into(),
            });
        }
        // SAFETY: `proc_id` was created by AudioDeviceCreateIOProcID on
        // `device` and has not been destroyed (`client` is still Some).
        let status = unsafe { AudioDeviceStart(self.device, self.proc_id) };
        check(status, "AudioDeviceStart")
    }

    /// Stop IO and destroy the IOProc, then reclaim the callback box.
    /// Idempotent: subsequent calls are no-ops. Failure statuses are logged,
    /// never discarded silently — there is no recovery from a failed stop,
    /// but it must be visible.
    pub fn stop(&mut self) {
        let Some(client) = self.client.take() else {
            return; // already stopped
        };
        // SAFETY: `proc_id` is live (guarded by `client`); stop-before-
        // destroy is the head of the teardown invariant (spike main.rs:243-251).
        let status = unsafe { AudioDeviceStop(self.device, self.proc_id) };
        if let Err(e) = check(status, "AudioDeviceStop") {
            log::error!("ioproc stop: {e}");
        }
        // SAFETY: same live proc_id; destroyed exactly once (client was Some).
        let status = unsafe { AudioDeviceDestroyIOProcID(self.device, self.proc_id) };
        if let Err(e) = check(status, "AudioDeviceDestroyIOProcID") {
            log::error!("ioproc stop: {e}");
        }
        // SAFETY: AudioDeviceDestroyIOProcID has returned, so the HAL
        // guarantees the IOProc is not in flight and will never be invoked
        // again — the box is exclusively ours to free (see struct Soundness).
        drop(unsafe { Box::from_raw(client.as_ptr()) });
    }
}

impl Drop for IoProcHandle {
    fn drop(&mut self) {
        self.stop();
    }
}
