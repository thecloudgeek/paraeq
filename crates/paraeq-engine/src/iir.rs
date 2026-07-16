//! Cascaded-biquad block processor with persistent per-channel state
//! (scipy.signal.sosfilt semantics). Oracle: prototype/paraeq/engine/iir_processor.py

struct ChannelState {
    sos: Vec<[f64; 6]>,
    zi: Vec<[f64; 2]>,
}

#[derive(Default)]
pub struct IIRProcessor {
    channels: Vec<Option<ChannelState>>,
}

impl IIRProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_sos(&mut self, channel: usize, sos: Vec<[f64; 6]>) {
        if self.channels.len() <= channel {
            self.channels.resize_with(channel + 1, || None);
        }
        let zi = vec![[0.0; 2]; sos.len()];
        self.channels[channel] = Some(ChannelState { sos, zi });
    }

    /// Number of channel slots configured via [`set_sos`](Self::set_sos).
    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Transplant per-channel delay state from `old` -- the outgoing
    /// processor on a coefficient swap -- so this cascade continues where
    /// the old one left off instead of restarting from zero (an audible
    /// step on every band edit otherwise; spec R1-7a). Per channel:
    /// sections beyond the old cascade's length are zeroed; state beyond
    /// the new cascade's length is discarded. Realtime-safe: bounded
    /// copies into `zi` already sized by [`set_sos`](Self::set_sos), no
    /// allocation.
    pub fn adopt_state_from(&mut self, old: &IIRProcessor) {
        for (new_ch, old_ch) in self.channels.iter_mut().zip(old.channels.iter()) {
            let (Some(new_state), Some(old_state)) = (new_ch.as_mut(), old_ch.as_ref()) else {
                continue;
            };
            let carried = new_state.zi.len().min(old_state.zi.len());
            new_state.zi[..carried].copy_from_slice(&old_state.zi[..carried]);
            for z in &mut new_state.zi[carried..] {
                *z = [0.0; 2];
            }
        }
    }

    /// Filter one block per channel in place, with persistent per-channel state.
    ///
    /// Each `input[ch]` is filtered by the cascaded-biquad state for that
    /// channel, and the result is written to `output[ch]`.
    ///
    /// Unified output contract: `output[ch].len()` must equal
    /// `input[ch].len()` on entry (asserted); contents are fully overwritten;
    /// the call never allocates. Realtime callers pass pre-sized buffers
    /// (e.g., `vec![0.0; block_len]` once, reused every call) — a mis-sized
    /// buffer is a loud panic, never a silent realtime allocation.
    pub fn process(&mut self, input: &[&[f64]], output: &mut [Vec<f64>]) {
        assert_eq!(
            input.len(),
            output.len(),
            "input/output channel count mismatch"
        );
        for (ch, block) in input.iter().enumerate() {
            let out = &mut output[ch];
            assert_eq!(
                out.len(),
                block.len(),
                "output[ch] length must equal input[ch] length (fully overwritten)"
            );
            out.copy_from_slice(block);
            if let Some(Some(state)) = self.channels.get_mut(ch) {
                for (sec, z) in state.sos.iter().zip(state.zi.iter_mut()) {
                    let [b0, b1, b2, _, a1, a2] = *sec;
                    for v in out.iter_mut() {
                        let x = *v;
                        let y = b0 * x + z[0];
                        z[0] = b1 * x - a1 * y + z[1];
                        z[1] = b2 * x - a2 * y;
                        *v = y;
                    }
                }
            }
        }
    }

    pub fn reset(&mut self) {
        for ch in self.channels.iter_mut().flatten() {
            for z in &mut ch.zi {
                *z = [0.0; 2];
            }
        }
    }
}
