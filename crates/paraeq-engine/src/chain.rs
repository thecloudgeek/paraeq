//! Realtime processing chain: `bypass? -> correction -> trim gain -> safety
//! clamp(+-1.0)` (spec line 83).
//!
//! [`RealtimeChain::process`] runs on the realtime audio thread: no locks, no
//! allocation, no logging. All scratch is preallocated in
//! [`RealtimeChain::new`]; corrections are constructed and warmed up on the
//! control plane via [`build_fir`] / [`build_iir`] and moved in whole through
//! [`RealtimeChain::set_correction`].

use crate::convolver::OverlapAddConvolver;
use crate::iir::IIRProcessor;

/// A ready-to-run correction processor (built + warmed up off-thread).
pub enum Correction {
    Fir(OverlapAddConvolver),
    Iir(IIRProcessor),
}

impl Correction {
    /// Clear filter state (overlap tails / biquad delay lines). Does not
    /// deallocate; realtime-safe.
    pub fn reset(&mut self) {
        match self {
            Correction::Fir(c) => c.reset(),
            Correction::Iir(p) => p.reset(),
        }
    }
}

/// What [`RealtimeChain::process`] did with the block.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChainOutcome {
    /// The correction processor ran on this block.
    pub corrected: bool,
    /// A correction was active but the HAL-supplied frame count did not fit
    /// its contract; the block was passed through (gain + clamp only).
    pub frame_mismatch: bool,
}

/// Per-stream realtime chain state. Owned by the realtime side; the control
/// plane only ever communicates with it through moved [`Correction`] values
/// (Task 5's rings) and atomics.
pub struct RealtimeChain {
    block_size: usize,
    channels: usize,
    correction: Option<Correction>,
    /// f64 up-cast scratch, `channels x block_size`, preallocated.
    in_f64: Vec<Vec<f64>>,
    /// f64 correction output scratch, `channels x block_size`, preallocated.
    out_f64: Vec<Vec<f64>>,
    prev_bypass: bool,
}

impl RealtimeChain {
    /// Build a chain for `channels` (1..=2 -- the parity tap is stereo) and
    /// the negotiated `block_size`. Preallocates all f64 scratch.
    pub fn new(channels: usize, block_size: usize) -> RealtimeChain {
        assert!(
            (1..=2).contains(&channels),
            "channels must be 1 or 2, got {channels}"
        );
        assert!(block_size > 0, "block_size must be nonzero");
        RealtimeChain {
            block_size,
            channels,
            correction: None,
            in_f64: vec![vec![0.0; block_size]; channels],
            out_f64: vec![vec![0.0; block_size]; channels],
            prev_bypass: false,
        }
    }

    /// The block size this chain was built for.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// The channel count this chain was built for.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Install (or clear) the correction, returning the previous one.
    ///
    /// Plain move -- realtime-safe. Used by the ring poll (Task 5): the old
    /// value must be shipped back to the control plane for deallocation,
    /// never dropped here.
    pub fn set_correction(&mut self, c: Option<Correction>) -> Option<Correction> {
        std::mem::replace(&mut self.correction, c)
    }

