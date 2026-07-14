//! Pure EQ correctness layer for the command surface: band/preamp validation,
//! SOS design, the rate-change re-send decision, and the plot response grid.
//!
//! This is the enforcement wall that keeps NaN/Inf coefficients out of the
//! realtime chain. `paraeq-dsp` and `paraeq-engine` validate nothing about the
//! *values* of a band -- `validate_correction` in the controller only rejects
//! structurally empty configs, so a degenerate band (`q = 0`, `fc >= Nyquist`)
//! would design NaN/Inf biquad coefficients and hand them straight to the tap.
//! Every command that accepts bands (Task 7) calls [`validate_bands`] first,
//! and the forwarder's rate-resend path re-validates at the NEW stream rate --
//! a band that is legal at 96/48 kHz can be `>= Nyquist` at 44.1 kHz.
//!
//! No Tauri types live here: it is pure functions over dsp/engine types, fully
//! unit-tested.

use paraeq_dsp::peq::{EQBand, ParametricEQ};
use paraeq_engine::controller::{CorrectionConfig, EngineState};

/// Maximum absolute per-band gain, in dB. `|gain_db|` above this is rejected.
pub const GAIN_LIMIT_DB: f64 = 30.0;
/// Upper bound of the accepted preamp range, in dB.
pub const PREAMP_MAX_DB: f64 = 10.0;
/// Lower bound of the accepted preamp range, in dB.
pub const PREAMP_MIN_DB: f64 = -30.0;
/// Maximum band Q. Q above this (or `<= 0`) is rejected.
pub const Q_MAX: f64 = 100.0;

/// Reject a band set before ANY coefficient design. A band is invalid when:
/// its `fc` is not finite or lies outside the open interval
/// `(0, sample_rate / 2)` (both ends exclusive -- `fc = 0` and `fc = Nyquist`
/// are rejected); its `q` is not finite, `<= 0`, or `> Q_MAX`; or its
/// `gain_db` is not finite or `|gain_db| > GAIN_LIMIT_DB`. Error strings name
/// the offending band index and field -- they surface verbatim in the UI.
///
/// An empty band set is valid (`Ok(())`); the caller clears the correction.
pub fn validate_bands(bands: &[EQBand], sample_rate: f64) -> Result<(), String> {
    let nyquist = sample_rate / 2.0;
    for (i, band) in bands.iter().enumerate() {
        if !band.fc.is_finite() {
            return Err(format!("band {i}: fc must be a finite number"));
        }
        if band.fc <= 0.0 || band.fc >= nyquist {
            return Err(format!(
                "band {i}: fc {} Hz must be between 0 and Nyquist ({nyquist} Hz), exclusive",
                band.fc
            ));
        }
        if !band.q.is_finite() {
            return Err(format!("band {i}: q must be a finite number"));
        }
        if band.q <= 0.0 || band.q > Q_MAX {
            return Err(format!(
                "band {i}: q {} must be greater than 0 and at most {Q_MAX}",
                band.q
            ));
        }
        if !band.gain_db.is_finite() {
            return Err(format!("band {i}: gain_db must be a finite number"));
        }
        if band.gain_db.abs() > GAIN_LIMIT_DB {
            return Err(format!(
                "band {i}: gain_db {} must be within +/-{GAIN_LIMIT_DB} dB",
                band.gain_db
            ));
        }
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

/// Design the engine correction for a validated band set. Returns `None` for an
/// empty band set (the caller sends `ClearCorrection` -- flat passthrough).
///
/// Otherwise returns a single SOS set (`sos_per_channel` of length 1); the
/// engine broadcasts one entry across both stereo channels. Callers MUST have
/// run [`validate_bands`] at this same `sample_rate` first -- this function
/// designs coefficients unconditionally and does not itself guard against
/// `fc >= Nyquist` / `q = 0` producing NaN/Inf.
pub fn design_correction(bands: &[EQBand], sample_rate: f64) -> Option<CorrectionConfig> {
    if bands.is_empty() {
        return None;
    }
    let peq = ParametricEQ {
        bands: bands.to_vec(),
        sample_rate,
    };
    Some(CorrectionConfig::Iir {
        sos_per_channel: vec![peq.combined_sos()],
    })
}

/// The forwarder's redesign trigger, pure and unit-tested.
///
/// Returns `Some(new_rate)` iff the snapshot carries a live stream, there are
/// bands to apply, and the stream's sample rate differs from `last_rate`. The
/// first-ever stream (`last_rate == None`) also triggers -- it is how a
/// correction queued before the stream geometry was known (and how the
/// persisted, hand-editable `settings.json` bands) get validated and designed
/// at the real rate before reaching `to_sos`.
///
/// Returns `None` when there is no stream, no bands, or the rate is unchanged.
pub fn resend_decision(
    last_rate: Option<f64>,
    snapshot: &EngineState,
    have_bands: bool,
) -> Option<f64> {
    if !have_bands {
        return None;
    }
    let rate = snapshot.stream.as_ref()?.sample_rate;
    if last_rate == Some(rate) {
        None
    } else {
        Some(rate)
    }
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

    // ---- design_correction ----

    #[test]
    fn design_of_empty_bands_is_none() {
        assert!(design_correction(&[], 48_000.0).is_none());
    }

    #[test]
    fn design_matches_combined_sos_directly() {
        let bands = [peaking(1_000.0, 3.0, 1.0), peaking(4_000.0, -2.0, 2.0)];
        let cfg = design_correction(&bands, 48_000.0).expect("non-empty bands design a config");
        let expected = ParametricEQ {
            bands: bands.to_vec(),
            sample_rate: 48_000.0,
        }
        .combined_sos();
        match cfg {
            CorrectionConfig::Iir { sos_per_channel } => {
                assert_eq!(sos_per_channel.len(), 1, "single broadcast SOS set");
                assert_eq!(sos_per_channel[0], expected, "no drift from combined_sos");
            }
            CorrectionConfig::Fir { .. } => panic!("PEQ design must be Iir, not Fir"),
        }
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

    #[test]
    fn resend_changed_rate_triggers() {
        let snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        assert_eq!(resend_decision(Some(48_000.0), &snap, true), Some(44_100.0));
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
