//! `t = 0` recovery for a bracketed capture: locate the timing markers, fit
//! the clock skew, resample the capture onto the playback clock, slice at the
//! recovered origin, and deconvolve.
//!
//! ONE function, used by BOTH captures. The Direct baseline and the Helper
//! verification capture differ in exactly one argument — the matched-filter
//! template — and that difference is visible at the call site by construction.
//! Two captures aligned by different means cannot be subtracted, and the
//! verification residual is a subtraction.
//!
//! # Why a resample and not an integer slice
//!
//! The microphone and the output device run on independent crystals. The
//! decision record's §Q6 measures ~12 ppm between them, which over a 7.7 s
//! file at 48 kHz is 4.4 samples of drift end to end — exactly the regime
//! where rounding `t = 0` to an integer sample is wrong, and exactly why §Q6's
//! adopted resolution is "compute the clock-rate difference, and **resample**
//! the capture to correct it — default on".
//!
//! # The index convention, which is the one place two clocks meet
//!
//! `SkewEstimate::intercept_samples` is measured on the **capture** clock.
//! [`paraeq_dsp::resample::resample_ratio`]'s output index is an **output**
//! clock index. Converting is not optional and it is not `floor(t0)`:
//!
//! ```text
//! r    = 1 / ratio                      // ratio = 1 + skew_ppm·1e-6
//! j0   = floor(t0 · r) = floor(t0 / ratio)
//! phi  = frac (t0 · r) = frac (t0 / ratio)
//! ```
//!
//! `the_t0_conversion_is_output_clock_not_input_clock` is its falsifier: at a
//! large enough skew the two conventions differ by whole samples, and a wrong
//! `t = 0` produces a plausible wrong correction rather than a visible
//! failure.
//!
//! # The credibility ladder
//!
//! Five rungs, refusing outward-in. Nothing here is a "best effort": an
//! untrustworthy `t = 0` is worse than no measurement, because every gate
//! downstream cuts the impulse response at that instant and none of them can
//! tell that it moved.
//!
//! | rung | condition | outcome |
//! |---|---|---|
//! | 0 | `locate` returned `None` | Refuse [`MeasurementDiagnostic::VerificationMarkersNotCredible`] |
//! | 1 | `\|skew_ppm\| >` [`MAX_CLOCK_ADJUST_PPM`] | Refuse [`MeasurementDiagnostic::ClockAdjustTooLarge`] |
//! | 2 | residual peak ≤ [`TWO_CLOCK_RESIDUAL_WARN_SAMPLES`] | silent |
//! | 3 | residual peak ≤ [`TWO_CLOCK_RESIDUAL_REFUSE_SAMPLES`] | Warn [`MeasurementDiagnostic::TwoClockResidualHigh`] |
//! | 4 | beyond that | Refuse [`MeasurementDiagnostic::VerificationMarkersNotFound`] |
//!
//! Rung 0 gets its own code rather than the generic not-found because it is
//! **the first thing that fails when the verification sweep is quiet**, and
//! "the sweep was too quiet to find the markers" and "the markers scattered"
//! have different remedies.
//!
//! Test tier: **Tier 3 (analytic)**. The invariant is a synthetic round trip —
//! a capture built with a KNOWN `t0` and a KNOWN ppm must come back as that
//! `t0` and that ppm — with no library delegate for arbitrary-ratio
//! fractional-phase alignment. A numpy transcription of our own layout and our
//! own kernel would launder our algebra into a fixture.

use crate::diagnostic::MeasurementDiagnostic;
use paraeq_dsp::deconvolution::deconvolve_ir;
use paraeq_dsp::gating::ImpulseResponse;
use paraeq_dsp::resample::resample_ratio;
use paraeq_dsp::two_clock::{self, MarkerLayout, SkewEstimate, MAX_CLOCK_ADJUST_PPM};

/// Below this per-marker residual peak (capture samples) the fit is clean and
/// nothing is said.
///
/// **`[NEEDS DATA]`.** A starting value, cut to the same shape as the other
/// `[NEEDS DATA]` constants in this crate: a matched-filter residual under two
/// samples is inside the parabolic refinement's own resolution, so there is
/// nothing to report. Retune from the first hardware runs, where the Direct
/// path's known `t = 0` gives the residual distribution this should be cut
/// against.
pub const TWO_CLOCK_RESIDUAL_WARN_SAMPLES: f64 = 2.0;

/// Past this per-marker residual peak (capture samples) the fit is refused.
///
/// **`[NEEDS DATA]`.** Twenty samples is 0.4 ms at 48 kHz — an order of
/// magnitude past the warn rung and still well inside one marker's main lobe,
/// so a fit this scattered is not a slightly worse answer, it is a different
/// answer on every marker.
pub const TWO_CLOCK_RESIDUAL_REFUSE_SAMPLES: f64 = 20.0;

