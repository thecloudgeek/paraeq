//! The closed-loop level ladder (MS-7/MS-8/MS-13/MS-17): noise floor → pilot
//! → solve → envelope → ≤6 dB rungs, and the SNR gate with its two remedies.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**, per the four-tier
//! convention and the `spline.rs:1-5` precedent. Level policy has no prototype
//! counterpart — the prototype's one playback constant is a guess about an
//! unknown chain. What `tests/test_ladder.rs` pins instead: the closed form of
//! the solve, every refusal edge in order, the rung plan's two invariants, the
//! SNR table row for row, and the remedy sequence.
//!
//! # This module computes; it never emits
//!
//! Deliberately I/O-free, like the rest of the policy in this crate. The
//! ladder is a checked state machine over *measurements*: the caller plays the
//! pilot and each rung through the [`StimulusSink`](crate::seam::StimulusSink)
//! that [`MeasurementSession`](crate::session::MeasurementSession) owns, and
//! hands the measured numbers back here. That is what keeps every gate in the
//! ladder unit-testable with no hardware, and it is why the session was built
//! with [`SolveOutcome`] as a boundary type rather than with the ladder inside
//! it.
//!
//! The sequence a caller runs, and the session call each step feeds:
//!
//! ```text
//! 0. preconditions           CalSummary::validate + MeasurementSession::begin
//! 1. LevelLadder::accept_noise_floor(&NoiseFloor)      (>= 1 s of silence)
//! 2. play the pilot          assemble_pilot(..) -> emit -> capture
//! 3. LevelLadder::solve(pilot_dbfs_rms) -> SolveOutcome
//! 4.   ^ steps 3 and 4 of the spec are one call: the envelope check is
//!      inside `solve`, so an out-of-envelope chain cannot be escalated into
//! 5. MeasurementSession::install_solve(outcome) -> SweepLevel
//!    for each LevelLadder::rungs(): play, capture, check_rung(..),
//!    MeasurementSession::record_rung(..)
//! 6. acknowledge -> sweep -> LevelLadder::evaluate_snr(..)
//! ```
//!
//! # The one rule that overrides every other
//!
//! **Remedies are input gain and sweep length. Never output level.** REW is
//! imperative and correct: *"If input levels are low DO NOT KEEP MAKING THE
//! TEST SIGNAL LOUDER."* [`Remedy`] has no variant that could, and
//! `test_ladder.rs` asserts across a full remedy sequence that the emitted
//! level never rises.
//!
//! A remedy may, however, **invalidate** the solve, and one does:
//! [`Remedy::RaiseInputGain`] leaves the acoustic output untouched but changes
//! what the mic reads, so the measured sensitivity, the projected SPL, every
//! rung's projection and the MS-11 pin all stop describing the live chain.
//! The ladder drops the solve rather than letting a stale Sens Factor produce
//! a plausible-looking SPL. See [`LevelLadder::confirm_input_gain`].

use crate::cal::{margined_emit_dbfs, CalSummary, PinnedGain};
use crate::diagnostic::{MeasurementDiagnostic, Refusal};
use crate::level::caps_for;
use crate::session::SolveOutcome;
use crate::stimulus::PILOT_LEVEL_DBFS_RMS;
use paraeq_dsp::fr::{smooth, Smoothing};
use paraeq_dsp::logf::{resample_db_to_log_grid, LogGrid, Prefilter};
use paraeq_dsp::targets::TransducerClass;
use paraeq_dsp::DspError;

/// Ladder step 1: the broadband noise floor must sit at or below this.
///
/// Absolute, not relative: below this the capture path is quiet enough that
/// the *relative* target (≤ −36 dBFS under the planned sweep-at-mic level) is
/// reachable by the two legal remedies. Above it, nothing but a quieter room
/// or a better input helps — and the one fix that would work, more output
/// level, is the one fix ParaEQ will not apply.
pub const NOISE_FLOOR_MAX_DBFS: f64 = -60.0;

/// Ladder step 1: the relative target. Reported, not enforced — the SNR gate
/// (MS-8) is the enforcing test, and it measures the same thing per band
/// instead of broadband.
pub const NOISE_FLOOR_TARGET_BELOW_SWEEP_DB: f64 = 36.0;

