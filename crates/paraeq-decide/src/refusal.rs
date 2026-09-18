//! The refusal and sanity checks — twenty-four of them, one per row of the
//! spec's refusal table plus the four codes that come from prose.
//!
//! "Refusal is what earns auto mode the right to hide everything. Each check
//! produces a typed [`Diagnostic`]; the front-ends differ only in how much of it
//! they render" (§ Refusal and Sanity Checks). Twenty table rows plus four codes
//! that come from prose — `CalHasTargetBakedIn`, `CoherentAveragingRejected`,
//! `FewPositions`, `SelfExclusionUnavailable`.
//!
//! Twenty-four checks over twenty-four codes. **Twenty-three of them can
//! construct a [`Diagnostic`]; `CoherentAveragingRejected` cannot**, and a
//! reader who greps this file for it and finds nothing has found the design
//! rather than an omission — see [`coherent_averaging`]. The remaining three
//! codes of the frozen twenty-seven (`VerificationPreampMismatch`,
//! `VerificationResidual`, `VerificationRoutingMismatch`) postdate the capture
//! and belong to the verification gate, not to this table.
//!
//! `Refuse` means *we do not know how to do this correctly and will not guess*;
//! `Warn` means *we did it, and here is what you should know*. The distinction
//! is not severity theatre: `decide()` turns any `Severity::Refuse` into
//! `Verdict::Refuse`, and a `Refuse` verdict carries `correction: None`, so the
//! system stays as it was. That equivalence is `lib.rs`'s `verdict_for` and
//! `tests/test_invariants.rs`'s two `refuse_iff_*` tests; nothing here may break
//! it, which is why no check ever returns a `Refuse` it does not mean.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**. These thresholds are
//! product policy; the prototype has nothing to say about them.
//! `tests/test_refusal.rs` gives every row one bundle that trips it and one that
//! does not.
//!
//! # Three rulings carried in, recorded so they are not rediscovered
//!
//! - **§ D-M — the thresholds are this crate's, not the capture layer's.** See
//!   the layering table below.
//! - **§ D-V — verification INVERTS MS-6.** The verification pass does not
//!   re-check `self_excluded`: the tap MUST see the helper, which is never
//!   excluded either way. `false` on a MEASUREMENT (not verification) capture is
//!   the Refuse, and [`self_exclusion`] reads `bundle.capture` only. If the
//!   measurement capture was contaminated the whole bundle is already refused,
//!   which is what verification relies on instead of a witness of its own.
//! - **§ D-Q — `TwoClock` is conditional now, and conditional on the ESTIMATE.**
//!   It fires as a `Warn` on a gated (FDW) path exactly when
//!   `capture.clock_skew_ppm` is `None`; `Some(ppm)` means the estimate was
//!   formed and applied, and the ppm rides on `Diagnostic::value`. There is
//!   **no rate compare** (ruling R-A3): the spec's `input_rate != output_rate`
//!   detection is a proxy that misses the common case, because a USB mic and a
//!   USB DAC both reporting 48 000 Hz are two crystals and one label. Equal
//!   nominal rates do not imply one clock — that IS the two-clock hazard.
//!
//! # The layering (§ D-M), threshold by threshold
//!
//! Three numbers differ between this table and `paraeq-measure`'s, and they are
//! **not in conflict — they are different gates in different layers**. The
//! capture layer's fire DURING a capture and refuse to emit another sample; this
//! crate's fire on the assembled bundle AFTERWARDS, when nothing can be
//! re-measured and the only question is whether the bundle can be trusted. They
//! must never be "reconciled" into one number.
//!
//! | this crate | the capture-layer gate it is NOT |
//! |---|---|
//! | [`CLIP_POSITION_DBFS`] = −0.3 dBFS, a post-hoc PEAK read | `capture::CLIP_THRESHOLD` = 1.0, "full scale exactly, not a hair under", counted per sample as the capture runs |
//! | [`CLIP_SESSION_BLOCK_FRACTION`] = 0.30, applied to the whole pass | `capture::CLIP_BLOCK_FRACTION` = 0.30, applied per BLOCK, which ends the run early with `MeasurementDiagnostic::InputClipping` |
//! | [`NOISE_FLOOR_MAX_DBFS`] = −24.0 | `ladder::NOISE_FLOOR_MAX_DBFS` = −60.0, which decides whether a sweep may be emitted at all |
//! | [`SNR_SOFT_DB`] = 25.0 / [`SNR_HARD_DB`] = 15.0 | `ladder::SNR_MEDIAN_ACCEPT_DB` = 40.0, `ladder::SNR_MIN_ACCEPT_DB` = 20.0 — the pre-capture ladder's accept gate, which has remedies available to it that a finished bundle does not |
//! | [`CAL_OUTLIER_DB`] = 1.5, passed as the argument | `compensation::DEFAULT_OUTLIER_DB` = 1.0, the loader's own warn level; `validate_cal_with_threshold` takes the threshold, so passing 1.5 needs no change there |
//! | `PathProfile::sensitivity_envelope_spl_per_dbfs` | `TransducerCaps::sensitivity_envelope_spl_per_dbfs`, the MS-17 table that owns the numbers and refuses before a sample is emitted; this crate mirrors it because it may not depend on `paraeq-measure` |
//!
//! **The one number that could not come across.** The session-clipping row is
//! REW's "more than 30 % of the samples in an input BLOCK at `|x| ≥ 0.997`", and
//! a bundle carries no blocks and no samples — [`crate::CaptureStats`] is three
//! summary numbers per pass. What survives the summarisation is the meter's
//! cumulative `clipped_samples` against the sweep's own length, and the
//! pigeonhole principle makes that a sound ONE-SIDED test: if more than 30 % of
//! a pass clipped, then more than 30 % of *some* block did. The converse does
//! not hold, which is why the capture layer's live per-block gate remains the
//! primary one and this is the post-hoc backstop. The magnitude likewise cannot
//! be applied here: every sample `CaptureMeter` counted is at or beyond full
//! scale, which is already beyond the row's 0.997, so the count is a lower bound
//! on the row's quantity and this gate can only ever under-fire.

