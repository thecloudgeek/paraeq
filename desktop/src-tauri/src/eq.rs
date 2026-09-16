//! Pure EQ correctness layer for the command surface: band/preamp validation,
//! correction-config assembly, the redesign re-send decision, and the plot
//! response grid.
//!
//! **This is no longer the enforcement wall -- `paraeq-engine` is.** R1-6 moved
//! the invariant into the engine: `CorrectionConfig::Peq` carries design INTENT
//! (bands + a provenance `design_rate`) and `build_correction` re-derives
//! coefficients at the LIVE stream rate on every rebuild, dropping any band
//! that is at or above the new Nyquist and refusing (flat pass-through) only
//! when nothing survives. So a band can no longer reach `to_sos` at a rate it
//! was not checked against, whatever call site sends it -- a future `paraeqd`
//! or the auto front-end included. The owner-decided home of the band guard
//! itself is `paraeq_engine::controller::{validate_band, validate_band_at}`
//! (`docs/decisions/2026-07-22-owner-value-calls.md`).
//!
//! What stays here is USER-FACING feedback: [`validate_bands`] wraps the
//! engine's per-band guard to produce the indexed, human-readable strings the
//! UI shows, so an edit is rejected with an explanation instead of silently
//! losing a band inside the engine.
//!
//! No Tauri types live here: it is pure functions over dsp/engine types, fully
//! unit-tested.

use paraeq_dsp::peq::{parse_autoeq, EQBand, ParametricEQ};
use paraeq_engine::controller::{validate_band_at, CorrectionConfig, EngineState};

// The per-band limits (`GAIN_LIMIT_DB`, `Q_MAX`, `Q_MIN`) moved to
// `paraeq_engine::controller` -- one source of truth for every consumer, the
// daemon seam included. Nothing here reads them any more; the tests import
// them from the engine directly.

/// Upper bound of the accepted preamp range, in dB.
pub const PREAMP_MAX_DB: f64 = 10.0;
/// Lower bound of the accepted preamp range, in dB.
pub const PREAMP_MIN_DB: f64 = -30.0;

/// Reject a band set before it reaches the engine, with a message naming the
/// offending band index and field -- they surface verbatim in the UI. A band
/// is invalid when: its `fc` is not finite or lies outside the open interval
/// `(0, sample_rate / 2)` (both ends exclusive -- `fc = 0` and `fc = Nyquist`
/// are rejected); its `q` is not finite, `< Q_MIN`, or `> Q_MAX`; or its
/// `gain_db` is not finite or `|gain_db| > GAIN_LIMIT_DB`.
///
/// A thin wrapper over `paraeq_engine::controller::validate_band_at`: the
/// engine owns the rules, this owns the wording and the index. It is
/// user-facing feedback at the rate the stream happens to be running now, NOT
/// the safety guarantee -- the engine re-checks every band at the live rate at
/// install time, so a rate change between this call and the install cannot
/// slip a NaN coefficient through.
///
/// An empty band set is valid (`Ok(())`); the caller clears the correction.
pub fn validate_bands(bands: &[EQBand], sample_rate: f64) -> Result<(), String> {
    for (i, band) in bands.iter().enumerate() {
        validate_band_at(band, sample_rate).map_err(|reason| format!("band {i}: {reason}"))?;
    }
    Ok(())
}

/// Reject a preamp value that is not finite or outside
/// `[PREAMP_MIN_DB, PREAMP_MAX_DB]` (inclusive).
pub fn validate_preamp(db: f64) -> Result<(), String> {
    if !db.is_finite() {
        return Err("preamp must be a finite number".to_string());
    }
    if !(PREAMP_MIN_DB..=PREAMP_MAX_DB).contains(&db) {
        return Err(format!(
            "preamp {db} dB must be between {PREAMP_MIN_DB} and {PREAMP_MAX_DB} dB"
        ));
    }
    Ok(())
}