/// Ladder step 1: silence capture duration. "Record ≥1 s of silence."
pub const NOISE_FLOOR_MIN_DURATION_S: f64 = 1.0;

/// Ladder step 5: the largest single escalation step, in dB.
pub const MAX_RUNG_STEP_DB: f64 = 6.0;

/// Ladder step 5: how far a rung's measured SPL may sit from its projection
/// before the climb aborts. Exceeding it is the signature of a wrong device,
/// a stale cal file, or a broken chain — none of which is a thing to climb
/// further into.
pub const RUNG_PROJECTION_TOLERANCE_DB: f64 = 6.0;

/// MS-8: median SNR at or above this, together with [`SNR_MIN_ACCEPT_DB`],
/// accepts cleanly.
pub const SNR_MEDIAN_ACCEPT_DB: f64 = 40.0;

/// MS-8: minimum in-band SNR required for a clean accept.
pub const SNR_MIN_ACCEPT_DB: f64 = 20.0;

/// MS-8: median SNR at or above this accepts with a `LowSnr` warning.
pub const SNR_MEDIAN_WARN_DB: f64 = 30.0;

/// MS-8: "at most 2 automatic remedy attempts before `SnrUnachievable`".
pub const MAX_REMEDIES: usize = 2;

/// How far one input-gain remedy moves the gain scalar, in units of the
/// CoreAudio 0.0..=1.0 normalized range.
///
/// **OPEN \[NEEDS DATA\]:** a normalized gain scalar is not dB, and CoreAudio
/// exposes no way to ask a device what one step of it is worth acoustically.
/// So the remedy cannot be "raise by N dB"; it raises by a tenth of the
/// register's travel and re-measures, which is what a human does. Two attempts
/// is the whole budget, so this cannot walk the gain up indefinitely.
pub const INPUT_GAIN_REMEDY_STEP: f64 = 0.1;

/// The octave fraction the SNR gate is evaluated on: "Defined on the smoothed
/// (1/6-octave) magnitudes over the class's correction band."
pub const SNR_SMOOTHING_FRACTION: u32 = 6;

/// A silence capture, reduced to the two things the ladder needs from it.
///
/// Built by [`NoiseFloor::measure`] from raw samples, or directly from an
/// already-analyzed capture. The spectrum is 1/6-octave-smoothed on a
/// [`LogGrid`] so the SNR gate can subtract it from a sweep magnitude
/// bin-for-bin.
#[derive(Clone, Debug, PartialEq)]
pub struct NoiseFloor {
    broadband_dbfs: f64,
    duration_s: f64,
    freqs_hz: Vec<f64>,
    spectrum_db: Vec<f64>,
}

impl NoiseFloor {
    /// Analyze a silence capture: broadband RMS in dBFS, plus the
    /// 1/6-octave-smoothed magnitude on `grid`.
    ///
    /// The linear FFT axis is resampled onto the log grid *before* smoothing,
    /// with the anti-comb prefilter. Smoothing on the raw rfft axis instead
    /// would be O(N²) over ~24 000 bins for a one-second capture at 48 kHz —
    /// and would then have to be resampled anyway to line up with the sweep.
    ///
    /// # Errors
    ///
    /// `InvalidInput` on an empty capture, a non-positive sample rate, or
    /// non-finite samples. Non-finite is an error rather than a sanitize
    /// because this is analysis, not the emit path: a NaN in the *floor*
    /// silently zeroes the SNR of every band that touches it.
    pub fn measure(samples: &[f64], sample_rate: u32, grid: &LogGrid) -> Result<Self, DspError> {
        if samples.is_empty() {
            return Err(DspError::InvalidInput(
                "noise floor: empty capture".to_owned(),
            ));
        }
        if sample_rate == 0 {
            return Err(DspError::InvalidInput(
                "noise floor: sample rate is 0".to_owned(),
            ));
        }
        if samples.iter().any(|v| !v.is_finite()) {
            return Err(DspError::InvalidInput(
                "noise floor: capture contains non-finite samples".to_owned(),
            ));
        }
        let mean_square = samples.iter().map(|v| v * v).sum::<f64>() / samples.len() as f64;
        // The 1e-20 floor keeps digital silence finite: log10(0) is -inf, and
        // an infinite floor makes every later comparison lie.
        let broadband_dbfs = 10.0 * mean_square.max(1e-20).log10();
        Ok(Self {
            broadband_dbfs,
            duration_s: samples.len() as f64 / f64::from(sample_rate),
            freqs_hz: grid.freqs().to_vec(),
            spectrum_db: analyze_magnitude_db(samples, sample_rate, grid)?,
        })
    }

