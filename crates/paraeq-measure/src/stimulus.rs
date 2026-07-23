//! Stimulus assembly (MS-4) with the MS-3 post-fade assertion set: dsp
//! generate → fade → DC-block → scale → verify → emit-guard.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**, per the four-tier
//! convention and the `spline.rs:1-5` precedent. The fade durations, the
//! pilot definition and the DC gate are product policy; the assembly's
//! correctness properties — exact-zero endpoints, monotone fade-out,
//! |mean| < 1e-4, bounded peak, all-finite — are analytic invariants with
//! closed forms, not oracle ports. `generate_sweep` keeps its Tier 1 oracle
//! in `paraeq-dsp`, untouched.
//!
//! This module is the safety interlock on the one signal path the engine's
//! clamps never see: whenever tap self-exclusion succeeds, the stimulus never
//! traverses `RealtimeChain`, so it inherits neither the trim gain nor either
//! ±1.0 clamp site. Everything a sink receives therefore goes through this
//! pipeline — the buffer type is constructible only here, carries its
//! [`SweepLevel`] provenance, and passes the [`emit_guard`] as the last stage
//! before the seam (mirroring the `SweepLevel` newtype philosophy: a raw
//! `Vec<f64>` cannot masquerade as a verified stimulus).
//!
//! Two stimulus kinds exist at this stage: the sweep and the 300 Hz pilot.
//! The pilot's sine is generated locally in this module — a fixed-frequency
//! sine is *assembly*, not oracle-bearing DSP: `generate_sweep` has a pinned
//! prototype oracle because the Farina sweep law is ported numerics, while a
//! sine has a closed form whose invariants (exact RMS, zero-crossing count,
//! endpoints) are directly assertable. Pink noise for MMM (continuous energy,
//! different crest factor, its own level-safety row per the spec's OPEN item)
//! is a later stage and is deliberately absent here.
//!
//! # The DC-block (resolves the spec's OPEN \[OWNER + NEEDS DATA\] item)
//!
//! `measurement-safety-design.md` § Fade and DC records that the MS-3
//! `|mean(x)| < 1e-4` gate is **infeasible for the sweep as written**: a
//! 5 s / 20 Hz–20 kHz / 48 kHz sweep carries 1.24e-3 of DC after the policy
//! fade — an LF stationary-phase residue, not an edge artefact, so no
//! fade-out can touch it. The spec offers two sanctioned resolutions
//! ("restate the threshold for sweeps, or DC-block in the MS-4 stimulus
//! assembly — but **do not inflate the fade-in**") and this module takes the
//! second: the private `dc_block` stage runs between fade and scale.
//!
//! Why not the two obvious alternatives:
//!
//! - **Raw mean subtraction is wrong.** `x[i] -= mean(x)` shifts *every*
//!   sample, including the first and last, from exactly 0.0 to exactly
//!   `-mean` — re-introducing at both edges a step discontinuity (≈1.2e-3,
//!   −58 dBFS) that violates MS-3's first assertion and is a miniature of the
//!   very click `apply_fade` exists to remove.
//! - **A longer fade-in is spec-rejected.** A ~100 ms fade-in does get under
//!   the gate, but it costs real LF energy: 10 ms at a 20 Hz start is already
//!   only 0.2 cycles, and the LF end is exactly where a room measurement is
//!   SNR-poorest. The spec's § Fade and its OPEN block both forbid this.
//!
//! Instead, subtract an **envelope-shaped** offset: with `env` the applied
//! fade envelope at unity plateau (reconstructed bit-exactly by running
//! `apply_fade` over a buffer of ones) and `c = mean(x_faded) / mean(env)`,
//! set `x -= c·env`. Then `mean(x) = mean(x_faded) − c·mean(env) = 0`
//! identically (to fp roundoff), and because the faded signal is `s·env`, the
//! result **factors as `env·(s − c)`** — the applied envelope is unchanged,
//! so `env(0) = env(end) = 0` keeps the endpoints at exactly 0.0 and the
//! fade-out's monotonicity survives untouched. One identity buys all three
//! MS-3 properties at once; `test_stimulus.rs` confirms the spec's measured
//! worst case (5 s / 20 Hz–20 kHz / 48 kHz, the 1.24e-3 sweep) passes the
//! 1e-4 gate at both −20 and −12 dBFS scaling.

