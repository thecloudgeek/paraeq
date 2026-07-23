//! Cal-load safety (MS-9/MS-10/MS-11): the parsed-calibration summary, the
//! full-scale witness rule, and the gain-referenced 6 dB margin.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**, per the four-tier
//! convention and the `spline.rs:1-5` precedent. The margin constant, the
//! gain-match tolerance and the witness rule are product policy; what is
//! pinned (`tests/test_cal.rs`) is every refusal edge of
//! [`CalSummary::validate`], the full-scale identity, and MS-11's exact
//! guarantee that a gain-read-back mismatch derates the emitted level to
//! ≤ solved − 6 dB.
//!
//! Three spec rules live here, all enforced at cal load — before a session
//! exists, and therefore before any sample could possibly be emitted:
//!
//! - **MS-9** — a missing or unparseable Sens Factor refuses via
//!   [`MicSensitivity::from_cal`]; no code path may fall back to an uncapped
//!   sweep. The `Err` arm carries a [`Refusal`] and no summary, so the
//!   fallback is unrepresentable, not merely forbidden.
//! - **MS-10** — REW's rule that an SPL limit above the input's clipping
//!   point offers no protection: the class cap is validated against the mic's
//!   full-scale SPL, and a cap the mic cannot witness refuses
//!   `CapExceedsMicFullScale`.
//! - **MS-11** — the spec's § Calibration margin: the UMIK-1 Sens Factor is
//!   captured at 100% input gain while REW-conditioned users run other gains,
//!   and community reports find factory values off by more than 4 dB. The
//!   input gain is therefore *pinned* into the summary ([`CalSummary::pin_gain`])
//!   and read back; sensitivity is treated as gain-referenced, and a
//!   read-back that disagrees with the reference derates the emitted level by
//!   [`CAL_ERROR_MARGIN_DB`] ([`margined_emit_dbfs`]).

use crate::diagnostic::{CalSensitivity, MeasurementDiagnostic, MicSensitivity, Refusal};
use crate::level::caps_for;
use paraeq_dsp::targets::TransducerClass;

/// The spec's "≥6 dB of margin against cal error" (§ Calibration margin),
/// applied when — and only when — the gain read-back disagrees with the cal's
/// reference gain. The blanket headroom against *unknown* error is already in
/// the quiet targets and hard caps; this margin is against *evidenced* error:
/// a chain whose input gain provably is not the one the Sens Factor was
/// captured at.
pub const CAL_ERROR_MARGIN_DB: f64 = 6.0;

/// Two input-gain scalars within this of each other are the same gain.
/// Gains follow the CoreAudio volume-scalar convention (0.0..=1.0), so 0.01
/// is 1% of the register's full travel — below any step a real device driver
/// exposes, and far below a gain difference that could move sensitivity
/// audibly.
pub const GAIN_MATCH_TOLERANCE: f64 = 0.01;

/// The parsed-calibration summary: what a cal-file load must prove before a
/// measurement session may exist (MS-9/MS-10), and the reference the input
/// gain is pinned against (MS-11).
///
/// Constructible only through [`CalSummary::validate`]; the fields are
/// private and there is no `Default`, mirroring [`MicSensitivity`]'s
/// philosophy — holding one *is* the proof the cal-load gates passed for this
/// class. The class is captured in the summary so a cal validated for one
/// class cannot be replayed against another class's cap.
#[derive(Clone, Debug, PartialEq)]
pub struct CalSummary {
    class: TransducerClass,
    file_identity: String,
    reference_input_gain: f64,
    sensitivity: MicSensitivity,
}