/// Assemble the engine correction for a band set. Returns `None` for an empty
/// band set (the caller sends `ClearCorrection` -- flat passthrough).
///
/// It no longer DESIGNS anything: under R1-6 it hands the engine the bands
/// themselves as `CorrectionConfig::Peq`, and the engine derives coefficients
/// at whatever rate the stream is actually running. `sample_rate` becomes the
/// config's `design_rate`, which is **provenance only** -- it records the rate
/// the user was looking at, and the engine never compares it. That is what
/// lets a correction survive an AirPods 44.1<->48 kHz handoff with its bands
/// still where the user put them.
///
/// Otherwise returns a single band set (`bands` of length 1); the engine
/// broadcasts one entry across both stereo channels.
pub fn design_correction(bands: &[EQBand], sample_rate: f64) -> Option<CorrectionConfig> {
    if bands.is_empty() {
        return None;
    }
    Some(CorrectionConfig::Peq {
        bands: vec![bands.to_vec()],
        design_rate: sample_rate,
    })
}

/// The forwarder's redesign trigger, pure and unit-tested -- now a FALLBACK,
/// not the rate-change mechanism.
///
/// R1-6 moved the rate handling into the engine, so a bare rate change no
/// longer needs anything from the desktop: the engine re-derives a `Peq`
/// correction at the new rate inside the very start that used to install
/// stale coefficients. What is left are the two cases the engine cannot
/// handle by itself:
///
/// 1. **The engine published a refusal** (`correction_rate_mismatch` is set).
///    The config it holds cannot be re-derived at the live rate, and the
///    desktop is the only layer still holding the design intent, so it
///    re-sends. This is the field the spec says to key off (`:424`).
/// 2. **The first-ever stream** (`last_rate == None`), which is how a
///    correction queued before the stream geometry was known -- and the
///    persisted, hand-editable `settings.json` bands -- get validated at a
///    real rate and stamped with a real `design_rate`.
///
/// `last_rate` has correspondingly narrowed to "have we seen a stream yet";
/// the rate it carries is only echoed back for the caller's bookkeeping.
///
/// Returns `None` when there is no stream, no bands, or neither case applies.
pub fn resend_decision(
    last_rate: Option<f64>,
    snapshot: &EngineState,
    have_bands: bool,
) -> Option<f64> {
    if !have_bands {
        return None;
    }
    let rate = snapshot.stream.as_ref()?.sample_rate;
    // Always redesign at the LIVE rate, whatever rate the flag names -- they
    // are equal by construction, and the stream is the authority.
    if snapshot.correction_rate_mismatch.is_some() || last_rate.is_none() {
        Some(rate)
    } else {
        None
    }
}

/// The outcome of a successful AutoEQ file import, returned to the UI so it can
/// report how many bands were applied and whether the file's preamp had to be
/// clamped into the accepted range (decision 2: a wild file preamp is clamped,
/// not rejected, so bands + preamp always apply together).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ImportResult {
    pub band_count: usize,
    pub preamp_clamped: bool,
    pub preamp_db: f64,
}

/// Parse AutoEQ preset text into the bands to apply plus an [`ImportResult`].
///
/// A file with no `ON` filter lines is an error carrying the prototype's exact
/// message. Otherwise the parsed preamp is CLAMPED into
/// `[PREAMP_MIN_DB, PREAMP_MAX_DB]` (so it always passes [`validate_preamp`] and
/// a wild file value never turns into a rejected-after-bands partial apply), and
/// `preamp_clamped` records whether the clamp changed the value. The bands are
/// NOT validated here -- the caller applies them through the validating apply
/// path at the live rate.
pub fn prepare_import(text: &str) -> Result<(Vec<EQBand>, ImportResult), String> {
    let parsed = parse_autoeq(text);
    if parsed.bands.is_empty() {
        return Err("No AutoEQ filter lines found in the file.".to_string());
    }
    let clamped = parsed.preamp_db.clamp(PREAMP_MIN_DB, PREAMP_MAX_DB);
    let result = ImportResult {
        band_count: parsed.bands.len(),
        preamp_clamped: clamped != parsed.preamp_db,
        preamp_db: clamped,
    };
    Ok((parsed.bands, result))
}