    pub fn broadband_dbfs(&self) -> f64 {
        self.broadband_dbfs
    }

    pub fn duration_s(&self) -> f64 {
        self.duration_s
    }

    pub fn freqs_hz(&self) -> &[f64] {
        &self.freqs_hz
    }

    pub fn spectrum_db(&self) -> &[f64] {
        &self.spectrum_db
    }
}

/// What the SNR gate decided (MS-8), row for row with the spec's table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SnrOutcome {
    /// median ≥ 40 dB **and** min in-band ≥ 20 dB.
    Accept { median_db: f64, min_db: f64 },
    /// median ≥ 30 dB, but not the clean row. Proceed, warned.
    AcceptWithWarning {
        diagnostic: MeasurementDiagnostic,
        median_db: f64,
        min_db: f64,
    },
    /// Below the warn row, and a remedy is available.
    Remedy {
        median_db: f64,
        min_db: f64,
        remedy: Remedy,
    },
    /// Below the warn row with no remedy left: the budget is spent, or neither
    /// legal remedy can move.
    Refuse {
        median_db: f64,
        min_db: f64,
        refusal: Refusal,
    },
}

/// The only two things the ladder may change to fix SNR.
///
/// There is deliberately no third variant. Output level is not a remedy, and
/// the absence of the variant is the enforcement — a code path that "fixes"
/// SNR by turning the sweep up does not typecheck.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Remedy {
    /// Extend the sweep. Doubling buys ~3 dB of coherent gain, and MS-13 caps
    /// the result at the class's `max_sweep_len_s` — 16× duration for 12 dB is
    /// exactly the tweeter-overheating regime REW warns about, and the
    /// two-clock skew evidence stops at 5 s besides.
    ExtendSweep { from_s: f64, to_s: f64 },
    /// Raise the mic's input gain.
    ///
    /// **MS-11 interaction, which the caller must handle and this type cannot:**
    /// sensitivity is gain-referenced, so moving the input gain away from the
    /// cal's reference makes [`PinnedGain::matches`] false and derates the
    /// emitted level by [`crate::CAL_ERROR_MARGIN_DB`]. That helps when the
    /// floor is *electrical* (the signal rises against a fixed converter
    /// floor) and does nothing when it is *acoustic*. Either re-pin against a
    /// cal captured at the new gain, or accept the derate — and re-evaluate,
    /// because the next [`LevelLadder::evaluate_snr`] is what says whether it
    /// actually helped.
    RaiseInputGain { from: f64, to: f64 },
}

/// One rung of the escalation, with the projection its measurement is checked
/// against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rung {
    pub index: u32,
    pub level_dbfs_rms: f64,
    pub projected_spl_db: f64,
}

/// The ladder itself: a checked sequence, one direction only.
///
/// Every step is a **gate**: failure of a step is failure of the run, not an
/// input to a heuristic. Out-of-order calls are a programming error and are
/// reported as such rather than silently reordered.
#[derive(Clone, Debug)]
pub struct LevelLadder {
    cal: CalSummary,
    class: TransducerClass,
    floor_accepted: bool,
    gain_remedy_used: bool,
    input_gain: f64,
    length_remedy_used: bool,
    pin: PinnedGain,
    remedies_used: usize,
    sens_factor_dbfs: f64,
    solve: Option<Solved>,
    sweep_len_s: f64,
}

/// The frozen result of step 3, kept so rungs can be projected against it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Solved {
    chain_sensitivity_spl_per_dbfs: f64,
    emitted_dbfs_rms: f64,
    outcome: SolveOutcome,
    rungs: usize,
    rung_step_db: f64,
}

/// Out-of-order use, or a step that cannot run yet.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum LadderError {
    #[error("the noise floor has not been accepted yet")]
    NoFloor,
    #[error("no solve has been installed yet")]
    NoSolve,
    #[error("rung index {index} does not exist (the plan has {planned})")]
    NoSuchRung { index: u32, planned: usize },
    #[error("refused: {0:?}")]
    Refused(Refusal),
}