/// Below this marker-to-pre-roll-floor margin (dB) the fit is warned about.
///
/// **`[NEEDS DATA]`.** The matched filter's own credibility floor is six times
/// the off-peak correlation RMS (≈15.6 dB), so a marker margin near that
/// number is one quieter run away from rung 0. Twenty dB gives a little more
/// than the floor itself; retune once a real rig says how the margin falls
/// with `L_verify`.
pub const MARKER_SNR_WARN_DB: f64 = 20.0;

/// One aligned capture. Everything a caller needs to attach evidence and to
/// build a bundle, and nothing it would have to re-derive.
#[derive(Clone, Debug)]
pub struct Alignment {
    /// The least-squares marker fit. Evidence only — the ladder above gates
    /// the fit's ADMISSIBILITY, never the residual figure itself.
    pub fit: SkewEstimate,
    /// The deconvolved impulse response, time axis intact, cut at the
    /// recovered origin.
    pub ir: ImpulseResponse,
    /// `1 + skew_ppm·1e-6`. The capture was resampled by its reciprocal.
    pub ratio: f64,
    /// The recovered transport offset on the CAPTURE clock, in capture
    /// samples — the number the slice index was derived from, carried so a
    /// reader never has to guess which clock it is on.
    pub t0_capture_samples: f64,
    /// Non-blocking diagnostics raised along the way.
    pub warnings: Vec<MeasurementDiagnostic>,
}

/// Everything [`align`] needs. A record rather than six positional arguments
/// because two of them are `&[f64]` buffers that would otherwise be one
/// transposition away from each other.
pub struct AlignRequest<'a> {
    /// The recorded buffer, on the capture clock.
    pub capture: &'a [f64],
    /// The bracket the file was assembled with. The SAME value assembly used —
    /// never a second layout, or the expected marker positions describe a
    /// different file.
    pub layout: &'a MarkerLayout,
    pub sample_rate_hz: u32,
    /// The reference sweep to deconvolve against: the levelled sweep, not the
    /// bracketed file.
    pub sweep: &'a [f64],
    /// The sweep's length in samples, for the expected marker positions.
    pub sweep_len: usize,
    /// The waveform to matched-filter against.
    ///
    /// **Baseline:** [`paraeq_dsp::two_clock::layout_marker`], raw.
    /// **Verification:** that marker filtered through the installed correction
    /// at the live rate and scaled by the armed preamp. The verification
    /// marker traverses the cascade on its way to the mic, so correlating a
    /// raw template against a chain-shaped marker biases the peak by the
    /// cascade's group delay in the marker's band — common-mode across both
    /// ends of the bracket, so it lands entirely in the intercept, which is
    /// exactly the number used as `t = 0`, and is invisible to a
    /// residual-scatter gate.
    pub template: &'a [f64],
}

