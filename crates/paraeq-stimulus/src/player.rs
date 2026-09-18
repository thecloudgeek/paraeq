//! The audio half, behind a trait so everything else in this crate is testable
//! with no device.
//!
//! The trait is `paraeq_coreaudio::RenderSink` — the re-exported one, not a
//! second spelling of it. A second spelling would be a second contract to keep
//! in step with the real device.
//!
//! **This process applies no gain, ever.** Same contract the in-process
//! stimulus sink keeps: the block arrives already at level, and multiplying
//! here would double-apply it. There is no `--level` flag to disagree with the
//! file, and `RenderSink` has no level parameter to mis-pass.

use paraeq_coreaudio::{MeasureError, RenderSink, StreamFormat};

use crate::backstop::guard_block;
use crate::protocol::{ExitCode, Refusal};
use crate::ramp;

/// How a play ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Something asked for an abort: the stimulus was faded over `ramp_frames`
    /// and the device torn down. This is exit 0, not a failure.
    Aborted {
        frames_emitted: u64,
        ramp_frames: u64,
    },
    /// Played to the end.
    Done {
        clamped: u64,
        frames_emitted: u64,
        sanitized: u64,
    },
}

/// Owns the sink for the length of one stimulus.
pub struct Player {
    format: StreamFormat,
    sink: Box<dyn RenderSink>,
}

impl Player {
    pub fn new(sink: Box<dyn RenderSink>) -> Player {
        Player {
            format: sink.format(),
            sink,
        }
    }

    pub fn format(&self) -> &StreamFormat {
        &self.format
    }

    /// Play `samples`, polling `aborting` once per block.
    ///
    /// The cadence is real rather than nominal because `RenderSink::write` is
    /// PACED by the device: it returns only once the device is within about one
    /// block of caught up, so the poll happens roughly once per block of real
    /// time and the fade reaches the device within about one block of the
    /// trigger. That is MS-14's "ramp to zero over ~5 ms starting on the first
    /// block after the trigger", across a process boundary.
    ///
    /// On abort the sink is ARMED first (`ramp_out`), then handed exactly one
    /// more block: the next `ramp_len` samples of the stimulus, faded. Arming
    /// first is what guarantees nothing at level can follow the fade — the
    /// alternative, stopping, is itself the full-scale click the fade exists to
    /// prevent.
    ///
    /// Every exit path from here — abort, end of file, a sink error — ends in
    /// `stop`, which destroys the render device. That is the invariant this
    /// whole process is built around.
    pub fn play(
        &mut self,
        samples: &[f32],
        aborting: &dyn Fn() -> bool,
        mut on_progress: impl FnMut(u64),
    ) -> Result<Outcome, Refusal> {
        let block_frames = self.format.frames_per_block.max(1);
        let rate = self.format.sample_rate_hz;
        let mut clamped = 0u64;
        let mut sanitized = 0u64;
        let mut emitted = 0u64;
        let mut scratch: Vec<f32> = Vec::with_capacity(block_frames);
        let mut next_progress = block_frames as u64;

        let mut offset = 0usize;
        while offset < samples.len() {
            if aborting() {
                let ramp_frames = self.abort_from(samples, offset, rate)?;
                return Ok(Outcome::Aborted {
                    frames_emitted: emitted,
                    ramp_frames,
                });
            }
            let end = (offset + block_frames).min(samples.len());
            scratch.clear();
            scratch.extend_from_slice(&samples[offset..end]);
            let (c, s) = guard_block(&mut scratch);
            clamped += c;
            sanitized += s;
            self.sink.write(&scratch).map_err(sink_refusal)?;
            emitted += (end - offset) as u64;
            offset = end;
            if emitted >= next_progress {
                on_progress(emitted);
                // Roughly four times a second at any sane geometry, without a
                // clock on this path.
                next_progress = emitted + (rate.max(1.0) as u64) / 4;
            }
        }

        // One last look: an abort that arrived during the final block still
        // gets a fade rather than a hard end-of-file stop.
        if aborting() {
            let ramp_frames = self.abort_from(samples, samples.len(), rate)?;
            return Ok(Outcome::Aborted {
                frames_emitted: emitted,
                ramp_frames,
            });
        }

        self.sink.stop();
        Ok(Outcome::Done {
            clamped,
            frames_emitted: emitted,
            sanitized,
        })
    }

    /// Arm the sink, write one faded block, tear down.
    ///
    /// The faded block is the stimulus's own next samples where there are any,
    /// and silence past the end — fading from where the signal actually is,
    /// rather than re-rendering it, is what MS-14 asks for.
    fn abort_from(&mut self, samples: &[f32], offset: usize, rate: f64) -> Result<u64, Refusal> {
        self.sink.ramp_out();
        let len = ramp::ramp_len(rate);
        let mut tail = vec![0.0f32; len];
        let available = samples.len().saturating_sub(offset).min(len);
        tail[..available].copy_from_slice(&samples[offset..offset + available]);
        let faded = ramp::apply(&mut tail, rate) as u64;
        guard_block(&mut tail);
        let written = self.sink.write(&tail);
        // Tear down whatever the write did: a failed fade is still a device
        // that has to be destroyed, and leaving it open to report the error
        // would be the leak this ladder exists to prevent.
        self.sink.stop();
        written.map_err(sink_refusal)?;
        Ok(faded)
    }

    /// Tear the device down. Idempotent; safe to call on every exit path.
    pub fn stop(&mut self) {
        self.sink.stop();
    }
}

/// A sink failure is a stall until proven otherwise: the one thing that stops a
/// paced write is a device that quit consuming, and that is exit code 5.
fn sink_refusal(error: MeasureError) -> Refusal {
    Refusal::new(ExitCode::Stalled, error.to_string())
}