use crate::analysis::AnalysisProducts;
use crate::bundle::MeasurementBundle;
use crate::decision::{Decision, Domain, InRange};
use crate::decisions::Decisions;
use crate::outcome::{Diagnostic, DiagnosticCode, Severity};
use crate::profile::{profile_for, AveragingMode, CouplingPath, GatingMode, PathProfile};
use paraeq_dsp::compensation::CalWarningKind;
use paraeq_dsp::logf::LogGrid;
use serde::Serialize;

// ---------------------------------------------------------------------------
// The thresholds. Alphabetical. Every one is this crate's OWN number, taken
// from the decision table; see the module header for what each one is NOT.
// ---------------------------------------------------------------------------

/// Span of the averaged curve over `[low_corner, 10 kHz]` above which the thing
/// measured is not a loudspeaker or a headphone.
const ABSURD_SPAN_DB: f64 = 40.0;

/// The top of the span band. The bottom is the decided `low_corner_hz`.
const ABSURD_SPAN_MAX_HZ: f64 = 10_000.0;

/// Midband tilt above which the same row fires, taken as an absolute value: a
/// −25 dB/decade midband is as absurd as a +25 dB/decade one.
const ABSURD_TILT_DB_PER_DECADE: f64 = 20.0;

/// `|g[i] − (g[i−1]+g[i+1])/2| > 1.5 dB` is a bad cal point. Not hypothetical: a
/// shipping vendor file carries a bogus `0.0000` at 19.611 Hz between −3.13 and
/// −3.11, a 3.12 dB error inside the full-authority band.
const CAL_OUTLIER_DB: f64 = 1.5;

/// Any capture peak at or above this loses that position.
const CLIP_POSITION_DBFS: f64 = -0.3;

/// REW's rule: more than this fraction clipped invalidates the measurement
/// regardless of level. Strictly greater — exactly 30 % passes, as REW words it.
const CLIP_SESSION_BLOCK_FRACTION: f64 = 0.30;

/// The coupler's HF outlier band: "above 1 kHz".
const COUPLER_HF_MIN_HZ: f64 = 1000.0;

/// The coupler's LF outlier band, where a seal problem lives.
const COUPLER_LF_HZ: (f64, f64) = (20.0, 200.0);

/// The midband the tilt is measured over — the same 200 Hz–2 kHz the midband
/// reference `M` and `align_spl_band` use.
const MIDBAND_HZ: (f64, f64) = (200.0, 2000.0);

/// Dirac's gate on the silence capture. The TARGET is −36; this is the refusal.
const NOISE_FLOOR_MAX_DBFS: f64 = -24.0;

/// A capture whose peak never got here heard nothing.
const NO_SIGNAL_PEAK_DBFS: f64 = -50.0;

/// A capture whose broadband RMS is below this heard nothing.
const NO_SIGNAL_RMS_DBFS: f64 = -60.0;

/// Mean |deviation| from the cohort above which a position is an outlier. One
/// number, three rows: the BAND and the SEVERITY are what differ.
const OUTLIER_DEVIATION_DB: f64 = 6.0;

/// Median σ(f) below the transition above which the positions disagree where
/// they should agree.
///
/// Equal to `authority::SIGMA_NONE_DB` by coincidence of value, not by
/// derivation — and the comparison is STRICTLY greater, so the
/// all-`SIGMA_NONE_DB` σ that the unanalysable path publishes does not read as
/// a variance refusal. A bundle that could not be analysed earns its refusal
/// from the row that describes what is actually wrong with it.
const SIGMA_MEDIAN_REFUSE_DB: f64 = 6.0;

/// Below this the measurement cannot be trusted at all.
const SNR_HARD_DB: f64 = 15.0;

/// Below this the position is used and de-weighted.
const SNR_SOFT_DB: f64 = 25.0;

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// Every refusal and warning the bundle earns, in a stable order.
///
/// Takes the decided values as well as the bundle because most rows are graded
/// against a decision rather than against a raw input — the SNR rows against
/// `correction_range`, the variance row against `transition_hz` and
/// `low_corner_hz`, the position-count rows against `positions_default`.
///
/// The order is alphabetical by check, which is this repo's convention and, more
/// importantly, is FIXED: `fixtures/decide/<case>/expected.json` compares
/// `diagnostics` as a list, so a reordering is a fixture diff.
pub(crate) fn diagnostics(
    bundle: &MeasurementBundle,
    decisions: &Decisions,
    analysis: &AnalysisProducts,
) -> Vec<Diagnostic> {
    // The same grid `decide()` analysed on. Rebuilt rather than threaded through
    // because `LogGrid::standard()` is a constant-valued constructor and the
    // alternative is a fourth argument that could disagree with the third.
    let grid = LogGrid::standard();
    let freqs = grid.freqs();
    let profile = profile_for(decisions.class.value);

    let mut out = Vec::new();
    out.extend(absurd_curve(decisions, analysis, freqs));
    out.extend(cal_defects(bundle));
    out.extend(cal_has_target_baked_in(bundle));
    out.extend(clipping_position(bundle));
    out.extend(clipping_session(bundle));
    out.extend(coherent_averaging(decisions));
    out.extend(excessive_variance(decisions, analysis, freqs));
    out.extend(low_snr(bundle));
    out.extend(mic_not_connected(bundle));
    out.extend(no_signal(bundle));
    out.extend(noise_floor_too_high(bundle));
    out.extend(override_out_of_domain(bundle, decisions));
    out.extend(position_count(bundle, profile));
    out.extend(position_outliers(bundle, analysis, profile, freqs));
    out.extend(self_exclusion(bundle));
    out.extend(sweep_rate_mismatch(bundle));
    out.extend(two_clock(bundle, profile));
    out.extend(wrong_transducer(bundle, decisions, profile));
    out
}

// ---------------------------------------------------------------------------
// The curve rows
// ---------------------------------------------------------------------------