use crate::diagnostic::MeasurementDiagnostic;
use crate::level::SweepLevel;
use crate::seam::StimulusSink;
use crate::MeasureError;
use paraeq_dsp::sweep::{apply_fade, generate_sweep};
use paraeq_dsp::targets::TransducerClass;

/// The MS-3 DC gate: `|mean(x)|` must be under this before a buffer reaches a
/// sink. The pilot clears it natively; the sweep clears it only because of
/// the DC-block (see the module header).
pub const DC_GATE_ABS_MEAN: f64 = 1e-4;

/// Policy fade-in, ms. Lives here, not in `paraeq-dsp` — durations are
/// product policy, the window math is DSP. 10 ms is belt-and-braces: the
/// sweep starts at an exact zero with a gentle slope, and a longer fade-in
/// would cost real LF energy (0.2 cycles at a 20 Hz start already).
pub const FADE_IN_MS: f64 = 10.0;

/// Policy fade-out, ms. Mandatory: an un-faded sweep ends in a broadband
/// click at up to −0.77 dBFS, exactly where tweeters are most fragile. 50 ms
/// is ~1000 cycles at the 20 kHz stop, so the fade is adiabatic there.
pub const FADE_OUT_MS: f64 = 50.0;

/// Pilot frequency (spec § The Level Ladder, step 2).
pub const PILOT_FREQ_HZ: f64 = 300.0;

/// Pilot level, dBFS RMS — deliberately far below any cap, because the pilot
/// is the probe that discovers the chain and must be safe on a chain we know
/// nothing about. Fixed policy: [`assemble_pilot`] takes no level parameter,
/// so a hot pilot is unrepresentable. (Reference point: EARS uses −20 dBFS.)
pub const PILOT_LEVEL_DBFS_RMS: f64 = -40.0;

/// Pilot maximum duration, s (spec: "≤ 1 s"). A validated parameter of
/// [`assemble_pilot`], not a constant duration, because the ladder may want a
/// shorter probe.
pub const PILOT_MAX_DURATION_S: f64 = 1.0;

/// Which stimulus a buffer carries. Pink noise for MMM is a later stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StimulusKind {
    Pilot,
    Sweep,
}

/// A stimulus that provably went through the full assembly pipeline.
///
/// Fields are private and there is no other constructor, no `Default`, no
/// `From<Vec<f64>>`: the only way to obtain one is [`assemble_sweep`] /
/// [`assemble_pilot`], which end in [`verify_stimulus`] — so holding this
/// type *is* the proof that the MS-3 assertion set passed on these samples at
/// this level. The samples are already scaled to [`Self::level`]; the level
/// rides along as provenance for the seam and the session log.
#[derive(Clone, Debug, PartialEq)]
pub struct AssembledStimulus {
    kind: StimulusKind,
    level: SweepLevel,
    sample_rate_hz: u32,
    samples: Vec<f64>,
}