impl From<Refusal> for LadderError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl LevelLadder {
    /// Open a ladder on a validated cal (MS-9/MS-10 already refused at
    /// [`CalSummary::validate`]) and the live input-gain read-back.
    ///
    /// `sweep_len_s` is the planned sweep duration, clamped on entry to the
    /// class's `max_sweep_len_s` (MS-13). Clamped rather than refused because
    /// this is a plan, not an emitted level: the caller asking for a longer
    /// sweep than the class allows gets the class's, and the length remedy
    /// then has nowhere left to go, which is exactly the intended behaviour.
    pub fn new(cal: &CalSummary, input_gain_read_back: f64, sweep_len_s: f64) -> Self {
        let class = cal.class();
        let max_len = caps_for(class).max_sweep_len_s;
        Self {
            cal: cal.clone(),
            class,
            floor_accepted: false,
            gain_remedy_used: false,
            input_gain: input_gain_read_back,
            length_remedy_used: false,
            pin: cal.pin_gain(input_gain_read_back),
            remedies_used: 0,
            sens_factor_dbfs: cal.sensitivity().sens_factor_dbfs(),
            solve: None,
            // A non-finite or non-positive request falls back to the cap
            // rather than propagating into a sweep length.
            sweep_len_s: if sweep_len_s.is_finite() && sweep_len_s > 0.0 {
                sweep_len_s.min(max_len)
            } else {
                max_len
            },
        }
    }

    /// **Step 1.** Accept the silence capture, or refuse.
    ///
    /// Two gates, both positive requirements so a NaN refuses: at least
    /// [`NOISE_FLOOR_MIN_DURATION_S`] of silence, and a broadband floor at or
    /// below [`NOISE_FLOOR_MAX_DBFS`].
    ///
    /// Both refuse with the same diagnostic, `SnrUnachievable`, and the two are
    /// not distinguished. That is a wart worth naming: a too-short capture is
    /// an internal sequencing error, not a noisy room, and the user is told to
    /// close a window either way. It is left as-is because the caller controls
    /// the capture length and a short one is its bug to fix, not the user's —
    /// but a future `MeasurementDiagnostic` variant would be the honest fix.
    pub fn accept_noise_floor(&mut self, floor: &NoiseFloor) -> Result<(), Refusal> {
        let quiet =
            floor.broadband_dbfs.is_finite() && floor.broadband_dbfs <= NOISE_FLOOR_MAX_DBFS;
        let long_enough =
            floor.duration_s.is_finite() && floor.duration_s >= NOISE_FLOOR_MIN_DURATION_S;
        if !(quiet && long_enough) {
            return Err(Refusal::new(MeasurementDiagnostic::SnrUnachievable));
        }
        self.floor_accepted = true;
        Ok(())
    }