/// "Span of the averaged curve over `[low_corner, 10 kHz]`; or midband tilt over
/// 200 Hz–2 kHz" — `span > 40 dB or tilt > 20 dB/decade` ⇒ `Refuse`.
///
/// One code, two independent halves, one diagnostic: the worst channel decides,
/// the same posture the engine takes for the preamp. A stereo capture with one
/// absurd channel is an absurd capture.
fn absurd_curve(
    decisions: &Decisions,
    analysis: &AnalysisProducts,
    freqs: &[f64],
) -> Option<Diagnostic> {
    let low = decisions.low_corner_hz.value;
    // `worst` is `(margin, the number to report)`. The two halves are in
    // DIFFERENT UNITS — dB and dB/decade — so "the worst" is the larger margin
    // over each one's own threshold, which is dimensionless. Comparing a 41 dB
    // span against a 21 dB/decade slope directly would be comparing a height to
    // a gradient and would report whichever happened to be numerically bigger.
    let mut worst: Option<(f64, f64)> = None;
    for curve in &analysis.averaged_db {
        if curve.len() != freqs.len() {
            continue;
        }
        let span = span_db(freqs, curve, low, ABSURD_SPAN_MAX_HZ)
            .filter(|span| *span > ABSURD_SPAN_DB)
            .map(|span| (span / ABSURD_SPAN_DB, span));
        let tilt = tilt_db_per_decade(freqs, curve, MIDBAND_HZ.0, MIDBAND_HZ.1)
            .map(f64::abs)
            .filter(|tilt| *tilt > ABSURD_TILT_DB_PER_DECADE)
            .map(|tilt| (tilt / ABSURD_TILT_DB_PER_DECADE, tilt));
        for (margin, value) in span.into_iter().chain(tilt) {
            if worst.is_none_or(|(w, _)| margin > w) {
                worst = Some((margin, value));
            }
        }
    }
    let (_, value) = worst?;
    Some(Diagnostic {
        code: DiagnosticCode::AbsurdCurve,
        position: None,
        remedy: "This doesn't look like a loudspeaker or a headphone. Check the mic calibration \
                 file matches the mic."
            .to_string(),
        severity: Severity::Refuse,
        value: Some(value),
    })
}

/// "Multi-position data reaching a **vector/coherent** routine ⇒
/// `Refuse(CoherentAveragingRejected)`".
///
/// A tripwire on a door the type system already locked: `AveragingMode` has no
/// coherent variant, `fr::average_measurements_vector` is never called from
/// `decide()`, and that function refuses `n > 1` anyway. The match is EXHAUSTIVE
/// WITH NO WILDCARD on purpose — adding a coherent mode stops this compiling,
/// which is the only way a tripwire that can never fire keeps its value.
fn coherent_averaging(decisions: &Decisions) -> Option<Diagnostic> {
    match decisions.averaging.value {
        AveragingMode::DbMean | AveragingMode::Power => None,
    }
}

/// "median σ(f) over `[low_corner, f_t]` > 6.0 dB ⇒ Refuse" — the positions
/// disagree in the bass, where they should not.
fn excessive_variance(
    decisions: &Decisions,
    analysis: &AnalysisProducts,
    freqs: &[f64],
) -> Option<Diagnostic> {
    if analysis.sigma_db.len() != freqs.len() || analysis.per_position_db.len() < 2 {
        return None;
    }
    let indices = band(
        freqs,
        decisions.low_corner_hz.value,
        decisions.transition_hz.value,
    );
    let mut values: Vec<f64> = indices
        .iter()
        .map(|i| analysis.sigma_db[*i])
        .filter(|v| v.is_finite())
        .collect();
    let median = median(&mut values)?;
    if median <= SIGMA_MEDIAN_REFUSE_DB {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::ExcessiveVariance,
        position: None,
        remedy: "Your positions disagree even in the bass, where they shouldn't. Did the mic move \
                 rooms, or is one speaker off?"
            .to_string(),
        severity: Severity::Refuse,
        value: Some(median),
    })
}

