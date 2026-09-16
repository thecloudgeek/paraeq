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
//! [`Correction`] value is shallow).

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
    /// Total realtime IOProc invocations observed (rt writes) -- every
    /// invocation counts, including blocks the backend skipped before
    /// reaching the chain (via [`RtProcessor::note_skipped_block`]), NOT
    /// just processed blocks. The watchdog keys "the system is rendering
    /// audio" off this counter; if skips stopped it, a persistently broken
    /// session would misreport as benign `Idle`.
    pub callbacks: AtomicU64,
    /// Output samples the +-1.0 clamp engaged on (rt writes), summed over
    /// both chain paths and counted PER SAMPLE PER CHANNEL -- a fully-clipped
    /// stereo block of 512 frames counts 1024, so the drawer must not divide
    /// by the channel count. R1-8's release-blocking half: it is R1-1's
    /// falsifier, and a nonzero count while `auto_preamp_db` is active is a
    /// BUG SIGNAL, not a user error (spec `:571`).
    pub clipped_samples: AtomicU64,
    /// Per-block multiplicative release coefficient for `peak_in_bits` and
    /// `peak_out_bits`, as f32 bits (control writes ONCE per start, rt reads
    /// every block). R1-8's decay without a clock on the realtime lane: the
    /// controller computes it from the negotiated `StreamInfo` at
    /// `start_with`, the rt lane spends one multiply per block applying it
    /// (spec `:515-530`).
    ///
    /// DEFAULTS TO 1.0 -- no decay, i.e. the monotonic peak hold this field
    /// replaces. 0.0 would look like a safer default and is not: it turns the
    /// meter into a PER-BLOCK peak on every path that builds
    /// `RtShared::default()` without going through the controller (every
    /// synthetic-block test in `tests/`, and any future headless driver), so
    /// the defect would surface as "the meter reads right in production and
    /// wrong everywhere else".
    pub decay_per_block_bits: AtomicU32,
    /// Blocks passed through because the frame count did not fit the active
    /// correction (rt writes).
    pub frame_mismatch_blocks: AtomicU64,
    /// Linear trim gain as f32 bits (control writes, rt reads).
    pub gain_bits: AtomicU32,
    /// Non-finite (NaN/inf) samples sanitized to 0.0 (rt writes): input
    /// samples zeroed at the backend's capture boundary
    /// ([`RtProcessor::note_invalid_samples`]) plus correction outputs
    /// zeroed by the chain's output backstop. Logging happens on the
    /// controller tick, never the realtime thread.
    pub invalid_samples: AtomicU64,
    /// Blocks with at least one nonzero input sample (rt writes) -- the
    /// watchdog's raw "audio is flowing" signal.
    pub nonzero_blocks: AtomicU64,
    /// Input METER, as f32 bits (rt writes): the largest input |sample| in
    /// this block, or the previous value released by `decay_per_block_bits`,
    /// whichever is larger. R1-8's decaying half (spec `:515`) -- it falls at
    /// the broadcast-standard 20 dB / 1.7 s, so it answers "how loud is the
    /// music right now", which a session maximum cannot. The session maximum
    /// lives on in `peak_in_session_bits`.
    pub peak_in_bits: AtomicU32,
    /// Maximum input |sample| seen SINCE THE SESSION STARTED, as f32 bits
    /// (rt writes) -- the monotonic session statistic, kept because it is
    /// genuinely useful and free (spec `:552`). It is the ONLY monotonic
    /// peak: `peak_in_bits` decays, so each field now means exactly one
    /// thing (spec `:532`).
    pub peak_in_session_bits: AtomicU32,
    /// Output METER, as f32 bits (rt writes), taken PRE-clamp (see
    /// [`crate::chain::ChainOutcome::peak_out`]) so it reports the real
    /// overshoot instead of saturating at 1.0. Released by the SAME
    /// `decay_per_block_bits` coefficient as `peak_in_bits`.
    ///
    /// The spec specifies decay only for the input peak, and this applies its
    /// own reasoning to the output one: R1-8's defect statement is that a
    /// non-decaying peak is "a session statistic wearing a meter's clothes"
    /// (spec `:505`), and `:571` makes "the Advanced drawer shows output peak"
    /// a meter's job. One coefficient also means the two numbers read
    /// comparably, which is the whole point of showing them together. There
    /// is no output session maximum: nothing asks for one, and
    /// `clipped_samples` already answers "did it ever go over".
    pub peak_out_bits: AtomicU32,
    /// Latest output-vs-input sample-time delta as f64 bits (rt writes).
    pub sample_time_delta_bits: AtomicU64,
    /// Blocks the backend skipped before reaching the chain (e.g. oversize
    /// HAL buffers; rt writes via [`RtProcessor::note_skipped_block`]).
    pub skipped_blocks: AtomicU64,
    /// All-zero input blocks (rt writes) -- the watchdog's silence signal.
    pub zero_blocks: AtomicU64,
}