/// The magnitude response the plot draws: the composite curve over `freqs`
/// plus each band's own curve. `per_band[i]` is band `i` evaluated alone.
/// `composite` is all bands cascaded (its dB values equal the per-band dB sum
/// to within floating-point floor error). `sample_rate` is the rate the curves
/// were computed at, echoed back so the caller can label the plot.
///
/// Serialized as the `eq_response` command result (Task 7); `desktop/ui`
/// mirrors this shape by hand.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ResponseData {
    pub composite: Vec<f64>,
    pub per_band: Vec<Vec<f64>>,
    pub sample_rate: f64,
}

/// Evaluate [`ResponseData`] for `bands` over the frequency grid `freqs` at
/// `sample_rate`. The composite is one cascaded `frequency_response` over all
/// bands; each `per_band` entry is a one-band `ParametricEQ` (parity with the
/// prototype's per-band evaluation). Empty bands yield an all-zero composite
/// and an empty `per_band`.
pub fn response(bands: &[EQBand], freqs: &[f64], sample_rate: f64) -> ResponseData {
    let composite = ParametricEQ {
        bands: bands.to_vec(),
        sample_rate,
    }
    .frequency_response(freqs);
    let per_band = bands
        .iter()
        .map(|band| {
            ParametricEQ {
                bands: vec![band.clone()],
                sample_rate,
            }
            .frequency_response(freqs)
        })
        .collect();
    ResponseData {
        composite,
        per_band,
        sample_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paraeq_dsp::peq::FilterType;
    use paraeq_engine::backend::StreamInfo;
    use paraeq_engine::controller::{GAIN_LIMIT_DB, Q_MAX, Q_MIN};
    use paraeq_engine::status::EngineStatus;

    fn peaking(fc: f64, gain_db: f64, q: f64) -> EQBand {
        EQBand {
            filter_type: FilterType::Peaking,
            fc,
            gain_db,
            q,
        }
    }

    /// A `Stopped` engine snapshot carrying an optional stream, for the
    /// `resend_decision` cases. Only `stream` is load-bearing here.
    fn snapshot_with_stream(stream: Option<StreamInfo>) -> EngineState {
        EngineState {
            bypass: false,
            correction: None,
            correction_rate_mismatch: None,
            enabled: true,
            frame_mismatch_blocks: 0,
            gain_db: 0.0,
            input_peak: 0.0,
            latency_ms: None,
            status: EngineStatus::Stopped,
            stream,
        }
    }

    fn stream_at(sample_rate: f64) -> StreamInfo {
        StreamInfo {
            buffer_frames: 512,
            channels: 2,
            device_uid: "uid-1".into(),
            sample_rate,
        }
    }

    // ---- validate_bands: fc bounds ----

    #[test]
    fn fc_zero_is_rejected() {
        assert!(validate_bands(&[peaking(0.0, 3.0, 1.0)], 48_000.0).is_err());
    }

    #[test]
    fn fc_at_nyquist_is_rejected() {
        // fc == sample_rate / 2 is the exclusive upper bound.
        assert!(validate_bands(&[peaking(24_000.0, 3.0, 1.0)], 48_000.0).is_err());
    }

    #[test]
    fn fc_nan_is_rejected() {
        assert!(validate_bands(&[peaking(f64::NAN, 3.0, 1.0)], 48_000.0).is_err());
    }

    #[test]
    fn fc_infinite_is_rejected() {
        assert!(validate_bands(&[peaking(f64::INFINITY, 3.0, 1.0)], 48_000.0).is_err());
    }

    #[test]
    fn fc_in_band_is_accepted() {
        assert!(validate_bands(&[peaking(1_000.0, 3.0, 1.0)], 48_000.0).is_ok());
    }

    /// The reviewer-added rate-revalidation case, and the reason the forwarder
    /// re-checks bands at the NEW rate: fc = 23 kHz is legal at 96 kHz and
    /// 48 kHz (below Nyquist) but `>= Nyquist` at 44.1 kHz, where designing it
    /// would emit NaN/Inf coefficients. `validate_bands` MUST reject it at
    /// 44.1 kHz and accept it at 48/96 kHz.
    #[test]
    fn fc_23k_valid_at_96k_and_48k_but_rejected_at_44100() {
        let band = [peaking(23_000.0, 3.0, 1.0)];
        assert!(
            validate_bands(&band, 96_000.0).is_ok(),
            "23 kHz is below Nyquist at 96 kHz"
        );
        assert!(
            validate_bands(&band, 48_000.0).is_ok(),
            "23 kHz is below Nyquist at 48 kHz"
        );
        assert!(
            validate_bands(&band, 44_100.0).is_err(),
            "23 kHz is >= Nyquist (22050 Hz) at 44.1 kHz and must be rejected"
        );
    }

    // ---- validate_bands: q bounds ----

    #[test]
    fn q_zero_is_rejected() {
        assert!(validate_bands(&[peaking(1_000.0, 3.0, 0.0)], 48_000.0).is_err());
    }

    #[test]
    fn q_negative_is_rejected() {
        assert!(validate_bands(&[peaking(1_000.0, 3.0, -1.0)], 48_000.0).is_err());
    }

    #[test]
    fn q_above_max_is_rejected() {
        assert!(validate_bands(&[peaking(1_000.0, 3.0, 101.0)], 48_000.0).is_err());
    }

    #[test]
    fn q_at_max_is_accepted() {
        assert!(validate_bands(&[peaking(1_000.0, 3.0, Q_MAX)], 48_000.0).is_ok());
    }

    #[test]
    fn q_below_min_is_rejected() {
        // A finite, positive q just under Q_MIN is rejected before design.
        assert!(validate_bands(&[peaking(1_000.0, 3.0, Q_MIN - 0.01)], 48_000.0).is_err());
        // A subnormal q would overflow alpha = sin(w0)/(2*q) to +inf and design
        // NaN SOS coefficients -- exactly what this lower bound exists to stop.
        assert!(validate_bands(&[peaking(1_000.0, 3.0, 1e-310)], 48_000.0).is_err());
    }

    #[test]
    fn q_at_min_and_normal_small_q_are_accepted() {
        assert!(validate_bands(&[peaking(1_000.0, 3.0, Q_MIN)], 48_000.0).is_ok());
        // A realistically-low shelf/wide-band Q well above Q_MIN.
        assert!(validate_bands(&[peaking(1_000.0, 3.0, 0.7)], 48_000.0).is_ok());
    }

    #[test]
    fn q_nan_is_rejected() {
        assert!(validate_bands(&[peaking(1_000.0, 3.0, f64::NAN)], 48_000.0).is_err());
    }

    // ---- validate_bands: gain bounds ----

    #[test]
    fn gain_at_limit_is_accepted() {
        assert!(validate_bands(&[peaking(1_000.0, GAIN_LIMIT_DB, 1.0)], 48_000.0).is_ok());
        assert!(validate_bands(&[peaking(1_000.0, -GAIN_LIMIT_DB, 1.0)], 48_000.0).is_ok());
    }

    #[test]
    fn gain_over_limit_is_rejected() {
        assert!(validate_bands(&[peaking(1_000.0, 30.1, 1.0)], 48_000.0).is_err());
        assert!(validate_bands(&[peaking(1_000.0, -30.1, 1.0)], 48_000.0).is_err());
    }

    #[test]
    fn gain_nan_is_rejected() {
        assert!(validate_bands(&[peaking(1_000.0, f64::NAN, 1.0)], 48_000.0).is_err());
    }

    // ---- validate_bands: empty + index reporting ----

    #[test]
    fn empty_bands_validate_ok() {
        assert!(validate_bands(&[], 48_000.0).is_ok());
    }

    #[test]
    fn error_names_the_offending_band_index() {
        let bands = [peaking(1_000.0, 3.0, 1.0), peaking(1_000.0, 3.0, 0.0)];
        let err = validate_bands(&bands, 48_000.0).unwrap_err();
        assert!(err.contains("band 1"), "error was: {err}");
    }

    // ---- validate_preamp ----

    #[test]
    fn preamp_bounds() {
        assert!(validate_preamp(0.0).is_ok());
        assert!(validate_preamp(PREAMP_MIN_DB).is_ok());
        assert!(validate_preamp(PREAMP_MAX_DB).is_ok());
        assert!(validate_preamp(PREAMP_MIN_DB - 0.1).is_err());
        assert!(validate_preamp(PREAMP_MAX_DB + 0.1).is_err());
        assert!(validate_preamp(f64::NAN).is_err());
        assert!(validate_preamp(f64::INFINITY).is_err());
    }

    // ---- prepare_import ----

    #[test]
    fn import_zero_bands_is_error_with_prototype_message() {
        let err = prepare_import("Preamp: -3.0 dB\nnot a filter line\n").unwrap_err();
        assert_eq!(err, "No AutoEQ filter lines found in the file.");
    }

    #[test]
    fn import_clamps_out_of_range_preamp() {
        let text = "Preamp: -50.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 3.0 dB Q 1.000\n";
        let (bands, result) = prepare_import(text).unwrap();
        assert_eq!(bands.len(), 1);
        assert_eq!(result.band_count, 1);
        assert_eq!(result.preamp_db, PREAMP_MIN_DB);
        assert!(result.preamp_clamped);
    }

    #[test]
    fn import_clamps_high_out_of_range_preamp() {
        let text = "Preamp: 25.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 3.0 dB Q 1.000\n";
        let (_, result) = prepare_import(text).unwrap();
        assert_eq!(result.preamp_db, PREAMP_MAX_DB);
        assert!(result.preamp_clamped);
    }

    #[test]
    fn import_in_range_preamp_is_not_clamped() {
        let text = "Preamp: -5.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 3.0 dB Q 1.000\n";
        let (_, result) = prepare_import(text).unwrap();
        assert_eq!(result.preamp_db, -5.0);
        assert!(!result.preamp_clamped);
    }

    // ---- design_correction ----

    #[test]
    fn design_of_empty_bands_is_none() {
        assert!(design_correction(&[], 48_000.0).is_none());
    }

    /// Replaces `design_matches_combined_sos_directly`, whose
    /// `panic!("PEQ design must be Iir, not Fir")` inverted under R1-6: the
    /// desktop must now hand over INTENT, not coefficients, or the engine has
    /// nothing to re-derive from on a rate change.
    #[test]
    fn design_emits_peq_with_live_design_rate() {
        let bands = [peaking(1_000.0, 3.0, 1.0), peaking(4_000.0, -2.0, 2.0)];
        let cfg = design_correction(&bands, 44_100.0).expect("non-empty bands make a config");
        match cfg {
            CorrectionConfig::Peq {
                bands: sets,
                design_rate,
            } => {
                assert_eq!(sets.len(), 1, "single broadcast band set");
                assert_eq!(sets[0], bands.to_vec(), "bands pass through untouched");
                assert_eq!(design_rate, 44_100.0, "provenance is the live rate");
            }
            other => panic!("PEQ design must be Peq, got {other:?}"),
        }
    }

    /// ...and the bands it hands over still design to exactly the
    /// coefficients the old direct path produced, when the engine builds them
    /// at the same rate. Guards against a silent numeric change hiding inside
    /// the shape change.
    #[test]
    fn peq_config_designs_to_the_same_coefficients_as_before() {
        let bands = [peaking(1_000.0, 3.0, 1.0), peaking(4_000.0, -2.0, 2.0)];
        let expected = ParametricEQ {
            bands: bands.to_vec(),
            sample_rate: 48_000.0,
        }
        .combined_sos();
        let derived: Vec<[f64; 6]> = bands.iter().map(|b| b.to_sos(48_000.0)).collect();
        assert_eq!(derived, expected, "no drift from combined_sos");
    }

    // ---- resend_decision ----

    #[test]
    fn resend_first_stream_triggers() {
        let snap = snapshot_with_stream(Some(stream_at(48_000.0)));
        assert_eq!(resend_decision(None, &snap, true), Some(48_000.0));
    }

    #[test]
    fn resend_unchanged_rate_is_none() {
        let snap = snapshot_with_stream(Some(stream_at(48_000.0)));
        assert_eq!(resend_decision(Some(48_000.0), &snap, true), None);
    }

    /// THE ONE INVERTED ASSERTION (R1-6). This test was
    /// `resend_changed_rate_triggers`; a bare rate change no longer needs a
    /// desktop re-send, because the engine re-derived the correction at the
    /// new rate inside the same start. Re-pointed, not deleted -- the spec
    /// requires this suite to survive (`:434`).
    #[test]
    fn changed_rate_alone_no_longer_triggers() {
        let snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        assert_eq!(resend_decision(Some(48_000.0), &snap, true), None);
    }

    /// ...and this is what replaces it: the engine's published refusal is the
    /// trigger now (spec `:424`).
    #[test]
    fn mismatch_flag_triggers() {
        let mut snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        snap.correction_rate_mismatch = Some(44_100.0);
        assert_eq!(resend_decision(Some(44_100.0), &snap, true), Some(44_100.0));
    }

    /// A refusal with no bands to redesign from is still nothing to do.
    #[test]
    fn mismatch_flag_without_bands_is_none() {
        let mut snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        snap.correction_rate_mismatch = Some(44_100.0);
        assert_eq!(resend_decision(Some(44_100.0), &snap, false), None);
    }

    #[test]
    fn resend_no_bands_is_none() {
        let snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        assert_eq!(resend_decision(Some(48_000.0), &snap, false), None);
    }

    #[test]
    fn resend_no_stream_is_none() {
        let snap = snapshot_with_stream(None);
        assert_eq!(resend_decision(Some(48_000.0), &snap, true), None);
    }

    // ---- response ----

    #[test]
    fn response_of_empty_bands_is_all_zeros() {
        let freqs = [20.0, 200.0, 2_000.0, 20_000.0];
        let data = response(&[], &freqs, 48_000.0);
        assert_eq!(data.composite, vec![0.0; freqs.len()]);
        assert!(data.per_band.is_empty());
        assert_eq!(data.sample_rate, 48_000.0);
    }

    #[test]
    fn response_composite_approximates_per_band_sum() {
        // Two boosting peaking bands: |H| >= 1 everywhere, so the +1e-10
        // magnitude floor is negligible and the cascaded composite dB equals
        // the per-band dB sum to well within 1e-9 (same-math sanity, mirroring
        // the prototype's per-band-sum composite path).
        let bands = [peaking(500.0, 4.0, 1.2), peaking(5_000.0, 3.0, 0.8)];
        let freqs = [50.0, 200.0, 500.0, 1_000.0, 5_000.0, 12_000.0];
        let data = response(&bands, &freqs, 48_000.0);
        assert_eq!(data.per_band.len(), 2);
        for (k, &c) in data.composite.iter().enumerate() {
            let sum: f64 = data.per_band.iter().map(|pb| pb[k]).sum();
            assert!(
                (c - sum).abs() < 1e-9,
                "composite {c} != per-band sum {sum} at freq index {k}"
            );
        }
    }
}