/// The three outlier rows. One threshold, three bands, two severities.
///
/// The deviation is LEAVE-ONE-OUT — the position against the mean of the
/// OTHERS, not against a cohort mean it is itself inside. With five positions an
/// all-inclusive mean dilutes a 7 dB outlier to 5.6 dB and the row never fires,
/// which is the failure mode this comment exists to prevent.
fn position_outliers(
    bundle: &MeasurementBundle,
    analysis: &AnalysisProducts,
    profile: &PathProfile,
    freqs: &[f64],
) -> Vec<Diagnostic> {
    let curves = &analysis.per_position_db;
    if curves.len() < 2
        || curves.len() != bundle.positions.len()
        || curves.iter().any(|c| c.len() != freqs.len())
    {
        return Vec::new();
    }
    // Coupler: two bands, two severities. Room: the whole band, one warning —
    // "check the mic wasn't against a wall" is not a reason to throw a capture
    // away.
    let rows: &[(Vec<usize>, DiagnosticCode, Severity)] = &match profile.coupling {
        CouplingPath::Coupler => vec![
            (
                band(freqs, COUPLER_LF_HZ.0, COUPLER_LF_HZ.1),
                DiagnosticCode::PositionOutlierCouplerLf,
                Severity::Refuse,
            ),
            (
                band(freqs, COUPLER_HF_MIN_HZ, f64::INFINITY),
                DiagnosticCode::PositionOutlierCouplerHf,
                Severity::Warn,
            ),
        ],
        CouplingPath::Room => vec![(
            band(freqs, 0.0, f64::INFINITY),
            DiagnosticCode::PositionOutlierRoom,
            Severity::Warn,
        )],
    };

    let mut out = Vec::new();
    for (p, position) in bundle.positions.iter().enumerate() {
        for (indices, code, severity) in rows {
            let Some(deviation) = cohort_deviation_db(curves, p, indices) else {
                continue;
            };
            if deviation <= OUTLIER_DEVIATION_DB {
                continue;
            }
            let i = position.index;
            out.push(Diagnostic {
                code: *code,
                position: Some(i),
                remedy: match code {
                    DiagnosticCode::PositionOutlierCouplerLf => format!(
                        "Position {i}'s bass is {deviation:.0} dB below the others. That's a seal \
                         problem, not the headphone. Reseat and measure again."
                    ),
                    DiagnosticCode::PositionOutlierCouplerHf => format!(
                        "Position {i} differs up top. That's normal placement scatter — we \
                         averaged it in."
                    ),
                    _ => format!(
                        "Position {i} is unusual. Averaged in; check the mic wasn't against a wall."
                    ),
                },
                severity: *severity,
                value: Some(deviation),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The level rows
// ---------------------------------------------------------------------------

/// "Any sample `≥ −0.3 dBFS` (0.9661) ⇒ Refuse *that position*."
///
/// A PEAK test, and the bundle answers it exactly: `CaptureStats::peak_dbfs` is
/// the meter's decayed peak for the pass.
fn clipping_position(bundle: &MeasurementBundle) -> Vec<Diagnostic> {
    bundle
        .positions
        .iter()
        .filter(|p| p.capture.peak_dbfs >= CLIP_POSITION_DBFS)
        .map(|p| {
            let i = p.index;
            Diagnostic {
                code: DiagnosticCode::ClippingPosition,
                position: Some(i),
                remedy: format!(
                    "Position {i} clipped. We dropped it — reduce input gain by 6 dB and \
                     re-measure just that one."
                ),
                severity: Severity::Refuse,
                value: Some(p.capture.peak_dbfs),
            }
        })
        .collect()
}

/// REW's sustained-clipping rule, as the decision layer can see it: see the
/// module header's "the one number that could not come across" for why the
/// fraction is taken over the whole pass and why that is sound one-sidedly.
///
/// Session-scoped, so `position` stays `None` — the position-scoped read of the
/// same physical event is [`clipping_position`], and the two rows exist
/// separately because one overshoot loses a capture while sustained railing
/// loses the session.
fn clipping_session(bundle: &MeasurementBundle) -> Option<Diagnostic> {
    let sweep_samples =
        bundle.capture.sweep.duration_s * f64::from(bundle.capture.sweep_rate.max(1));
    if sweep_samples <= 0.0 || !sweep_samples.is_finite() {
        return None;
    }
    let worst = bundle
        .positions
        .iter()
        .map(|p| p.capture.clipped_samples as f64 / sweep_samples)
        .fold(0.0f64, f64::max);
    if worst <= CLIP_SESSION_BLOCK_FRACTION {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::ClippingSession,
        position: None,
        remedy: "The mic input clipped. Turn the input gain down 6 dB and measure again."
            .to_string(),
        severity: Severity::Refuse,
        value: Some(worst),
    })
}

/// The two SNR rows, per position. Hard outranks soft: a position that trips 15
/// dB has already tripped 25 and must not be reported twice.
///
/// **Composed, and the composition is a gap worth naming.** The row is "capture
/// RMS − floor RMS in `correction_range`", i.e. a per-band SNR — and the bundle
/// carries no per-position captured SPECTRUM. `CaptureStats::rms_dbfs` is
/// broadband and `NoiseFloor::spectrum_db` is the floor's, so the band
/// restriction cannot be honoured here. This is the broadband read of the same
/// quantity: the worst channel's floor against each pass's own RMS.
///
/// **The soft row's "+ de-weight" is NOT wired here** and cannot be: the
/// averaging that would carry the weights ran before this pass did. Per § D-P's
/// own interim ("emit the Warn and do **not** de-weight, **and say so**"), the
/// weighting belongs at the analysis stage, beside
/// `fr::average_measurements_rms_weighted`.
fn low_snr(bundle: &MeasurementBundle) -> Vec<Diagnostic> {
    let Some(floor) = worst_noise_floor_dbfs(bundle) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for position in &bundle.positions {
        if !position.capture.rms_dbfs.is_finite() {
            continue;
        }
        let snr = position.capture.rms_dbfs - floor;
        let i = position.index;
        if snr < SNR_HARD_DB {
            out.push(Diagnostic {
                code: DiagnosticCode::LowSnrHard,
                position: Some(i),
                // VERBATIM, and the emphasis is load-bearing: turning the sweep
                // up raises the signal and the room's reflected noise together.
                remedy: "Too much background noise to trust this. Fix the noise — **do not** turn \
                         the sweep up; that doesn't help."
                    .to_string(),
                severity: Severity::Refuse,
                value: Some(snr),
            });
        } else if snr < SNR_SOFT_DB {
            out.push(Diagnostic {
                code: DiagnosticCode::LowSnrSoft,
                position: Some(i),
                remedy: format!(
                    "Position {i} was noisy ({snr:.0} dB). We used it, but weighted it down."
                ),
                severity: Severity::Warn,
                value: Some(snr),
            });
        }
    }
    out
}

/// "Per-position capture RMS and peak — RMS `< −60 dBFS` or peak `< −50 dBFS`."
///
/// Position-scoped in `position` so the drawer can say which pass was silent,
/// but session-refusing in severity: the spec's row is a plain `Refuse`, and the
/// copy it renders is about the whole chain rather than about one capture.
fn no_signal(bundle: &MeasurementBundle) -> Vec<Diagnostic> {
    bundle
        .positions
        .iter()
        .filter(|p| {
            p.capture.rms_dbfs < NO_SIGNAL_RMS_DBFS || p.capture.peak_dbfs < NO_SIGNAL_PEAK_DBFS
        })
        .map(|p| Diagnostic {
            code: DiagnosticCode::NoSignal,
            position: Some(p.index),
            remedy: "We heard nothing. Check the mic is selected as the input and the sweep is \
                     going to the right speakers."
                .to_string(),
            severity: Severity::Refuse,
            value: Some(p.capture.rms_dbfs),
        })
        .collect()
}

/// "Silence capture RMS in the analysis band `> −24 dBFS` (Dirac's gate; target
/// −36) ⇒ Refuse."
///
/// The worst channel decides: a floor is a property of the room, and one noisy
/// input is enough to make the measurement untrustworthy.
fn noise_floor_too_high(bundle: &MeasurementBundle) -> Option<Diagnostic> {
    let worst = worst_noise_floor_dbfs(bundle)?;
    if worst <= NOISE_FLOOR_MAX_DBFS {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::NoiseFloorTooHigh,
        position: None,
        remedy: "The room is too noisy to measure. Turn off fans/AC and try again.".to_string(),
        severity: Severity::Refuse,
        value: Some(worst),
    })
}

/// The LOUDEST channel's silence-capture RMS, which is the one both level rows
/// grade against. `None` when no channel reported a finite floor — the two rows
/// then have no evidence, and a missing floor is not the same claim as a quiet
/// one.
fn worst_noise_floor_dbfs(bundle: &MeasurementBundle) -> Option<f64> {
    bundle
        .noise_floor
        .rms_dbfs
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(None, |worst: Option<f64>, v| {
            Some(worst.map_or(v, |w| w.max(v)))
        })
}

// ---------------------------------------------------------------------------
// The capture-identity and topology rows
// ---------------------------------------------------------------------------

/// "`capture.input_uid` no longer resolves ⇒ Refuse." Resolving a UID is I/O,
/// which `decide()` forbids itself, so the capture layer records the boolean
/// witness and this reads it.
fn mic_not_connected(bundle: &MeasurementBundle) -> Option<Diagnostic> {
    if bundle.capture.input_present {
        return None;
    }
    let name = &bundle.capture.input_uid;
    Some(Diagnostic {
        code: DiagnosticCode::MicNotConnected,
        position: None,
        remedy: format!("The microphone we measured with ({name}) isn't connected any more."),
        severity: Severity::Refuse,
        value: None,
    })
}

/// MS-6's post-capture half: `capture.self_excluded == false` means the tap's
/// fail-open path left ParaEQ's own audio tapped, so the baseline was measured
/// through the correction engine and the stimulus topology is unvalidated.
///
/// **§ D-V — do not copy this into the verification path.** Verification INVERTS
/// it: the helper child process must BE seen by the tap, and it is never
/// excluded either way. `Verification` therefore carries no `self_excluded` of
/// its own and none is invented here; the verification pass relies on this row
/// having passed for the baseline it subtracts against.
fn self_exclusion(bundle: &MeasurementBundle) -> Option<Diagnostic> {
    if bundle.capture.self_excluded {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::SelfExclusionUnavailable,
        position: None,
        remedy: "ParaEQ could not keep its own audio out of the measurement. Restart ParaEQ \
                 before measuring."
            .to_string(),
        severity: Severity::Refuse,
        value: None,
    })
}

/// "`ir.sample_rate != capture.sweep_rate` ⇒ Refuse." The deconvolution and the
/// stimulus that produced it disagree about time itself.
fn sweep_rate_mismatch(bundle: &MeasurementBundle) -> Vec<Diagnostic> {
    bundle
        .positions
        .iter()
        .filter(|p| p.ir.sample_rate != bundle.capture.sweep_rate)
        .map(|p| Diagnostic {
            code: DiagnosticCode::SweepRateMismatch,
            position: Some(p.index),
            // The spec's row, verbatim: this is the one remedy addressed to us
            // rather than to the user.
            remedy: "Internal error — this is a bug, not a user condition.".to_string(),
            severity: Severity::Refuse,
            value: Some(f64::from(p.ir.sample_rate)),
        })
        .collect()
}

/// "Two-clock (gated paths) ⇒ Warn", fired on the SKEW ESTIMATE rather than on
/// a rate compare.
///
/// Keyed on GATING rather than on the class, because the spec's own qualifier is
/// "(gated paths)" and the stake is a trustworthy t=0: the coupler does not gate
/// and has nothing to lose.
///
/// **§ D-Q, as ruling R-A3 reads it.** The decision record post-dates the spec:
/// the skew is now estimated from bracketed timing markers and resampled away,
/// default on, with this Warn as the fallback *when no estimate can be formed*.
/// So `Some(ppm)` earns no row, and `None` on a gated path earns one — that is
/// the whole condition.
///
/// **The rate compare is gone, and its absence is the fix.** The spec's
/// Detection column reads `input_rate != output_rate`, and the guard built from
/// it made this row unreachable on exactly the configuration it is about: a USB
/// mic and a USB DAC both reporting 48 000 Hz are two crystals, not one clock,
/// and the nominal rate is a label rather than a measurement. Every golden
/// bundle in `fixtures/decide/` captures at 48 000/48 000, so the row was graded
/// by no golden case at all. `value` carries the ppm when one was formed, which
/// is `None` on every path that reaches the `Some(Diagnostic)` below — read from
/// the field rather than written as a literal so the two cannot drift.
fn two_clock(bundle: &MeasurementBundle, profile: &PathProfile) -> Option<Diagnostic> {
    let gated = matches!(profile.gating, GatingMode::Fdw { .. });
    if !gated || bundle.capture.clock_skew_ppm.is_some() {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::TwoClock,
        position: None,
        remedy: "Your mic and speakers run on different clocks. We compensate, but \
                 timing-sensitive results are approximate."
            .to_string(),
        severity: Severity::Warn,
        value: bundle.capture.clock_skew_ppm,
    })
}

/// MS-17's envelope: "Solved chain sensitivity vs the path's expected envelope,
/// outside ⇒ Refuse". A **safety** event, not a quality event.
///
/// The envelope is `PathProfile::sensitivity_envelope_spl_per_dbfs`, which
/// mirrors `TransducerCaps::sensitivity_envelope_spl_per_dbfs` because this
/// crate may not depend on `paraeq-measure`; a cross-crate test compares the two
/// tables field for field. **E5: the numbers themselves are `[NEEDS DATA]`** —
/// engineering estimates with one end pinned by the level ladder — so a refusal
/// here is as good as that table and no better.
///
/// `None` means the level solve produced no sensitivity, so there is nothing to
/// compare and the cross-check does not fire. It must not invent a value: an
/// assumed sensitivity is exactly the thing MS-17 exists to refuse.
fn wrong_transducer(
    bundle: &MeasurementBundle,
    decisions: &Decisions,
    profile: &PathProfile,
) -> Option<Diagnostic> {
    let solved = bundle.capture.chain_sensitivity_spl_per_dbfs?;
    if !solved.is_finite() || profile.sensitivity_envelope_spl_per_dbfs.contains(&solved) {
        return None;
    }
    let class = decisions.class.value.display_name();
    Some(Diagnostic {
        code: DiagnosticCode::WrongTransducer,
        position: None,
        remedy: format!(
            "The levels don't look like {class}. An empty jig, headphones sitting on a desk, or a \
             sweep going to the laptop speakers all look like this."
        ),
        severity: Severity::Refuse,
        value: Some(solved),
    })
}

// ---------------------------------------------------------------------------
// The cohort rows
// ---------------------------------------------------------------------------

/// "`< 3` ⇒ `Refuse(TooFewPositions)`. Below `positions_default` ⇒
/// `Warn(FewPositions)`."
///
/// Graded against `bundle.positions.len()`, the PHYSICAL count, not against
/// `decisions.positions_n.value`: the decision echoes the count and the drawer
/// can override the echo, and an override must not be able to conjure a capture
/// that was never taken. The hard minimum is read from
/// `PathProfile::positions_domain`'s lower bound — one source for the number the
/// domain and the refusal both use.
fn position_count(bundle: &MeasurementBundle, profile: &PathProfile) -> Option<Diagnostic> {
    let n = bundle.positions.len();
    let minimum = *profile.positions_domain.start();
    // `rules::position_noun`, NOT `PathProfile::reposition_noun`: the latter is
    // the imperative retry phrase ("move the mic ~30 cm") and rendered
    // "We need at least 3 move the mic ~30 cms" here. Ruling R-A11.
    let noun = crate::rules::position_noun(profile);
    if n < minimum {
        return Some(Diagnostic {
            code: DiagnosticCode::TooFewPositions,
            position: None,
            remedy: format!(
                "We need at least {minimum} {noun}s to tell your system apart from where you put \
                 the mic."
            ),
            severity: Severity::Refuse,
            value: Some(n as f64),
        });
    }
    if n >= profile.positions_default {
        return None;
    }
    Some(Diagnostic {
        code: DiagnosticCode::FewPositions,
        position: None,
        remedy: format!(
            "We averaged {n} {noun}s. More positions mean we can tell your room's problems apart \
             from your chair's."
        ),
        severity: Severity::Warn,
        value: Some(n as f64),
    })
}

// ---------------------------------------------------------------------------
// The calibration rows
// ---------------------------------------------------------------------------

/// `CalMissing`, `CalMalformed` and `CalNeighbourOutlier`, in one pass because
/// they are one file.
///
/// The frequency and outlier defects come from
/// `compensation::validate_cal_with_threshold`, which already takes the
/// threshold as an argument — so passing [`CAL_OUTLIER_DB`] needs no change to
/// `compensation::DEFAULT_OUTLIER_DB`, and the two layers keep their own numbers
/// (§ D-M). Non-finite values are checked here because the loader's own parser
/// rejects them before they can reach a `CalFile`, and a hand-edited bundle can
/// still carry one.
fn cal_defects(bundle: &MeasurementBundle) -> Vec<Diagnostic> {
    let Some(cal) = &bundle.cal else {
        return vec![Diagnostic {
            code: DiagnosticCode::CalMissing,
            position: None,
            remedy: "We need your mic's calibration file. Without it we're measuring the mic, not \
                     your speakers."
                .to_string(),
            severity: Severity::Refuse,
            value: None,
        }];
    };

    let (freqs, gains) = (&cal.curve.0, &cal.curve.1);
    let malformed = |row: usize| Diagnostic {
        code: DiagnosticCode::CalMalformed,
        position: None,
        remedy: format!("The calibration file is malformed at row {row}."),
        severity: Severity::Refuse,
        value: Some(row as f64),
    };
    // Two points is the fewest a curve can be interpolated between, and
    // `apply_compensation` edge-holds outside the range — so a one-point "curve"
    // is a constant offset wearing a calibration's clothes, which is the shape
    // this row exists to refuse. The row number reported is the first one the
    // file does not have.
    if freqs.len() != gains.len() || freqs.len() < 2 {
        return vec![malformed(freqs.len().min(gains.len()))];
    }
    // Non-finite and non-positive frequencies are checked here rather than left
    // to the validator: `parse_cal` rejects them at load, so only a hand-edited
    // bundle can carry one, and the validator's log interpolant would answer
    // `None` for it and report nothing at all.
    if let Some(row) = freqs
        .iter()
        .zip(gains)
        .position(|(f, g)| !f.is_finite() || !g.is_finite() || *f <= 0.0)
    {
        return vec![malformed(row)];
    }

    let parsed = paraeq_dsp::compensation::CalFile {
        again_db: None,
        freqs: freqs.clone(),
        gains_db: gains.clone(),
        ignored_lines: Vec::new(),
        sens_factor_db: cal.sensitivity_db,
        serial: cal.serial.clone(),
    };
    // One code for `Outlier` and `SuspectZero` both: an exact 0.0000 between
    // non-zero neighbours is the vendor's own signature and earns its own
    // message from the validator, but the user's remedy is the same and the
    // frozen vocabulary has one code for it.
    let outlier = |f: f64, g: f64, a: f64, b: f64| {
        // The ROW's own formula — `|g[i] − (g[i−1]+g[i+1])/2|` — rather than the
        // validator's log-frequency interpolant, which is the stricter GATE but
        // not the number the copy quotes. On the vendor file the two agree to
        // 0.001 dB.
        let deviation = (g - 0.5 * (a + b)).abs();
        Diagnostic {
            code: DiagnosticCode::CalNeighbourOutlier,
            position: None,
            remedy: format!(
                "The calibration file has a bad value at {f:.3} Hz ({g:.2} dB between neighbours \
                 of {a:.2} and {b:.2})."
            ),
            severity: Severity::Refuse,
            value: Some(deviation),
        }
    };

    // **The neighbour-outlier half does not run on the three EARS variants**
    // (ruling R-A2, `OPEN [OWNER]` and reversible). An HEQ/HPN/IDF calibration
    // has a target subtracted into the curve — that is what
    // `CalHasTargetBakedIn` is about — so the target's own shape (the Harman
    // 5 kHz dip, 4.7 dB from the line through its neighbours on a twelve-point
    // vendor grid) is indistinguishable from a bad point: both are "one value
    // far from its neighbours", and the variant is the only evidence there is.
    // Refusing here would refuse every EARS jig shipped with the calibration
    // its own vendor supplies, for carrying the shape it is supposed to carry.
    //
    // The MALFORMED half still runs: a duplicate or non-monotonic frequency is
    // a defect in the FILE's structure and no target explains one.
    //
    // **To reverse this**, delete the `skip_outliers` guard below. What would
    // justify reversing it is a density rule — the row is only meaningful on a
    // grid fine enough that a target's slope cannot look like a step — which
    // needs a number nobody has yet.
    let skip_outliers = matches!(
        cal.variant,
        crate::bundle::CalVariant::EarsHeq
            | crate::bundle::CalVariant::EarsHpn
            | crate::bundle::CalVariant::EarsIdf
    );

    let mut out = Vec::new();
    for warning in paraeq_dsp::compensation::validate_cal_with_threshold(&parsed, CAL_OUTLIER_DB) {
        let f = warning.freq_hz;
        match warning.kind {
            CalWarningKind::DuplicateFreq | CalWarningKind::NonMonotonicFreq => {
                let row = freqs.iter().position(|v| *v == f).unwrap_or(0);
                out.push(malformed(row));
            }
            CalWarningKind::Outlier {
                neighbours_db: (a, b),
                value_db,
            } => {
                if !skip_outliers {
                    out.push(outlier(f, value_db, a, b));
                }
            }
            // Exact zero by construction, which is why the variant carries no
            // value of its own.
            CalWarningKind::SuspectZero {
                neighbours_db: (a, b),
            } => {
                if !skip_outliers {
                    out.push(outlier(f, 0.0, a, b));
                }
            }
        }
    }
    out
}

/// "Coupler, EARS HEQ/HPN/IDF cal: force `flat.csv` + `Warn`."
///
/// The forcing itself is the `target` rule's (B7b); this is the row that tells
/// the user why their target choice was taken away. A Warn, not a Refuse: the
/// measurement is usable, it just already contains a target.
fn cal_has_target_baked_in(bundle: &MeasurementBundle) -> Option<Diagnostic> {
    let cal = bundle.cal.as_ref()?;
    match cal.variant {
        crate::bundle::CalVariant::EarsHeq
        | crate::bundle::CalVariant::EarsHpn
        | crate::bundle::CalVariant::EarsIdf => Some(Diagnostic {
            code: DiagnosticCode::CalHasTargetBakedIn,
            position: None,
            remedy: "Your EARS calibration already has a target baked into it. Applying another \
                     would apply it twice."
                .to_string(),
            severity: Severity::Warn,
            value: None,
        }),
        crate::bundle::CalVariant::Plain => None,
    }
}

// ---------------------------------------------------------------------------
// The override row
// ---------------------------------------------------------------------------

/// § D-N, verbatim: "**Clamp into the domain, set `source: UserOverride` with
/// the clamped value, and emit `DiagnosticCode::OverrideOutOfDomain` at
/// `Severity::Warn`**, naming the decision and both numbers."
///
/// **This is the DIAGNOSTIC half only.** The clamp lands in `rules::resolve`,
/// the one place an override arrives, and `rules.rs` is not this item's file —
/// its own doc comment already names itself as the landing site. Until it lands,
/// the override is taken as given and this row says so rather than leaving the
/// user with a value the drawer should never have offered.
///
/// **The two by-construction exceptions are checked like everything else now**
/// (ruling R-A4). `authority` and `target` used to be omitted from this list,
/// because their `Choice` lists carry a run-specific REPRESENTATIVE rather than
/// an enumeration of every legal value and a membership test answered `false`
/// for legitimate overrides:
///
/// * `authority` — `Custom(curve)` cannot be enumerated at all.
///   `AuthorityCurve` is sealed (only `build_authority` makes one, and
///   deserialization goes through a guarded `TryFrom`), so every representable
///   `Custom` has already been validated and is in-domain by construction.
/// * `target` — `Parametric { .. }` is a four-number shape, so moving the tilt
///   inside `targets`' own `-1.5..=0.0` reads as illegal against a list.
///
/// The exception now lives in [`crate::decision::InRange::legal_by_construction`],
/// which `Domain::contains` consults first — one named, tested place instead of
/// two omissions a reader has to notice. So a `Custom` ceiling and a re-tilted
/// house curve pass silently, while an `authority` or `target` override that
/// really is outside the domain (a `Curve` naming a target the bundle does not
/// carry) finally earns its row instead of passing unremarked.
fn override_out_of_domain(bundle: &MeasurementBundle, decisions: &Decisions) -> Vec<Diagnostic> {
    let over = &bundle.overrides;
    let mut out = Vec::new();
    row(
        &mut out,
        "align_spl_band",
        over.align_spl_band.as_ref(),
        &decisions.align_spl_band,
        None,
    );
    row(
        &mut out,
        "authority",
        over.authority.as_ref(),
        &decisions.authority,
        None,
    );
    row(
        &mut out,
        "averaging",
        over.averaging.as_ref(),
        &decisions.averaging,
        None,
    );
    row(
        &mut out,
        "class",
        over.class.as_ref(),
        &decisions.class,
        None,
    );
    row(
        &mut out,
        "clock_adjust",
        over.clock_adjust.as_ref(),
        &decisions.clock_adjust,
        None,
    );
    row(
        &mut out,
        "correction_kind",
        over.correction_kind.as_ref(),
        &decisions.correction_kind,
        None,
    );
    row(
        &mut out,
        "correction_range",
        over.correction_range.as_ref(),
        &decisions.correction_range,
        None,
    );
    row(
        &mut out,
        "fdw_post_cycles",
        over.fdw_post_cycles.as_ref(),
        &decisions.fdw_post_cycles,
        Some(decisions.fdw_post_cycles.value),
    );
    row(
        &mut out,
        "fdw_pre_cycles",
        over.fdw_pre_cycles.as_ref(),
        &decisions.fdw_pre_cycles,
        Some(decisions.fdw_pre_cycles.value),
    );
    row(
        &mut out,
        "flatness_target_db",
        over.flatness_target_db.as_ref(),
        &decisions.flatness_target_db,
        Some(decisions.flatness_target_db.value),
    );
    row(
        &mut out,
        "left_window_ms",
        over.left_window_ms.as_ref(),
        &decisions.left_window_ms,
        Some(decisions.left_window_ms.value),
    );
    row(
        &mut out,
        "low_corner_hz",
        over.low_corner_hz.as_ref(),
        &decisions.low_corner_hz,
        Some(decisions.low_corner_hz.value),
    );
    row(
        &mut out,
        "max_filters",
        over.max_filters.as_ref(),
        &decisions.max_filters,
        Some(decisions.max_filters.value as f64),
    );
    row(
        &mut out,
        "positions_n",
        over.positions_n.as_ref(),
        &decisions.positions_n,
        Some(decisions.positions_n.value as f64),
    );
    row(
        &mut out,
        "preamp_db",
        over.preamp_db.as_ref(),
        &decisions.preamp_db,
        Some(decisions.preamp_db.value),
    );
    row(
        &mut out,
        "q_cap",
        over.q_cap.as_ref(),
        &decisions.q_cap,
        None,
    );
    row(
        &mut out,
        "right_window_ms",
        over.right_window_ms.as_ref(),
        &decisions.right_window_ms,
        Some(decisions.right_window_ms.value),
    );
    row(
        &mut out,
        "shelves",
        over.shelves.as_ref(),
        &decisions.shelves,
        None,
    );
    row(
        &mut out,
        "smoothing",
        over.smoothing.as_ref(),
        &decisions.smoothing,
        None,
    );
    row(
        &mut out,
        "target",
        over.target.as_ref(),
        &decisions.target,
        None,
    );
    row(
        &mut out,
        "transition_hz",
        over.transition_hz.as_ref(),
        &decisions.transition_hz,
        Some(decisions.transition_hz.value),
    );
    row(
        &mut out,
        "window_type",
        over.window_type.as_ref(),
        &decisions.window_type,
        None,
    );
    out
}

/// One decision's out-of-domain check. `value` is the CLAMPED number where the
/// decision has one — the number that was actually used — so the drawer's
/// margin display means something; a band or an enum has none, and inventing a
/// stand-in would put a number there that means nothing.
///
/// **Ruling R-A6 moved both the number and the sentence.** Until § D-N's clamp
/// landed in `rules::resolve` the override WAS taken as given, `value` carried
/// the requested number and the remedy said "We used it as asked" — all three
/// consistent, and all three describing behaviour the ruling forbids. The
/// remedy now names what was asked for AND what was used, which are two
/// different numbers exactly when this row fires.
fn row<T>(
    out: &mut Vec<Diagnostic>,
    id: &str,
    requested: Option<&T>,
    decided: &Decision<T>,
    value: Option<f64>,
) where
    T: InRange + PartialEq + Serialize,
{
    let Some(requested) = requested else { return };
    if decided.domain.contains(requested) {
        return;
    }
    let asked = render(requested);
    let legal = describe(&decided.domain);
    let used = render(&decided.value);
    out.push(Diagnostic {
        code: DiagnosticCode::OverrideOutOfDomain,
        position: None,
        remedy: format!(
            "The Advanced drawer set {id} to {asked}, which is outside what this measurement \
             supports ({legal}). We used {used} instead — the closest value this measurement \
             supports."
        ),
        severity: Severity::Warn,
        value,
    });
}

/// A decision value as the shortest honest string. JSON rather than `Debug`
/// because the drawer writes overrides as JSON and the user is looking at that
/// same spelling.
fn render<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "an unrenderable value".to_string())
}

