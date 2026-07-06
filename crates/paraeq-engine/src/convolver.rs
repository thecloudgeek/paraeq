//! Block overlap-add FFT convolver, block-for-block parity with
//! prototype/paraeq/engine/convolver.py (accumulating-remainder scheme).

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex;
use std::sync::Arc;

/// Block-based overlap-add FIR convolver, one FIR per channel.
///
/// A single FIR is broadcast to every input channel (mirrors the Python
/// oracle's `min(ch, n_firs - 1)` channel-to-FIR mapping), so a mono FIR
/// can drive a stereo (or wider) stream without panicking.
///
/// **Allocation contract:** [`OverlapAddConvolver::new`] preallocates all
/// scratch buffers and FIR spectra. [`OverlapAddConvolver::process`] is
/// allocation-free from its second call onward; the *first* call may
/// allocate per-channel overlap state sized to the channel count passed in
/// that call. Realtime callers must therefore warm up with one off-thread
/// `process` call (with the real channel count) before going live, matching
/// the realtime-engine's warm-up-then-swap-in contract.
pub struct OverlapAddConvolver {
    block_size: usize,
    n_fft: usize,
    fir_spectra: Vec<Vec<Complex<f64>>>,
    /// per-channel accumulated tail; length n_fft - block_size
    overlaps: Vec<Vec<f64>>,
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
    // preallocated scratch
    time_scratch: Vec<f64>,
    spec_scratch: Vec<Complex<f64>>,
    full_scratch: Vec<f64>,
}

impl OverlapAddConvolver {
    /// Build a convolver for the given per-channel FIRs and block size.
    ///
    /// `firs` must be non-empty. If fewer FIRs are supplied than channels
    /// seen at `process` time, the last FIR is reused for the remaining
    /// channels (channel `ch` uses `firs[ch.min(firs.len() - 1)]`).
    pub fn new(firs: Vec<Vec<f64>>, block_size: usize) -> Self {
        assert!(!firs.is_empty(), "at least one FIR is required");
        let fir_len = firs.iter().map(Vec::len).max().unwrap();
        let n_fft = (block_size + fir_len - 1).next_power_of_two();
        let mut planner = RealFftPlanner::<f64>::new();
        let fwd = planner.plan_fft_forward(n_fft);
        let inv = planner.plan_fft_inverse(n_fft);
        let fir_spectra = firs
            .iter()
            .map(|f| {
                let mut padded = vec![0.0; n_fft];
                padded[..f.len()].copy_from_slice(f);
                let mut spec = fwd.make_output_vec();
                fwd.process(&mut padded, &mut spec).unwrap();
                spec
            })
            .collect();
        let spec_scratch = fwd.make_output_vec();
        Self {
            block_size,
            n_fft,
            fir_spectra,
            overlaps: Vec::new(), // sized on first process()
            fwd,
            inv,
            time_scratch: vec![0.0; n_fft],
            spec_scratch,
            full_scratch: vec![0.0; n_fft],
        }
    }

    /// Filter one block per channel in place.
    ///
    /// Each `input[ch]` and `output[ch]` must be exactly `block_size`
    /// samples long; `output[ch]` is fully overwritten. See the struct docs
    /// for the allocation contract (first call may allocate, steady state
    /// does not).
    pub fn process(&mut self, input: &[&[f64]], output: &mut [Vec<f64>]) {
        assert_eq!(
            input.len(),
            output.len(),
            "input/output channel count mismatch"
        );
        if self.overlaps.len() < input.len() {
            self.overlaps
                .resize_with(input.len(), || vec![0.0; self.n_fft - self.block_size]);
        }
        for (ch, block) in input.iter().enumerate() {
            assert_eq!(block.len(), self.block_size, "block size mismatch");
            let fir_idx = ch.min(self.fir_spectra.len() - 1);

            self.time_scratch[..self.block_size].copy_from_slice(block);
            self.time_scratch[self.block_size..].fill(0.0);
            self.fwd
                .process(&mut self.time_scratch, &mut self.spec_scratch)
                .unwrap();

            for (s, f) in self.spec_scratch.iter_mut().zip(&self.fir_spectra[fir_idx]) {
                *s *= f;
            }

            self.inv
                .process(&mut self.spec_scratch, &mut self.full_scratch)
                .unwrap();
            let scale = 1.0 / self.n_fft as f64;
            for v in self.full_scratch.iter_mut() {
                *v *= scale;
            }

            // Accumulating-remainder overlap-add: fold the previous block's
            // stored tail into this block's full convolution result...
            for (v, o) in self.full_scratch.iter_mut().zip(&self.overlaps[ch]) {
                *v += o;
            }
            // ...emit the first `block_size` samples as output...
            output[ch][..self.block_size].copy_from_slice(&self.full_scratch[..self.block_size]);
            // ...and stash the (already-accumulated) tail as the new overlap.
            self.overlaps[ch].copy_from_slice(&self.full_scratch[self.block_size..]);
        }
    }

    /// Clear accumulated overlap state (does not deallocate).
    pub fn reset(&mut self) {
        for o in &mut self.overlaps {
            o.fill(0.0);
        }
    }
}
