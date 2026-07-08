//! Lock-free control-plane <-> realtime sharing.
//!
//! The realtime side owns its mutable DSP state ([`RealtimeChain`]) inside
//! [`RtProcessor`]; cross-thread communication is `Arc<RtShared>` (atomics
//! only, shared `&`) plus two `rtrb` SPSC rings: control -> rt carries
//! [`RtMsg`] processor swaps, rt -> control returns [`Retired`] configs so
//! deallocation always happens on the control thread. No `&mut` is ever
//! aliased across threads (the spike's anti-pattern).
//!
//! Realtime lane rules: [`RtLink::poll`] and [`RtProcessor::process_block`]
//! never lock, never allocate, never deallocate, never log. A swap is a
//! by-value move through a preallocated ring slot (a small memcpy -- the
//! [`Correction`] enum is shallow).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::chain::{Correction, RealtimeChain};
use crate::EngineError;

/// All-atomic telemetry + parameters shared between the control plane and
/// the realtime callback. All loads/stores are `Ordering::Relaxed`:
/// telemetry plus single-writer parameters, no cross-variable invariants.
pub struct RtShared {
    /// User bypass (control writes, rt reads).
    pub bypass: AtomicBool,
    /// Total realtime callbacks observed (rt writes).
    pub callbacks: AtomicU64,
    /// Blocks passed through because the frame count did not fit the active
    /// correction (rt writes).
    pub frame_mismatch_blocks: AtomicU64,
    /// Linear trim gain as f32 bits (control writes, rt reads).
    pub gain_bits: AtomicU32,
    /// Blocks with at least one nonzero input sample (rt writes) -- the
    /// watchdog's raw "audio is flowing" signal.
    pub nonzero_blocks: AtomicU64,
    /// Maximum input |sample| seen, as f32 bits (rt writes).
    pub peak_in_bits: AtomicU32,
    /// Latest output-vs-input sample-time delta as f64 bits (rt writes).
    pub sample_time_delta_bits: AtomicU64,
    /// Blocks the backend skipped before reaching the chain (e.g. oversize
    /// HAL buffers; backend writes -- Task 12).
    pub skipped_blocks: AtomicU64,
    /// All-zero input blocks (rt writes) -- the watchdog's silence signal.
    pub zero_blocks: AtomicU64,
}

impl Default for RtShared {
    fn default() -> Self {
        Self {
            bypass: AtomicBool::new(false),
            callbacks: AtomicU64::new(0),
            frame_mismatch_blocks: AtomicU64::new(0),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            nonzero_blocks: AtomicU64::new(0),
            peak_in_bits: AtomicU32::new(0),
            sample_time_delta_bits: AtomicU64::new(0),
            skipped_blocks: AtomicU64::new(0),
            zero_blocks: AtomicU64::new(0),
        }
    }
}

impl RtShared {
    /// Linear trim gain.
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }

    /// Set the linear trim gain (control plane).
    pub fn set_gain(&self, gain: f32) {
        self.gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Maximum input |sample| observed so far.
    pub fn peak_in(&self) -> f32 {
        f32::from_bits(self.peak_in_bits.load(Ordering::Relaxed))
    }

    /// Latest output-vs-input sample-time delta (frames).
    pub fn sample_time_delta(&self) -> f64 {
        f64::from_bits(self.sample_time_delta_bits.load(Ordering::Relaxed))
    }

    /// Store the sample-time delta (realtime side).
    pub fn store_sample_time_delta(&self, delta: f64) {
        self.sample_time_delta_bits
            .store(delta.to_bits(), Ordering::Relaxed);
    }
}

/// Control -> realtime message, moved by value through a ring slot.
pub enum RtMsg {
    /// Install this correction (or clear with `None`); the previous one is
    /// returned through the retire ring.
    Correction(Option<Correction>),
}

/// Realtime -> control return channel payload: a correction the realtime
/// side no longer owns. Dropped (freed) on the control thread only.
pub struct Retired(pub Option<Correction>);

/// Control-plane half of the link pair.
pub struct ControlLink {
    retire_rx: Consumer<Retired>,
    swap_tx: Producer<RtMsg>,
}