/// A domain in words. `Choice` lists a count rather than 957-point curves.
fn describe<T: Serialize>(domain: &Domain<T>) -> String {
    match domain {
        Domain::Choice(alternatives) => {
            format!("one of the {} values this path offers", alternatives.len())
        }
        // Unreachable: `Domain::Derived` contains everything, so `row` returns
        // before it can ask. Spelled out rather than wildcarded so that a fourth
        // domain shape has to come past this function.
        Domain::Derived => "derived from the measurement".to_string(),
        Domain::Range { max, min, .. } => format!("{} to {}", render(min), render(max)),
    }
}

// ---------------------------------------------------------------------------
// Curve arithmetic. Local because each is three lines and none of them is DSP:
// `paraeq-dsp` owns transforms, not the reading of a curve someone else made.
// ---------------------------------------------------------------------------

/// The indices of `freqs` inside `[lo, hi]`, both ends inclusive. Empty when the
/// band does not intersect the grid, which every caller treats as "no evidence"
/// rather than as "no problem".
fn band(freqs: &[f64], lo: f64, hi: f64) -> Vec<usize> {
    freqs
        .iter()
        .enumerate()
        .filter(|(_, f)| **f >= lo && **f <= hi)
        .map(|(i, _)| i)
        .collect()
}

/// `max − min` over the band.
fn span_db(freqs: &[f64], curve: &[f64], lo: f64, hi: f64) -> Option<f64> {
    let indices = band(freqs, lo, hi);
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for i in indices {
        let v = curve[i];
        if !v.is_finite() {
            continue;
        }
        low = low.min(v);
        high = high.max(v);
    }
    (low.is_finite() && high.is_finite()).then_some(high - low)
}