    /// Process one block: `bypass? -> correction -> gain -> clamp(+-1.0)`.
    ///
    /// Realtime-safe: never allocates, never locks, and NEVER panics on a
    /// HAL-supplied frame count. Correction applies only when the frame
    /// count fits its contract: `Fir` requires exactly `block_size` frames;
    /// `Iir` accepts any `frames <= block_size` (scratch is resized within
    /// its preallocated capacity). Any other frame count passes the block
    /// through (gain + clamp, pure f32 path) and reports `frame_mismatch`.
    /// Persistent frame-size changes arrive as FormatChanged backend events
    /// and trigger a chain rebuild on the control plane.
    ///
    /// Sample path when correcting: f32 up-cast to f64, correction in f64,
    /// down-cast to f32, then gain, then clamp.
    pub fn process(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        bypass: bool,
        gain: f32,
    ) -> ChainOutcome {
        // Bypassed -> active edge: reset correction state so no stale
        // transient thumps into the re-engaged output.
        if self.prev_bypass && !bypass {
            if let Some(c) = self.correction.as_mut() {
                c.reset();
            }
        }
        self.prev_bypass = bypass;

        let frames = input.first().map_or(0, |ch| ch.len());
        // Uniform frame guard: correction only runs on a well-formed block
        // (expected channel count, every channel the same length).
        let uniform = input.len() == self.channels
            && output.len() == self.channels
            && input.iter().all(|ch| ch.len() == frames)
            && output.iter().all(|ch| ch.len() == frames);
        let fits = uniform
            && frames > 0
            && match &self.correction {
                Some(Correction::Fir(_)) => frames == self.block_size,
                Some(Correction::Iir(_)) => frames <= self.block_size,
                None => true,
            };

        if !bypass && fits && self.correction.is_some() {
            // Split borrows: scratch and correction borrowed independently.
            let Self {
                channels,
                correction,
                in_f64,
                out_f64,
                ..
            } = self;
            let channels = *channels;

            for (dst, src) in in_f64.iter_mut().zip(input.iter()) {
                dst.resize(frames, 0.0); // within preallocated capacity
                for (d, s) in dst.iter_mut().zip(src.iter()) {
                    *d = f64::from(*s);
                }
            }
            for dst in out_f64.iter_mut() {
                dst.resize(frames, 0.0); // within preallocated capacity
            }

            // Stack-array input views sliced to the channel count -- never a
            // Vec of refs (that would allocate on the realtime path).
            let empty: &[f64] = &[];
            let views: [&[f64]; 2] = [
                in_f64.first().map_or(empty, |v| v.as_slice()),
                in_f64.get(1).map_or(empty, |v| v.as_slice()),
            ];
            match correction.as_mut().expect("checked is_some above") {
                Correction::Fir(c) => c.process(&views[..channels], &mut out_f64[..channels]),
                Correction::Iir(p) => p.process(&views[..channels], &mut out_f64[..channels]),
            }

            for (out_ch, y_ch) in output.iter_mut().zip(out_f64.iter()) {
                for (o, y) in out_ch.iter_mut().zip(y_ch.iter()) {
                    *o = ((*y as f32) * gain).clamp(-1.0, 1.0);
                }
            }
            ChainOutcome {
                corrected: true,
                frame_mismatch: false,
            }
        } else {
            // Pass-through: gain + clamp, pure f32 path, no scratch needed.
            // Tolerates ANY input/output shape (zip stops at the shorter).
            for (out_ch, in_ch) in output.iter_mut().zip(input.iter()) {
                for (o, s) in out_ch.iter_mut().zip(in_ch.iter()) {
                    *o = (*s * gain).clamp(-1.0, 1.0);
                }
            }
            let frame_mismatch = !bypass && self.correction.is_some() && !fits;
            if frame_mismatch {
                // A mismatched block interrupted an ACTIVE correction: the
                // samples it never saw make its overlap tail / biquad state
                // stale, so clear it (reset is fill(0.0)-only -- rt-safe)
                // and let the correction resume clean on the next
                // conforming block instead of emitting a stale tail.
                if let Some(c) = self.correction.as_mut() {
                    c.reset();
                }
            }
            ChainOutcome {
                corrected: false,
                frame_mismatch,
            }
        }
    }
}

/// Warm a freshly built correction up (control plane only): one silent
/// `process` call at the real channel count satisfies both processors'
/// warm-up contracts (the convolver's first call allocates per-channel
/// overlap state), then reset so no warm-up residue reaches the stream.
fn warm_up(correction: &mut Correction, channels: usize, block_size: usize) {
    let silent = vec![0.0f64; block_size];
    let inputs: Vec<&[f64]> = (0..channels).map(|_| silent.as_slice()).collect();
    let mut outputs = vec![vec![0.0f64; block_size]; channels];
    match correction {
        Correction::Fir(c) => c.process(&inputs, &mut outputs),
        Correction::Iir(p) => p.process(&inputs, &mut outputs),
    }
    correction.reset();
}

/// Build + warm up a FIR correction. Control plane only (allocates).
pub fn build_fir(firs: Vec<Vec<f64>>, channels: usize, block_size: usize) -> Correction {
    let mut c = Correction::Fir(OverlapAddConvolver::new(firs, block_size));
    warm_up(&mut c, channels, block_size);
    c
}

/// Build + warm up an IIR correction. Control plane only (allocates).
///
/// When fewer SOS sets are supplied than `channels`, the LAST provided set
/// is broadcast to the remaining channels (mirroring
/// [`OverlapAddConvolver`]'s documented mono-FIR broadcast), so a mono
/// config corrects every channel of a stereo chain identically.
/// `sos_per_channel` must be non-empty.
pub fn build_iir(
    sos_per_channel: Vec<Vec<[f64; 6]>>,
    channels: usize,
    block_size: usize,
) -> Correction {
    assert!(
        !sos_per_channel.is_empty(),
        "at least one SOS set is required"
    );
    let mut p = IIRProcessor::new();
    for ch in 0..channels.max(sos_per_channel.len()) {
        let sos = sos_per_channel[ch.min(sos_per_channel.len() - 1)].clone();
        p.set_sos(ch, sos);
    }
    let mut c = Correction::Iir(p);
    warm_up(&mut c, channels, block_size);
    c
}