impl ControlLink {
    /// Queue a message for the realtime side. Ring-full is an error; the
    /// controller retries next tick (it retains the config as source of
    /// truth, so the dropped message is rebuildable).
    pub fn send(&mut self, msg: RtMsg) -> Result<(), EngineError> {
        self.swap_tx
            .push(msg)
            .map_err(|_| EngineError::RingFull("control->rt swap ring full".into()))
    }

    /// Drop all retired configs on the control thread ("retired configs are
    /// freed off-thread", spec line 85). Returns how many were freed.
    pub fn drain_retired(&mut self) -> usize {
        let mut n = 0;
        while self.retire_rx.pop().is_ok() {
            n += 1;
        }
        n
    }
}

/// Realtime half of the link pair.
pub struct RtLink {
    retire_tx: Producer<Retired>,
    swap_rx: Consumer<RtMsg>,
}

impl RtLink {
    /// Apply pending swaps to the chain. Realtime-safe: no allocation, no
    /// deallocation, no locks.
    ///
    /// A swap is gated on retire-ring space: with no free retire slot the
    /// message stays queued (deferred to a later poll), so the realtime
    /// side can never be forced to drop -- i.e. deallocate -- a processor.
    pub fn poll(&mut self, chain: &mut RealtimeChain) {
        while self.retire_tx.slots() > 0 {
            match self.swap_rx.pop() {
                Ok(RtMsg::Correction(c)) => {
                    let old = chain.set_correction(c);
                    // Cannot fail: slots() > 0 was checked and we are the
                    // only producer on this ring.
                    let pushed = self.retire_tx.push(Retired(old));
                    debug_assert!(pushed.is_ok());
                }
                Err(_) => break,
            }
        }
    }
}

/// Create a linked control/realtime pair with `capacity` slots per ring.
pub fn links(capacity: usize) -> (ControlLink, RtLink) {
    let (swap_tx, swap_rx) = RingBuffer::<RtMsg>::new(capacity);
    let (retire_tx, retire_rx) = RingBuffer::<Retired>::new(capacity);
    (
        ControlLink { retire_rx, swap_tx },
        RtLink { retire_tx, swap_rx },
    )
}

/// The one realtime entry point every backend drives. Owns the chain, its
/// half of the rings, and the shared atomics.
pub struct RtProcessor {
    chain: RealtimeChain,
    link: RtLink,
    shared: Arc<RtShared>,
}

impl RtProcessor {
    pub fn new(shared: Arc<RtShared>, link: RtLink, chain: RealtimeChain) -> RtProcessor {
        RtProcessor {
            chain,
            link,
            shared,
        }
    }

    /// The shared atomics (for the controller's telemetry reads).
    pub fn shared(&self) -> Arc<RtShared> {
        Arc::clone(&self.shared)
    }

    /// Process one callback's block. Realtime-safe: no locks, no
    /// allocation, no logging; never panics on HAL-supplied frame counts.
    ///
    /// Order: counters -> input scan -> swap poll -> read params ->
    /// chain process -> mismatch telemetry.
    pub fn process_block(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        out_in_sample_delta: f64,
    ) {
        let shared = &self.shared;
        shared.callbacks.fetch_add(1, Ordering::Relaxed);
        shared.store_sample_time_delta(out_in_sample_delta);

        // Input scan: peak + all-zero detection (the watchdog's raw signal).
        let mut peak = 0.0f32;
        for ch in input {
            for &s in *ch {
                let a = s.abs();
                if a > peak {
                    peak = a;
                }
            }
        }
        if peak == 0.0 {
            shared.zero_blocks.fetch_add(1, Ordering::Relaxed);
        } else {
            shared.nonzero_blocks.fetch_add(1, Ordering::Relaxed);
            // Single writer (this thread): load-compare-store is race-free.
            if peak > shared.peak_in() {
                shared.peak_in_bits.store(peak.to_bits(), Ordering::Relaxed);
            }
        }

        self.link.poll(&mut self.chain);

        let bypass = shared.bypass.load(Ordering::Relaxed);
        let gain = shared.gain();
        let outcome = self.chain.process(input, output, bypass, gain);
        if outcome.frame_mismatch {
            shared.frame_mismatch_blocks.fetch_add(1, Ordering::Relaxed);
        }
    }
}