impl AssembledStimulus {
    pub fn duration_s(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate_hz as f64
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn kind(&self) -> StimulusKind {
        self.kind
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// The [`SweepLevel`] these samples are already scaled to (provenance —
    /// see [`Self::emit_to`]).
    pub fn level(&self) -> SweepLevel {
        self.level
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Read-only view for analysis and logging. Emission goes through
    /// [`Self::emit_to`], never through this accessor — the emit guard is the
    /// last stage before a sink, and only `emit_to` runs it.
    pub fn samples(&self) -> &[f64] {
        &self.samples
    }

    /// Run the MS-4 [`emit_guard`] over a copy of the samples — the last
    /// stage before the seam — and hand the guarded buffer to `sink` together
    /// with this stimulus's [`SweepLevel`].
    ///
    /// Returns the guard's warning diagnostics (empty for a verified
    /// stimulus; the guard is belt-and-braces, not a processing stage).
    ///
    /// Note for the Stage-4 sink implementation: the buffer handed over is
    /// **already at `level`** — verification's `max|x| ≤ 1.0` runs
    /// post-scale, and the emit guard clamps final samples. The `level`
    /// argument of [`StimulusSink::emit`] is provenance to log and verify
    /// against, not a second gain stage; `seam.rs`'s scaffold-era comment
    /// ("implementations scale by `level`") predates assembly and is
    /// reconciled at integration.
    pub fn emit_to(
        &self,
        sink: &mut dyn StimulusSink,
    ) -> Result<Vec<MeasurementDiagnostic>, MeasureError> {
        let mut guarded = self.samples.clone();
        let counts = emit_guard(&mut guarded);
        sink.emit(&guarded, self.level)?;
        Ok(counts.warnings())
    }
}

/// What the MS-4 emit guard did. Nonzero counts surface as
/// [`MeasurementDiagnostic`] warnings via [`Self::warnings`]; a verified
/// stimulus produces zeros.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GuardCounts {
    /// Finite samples outside ±1.0, clamped in.
    pub clamped: u64,
    /// Non-finite samples (NaN, ±∞) zeroed. NaN must be zeroed, not clamped:
    /// `NaN.clamp(-1.0, 1.0)` is NaN by documented Rust behaviour.
    pub sanitized: u64,
}

impl GuardCounts {
    /// The warning diagnostics these counts surface as:
    /// [`MeasurementDiagnostic::EmitClamped`] and/or
    /// [`MeasurementDiagnostic::EmitNonFiniteSanitized`], each carrying its
    /// count. Empty when nothing fired.
    pub fn warnings(self) -> Vec<MeasurementDiagnostic> {
        let mut warnings = Vec::new();
        if self.clamped > 0 {
            warnings.push(MeasurementDiagnostic::EmitClamped {
                count: self.clamped,
            });
        }
        if self.sanitized > 0 {
            warnings.push(MeasurementDiagnostic::EmitNonFiniteSanitized {
                count: self.sanitized,
            });
        }
        warnings
    }
}

/// Refusals and parameter errors of the assembly pipeline. `Defect` wraps the
/// MS-3 diagnostic — a stimulus that fails verification never reaches a sink,
/// and never silently clamps-and-continues.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum StimulusError {
    #[error("assembled stimulus failed the MS-3 assertion set: {0:?}")]
    Defect(MeasurementDiagnostic),
    #[error(
        "band {f_start_hz}\u{2013}{f_end_hz} Hz is invalid at {sample_rate_hz} Hz \
         (need 0 < f_start < f_end < Nyquist)"
    )]
    InvalidBand {
        f_end_hz: f64,
        f_start_hz: f64,
        sample_rate_hz: u32,
    },
    #[error("invalid level: {0}")]
    Level(#[from] crate::level::LevelError),
    #[error("duration {requested_s} s is not a positive finite duration")]
    NonPositiveDuration { requested_s: f64 },
    #[error("pilot duration {requested_s} s exceeds the {max_s} s spec maximum")]
    PilotTooLong { max_s: f64, requested_s: f64 },
    #[error(
        "{len} samples cannot carry the {fade_in} + {fade_out} sample policy fades \
         with a plateau"
    )]
    TooShortForFades {
        fade_in: usize,
        fade_out: usize,
        len: usize,
    },
}

/// Assemble the measurement sweep: `generate_sweep` → policy fades → DC-block
/// → scale to `level` → [`verify_stimulus`].
///
/// Band and duration are validated here only for well-formedness (positive
/// finite duration; `0 < f_start < f_end < Nyquist`). The per-class caps on
/// `f_start` and sweep length (MS-12/MS-13) are the level ladder's to
/// enforce — the ladder is the sole intended caller and arrives in a later
/// stage.
pub fn assemble_sweep(
    duration_s: f64,
    sample_rate_hz: u32,
    f_start_hz: f64,
    f_end_hz: f64,
    level: SweepLevel,
) -> Result<AssembledStimulus, StimulusError> {
    validate_duration(duration_s)?;
    // Written as a positive requirement so NaN (which passes no `>`
    // comparison) lands in the refusal arm, never in the generate call.
    let band_ok =
        f_start_hz > 0.0 && f_end_hz > f_start_hz && f_end_hz < f64::from(sample_rate_hz) / 2.0;
    if !band_ok {
        return Err(StimulusError::InvalidBand {
            f_end_hz,
            f_start_hz,
            sample_rate_hz,
        });
    }
    let samples = generate_sweep(duration_s, sample_rate_hz, f_start_hz, f_end_hz);
    finish(samples, StimulusKind::Sweep, level, sample_rate_hz)
}