impl CalSummary {
    /// The cal-load gate, in order:
    ///
    /// 1. **MS-9** — sensitivity through [`MicSensitivity::from_cal`]:
    ///    missing refuses `SensitivityMissing`; unparseable or non-finite
    ///    refuses `SensitivityUnparseable`.
    /// 2. Reference gain must be a finite scalar in 0.0..=1.0 — written as a
    ///    positive requirement so NaN lands in the refusal arm. A
    ///    gain-referenced sensitivity with a meaningless reference did not
    ///    meaningfully parse: `SensitivityUnparseable`.
    /// 3. **MS-10** — the class cap must be at or below the mic's full-scale
    ///    SPL, else `CapExceedsMicFullScale`: a cap the mic cannot witness
    ///    could never fire.
    pub fn validate(
        class: TransducerClass,
        file_identity: String,
        sensitivity: CalSensitivity,
        reference_input_gain: f64,
    ) -> Result<Self, Refusal> {
        // MS-9: the sole path to a usable sensitivity.
        let sensitivity = MicSensitivity::from_cal(sensitivity)?;
        // Positive requirement: NaN passes no comparison, so it refuses.
        let gain_ok =
            reference_input_gain.is_finite() && (0.0..=1.0).contains(&reference_input_gain);
        if !gain_ok {
            return Err(Refusal::new(MeasurementDiagnostic::SensitivityUnparseable));
        }
        let summary = Self {
            class,
            file_identity,
            reference_input_gain,
            sensitivity,
        };
        // MS-10: a cap the mic cannot witness could never fire. Both sides
        // are finite here (sensitivity is validated; the caps are constants),
        // so the comparison cannot be NaN-blind.
        if caps_for(class).spl_refuse_db > summary.mic_full_scale_spl_db() {
            return Err(Refusal::new(MeasurementDiagnostic::CapExceedsMicFullScale));
        }
        Ok(summary)
    }

    /// The class this summary was validated for.
    pub fn class(&self) -> TransducerClass {
        self.class
    }

    /// Cal-file identity for the session log (MS-23): file name plus whatever
    /// the loader knows (serial, mic model). Opaque here.
    pub fn file_identity(&self) -> &str {
        &self.file_identity
    }

    /// The loudest SPL this mic can register: `94 − sens_factor` dB SPL, by
    /// the definition of the Sens Factor (dBFS at 94 dB SPL ⇒ 0 dBFS at
    /// `94 − sens_factor`). Derived, not stored — a second copy would be a
    /// second place for MS-10 to not be applied.
    pub fn mic_full_scale_spl_db(&self) -> f64 {
        self.sensitivity.spl_from_dbfs(0.0)
    }

    /// Pin the input gain (MS-11): capture the live read-back against this
    /// cal's reference. The session records both in its log and feeds the pin
    /// to [`margined_emit_dbfs`] at every level decision.
    pub fn pin_gain(&self, read_back: f64) -> PinnedGain {
        PinnedGain {
            read_back,
            reference: self.reference_input_gain,
        }
    }

    /// The gain the Sens Factor is referenced to (CoreAudio volume-scalar
    /// convention, validated 0.0..=1.0).
    pub fn reference_input_gain(&self) -> f64 {
        self.reference_input_gain
    }

    /// The validated sensitivity (MS-9's proof-carrying type).
    pub fn sensitivity(&self) -> MicSensitivity {
        self.sensitivity
    }
}

/// An input gain that was pinned and read back (MS-11). Carries both numbers
/// so the session log records the read-back verbatim, and answers exactly one
/// question: is the chain at the gain the sensitivity was captured at?
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinnedGain {
    read_back: f64,
    reference: f64,
}

impl PinnedGain {
    /// Written as a positive requirement so an unreadable (NaN) read-back is
    /// a mismatch, never a match — the derate is the safe direction.
    pub fn matches(&self) -> bool {
        (self.read_back - self.reference).abs() <= GAIN_MATCH_TOLERANCE
    }

    /// The live read-back, verbatim (may be NaN if the read failed).
    pub fn read_back(&self) -> f64 {
        self.read_back
    }

    /// The cal's reference gain.
    pub fn reference(&self) -> f64 {
        self.reference
    }
}

/// MS-11's level decision: the dBFS level to emit given the solve and the
/// pinned gain. A matching read-back emits the solved level unchanged; a
/// mismatch — including an unreadable read-back — derates by
/// [`CAL_ERROR_MARGIN_DB`], so the emitted level is ≤ solved − 6 dB exactly
/// as the requirement words it. Quiet is the safe direction: the derate can
/// cost SNR, never safety, and the SNR remedies (input gain, length) never
/// touch output level anyway.
///
/// The result still goes through `SweepLevel::new` — the sole level
/// constructor — at the session's solve boundary; this function decides the
/// number, not the type.
pub fn margined_emit_dbfs(solved_dbfs_rms: f64, pin: PinnedGain) -> f64 {
    if pin.matches() {
        solved_dbfs_rms
    } else {
        solved_dbfs_rms - CAL_ERROR_MARGIN_DB
    }
}