impl Default for RtShared {
    fn default() -> Self {
        Self {
            bypass: AtomicBool::new(false),
            callbacks: AtomicU64::new(0),
            clipped_samples: AtomicU64::new(0),
            decay_per_block_bits: AtomicU32::new(1.0f32.to_bits()),
            frame_mismatch_blocks: AtomicU64::new(0),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            invalid_samples: AtomicU64::new(0),
            nonzero_blocks: AtomicU64::new(0),
            peak_in_bits: AtomicU32::new(0),
            peak_in_session_bits: AtomicU32::new(0),
            peak_out_bits: AtomicU32::new(0),
            sample_time_delta_bits: AtomicU64::new(0),
            skipped_blocks: AtomicU64::new(0),
            zero_blocks: AtomicU64::new(0),
        }
    }
}

impl RtShared {
    /// Per-block release coefficient (1.0 = no decay).
    pub fn decay_per_block(&self) -> f32 {
        f32::from_bits(self.decay_per_block_bits.load(Ordering::Relaxed))
    }

    /// Set the per-block release coefficient (control plane). Called once
    /// per start, from the negotiated stream geometry, before the meters are
    /// read; see [`crate::controller::decay_per_block`] for the arithmetic.
    pub fn set_decay_per_block(&self, decay: f32) {
        self.decay_per_block_bits
            .store(decay.to_bits(), Ordering::Relaxed);
    }

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

    /// Maximum input |sample| observed since this session started.
    pub fn peak_in_session(&self) -> f32 {
        f32::from_bits(self.peak_in_session_bits.load(Ordering::Relaxed))
    }

    /// Maximum output |sample| observed so far, pre-clamp.
    pub fn peak_out(&self) -> f32 {
        f32::from_bits(self.peak_out_bits.load(Ordering::Relaxed))
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

    /// Record `n` non-finite input samples the backend zeroed at the
    /// capture boundary (the deinterleave copy -- where samples first
    /// become ours). NOT a callback count: the same invocation still runs
    /// [`process_block`](Self::process_block) on the sanitized block.
    /// Realtime-safe: one relaxed atomic add, skipped when `n == 0` so the
    /// happy path pays nothing.
    pub fn note_invalid_samples(&mut self, n: u64) {
        if n > 0 {
            self.shared.invalid_samples.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Record an IOProc invocation whose block the backend skipped before
    /// reaching [`process_block`](Self::process_block) (mismatched or
    /// oversize HAL buffers). Increments BOTH `callbacks` (which counts
    /// every IOProc invocation, not just processed blocks -- see the field
    /// doc) and `skipped_blocks`. Realtime-safe: two relaxed atomic adds.
    pub fn note_skipped_block(&mut self) {
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        self.shared.skipped_blocks.fetch_add(1, Ordering::Relaxed);
    }

    /// Process one callback's block. Realtime-safe: no locks, no
    /// allocation, no logging; never panics on HAL-supplied frame counts.
    ///
    /// Order: counters -> input scan -> swap poll -> read params ->
    /// chain process -> mismatch + non-finite telemetry.
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
            // The session statistic, with the monotonic logic unchanged
            // (spec `:552`). This one stays inside the branch: an all-zero
            // block can never raise a maximum.
            if peak > shared.peak_in_session() {
                shared
                    .peak_in_session_bits
                    .store(peak.to_bits(), Ordering::Relaxed);
            }
        }
        // R1-8's meter (spec `:518`), HOISTED OUT of the zero/nonzero branch
        // on purpose: the spec's snippet replaces a store that sat in the
        // `else`, and transcribing it there leaves SILENCE never decaying --
        // the meter would hold full scale forever after the music stopped,
        // which is the exact case it exists for. Pinned by
        // `tests/test_meters.rs::decay_runs_on_all_zero_blocks_too`.
        //
        // Single writer (this thread): load-compute-store is race-free, the
        // same invariant the monotonic store above relies on. One multiply
        // and one max per block; the coefficient is a plain relaxed load
        // (control writes it once per start, before the meters are read).
        let decay = shared.decay_per_block();
        shared.peak_in_bits.store(
            (peak.max(decay * shared.peak_in())).to_bits(),
            Ordering::Relaxed,
        );

        self.link.poll(&mut self.chain);

        let bypass = shared.bypass.load(Ordering::Relaxed);
        let gain = shared.gain();
        let outcome = self.chain.process(input, output, bypass, gain);
        if outcome.frame_mismatch {
            shared.frame_mismatch_blocks.fetch_add(1, Ordering::Relaxed);
        }
        // Output-backstop sanitizations share the capture-boundary counter:
        // one published number for "non-finite samples were zeroed".
        if outcome.nonfinite_outputs > 0 {
            shared
                .invalid_samples
                .fetch_add(u64::from(outcome.nonfinite_outputs), Ordering::Relaxed);
        }
        // R1-8's fold, in the shape the frame-mismatch fold above already
        // uses. Both meters come off the chain's single pre-clamp scan, so
        // they cannot disagree with each other.
        if outcome.clipped > 0 {
            shared
                .clipped_samples
                .fetch_add(u64::from(outcome.clipped), Ordering::Relaxed);
        }
        // The output meter, released by the same coefficient (see
        // `peak_out_bits`). `ChainOutcome::peak_out` is produced on BOTH chain
        // paths and on every block, so this store is unconditional -- there is
        // no block whose peak is "unknown" and must be held.
        shared.peak_out_bits.store(
            (outcome.peak_out.max(decay * shared.peak_out())).to_bits(),
            Ordering::Relaxed,
        );
    }
}