/// Assemble the pilot: a locally generated [`PILOT_FREQ_HZ`] sine at exactly
/// [`PILOT_LEVEL_DBFS_RMS`], duration ≤ [`PILOT_MAX_DURATION_S`] (validated),
/// through the same fade → DC-block → scale → verify path as the sweep.
///
/// Takes a [`TransducerClass`] rather than a level: the pilot's level is
/// fixed policy and is still constructed through [`SweepLevel::new`] — the
/// sole level constructor — so the pilot carries the same provenance as a
/// sweep and a hot pilot cannot be expressed.
pub fn assemble_pilot(
    duration_s: f64,
    sample_rate_hz: u32,
    class: TransducerClass,
) -> Result<AssembledStimulus, StimulusError> {
    validate_duration(duration_s)?;
    if duration_s > PILOT_MAX_DURATION_S {
        return Err(StimulusError::PilotTooLong {
            max_s: PILOT_MAX_DURATION_S,
            requested_s: duration_s,
        });
    }
    // -40 dBFS RMS is below every class cap (test_level.rs pins this), but
    // the level still goes through the sole constructor rather than around
    // it — if a future table edit ever contradicted that, this refuses
    // instead of trusting a comment.
    let level = SweepLevel::new(PILOT_LEVEL_DBFS_RMS, class)?;
    let n = (duration_s * f64::from(sample_rate_hz)) as usize;
    let omega = 2.0 * std::f64::consts::PI * PILOT_FREQ_HZ / f64::from(sample_rate_hz);
    let samples: Vec<f64> = (0..n).map(|i| (omega * i as f64).sin()).collect();
    finish(samples, StimulusKind::Pilot, level, sample_rate_hz)
}

/// The MS-4 emit guard: clamp finite samples to ±1.0 and zero non-finite
/// ones, in place, each with a count. The **last** stage before a
/// [`StimulusSink`] — the stimulus path never traverses `RealtimeChain`, so
/// it cannot borrow the engine's clamps and carries its own.
///
/// Sanitize-then-clamp per sample, in that order: `NaN.clamp(…)` is NaN by
/// documented Rust behaviour, so the finiteness test must run first (±∞ would
/// clamp "correctly", but a non-finite sample is a defect to zero and count,
/// not a level to bring in-range).
pub fn emit_guard(block: &mut [f64]) -> GuardCounts {
    let mut counts = GuardCounts::default();
    for v in block.iter_mut() {
        if !v.is_finite() {
            *v = 0.0;
            counts.sanitized += 1;
        } else if v.abs() > 1.0 {
            *v = v.clamp(-1.0, 1.0);
            counts.clamped += 1;
        }
    }
    counts
}

/// The MS-3 post-fade assertion set, enforced before anything reaches a sink
/// (spec § Fade and DC), in order:
///
/// 1. every sample finite ([`MeasurementDiagnostic::StimulusNonFinite`]) —
///    first, because NaN would poison the later comparisons into lying;
/// 2. first and last samples exactly 0.0
///    ([`MeasurementDiagnostic::StimulusEndpointNonzero`]);
/// 3. fade-out envelope monotonically non-increasing
///    ([`MeasurementDiagnostic::StimulusFadeNotMonotone`]) — checked on
///    `envelope`, the fade envelope that was applied (unity plateau), because
///    monotonicity is a property of the envelope, not of the oscillating
///    samples; assembly holds it by construction and passes it in;
/// 4. `|mean| <` [`DC_GATE_ABS_MEAN`]
///    ([`MeasurementDiagnostic::StimulusDcOffset`]);
/// 5. `max|x| ≤ 1.0` after scaling
///    ([`MeasurementDiagnostic::StimulusOverFullScale`]) — any needed clamp
///    is a **defect** (the level solve is wrong), surfaced, never silently
///    applied.
///
/// # Panics
///
/// If `samples` is empty or the slices disagree in length — caller bugs, and
/// a safety gate fails loudly rather than guessing.
pub fn verify_stimulus(
    samples: &[f64],
    envelope: &[f64],
    fade_out: usize,
) -> Result<(), MeasurementDiagnostic> {
    assert_eq!(
        samples.len(),
        envelope.len(),
        "verify_stimulus: samples and applied envelope disagree in length"
    );
    assert!(!samples.is_empty(), "verify_stimulus: empty buffer");

    let non_finite = samples.iter().filter(|v| !v.is_finite()).count() as u64;
    if non_finite > 0 {
        return Err(MeasurementDiagnostic::StimulusNonFinite { count: non_finite });
    }
    if samples[0] != 0.0 || samples[samples.len() - 1] != 0.0 {
        return Err(MeasurementDiagnostic::StimulusEndpointNonzero);
    }
    let tail = samples.len().saturating_sub(fade_out);
    if envelope[tail..].windows(2).any(|w| w[1] > w[0]) {
        return Err(MeasurementDiagnostic::StimulusFadeNotMonotone);
    }
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    if mean.abs() >= DC_GATE_ABS_MEAN {
        return Err(MeasurementDiagnostic::StimulusDcOffset { mean });
    }
    let peak = samples.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    if peak > 1.0 {
        return Err(MeasurementDiagnostic::StimulusOverFullScale { peak });
    }
    Ok(())
}

