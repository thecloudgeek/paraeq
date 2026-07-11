//! RAII property listeners: HAL property-change notifications forwarded as
//! [`ListenerEvent`]s over a plain `std::sync::mpsc` channel. Used by the
//! backend (Task 12) for default-output / device-alive / format selectors —
//! all dispatched on non-realtime HAL notification threads, never from the
//! IO context.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::mpsc::Sender;

use objc2_core_audio::{
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener,
};

use crate::error::{check, CaError};

/// A property changed on `object`; `selector` says which one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListenerEvent {
    pub object: AudioObjectID,
    pub selector: u32,
}

/// Client-data box handed to the HAL.
///
/// It holds a plain `Sender` — deliberately NO `Mutex`: since Rust 1.72
/// `std::sync::mpsc::Sender<T: Send>` is `Sync`, so concurrent `send`s
/// through a shared `&Sender` from multiple HAL notification threads are
/// safe. These are non-realtime threads; blocking guarantees are not needed.
struct ListenerCtx {
    tx: Sender<ListenerEvent>,
}

/// The `AudioObjectPropertyListenerProc` trampoline (signature verified
/// against objc2-core-audio 0.3.2, AudioHardware.rs:660).
///
/// Panic policy: same as the IOProc trampoline (`ioproc::io_trampoline`) —
/// the ABI is `extern "C-unwind"`, which PROPAGATES unwinds into CoreAudio's
/// foreign HAL frames instead of aborting. An unwind escaping into HAL
/// frames could wedge the process with a tap alive (system muted), so the
/// closure body runs inside `catch_unwind` and a caught panic aborts the
/// process — macOS then tears the tap down and drops the mute.
unsafe extern "C-unwind" fn listener_trampoline(
    object: AudioObjectID,
    n_addresses: u32,
    addresses: NonNull<AudioObjectPropertyAddress>,
    client: *mut c_void,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `client` is the `Box::into_raw` ctx from `watch`, freed
        // only after `AudioObjectRemovePropertyListener` returns, so it is
        // live here. Shared `&` only — concurrent notification threads may
        // run this simultaneously, which is safe because `Sender` is `Sync`.
        let ctx = unsafe { &*client.cast::<ListenerCtx>() };
        // SAFETY: the HAL guarantees `addresses` points at `n_addresses`
        // contiguous AudioObjectPropertyAddress entries for this call.
        let addrs = unsafe { std::slice::from_raw_parts(addresses.as_ptr(), n_addresses as usize) };
        for a in addrs {
            // Send failure = receiver dropped = the listener is being torn
            // down; nothing useful to do with the event.
            let _ = ctx.tx.send(ListenerEvent {
                object,
                selector: a.mSelector,
            });
        }
    }));
    if result.is_err() {
        // Never unwind into HAL frames (see doc comment above).
        std::process::abort();
    }
    0
}

/// RAII registration of one property listener on one AudioObject.
///
/// # Soundness
///
/// The ctx box must outlive the registration: it is leaked to the HAL at
/// `watch` and freed in `Drop` only AFTER `AudioObjectRemovePropertyListener`
/// returns. Removal must pass the SAME proc + ctx pointer used at add time
/// (AudioHardware.rs:914 contract) — `Drop` reuses the stored address and
/// ctx pointer with the same `listener_trampoline`.
pub struct PropertyListener {
    object: AudioObjectID,
    address: AudioObjectPropertyAddress,
    ctx: NonNull<ListenerCtx>,
}

// SAFETY: `object`/`address` are plain values; the ctx pointee holds a
// `Sender<ListenerEvent>` (Send, and the events are Copy) owned exclusively
// by this handle — the HAL only uses it via shared `&` from its notification
// threads. Moving the handle to another thread moves that ownership with it.
unsafe impl Send for PropertyListener {}

impl PropertyListener {
    /// Register a listener for `selector` (global scope, main element) on
    /// `object`. Every change notification becomes one [`ListenerEvent`] per
    /// changed address on `tx`.
    pub fn watch(
        object: AudioObjectID,
        selector: u32,
        tx: Sender<ListenerEvent>,
    ) -> Result<PropertyListener, CaError> {
        let address = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let ctx: *mut ListenerCtx = Box::into_raw(Box::new(ListenerCtx { tx }));
        // SAFETY: `address` is a live stack local (the HAL copies it);
        // `listener_trampoline` matches AudioObjectPropertyListenerProc;
        // `ctx` is a valid pointer the trampoline knows how to use.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                object,
                NonNull::from(&address),
                Some(listener_trampoline),
                ctx.cast::<c_void>(),
            )
        };
        if let Err(e) = check(status, "AudioObjectAddPropertyListener") {
            // SAFETY: registration failed, so the HAL holds no reference to
            // `ctx`; reclaiming the untouched box is sole ownership.
            drop(unsafe { Box::from_raw(ctx) });
            return Err(e);
        }
        Ok(PropertyListener {
            object,
            address,
            // `ctx` came from Box::into_raw, which never returns null.
            ctx: NonNull::new(ctx).expect("Box::into_raw returned null"),
        })
    }
}

impl Drop for PropertyListener {
    fn drop(&mut self) {
        // SAFETY: SAME proc + ctx pointer as at add time (the removal
        // contract, AudioHardware.rs:914); `address` is the stored copy of
        // the registered address.
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                self.object,
                NonNull::from(&self.address),
                Some(listener_trampoline),
                self.ctx.as_ptr().cast::<c_void>(),
            )
        };
        if let Err(e) = check(status, "AudioObjectRemovePropertyListener") {
            // Nothing more a Drop can do — but never silently discard it.
            log::error!("listener drop: {e}");
        }
        // SAFETY: removal has returned, so the HAL no longer invokes the
        // trampoline with this ctx — the box is exclusively ours to free.
        drop(unsafe { Box::from_raw(self.ctx.as_ptr()) });
    }
}