    /// **Steps 2–4**, as one call so an out-of-envelope chain cannot be
    /// escalated into: convert the pilot's measured level to SPL, solve the
    /// chain, check the envelope, and plan the rungs.
    ///
    /// - `SPL_measured = 94 + (dBFS_measured − Sens_Factor)`
    /// - `S = SPL_measured − L_pilot`, dB SPL per dBFS RMS
    /// - `L = SPL_target − S`, the solved level, pre-margin
    /// - the returned `projected_spl_db` is `S + margined(L)` — the SPL the
    ///   level that will actually be **emitted** projects, not the SPL the
    ///   pre-margin solve projects. That number is what MS-18 asks the user to
    ///   acknowledge, so it has to be the truthful one; the session recomputes
    ///   the same margin from `solved_dbfs_rms` and cannot disagree.
    ///
    /// # Errors
    ///
    /// [`LadderError::NoFloor`] if step 1 has not passed.
    /// `SensitivityOutOfEnvelope` if `S` is outside the class envelope, or is
    /// not finite (an unusable pilot measurement is not a chain to climb into).
    pub fn solve(&mut self, pilot_dbfs_rms: f64) -> Result<SolveOutcome, LadderError> {
        if !self.floor_accepted {
            return Err(LadderError::NoFloor);
        }
        let caps = caps_for(self.class);
        let spl_measured = 94.0 + (pilot_dbfs_rms - self.sens_factor_dbfs);
        let sensitivity = spl_measured - PILOT_LEVEL_DBFS_RMS;
        // MS-17, before anything is planned. Positive requirement: a NaN
        // sensitivity is outside every envelope.
        let in_envelope = sensitivity.is_finite()
            && caps
                .sensitivity_envelope_spl_per_dbfs
                .contains(&sensitivity);
        if !in_envelope {
            return Err(Refusal::new(MeasurementDiagnostic::SensitivityOutOfEnvelope).into());
        }
        let solved_dbfs_rms = caps.spl_target_db - sensitivity;
        let emitted_dbfs_rms = margined_emit_dbfs(solved_dbfs_rms, self.pin);
        let outcome = SolveOutcome {
            chain_sensitivity_spl_per_dbfs: sensitivity,
            projected_spl_db: sensitivity + emitted_dbfs_rms,
            solved_dbfs_rms,
        };
        // The climb starts at the pilot's level and never descends: a solved
        // level at or below the pilot is already safe, so there is nothing to
        // escalate and the plan is empty.
        let climb = emitted_dbfs_rms - PILOT_LEVEL_DBFS_RMS;
        let rungs = if climb > 0.0 {
            (climb / MAX_RUNG_STEP_DB).ceil() as usize
        } else {
            0
        };
        self.solve = Some(Solved {
            chain_sensitivity_spl_per_dbfs: sensitivity,
            emitted_dbfs_rms,
            outcome,
            rungs,
            // Equal steps rather than 6 dB steps and a short last one: the
            // point of the rung is to re-measure before committing, and a
            // 0.2 dB final rung measures nothing new.
            rung_step_db: if rungs > 0 { climb / rungs as f64 } else { 0.0 },
        });
        Ok(outcome)
    }

    /// **Step 5**, the plan: every rung from the pilot up to the emitted
    /// level, in equal steps of at most [`MAX_RUNG_STEP_DB`].
    ///
    /// The last rung is the emitted level itself, so the final measurement
    /// before the sweep is taken at the level the sweep will play at.
    pub fn rungs(&self) -> Result<Vec<Rung>, LadderError> {
        let solved = self.solve.ok_or(LadderError::NoSolve)?;
        Ok((1..=solved.rungs)
            .map(|k| {
                let level = if k == solved.rungs {
                    // Exact at the top, so the last rung is bit-for-bit the
                    // level `install_solve` constructed rather than the sum of
                    // k floating-point steps.
                    solved.emitted_dbfs_rms
                } else {
                    PILOT_LEVEL_DBFS_RMS + solved.rung_step_db * k as f64
                };
                Rung {
                    index: (k - 1) as u32,
                    level_dbfs_rms: level,
                    projected_spl_db: solved.chain_sensitivity_spl_per_dbfs + level,
                }
            })
            .collect())
    }

    /// **Step 5**, the gate: check one rung's measured SPL.
    ///
    /// Aborts on either of the spec's two conditions, checked in that order:
    /// over the class cap (`RungOverCap`), or more than
    /// [`RUNG_PROJECTION_TOLERANCE_DB`] from the projection
    /// (`SplProjectionMismatch`). Both are positive requirements, so a NaN
    /// measurement refuses as a mismatch rather than passing an unwitnessed
    /// cap.
    pub fn check_rung(&self, index: u32, measured_spl_db: f64) -> Result<(), LadderError> {
        let solved = self.solve.ok_or(LadderError::NoSolve)?;
        let rung = self.rungs()?.into_iter().find(|r| r.index == index).ok_or(
            LadderError::NoSuchRung {
                index,
                planned: solved.rungs,
            },
        )?;
        let cap = caps_for(self.class).spl_refuse_db;
        if measured_spl_db.is_finite() && measured_spl_db > cap {
            return Err(Refusal::new(MeasurementDiagnostic::RungOverCap).into());
        }
        let tracks = measured_spl_db.is_finite()
            && (measured_spl_db - rung.projected_spl_db).abs() <= RUNG_PROJECTION_TOLERANCE_DB;
        if !tracks {
            return Err(Refusal::new(MeasurementDiagnostic::SplProjectionMismatch).into());
        }
        Ok(())
    }

