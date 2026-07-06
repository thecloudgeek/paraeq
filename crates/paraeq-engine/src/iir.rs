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

    /// Filter one block per channel in place, with persistent per-channel state.
    ///
    /// Each `input[ch]` is filtered by the cascaded-biquad state for that
    /// channel, and the result is written to `output[ch]`. Each `output[ch]`
    /// must be a `Vec` with capacity >= `block_len`; `clear()+extend_from_slice`
    /// never reallocates once capacity suffices, so steady-state calls are
    /// allocation-free. Realtime callers should therefore pass pre-sized buffers
    /// (e.g., `vec![0.0; block_len]` once, reused every call) and warm up with
    /// one off-thread `process` call before going live.
    pub fn process(&mut self, input: &[&[f64]], output: &mut [Vec<f64>]) {
        assert_eq!(
            input.len(),
            output.len(),
            "input/output channel count mismatch"
        );
        for (ch, block) in input.iter().enumerate() {
            let out = &mut output[ch];
            out.clear();
            out.extend_from_slice(block);
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