/// Positive and finite, or refused. Written so NaN cannot pass.
fn validate_duration(duration_s: f64) -> Result<(), StimulusError> {
    if duration_s > 0.0 && duration_s.is_finite() {
        Ok(())
    } else {
        Err(StimulusError::NonPositiveDuration {
            requested_s: duration_s,
        })
    }
}

/// Policy milliseconds → samples at this rate.
fn fade_samples(ms: f64, sample_rate_hz: u32) -> usize {
    (ms / 1000.0 * f64::from(sample_rate_hz)).round() as usize
}

/// The shared back half of the pipeline: fade → DC-block → scale → verify.
///
/// The envelope is reconstructed by running `apply_fade` over a buffer of
/// ones — bit-exactly the multipliers the samples received, which is what
/// makes the DC-block's factorization `env·(s − c)` (module header) exact
/// rather than approximate.
fn finish(
    mut samples: Vec<f64>,
    kind: StimulusKind,
    level: SweepLevel,
    sample_rate_hz: u32,
) -> Result<AssembledStimulus, StimulusError> {
    let fade_in = fade_samples(FADE_IN_MS, sample_rate_hz);
    let fade_out = fade_samples(FADE_OUT_MS, sample_rate_hz);
    if samples.len() <= fade_in + fade_out {
        return Err(StimulusError::TooShortForFades {
            fade_in,
            fade_out,
            len: samples.len(),
        });
    }
    apply_fade(&mut samples, fade_in, fade_out);
    let mut envelope = vec![1.0; samples.len()];
    apply_fade(&mut envelope, fade_in, fade_out);
    dc_block(&mut samples, &envelope);
    scale_to(&mut samples, level);
    verify_stimulus(&samples, &envelope, fade_out).map_err(StimulusError::Defect)?;
    Ok(AssembledStimulus {
        kind,
        level,
        sample_rate_hz,
        samples,
    })
}

/// The envelope-shaped DC-block (module header): `x -= c·env` with
/// `c = mean(x)/mean(env)`. Zeroes the mean identically while leaving the
/// exact-zero endpoints and the fade envelope untouched — the two properties
/// a raw mean subtraction would destroy. `mean(env) > 0` is guaranteed by the
/// `TooShortForFades` gate: at least one plateau sample is at unity.
fn dc_block(x: &mut [f64], envelope: &[f64]) {
    let mean_x = x.iter().sum::<f64>() / x.len() as f64;
    let mean_env = envelope.iter().sum::<f64>() / envelope.len() as f64;
    let c = mean_x / mean_env;
    for (v, e) in x.iter_mut().zip(envelope) {
        *v -= c * e;
    }
}

/// Scale in place so the buffer's RMS is exactly `level` (dBFS RMS, full
/// scale = 1.0). Runs after fade and DC-block so what was solved is what
/// plays — the fades' RMS cost is compensated, not shrugged at.
fn scale_to(x: &mut [f64], level: SweepLevel) {
    let rms = (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt();
    let gain = 10f64.powf(level.dbfs_rms() / 20.0) / rms;
    for v in x.iter_mut() {
        *v *= gain;
    }
}