    /// **MS-8.** Evaluate the SNR gate over `band_hz`, and pick a remedy if it
    /// misses.
    ///
    /// **`sweep_magnitude_db` must come from [`analyze_magnitude_db`]**, the
    /// same function [`NoiseFloor::measure`] uses — not from a bare
    /// `compute_frequency_response`. `SNR(f)` is a *difference* of two levels,
    /// so it is meaningless unless both were measured on one scale, and a raw
    /// FFT magnitude scales with capture length: a 1 s floor against a 5 s
    /// sweep overstates SNR by ~6.6 dB, in the direction that promotes a
    /// should-have-warned measurement to a clean accept. See
    /// [`analyze_magnitude_db`] for the normalization and the measurement.
    ///
    /// The median and minimum are taken over the in-band bins only.
    ///
    /// Calling this **applies** the remedy it returns — the gain or the length
    /// is updated in place and the attempt is counted — so the caller re-runs
    /// the measurement and calls again. Two attempts, then
    /// `SnrUnachievable`.
    pub fn evaluate_snr(
        &mut self,
        sweep_magnitude_db: &[f64],
        floor: &NoiseFloor,
        band_hz: (f64, f64),
    ) -> Result<SnrOutcome, DspError> {
        if sweep_magnitude_db.len() != floor.spectrum_db.len() {
            return Err(DspError::InvalidInput(format!(
                "evaluate_snr: {} sweep bins vs {} floor bins",
                sweep_magnitude_db.len(),
                floor.spectrum_db.len()
            )));
        }
        let mut in_band: Vec<f64> = floor
            .freqs_hz
            .iter()
            .zip(sweep_magnitude_db)
            .zip(&floor.spectrum_db)
            .filter(|((f, _), _)| (band_hz.0..=band_hz.1).contains(*f))
            .map(|((_, s), n)| s - n)
            .collect();
        if in_band.is_empty() {
            return Err(DspError::InvalidInput(format!(
                "evaluate_snr: band {band_hz:?} selects no bins"
            )));
        }
        if in_band.iter().any(|v| !v.is_finite()) {
            return Err(DspError::InvalidInput(
                "evaluate_snr: non-finite SNR (check the sweep and floor magnitudes)".to_owned(),
            ));
        }
        in_band.sort_by(f64::total_cmp);
        let min_db = in_band[0];
        let median_db = median_of_sorted(&in_band);

        if median_db >= SNR_MEDIAN_ACCEPT_DB && min_db >= SNR_MIN_ACCEPT_DB {
            return Ok(SnrOutcome::Accept { median_db, min_db });
        }
        if median_db >= SNR_MEDIAN_WARN_DB {
            return Ok(SnrOutcome::AcceptWithWarning {
                diagnostic: MeasurementDiagnostic::LowSnr,
                median_db,
                min_db,
            });
        }
        match self.next_remedy() {
            Some(remedy) => Ok(SnrOutcome::Remedy {
                median_db,
                min_db,
                remedy,
            }),
            None => Ok(SnrOutcome::Refuse {
                median_db,
                min_db,
                refusal: Refusal::new(MeasurementDiagnostic::SnrUnachievable),
            }),
        }
    }

    /// The class this ladder was opened for.
    pub fn class(&self) -> TransducerClass {
        self.class
    }

    /// The current input-gain scalar, after any applied
    /// [`Remedy::RaiseInputGain`].
    pub fn input_gain(&self) -> f64 {
        self.input_gain
    }

    /// Record the input gain the device actually settled on, re-pin against
    /// the cal (MS-11), and invalidate any solve.
    ///
    /// [`Remedy::RaiseInputGain`] *requests* a gain; a real device quantizes
    /// (1/16 steps are common), so the caller sets it, reads it back, and tells
    /// the ladder what it got. Call this even when the read-back matches the
    /// request — it is the acknowledgement that the change happened.
    ///
    /// Always invalidates the solve, for the reason the remedy does: a new
    /// input gain is a new sensitivity, and re-using the old one would compute
    /// SPL through a Sens Factor that no longer describes the chain.
    pub fn confirm_input_gain(&mut self, read_back: f64) {
        self.apply_input_gain(read_back);
    }

    /// How many of the [`MAX_REMEDIES`] attempts have been spent.
    pub fn remedies_used(&self) -> usize {
        self.remedies_used
    }

