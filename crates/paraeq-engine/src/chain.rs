//! Realtime processing chain: `bypass? -> correction -> trim gain -> safety
//! clamp(+-1.0)` (spec line 83).
//!
//! [`RealtimeChain::process`] runs on the realtime audio thread: no locks, no
//! allocation, no logging. All scratch is preallocated in
//! [`RealtimeChain::new`]; corrections are constructed and warmed up on the
//! control plane via [`build_fir`] / [`build_iir`] and moved in whole through
//! [`RealtimeChain::set_correction`].

use paraeq_dsp::biquad::{is_stable, IDENTITY};

use crate::convolver::OverlapAddConvolver;
use crate::iir::IIRProcessor;

/// The correction processor itself, one variant per arm. Formerly the whole
/// of `Correction`; R1-1 moved it inside so the preamp can ride along.
pub enum CorrectionKind {
    Fir(OverlapAddConvolver),
    Iir(IIRProcessor),
}

/// A ready-to-run correction (built + warmed up off-thread) TOGETHER WITH the
/// preamp that protects it.
///
/// R1-1 (spec `:85-99`): the preamp must swap **atomically with the correction
/// it protects**. Routing it through `RtShared::gain_bits` would not -- the
/// correction arrives through the `rtrb` swap ring and the gain through a
/// relaxed store, with no ordering between them, so a +12 dB boost could run
/// un-preamped for a window. Carrying it here makes the swap one move.
///
/// The engine-hardening Decisions Log (`:20`) records the same call and the
/// alternative it rejected, verbatim: *"Preamp carrier | A `preamp_lin` field
/// **inside** `Correction`, applied only on the corrected path | (rejected) A
/// second atomic alongside `gain_bits` (correction and preamp would swap
/// non-atomically -> a window of un-preamped boost -> clipping)"*.
pub struct Correction {
    pub kind: CorrectionKind,
    /// Linear auto-preamp, `10^(preamp_db/20)` with `preamp_db <= 0`, so it
    /// is always in `(0, 1]`. Applied ONLY on the corrected path
    /// ([`RealtimeChain::process`]) -- spec `:99`, verbatim: "Critically,
    /// **the preamp applies only on the corrected path** -- ... the
    /// pass-through ... must not attenuate, because there is no boost to
    /// compensate." It composes with the user's trim (`RtShared::gain_bits`)
    /// as a product, i.e. they add in dB; the two are never folded into one
    /// number, so the user can move their trim without removing the headroom
    /// the boosts need.
    pub preamp_lin: f32,
}

impl Correction {
    /// Transplant filter state from the outgoing correction on a
    /// coefficient swap so the swap is click-free (spec R1-7a). Kind-aware:
    /// IIR delay lines are copied over ([`IIRProcessor::adopt_state_from`]);
    /// a no-op across kinds (a FIR overlap tail is `x_prev (*) h_old` and
    /// cannot be transplanted -- R1-7b's crossfade covers that arm) and
    /// across a channel-count change. `channels` is the stream width the
    /// chain processes: the processors' own slot counts are
    /// broadcast-inflated when a config carries more SOS sets than the
    /// stream has channels ([`build_iir`] sizes `channels.max(sets)`), so
    /// comparing raw slot counts would misread a set-count artifact as a
    /// channel-count change and reintroduce the click on that swap.
    /// Realtime-safe: a bounded memcpy over already-sized state, no
    /// allocation.
    pub fn adopt_state_from(&mut self, old: &Correction, channels: usize) {
        if let (CorrectionKind::Iir(new), CorrectionKind::Iir(old)) = (&mut self.kind, &old.kind) {
            if new.channels().min(channels) == old.channels().min(channels) {
                new.adopt_state_from(old);
            }
        }
    }

    /// Clear filter state (overlap tails / biquad delay lines). Does not
    /// deallocate; realtime-safe.
    pub fn reset(&mut self) {
        match &mut self.kind {
            CorrectionKind::Fir(c) => c.reset(),
            CorrectionKind::Iir(p) => p.reset(),
        }
    }
}

