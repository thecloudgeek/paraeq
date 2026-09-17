//! The capture runtime (MS-21, and MS-4's boundary 2): per-block metering with
//! decay and a resettable clip counter, plus the record loop that drives a
//! [`CaptureSource`].
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**. `tests/test_capture.rs`
//! pins the decay law, the clip counter's rise-and-reset, REW's 30%-of-a-block
//! rule at its exact boundary, and the loop's abort/short-read behaviour
//! against a mock source.
//!
//! # Why measurement cannot reuse the engine's metering
//!
//! Verified in the tree, and the reason MS-21 exists as a separate
//! requirement. Two grounds; only the first has moved since it was written:
//!
//! * **Shape.** `RtShared::peak_in` WAS a monotonic session maximum with no
//!   decay, and there was no clip counter at all. R1-8 has since given the
//!   engine both: `peak_in` is now a meter (the broadcast 20 dB / 1.7 s
//!   release, applied per block at a coefficient the controller derives from
//!   the stream geometry) and `clipped_samples` counts. It still cannot serve
//!   here. The release law is MS-21's own [`PEAK_DECAY_DB_PER_BLOCK`], not
//!   broadcast's; this clip count must RESET per attempt, where the engine's
//!   is a cumulative session disclosure that deliberately survives a
//!   teardown; and the abort path asks "did more than 30% of THIS block
//!   clip", which no held peak of either kind can answer.
//! * **Signal.** Decisively, and unchanged: those counters observe the
//!   **tap** path, which by construction never sees the stimulus --
//!   self-exclusion is the design goal, so the one signal that can injure a
//!   person is the one signal the engine's meters never see.
//!
//! # The 30% rule
//!
//! REW's: more than 30% of the samples in an input block clipped means the
//! measurement is invalid *regardless of level*, so it aborts rather than
//! warns. Note the asymmetry with the emit guard in [`crate::stimulus`]: an
//! output sample over full scale is clamped and counted as a defect, while an
//! input sample at full scale is a lost measurement — nothing downstream can
//! recover it, so there is nothing to clamp.

use crate::diagnostic::MeasurementDiagnostic;
use crate::seam::CaptureSource;
use crate::session::{AbortHandle, AbortReason};
use crate::MeasureError;

/// A sample at or beyond this magnitude is counted as clipped. Full scale
/// exactly, not a hair under: a converter that rails reports ±1.0, and
/// treating that as un-clipped is how a railed capture passes for a hot one.
pub const CLIP_THRESHOLD: f64 = 1.0;

/// REW's rule: more than this fraction of one block clipped invalidates the
/// measurement. Strictly greater — exactly 30% passes, as REW words it.
pub const CLIP_BLOCK_FRACTION: f64 = 0.30;

/// Per-block peak decay, in dB per block.
///
/// A decaying peak answers "how hot is the signal now", which the engine's
/// `input_peak_session` (its monotonic statistic) cannot -- and this release
/// is MS-21's own, not the broadcast 20 dB / 1.7 s the engine's `input_peak`
/// now falls at. 1.5 dB per block is ~11 ms of 20 dB fall at
/// 512 frames / 48 kHz — fast enough to track a sweep's level as it climbs,
/// slow enough that a meter driven off it does not flicker.
pub const PEAK_DECAY_DB_PER_BLOCK: f64 = 1.5;

/// Peak + clip metering for the capture path.
///
/// Peak is held in the linear domain and decayed multiplicatively per block;
/// [`Self::peak_dbfs`] is the display view. The clip counter is cumulative
/// until [`Self::reset_clips`], so a caller can scope it to one rung or one
/// sweep.
#[derive(Clone, Debug)]
pub struct CaptureMeter {
    blocks: u64,
    clipped_blocks: u64,
    clipped_samples: u64,
    decay: f64,
    non_finite_samples: u64,
    peak: f64,
}

impl Default for CaptureMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureMeter {
    pub fn new() -> Self {
        Self {
            blocks: 0,
            clipped_blocks: 0,
            clipped_samples: 0,
            decay: 10f64.powf(-PEAK_DECAY_DB_PER_BLOCK / 20.0),
            non_finite_samples: 0,
            peak: 0.0,
        }
    }

    /// Meter one block. Returns [`MeasurementDiagnostic::InputClipping`] when
    /// this block trips REW's 30% rule — a **blocking** diagnostic, so the
    /// caller aborts rather than warns.
    ///
    /// Non-finite samples are counted as clipped **and** separately as
    /// non-finite. Folding them into the clip count is what stops a NaN storm
    /// reading as silence — `NaN > peak` is false, which is exactly how the
    /// engine's watchdog mistakes one for an unplugged source today, and not
    /// inheriting that is the whole point of a separate capture meter.
    ///
    /// But they are not the same condition, and the diagnostic they raise
    /// tells the user to turn the input gain down, which does nothing about a
    /// NaN. [`Self::non_finite_samples`] lets a caller tell "your input is too
    /// hot" from "something upstream is broken" before rendering that advice.
    pub fn observe(&mut self, block: &[f64]) -> Option<MeasurementDiagnostic> {
        self.blocks += 1;
        self.peak *= self.decay;
        if block.is_empty() {
            return None;
        }
        let mut clipped = 0u64;
        let mut non_finite = 0u64;
        for &v in block {
            if !v.is_finite() {
                non_finite += 1;
                clipped += 1;
                continue;
            }
            if v.abs() >= CLIP_THRESHOLD {
                clipped += 1;
                continue;
            }
            let a = v.abs();
            if a > self.peak {
                self.peak = a;
            }
        }
        self.clipped_samples += clipped;
        self.non_finite_samples += non_finite;
        if clipped as f64 > CLIP_BLOCK_FRACTION * block.len() as f64 {
            self.clipped_blocks += 1;
            return Some(MeasurementDiagnostic::InputClipping);
        }
        None
    }