    /// The solved level, dBFS RMS, pre-margin. `None` before the solve.
    ///
    /// **Never moves.** Remedies do not touch it, and
    /// `test_ladder.rs::remedies_never_touch_the_output_level` asserts that
    /// across a full remedy sequence — the REW imperative, as a test.
    pub fn solved_level_dbfs_rms(&self) -> Option<f64> {
        self.solve.map(|s| s.outcome.solved_dbfs_rms)
    }

    /// The current planned sweep length, after any applied
    /// [`Remedy::ExtendSweep`]. Never exceeds the class's `max_sweep_len_s`.
    pub fn sweep_len_s(&self) -> f64 {
        self.sweep_len_s
    }

    /// Apply the next remedy in the fixed order — input gain, then length —
    /// skipping any that cannot move. Returns `None` when the budget is spent
    /// or neither can move.
    ///
    /// **Each kind is used at most once**, which is what makes MS-8's "the
    /// remedy sequence is (input gain, length)" a sequence rather than a
    /// preference: without the once-only rule, a gain with room left would
    /// take both attempts and the length remedy would never be tried.
    ///
    /// Only an *applied* remedy consumes an attempt. A gain already at 1.0 is
    /// not a spent attempt, it is an unavailable option, and charging the
    /// budget for it would spend the length remedy without ever trying it.
    /// Adopt a new input gain: re-pin against the cal so MS-11's derate is
    /// judged against the gain now in force, and drop the solve.
    fn apply_input_gain(&mut self, gain: f64) {
        self.input_gain = gain;
        self.pin = self.cal.pin_gain(gain);
        self.solve = None;
    }

    fn next_remedy(&mut self) -> Option<Remedy> {
        if self.remedies_used >= MAX_REMEDIES {
            return None;
        }
        if !self.gain_remedy_used && self.input_gain.is_finite() && self.input_gain < 1.0 {
            let from = self.input_gain;
            let to = (from + INPUT_GAIN_REMEDY_STEP).min(1.0);
            self.gain_remedy_used = true;
            self.remedies_used += 1;
            // Moving the input gain invalidates the solve, and quietly: the
            // acoustic output does not change, but the MIC's reading of it
            // does, so the measured sensitivity, the projected SPL, every
            // rung's projection and the MS-11 pin are all now about a chain
            // that no longer exists. Nothing downstream can notice on its own
            // — an SPL computed through a stale Sens Factor is a plausible
            // number, not an obviously broken one — so the ladder drops the
            // solve and makes the caller redo it.
            self.apply_input_gain(to);
            return Some(Remedy::RaiseInputGain { from, to });
        }
        let max_len = caps_for(self.class).max_sweep_len_s;
        if !self.length_remedy_used && self.sweep_len_s < max_len {
            let from_s = self.sweep_len_s;
            // MS-13: doubling buys ~3 dB, and the class cap binds it. The
            // ladder must not auto-extend past the cap.
            let to_s = (from_s * 2.0).min(max_len);
            self.sweep_len_s = to_s;
            self.length_remedy_used = true;
            self.remedies_used += 1;
            return Some(Remedy::ExtendSweep { from_s, to_s });
        }
        None
    }
}