/// Least-squares slope of dB against `log10(f)` over the band — dB per decade,
/// signed. A least-squares fit rather than the two endpoints, because two bins
/// of a real curve are two samples of its ripple and not its trend.
fn tilt_db_per_decade(freqs: &[f64], curve: &[f64], lo: f64, hi: f64) -> Option<f64> {
    let indices = band(freqs, lo, hi);
    let points: Vec<(f64, f64)> = indices
        .iter()
        .map(|i| (freqs[*i].log10(), curve[*i]))
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .collect();
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / n;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / n;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for (x, y) in &points {
        sxx += (x - mean_x) * (x - mean_x);
        sxy += (x - mean_x) * (y - mean_y);
    }
    (sxx > 0.0).then(|| sxy / sxx)
}

/// Mean |deviation| of `index`'s curve from the mean of the OTHERS over the
/// band. See [`position_outliers`] for why the cohort excludes the position
/// under test.
fn cohort_deviation_db(curves: &[Vec<f64>], index: usize, indices: &[usize]) -> Option<f64> {
    if curves.len() < 2 || indices.is_empty() {
        return None;
    }
    let others = (curves.len() - 1) as f64;
    let mut total = 0.0;
    let mut counted = 0usize;
    for &i in indices {
        let cohort: f64 = curves
            .iter()
            .enumerate()
            .filter(|(p, _)| *p != index)
            .map(|(_, c)| c[i])
            .sum::<f64>()
            / others;
        let deviation = curves[index][i] - cohort;
        if !deviation.is_finite() {
            continue;
        }
        total += deviation.abs();
        counted += 1;
    }
    (counted > 0).then(|| total / counted as f64)
}

/// The middle value, or the mean of the two middle values. Sorts in place;
/// non-finite values are the caller's to filter, because "half the curve is NaN"
/// is a different diagnosis from "the spread is large".
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        0.5 * (values[mid - 1] + values[mid])
    } else {
        values[mid]
    })
}