/// What [`RealtimeChain::process`] did with the block.
///
/// No `Eq`: `peak_out` is an f32 (R1-8). Nothing compares this struct whole --
/// every assertion in `tests/test_chain.rs` is per field -- so the derive was
/// free to drop.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChainOutcome {
    /// Samples (per channel, not per frame: a fully-clipped stereo block of
    /// 512 frames counts 1024) whose value exceeded the +-1.0 clamp on THIS
    /// block, measured before the clamp. Counted on both paths -- the
    /// pass-through clamps too (spec `:554`: "**both** of them"). Non-finite
    /// correction outputs are zeroed by the backstop BEFORE this scan and are
    /// never counted here; they are `nonfinite_outputs`.
    pub clipped: u32,
    /// The correction processor ran on this block.
    pub corrected: bool,
    /// A correction was active but the HAL-supplied frame count did not fit
    /// its contract; the block was passed through (gain + clamp only).
    pub frame_mismatch: bool,
    /// Non-finite correction outputs zeroed by the output backstop; any
    /// nonzero count also reset the correction state (a poisoned DF2T
    /// section emits NaN forever otherwise). Always 0 on the pass-through
    /// path (its clamp handles +-inf; input NaN is zeroed upstream at the
    /// capture boundary).
    pub nonfinite_outputs: u32,
    /// Largest `|sample|` this block produced, taken PRE-clamp, so it reports
    /// the real overshoot rather than saturating at 1.0.
    ///
    /// The spec contradicts itself here and this is the reconciliation: its
    /// fix snippet (`:558`) takes `let a = v.abs();` before the clamp, while
    /// its test row (`:577`) asserts `output_peak == 1.0` for a full-scale
    /// sine at +6 dB, which is only true post-clamp (pre-clamp it is ~2.0).
    /// Pre-clamp wins on three grounds: a post-clamp peak carries no
    /// information [`ChainOutcome::clipped`] does not (it saturates at exactly
    /// 1.0 precisely when `clipped > 0`); pre-clamp is the number that proves
    /// the preamp is right, which `:571` makes the Advanced drawer's job; and
    /// both then derive from the one quantity `a`, so they cannot disagree.
    pub peak_out: f32,
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
    ///
    /// On a swap (old and new both present) the incoming correction adopts
    /// the outgoing one's filter state ([`Correction::adopt_state_from`])
    /// so a coefficient edit does not restart the filters from zero -- an
    /// audible step on every band drag otherwise (spec R1-7a). The
    /// transplant happens here, on the realtime thread, because this is
    /// the only place the old processor still lives.
    pub fn set_correction(&mut self, mut c: Option<Correction>) -> Option<Correction> {
        if let (Some(new), Some(old)) = (c.as_mut(), self.correction.as_ref()) {
            new.adopt_state_from(old, self.channels);
        }
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
    /// down-cast to f32, then gain, then clamp -- with non-finite outputs
    /// zeroed + counted and the correction state reset (the output
    /// backstop; see [`ChainOutcome::nonfinite_outputs`]).
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
            && match self.correction.as_ref().map(|c| &c.kind) {
                Some(CorrectionKind::Fir(_)) => frames == self.block_size,
                Some(CorrectionKind::Iir(_)) => frames <= self.block_size,
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
            // R1-1: read the preamp off the correction it swapped in with.
            let preamp_lin = correction.as_ref().map_or(1.0, |c| c.preamp_lin);

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
            match &mut correction.as_mut().expect("checked is_some above").kind {
                CorrectionKind::Fir(c) => c.process(&views[..channels], &mut out_f64[..channels]),
                CorrectionKind::Iir(p) => p.process(&views[..channels], &mut out_f64[..channels]),
            }

            // Output backstop: the gain+clamp loop also zeroes non-finite
            // correction outputs and counts them (f32::clamp propagates NaN,
            // so the clamp alone is not a sanitizer). Finite samples take
            // the identical downcast -> gain -> clamp path bit-for-bit.
            //
            // R1-8 fuses the meter scan into the same loop (two extra compares
            // per sample). ORDER IS LOAD-BEARING: the R1-2 `is_finite` backstop
            // runs FIRST, so a zeroed non-finite sample is neither counted as a
            // clip nor allowed into `peak_out` -- it is already reported as
            // `nonfinite_outputs`, and a NaN would poison the peak fold.
            let mut clipped = 0u32;
            let mut nonfinite = 0u32;
            let mut peak_out = 0.0f32;
            for (out_ch, y_ch) in output.iter_mut().zip(out_f64.iter()) {
                for (o, y) in out_ch.iter_mut().zip(y_ch.iter()) {
                    let v = (*y as f32) * preamp_lin * gain;
                    if v.is_finite() {
                        let a = v.abs();
                        if a > peak_out {
                            peak_out = a;
                        }
                        if a > 1.0 {
                            clipped += 1;
                        }
                        *o = v.clamp(-1.0, 1.0);
                    } else {
                        nonfinite += 1;
                        *o = 0.0;
                    }
                }
            }
            if nonfinite > 0 {
                // Self-heal: a non-finite output usually means poisoned
                // filter state (one NaN makes a DF2T section NaN for its
                // lifetime), so clear it (reset is fill(0.0)-only --
                // rt-safe) instead of emitting noise forever. An audible
                // discontinuity, but a click beats permanent noise. Input
                // NaN never reaches here (zeroed at the capture boundary);
                // this fires on filter-internal non-finites OR on a finite
                // f64 output overflowing the f32 downcast (|y| > f32::MAX
                // casts to inf -- a Jury-stable cascade can still realize
                // >770 dB of gain), so the reset is not proof of NaN state
                // and must stay unconditional rather than keyed on it.
                if let Some(c) = correction.as_mut() {
                    c.reset();
                }
            }
            ChainOutcome {
                clipped,
                corrected: true,
                frame_mismatch: false,
                nonfinite_outputs: nonfinite,
                peak_out,
            }
        } else {
            // Pass-through: gain + clamp, pure f32 path, no scratch needed.
            // Tolerates ANY input/output shape (zip stops at the shorter).
            //
            // The same meter scan, because this path clamps too (spec `:554`:
            // "fuse into the gain/clamp loops -- **both** of them"), but NO
            // preamp multiply: spec `:99` -- bypass / frame-mismatch / no
            // correction "must not attenuate, because there is no boost to
            // compensate." Applying it here would make the bypassed side of
            // the product's headline A/B control quieter than the corrected
            // side by the whole preamp.
            let mut clipped = 0u32;
            let mut peak_out = 0.0f32;
            for (out_ch, in_ch) in output.iter_mut().zip(input.iter()) {
                for (o, s) in out_ch.iter_mut().zip(in_ch.iter()) {
                    let v = *s * gain;
                    let a = v.abs();
                    if a > peak_out {
                        peak_out = a;
                    }
                    if a > 1.0 {
                        clipped += 1;
                    }
                    *o = v.clamp(-1.0, 1.0);
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
                clipped,
                corrected: false,
                frame_mismatch,
                nonfinite_outputs: 0,
                peak_out,
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
    match &mut correction.kind {
        CorrectionKind::Fir(c) => c.process(&inputs, &mut outputs),
        CorrectionKind::Iir(p) => p.process(&inputs, &mut outputs),
    }
    correction.reset();
}

/// Build + warm up a FIR correction. Control plane only (allocates).
///
/// `preamp_lin` is R1-1's auto-preamp for this correction, computed by the
/// caller ([`crate::preamp::fir_preamp_db`]) and carried through so it swaps
/// atomically with the coefficients it protects.
pub fn build_fir(
    firs: Vec<Vec<f64>>,
    channels: usize,
    block_size: usize,
    preamp_lin: f32,
) -> Correction {
    let mut c = Correction {
        kind: CorrectionKind::Fir(OverlapAddConvolver::new(firs, block_size)),
        preamp_lin,
    };
    warm_up(&mut c, channels, block_size);
    c
}

/// Build + warm up an IIR correction. Control plane only (allocates).
/// Returns the correction and the count of substituted (dropped) sections.
///
/// `preamp_lin` is R1-1's auto-preamp for this correction, computed by the
/// caller from the SAME realized cascade that reaches this funnel, and carried
/// through so it swaps atomically with the coefficients it protects.
///
/// Stability backstop (spec R1-3): this is the single funnel every SOS row
/// passes through on its way to the realtime thread. Rows failing the
/// strict Jury check ([`is_stable`] -- non-finite coefficients, `q = 0`
/// NaN designs, at/above-Nyquist poles) are replaced with the identity
/// section BEFORE broadcast, so one bad band degrades to "that band did
/// nothing" instead of a runaway filter or no correction at all. Each bad
/// input row counts once toward the returned count regardless of how many
/// channels it broadcasts to.
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
    preamp_lin: f32,
) -> (Correction, usize) {
    assert!(
        !sos_per_channel.is_empty(),
        "at least one SOS set is required"
    );
    let mut substituted = 0;
    let sos_per_channel: Vec<Vec<[f64; 6]>> = sos_per_channel
        .into_iter()
        .map(|set| {
            set.into_iter()
                .map(|sos| {
                    if is_stable(&sos) {
                        sos
                    } else {
                        substituted += 1;
                        IDENTITY
                    }
                })
                .collect()
        })
        .collect();
    let mut p = IIRProcessor::new();
    for ch in 0..channels.max(sos_per_channel.len()) {
        let sos = sos_per_channel[ch.min(sos_per_channel.len() - 1)].clone();
        p.set_sos(ch, sos);
    }
    let mut c = Correction {
        kind: CorrectionKind::Iir(p),
        preamp_lin,
    };
    warm_up(&mut c, channels, block_size);
    (c, substituted)
}