    /// Blocks metered since construction.
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// Blocks that tripped the 30% rule.
    pub fn clipped_blocks(&self) -> u64 {
        self.clipped_blocks
    }

    /// Clipped samples since the last [`Self::reset_clips`], including the
    /// non-finite ones (see [`Self::observe`]).
    pub fn clipped_samples(&self) -> u64 {
        self.clipped_samples
    }

    /// Non-finite samples since the last [`Self::reset_clips`] — the subset of
    /// [`Self::clipped_samples`] that is a defect rather than a hot input.
    pub fn non_finite_samples(&self) -> u64 {
        self.non_finite_samples
    }

    /// The decayed peak, dBFS. Digital silence reads `f64::NEG_INFINITY`
    /// rather than a floored number — a meter that bottoms out at −100 dB
    /// invites a caller to treat −100 as a level.
    pub fn peak_dbfs(&self) -> f64 {
        if self.peak <= 0.0 {
            f64::NEG_INFINITY
        } else {
            20.0 * self.peak.log10()
        }
    }

    /// The decayed peak, linear.
    pub fn peak_linear(&self) -> f64 {
        self.peak
    }

    /// Zero the clip counters. The peak is deliberately left alone: it decays
    /// on its own, and resetting it would put a false floor under the very
    /// next block.
    pub fn reset_clips(&mut self) {
        self.clipped_blocks = 0;
        self.clipped_samples = 0;
        self.non_finite_samples = 0;
    }
}

/// What a capture run produced.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureRun {
    /// Why the run stopped short, if it did. `None` means `frames` were
    /// captured in full.
    pub ended_early: Option<CaptureEnd>,
    /// The captured samples, non-finites already zeroed (MS-4 boundary 2).
    pub samples: Vec<f64>,
    /// How many non-finite samples were zeroed on the way in.
    pub sanitized: u64,
}

/// Why a capture stopped before filling its buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaptureEnd {
    /// The abort handle fired between blocks.
    Aborted(AbortReason),
    /// The 30% clipping rule tripped: the measurement is invalid regardless of
    /// level, so there is nothing to gain by finishing it.
    Clipping,
    /// The source returned 0 frames — the stream ended.
    SourceExhausted,
}

/// Record `frames` from `source`, metering every block and polling `abort`
/// between blocks.
///
/// The polling cadence mirrors
/// [`MeasurementSession::sweep`](crate::session::MeasurementSession::sweep):
/// once per block, which is the granularity the seam's pacing contract
/// guarantees. A short read is not an error — the source contract says so —
/// but a zero-frame read ends the run, because retrying it forever is how a
/// dead stream becomes a hang.
///
/// Non-finite samples are zeroed and counted before they reach the caller
/// (MS-4's boundary 2): a NaN in a recorded IR propagates through `deconvolve`
/// into NaN correction coefficients, and one NaN poisons the DF2T feedback
/// state permanently. Metering sees them **before** they are zeroed, so a NaN
/// storm shows up as clipping rather than as silence.
///
/// # Errors
///
/// Propagates the source's own failures. A capture that ends early for any of
/// the [`CaptureEnd`] reasons is an outcome, not an error.
pub fn record(
    source: &mut dyn CaptureSource,
    frames: usize,
    meter: &mut CaptureMeter,
    abort: &AbortHandle,
) -> Result<CaptureRun, MeasureError> {
    let block = source.format().frames_per_block.max(1);
    let mut samples = Vec::with_capacity(frames);
    let mut sanitized = 0u64;
    let mut buffer = vec![0.0; block];
    let mut ended_early = None;
    while samples.len() < frames {
        if let Some(reason) = abort.triggered() {
            ended_early = Some(CaptureEnd::Aborted(reason));
            break;
        }
        let want = block.min(frames - samples.len());
        let got = source.capture(&mut buffer[..want])?;
        if got == 0 {
            ended_early = Some(CaptureEnd::SourceExhausted);
            break;
        }
        let chunk = &mut buffer[..got.min(want)];
        // Meter first, sanitize second: the meter must see the NaN.
        let clipping = meter.observe(chunk);
        for v in chunk.iter_mut() {
            if !v.is_finite() {
                *v = 0.0;
                sanitized += 1;
            }
        }
        samples.extend_from_slice(chunk);
        if clipping.is_some() {
            ended_early = Some(CaptureEnd::Clipping);
            break;
        }
    }
    Ok(CaptureRun {
        ended_early,
        samples,
        sanitized,
    })
}