/// The **one** magnitude analysis both sides of the SNR gate must go through:
/// `20·log10(|X_k| / √N)`, resampled onto `grid` and 1/6-octave smoothed.
///
/// # Why the `√N`, and why this is a shared function rather than a comment
///
/// A raw rfft magnitude scales with the capture length, so two captures of the
/// *same* acoustic scene analyzed over different durations land at different
/// levels — and `SNR(f) = sweep(f) − floor(f)` is then wrong by that
/// difference. Measured on this crate before the normalization existed: the
/// same synthetic noise floor read **6.6 dB lower over 1 s than over 5 s**
/// (tracking `10·log10(5) = 7.0`). A one-second floor against a five-second
/// sweep therefore *overstated* SNR by ~6.6 dB — the unsafe direction, since it
/// silently promotes a measurement that should have warned or triggered a
/// remedy into a clean accept.
///
/// `√N` is the right divisor rather than `N`: broadband noise has
/// `E|X_k| ∝ a·√N`, so dividing by `√N` makes the **noise floor**
/// length-invariant. Coherent content still gains `√N` at the bin level —
/// measured +3.4 dB per doubling on a raw tone, matching the 3 dB
/// [`Remedy::ExtendSweep`] buys. Note the 1/6-octave smoothing below then
/// averages that narrowband gain away by design, so **do not read this as a
/// per-bin coherent-gain meter**; it is a smoothed level, which is what the
/// spec defines the SNR criterion on.
///
/// # The neighbourhood average is in the POWER domain, deliberately
///
/// `logf::resample_db_to_log_grid`'s `AntiComb` prefilter averages the values
/// it is handed, so handing it dB would take a *geometric* mean of amplitudes
/// — which under-reports peaks badly (measured −8.3 dB on a tone). For a noise
/// floor that is not a rounding error but a safety inversion: real room floors
/// are dominated by **tonal** components (mains hum and its harmonics, fan
/// whine), a dB-domain average would read those several dB low, and an
/// understated floor **overstates** SNR — promoting a hum-swamped band past
/// the `min ≥ 20 dB` criterion that exists to catch exactly it. So the
/// resampler is fed linear power and the result converted back, which makes
/// the neighbourhood average an energy average.
///
/// This is a function, not a documented convention, because a convention only
/// one side follows is the bug it is meant to prevent.
pub fn analyze_magnitude_db(
    samples: &[f64],
    sample_rate: u32,
    grid: &LogGrid,
) -> Result<Vec<f64>, DspError> {
    if samples.is_empty() {
        return Err(DspError::InvalidInput(
            "analyze_magnitude_db: empty capture".to_owned(),
        ));
    }
    if sample_rate == 0 {
        return Err(DspError::InvalidInput(
            "analyze_magnitude_db: sample rate is 0".to_owned(),
        ));
    }
    if samples.iter().any(|v| !v.is_finite()) {
        return Err(DspError::InvalidInput(
            "analyze_magnitude_db: capture contains non-finite samples".to_owned(),
        ));
    }
    let (freqs_linear, mag_db) =
        paraeq_dsp::fr::compute_frequency_response(samples, sample_rate, None)?;
    // |X_k| / sqrt(N), carried as POWER for the resample (see the header).
    let norm_db = 10.0 * (samples.len() as f64).log10();
    let power: Vec<f64> = mag_db
        .iter()
        .map(|v| 10f64.powf((v - norm_db) / 10.0))
        .collect();
    // Resample onto the log grid BEFORE smoothing: the boxcar is O(N²), and
    // over the ~24 000 rfft bins of a one-second capture at 48 kHz that is
    // 576 M iterations for a result that has to be resampled anyway.
    //
    // `resample_db_to_log_grid` is used here as the plain scalar resampler it
    // is — its `AntiComb` arm averages whatever values it is handed, and what
    // this needs averaged is power, not dB. The name says `db` because dB is
    // its only other caller's domain; feeding it power is what makes the
    // neighbourhood average an ENERGY average, which is the whole point.
    let on_grid = resample_db_to_log_grid(
        &freqs_linear,
        &power,
        grid,
        Prefilter::AntiComb { fraction: 48 },
    )?;
    // Back to dB, with the same 1e-10 floor convention `fr` uses so a silent
    // bin is a finite number rather than -inf.
    let on_grid_db: Vec<f64> = on_grid
        .iter()
        .map(|p| 10.0 * p.max(1e-20).log10())
        .collect();
    smooth(&on_grid_db, grid, Smoothing::Fixed(SNR_SMOOTHING_FRACTION))
}

/// The band the SNR gate is evaluated over, per class.
///
/// The lower edge is the class's sweep `f_start` where the table gives one
/// (the room paths), and 20 Hz on the coupler paths, whose `f_start` is
/// per-DUT: that column is a *driver-excursion* limit, and an SNR band is not
/// a safety limit, so the coupler falls back to the audible band rather than
/// to a number that does not exist.
pub fn snr_band_hz(class: TransducerClass) -> (f64, f64) {
    (caps_for(class).f_start_hz.unwrap_or(20.0), 20_000.0)
}

/// Median of an already-sorted slice; the mean of the middle two for an even
/// count. Panics on empty — callers check.
fn median_of_sorted(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}