/// Locate → fit → ladder → resample → slice → deconvolve.
///
/// # Errors
///
/// A blocking [`MeasurementDiagnostic`]: one of the credibility ladder's
/// refusing rungs, or a capture that carries no direct arrival at all.
pub fn align(request: AlignRequest<'_>) -> Result<Alignment, MeasurementDiagnostic> {
    let AlignRequest {
        capture,
        layout,
        sample_rate_hz,
        sweep,
        sweep_len,
        template,
    } = request;
    let rate = f64::from(sample_rate_hz);

    // Rung 0. `locate` returns `None` when a picked peak fails the matched
    // filter's own credibility floor — the first thing that fails when the
    // verification sweep is quiet, which is why it has its own code.
    let hits = two_clock::locate(capture, template, layout, rate)
        .ok_or(MeasurementDiagnostic::VerificationMarkersNotCredible)?;
    let expected = two_clock::expected_marker_positions(layout, sweep_len, rate);
    // `estimate_skew` refuses fewer than two markers, mismatched lengths and a
    // degenerate expected set. All three mean the file and the analysis
    // disagree about what was assembled, which is not a scattered fit.
    let fit = two_clock::estimate_skew(&expected, &hits)
        .ok_or(MeasurementDiagnostic::VerificationMarkersNotCredible)?;

    // Rung 1. Written as a positive requirement so a NaN ppm — which passes no
    // comparison — lands in the refusal arm rather than being resampled by.
    let ppm_ok = fit.skew_ppm.is_finite() && fit.skew_ppm.abs() <= MAX_CLOCK_ADJUST_PPM;
    if !ppm_ok {
        return Err(MeasurementDiagnostic::ClockAdjustTooLarge {
            skew_ppm: fit.skew_ppm,
        });
    }

    // Rungs 2–4, same positive-requirement shape.
    let residual_ok = fit.residual_peak_samples.is_finite()
        && fit.residual_peak_samples <= TWO_CLOCK_RESIDUAL_REFUSE_SAMPLES;
    if !residual_ok {
        return Err(MeasurementDiagnostic::VerificationMarkersNotFound {
            residual_peak_samples: fit.residual_peak_samples,
        });
    }
    let mut warnings = Vec::new();
    if fit.residual_peak_samples > TWO_CLOCK_RESIDUAL_WARN_SAMPLES {
        warnings.push(MeasurementDiagnostic::TwoClockResidualHigh {
            residual_peak_samples: fit.residual_peak_samples,
        });
    }
    if let Some(margin_db) = marker_margin_db(capture, &hits, layout, rate) {
        if margin_db < MARKER_SNR_WARN_DB {
            warnings.push(MeasurementDiagnostic::VerificationMarkerSnrLow { margin_db });
        }
    }

    // The two-clock correction. `ratio > 1` means the CAPTURE clock ran fast
    // (more capture samples elapsed between markers than playback samples), so
    // the capture is compressed back onto the playback clock by its
    // reciprocal.
    let ratio = 1.0 + fit.skew_ppm * 1e-6;
    let inverse = 1.0 / ratio;
    let t0_capture_samples = fit.intercept_samples;
    let t0_output = t0_capture_samples * inverse;
    // A negative or unrepresentable origin means the markers were located
    // before the capture's own first sample, which the lead-in exists to make
    // impossible. Refuse rather than slice from somewhere invented.
    let origin_ok = ratio.is_finite() && ratio > 0.0 && t0_output.is_finite() && t0_output >= 0.0;
    if !origin_ok {
        return Err(MeasurementDiagnostic::VerificationMarkersNotCredible);
    }
    let j0 = t0_output.floor();
    let phase = t0_output - j0;
    let resampled = resample_ratio(capture, inverse, phase);
    let j0 = j0 as usize;
    if j0 >= resampled.len() {
        // The recovered origin sits past the end of the capture: a fit formed,
        // but it describes a file this recording does not contain.
        return Err(MeasurementDiagnostic::VerificationMarkersNotFound {
            residual_peak_samples: fit.residual_peak_samples,
        });
    }

    let ir = deconvolve_ir(&resampled[j0..], sweep, sample_rate_hz)
        // The only reachable failure here is "no direct arrival": the slice is
        // all-zero or non-finite, i.e. nothing usable was captured. That is
        // the same user-visible cause as rung 0 and carries the same remedy.
        .map_err(|_| MeasurementDiagnostic::VerificationMarkersNotCredible)?;

    Ok(Alignment {
        fit,
        ir,
        ratio,
        t0_capture_samples,
        warnings,
    })
}

/// How far the located markers sit above the capture's own pre-roll floor, in
/// dB, or `None` when there is not enough pre-roll to estimate a floor from.
///
/// The pre-roll is the file's lead-in: silence by construction, so whatever is
/// there is room and system noise. Measuring the margin against it answers the
/// question E13 asks — at the quietest `L_verify` a large boost produces, are
/// the markers still comfortably findable? — one run before the answer becomes
/// "no" and rung 0 refuses.
fn marker_margin_db(
    capture: &[f64],
    hits: &[f64],
    layout: &MarkerLayout,
    sample_rate_hz: f64,
) -> Option<f64> {
    /// Below this many pre-roll samples there is not enough to estimate a
    /// floor from, and a floor estimated from a handful of samples would warn
    /// or stay silent at random.
    const MIN_FLOOR_SAMPLES: usize = 64;

    let first = hits.first()?;
    if !first.is_finite() || *first < 0.0 {
        return None;
    }
    let floor_end = (*first as usize).min(capture.len());
    if floor_end < MIN_FLOOR_SAMPLES {
        return None;
    }
    let floor = &capture[..floor_end];
    let floor_rms = (floor.iter().map(|v| v * v).sum::<f64>() / floor.len() as f64).sqrt();
    // Positive requirement, so a NaN floor skips the check rather than
    // producing a NaN margin that compares false against every threshold.
    let floor_is_real = floor_rms > 0.0;
    if !floor_is_real {
        // Digital silence before the first marker: the margin is unbounded, so
        // there is nothing to warn about.
        return None;
    }

    let marker_frames = layout.marker_frames(sample_rate_hz).max(1);
    let mut peak = 0.0f64;
    for hit in hits {
        if !hit.is_finite() || *hit < 0.0 {
            continue;
        }
        let start = (*hit as usize).min(capture.len());
        let end = (start + marker_frames).min(capture.len());
        for v in &capture[start..end] {
            peak = peak.max(v.abs());
        }
    }
    let peak_is_real = peak > 0.0;
    if !peak_is_real {
        return None;
    }
    Some(20.0 * (peak / floor_rms).log10())
}
