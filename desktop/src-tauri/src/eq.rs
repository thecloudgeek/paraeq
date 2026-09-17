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
use paraeq_engine::controller::{validate_band_at, CorrectionConfig, EngineCommand, EngineState};

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

/// The forwarder's carried re-send bookkeeping. Two questions, both of which
/// need memory across snapshots:
///
/// * `last_rate` -- "have we seen a stream yet" (see [`resend_decision`]).
/// * `resent_for` -- the refused rate the forwarder has ALREADY supplied
///   design intent for. `EngineState::correction_rate_mismatch` latches until
///   a rebuild succeeds, so without this the forwarder would answer the same
///   standing refusal on every published snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ResendState {
    pub last_rate: Option<f64>,
    pub resent_for: Option<f64>,
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
///    re-sends. This is the field the spec says to key off (`R1-6 § Fix 4`).
/// 2. **The first-ever stream** (`last_rate == None`), which is how a
///    correction queued before the stream geometry was known -- and the
///    persisted, hand-editable `settings.json` bands -- get validated at a
///    real rate and stamped with a real `design_rate`.
///
/// `last_rate` has correspondingly narrowed to "have we seen a stream yet";
/// the rate it carries is only echoed back for the caller's bookkeeping.
///
/// **This is a LEVEL predicate on a LATCHING field, not an edge.**
/// `correction_rate_mismatch` is re-set by `send_correction` on every refused
/// rebuild, so it is still `Some(..)` on the snapshot that follows our own
/// re-send -- and the re-send cannot clear it, because the same bands at the
/// same rate are refused identically. Deciding whether the forwarder has
/// ALREADY answered this refusal is [`resend_command`]'s job, through
/// [`ResendState`]; call that, not this, from a loop over snapshots.
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

/// The forwarder's whole correction reconcile, pure so it can be tested
/// without a Tauri app handle: given its carried [`ResendState`], the engine
/// snapshot and the desktop's band set, return the command to send -- and
/// advance the state.
///
/// **It fires on the EDGE of a refusal, not its level**, which is the whole
/// reason the state is carried. [`resend_decision`] answers "the engine is
/// asking for design intent at this rate", and it keeps saying so: the flag
/// latches (`send_correction` re-sets it on every refused rebuild) and a
/// re-send of the same bands at the same rate is refused identically, because
/// `build_correction`'s Peq arm errors on `kept == 0 && bands_dropped > 0`, a
/// pure function of (bands, stream rate). Nothing in that cycle can end it.
/// Answering on the level therefore redesigns, refuses and logs once per
/// PUBLISHED SNAPSHOT -- and since R1-8's `input_peak` decays every block,
/// `publish` fires at the full tick rate (4 Hz by default) for as long as
/// audio plays. Measured: ~4.5 re-sends and ~4.5 `correction refused` warn
/// lines per second, indefinitely, for a state the app has already disclosed
/// as "EQ paused".
///
/// One re-send per refused rate is enough, and the mismatch arm is kept
/// rather than dropped: the engine self-heals a `Peq` by itself at the next
/// legal rate (`test_rate_independence.rs::a_refused_peq_installs_itself_at_the_next_legal_rate`),
/// but a baked `Fir`/`Iir` it cannot re-derive has no other repair, and that
/// is the fallback R1-6 § Fix 4 and D-12 both say to keep.
///
/// **It does not gate the band set, and it can never clear one.** The
/// forwarder used to re-validate every band at the live rate here and send a
/// whole-set [`EngineCommand::ClearCorrection`] if ANY single band failed.
/// That is the behaviour D-10 ruled against
/// (`docs/decisions/2026-09-16-post-merge-and-stage6-calls.md`, and
/// `crates/paraeq-dsp/DIVERGENCES.md` #18): `build_correction` drops only the
/// bands that are illegal at the live rate, counts them, and refuses the
/// whole configuration only when nothing survives. Handing the set over
/// unconditionally is what lets the engine apply that rule.
///
/// It also repairs a worse consequence of the clear. `ClearCorrection` sets
/// the engine's retained `correction` to `None`, and the publish that
/// follows therefore reports `correction_rate_mismatch: None` — so BOTH of
/// [`resend_decision`]'s triggers went permanently false (the flag was
/// cleared, and `last_rate` was already `Some(..)`). An AirPods 48 → 44.1 →
/// 48 kHz round trip lost the user's EQ for the rest of the process, with
/// the UI still drawing the bands the engine no longer held. Nothing
/// re-sends on a stream or device change; only a band edit or a profile
/// switch does. Leaving the intent in the engine makes the round trip
/// self-healing, which is pinned in `paraeq-engine` by
/// `test_rate_independence.rs::a_refused_peq_installs_itself_at_the_next_legal_rate`.
///
/// [`validate_bands`] keeps its job — the indexed, user-facing message on an
/// EDIT — at the two command call sites. It was never the safety wall here.
pub fn resend_command(
    state: &mut ResendState,
    snapshot: &EngineState,
    bands: &[EQBand],
) -> Option<EngineCommand> {
    // The engine is no longer naming a rate: forget what we last supplied, so
    // the NEXT refusal is a fresh edge. Deliberately before the `?` below --
    // it must run on every snapshot, including ones with no bands and no
    // stream, or a refusal that clears while the user has no EQ loaded would
    // leave `resent_for` armed against the refusal after it.
    let refused = snapshot.correction_rate_mismatch.is_some();
    if !refused {
        state.resent_for = None;
    }

    let rate = resend_decision(state.last_rate, snapshot, !bands.is_empty())?;
    if refused && state.resent_for == Some(rate) {
        return None;
    }

    let config = design_correction(bands, rate)?;
    state.last_rate = Some(rate);
    state.resent_for = if refused { Some(rate) } else { None };
    Some(EngineCommand::SetCorrection(config))
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
            auto_preamp_db: None,
            bypass: false,
            clipped_samples: 0,
            correction: None,
            correction_rate_mismatch: None,
            enabled: true,
            frame_mismatch_blocks: 0,
            gain_db: 0.0,
            input_peak: 0.0,
            input_peak_session: 0.0,
            invalid_samples: 0,
            latency_ms: None,
            output_peak: 0.0,
            self_excluded: false,
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

    /// The rate-DEPENDENT half of the relocated band guard (D-13): fc = 23 kHz
    /// is legal at 96 kHz and 48 kHz (below Nyquist) but `>= Nyquist` at
    /// 44.1 kHz, where designing it would emit NaN/Inf coefficients. This pins
    /// `paraeq_engine::controller::validate_band_at`'s exclusive Nyquist bound
    /// at both sides of 22.05 kHz, and with it the edit-time message the user
    /// actually reads.
    ///
    /// It is NOT about the forwarder. The forwarder used to re-check bands at
    /// the new rate and clear the whole set on failure; D-10 ruled against
    /// that and 8d27016 removed it (see [`resend_command`], and
    /// `resend_command_hands_over_a_band_illegal_at_the_live_rate` below,
    /// which drives this same 23 kHz pivot to assert the opposite). What
    /// happens to this band at install time is that `build_correction` DROPS
    /// and COUNTS it -- pinned in the engine by
    /// `test_rate_independence.rs::peq_band_at_or_above_new_nyquist_is_dropped_and_counted`.
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
    /// requires this suite to survive (`R1-6 § Tests`).
    #[test]
    fn changed_rate_alone_no_longer_triggers() {
        let snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        assert_eq!(resend_decision(Some(48_000.0), &snap, true), None);
    }

    /// ...and this is what replaces it: the engine's published refusal is the
    /// trigger now (spec `R1-6 § Fix 4`).
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

    // ---- resend_command ----

    /// D-10's round trip, and the regression that motivated this function.
    /// A band above the LIVE Nyquist is handed to the engine unchanged: the
    /// engine drops that band and keeps the rest (or, when nothing survives,
    /// refuses and RETAINS the intent so a later legal rate installs it).
    /// The forwarder used to send a whole-set `ClearCorrection` here, which
    /// destroyed the retained intent and, since the clear also cleared
    /// `correction_rate_mismatch`, left both re-send conditions false for
    /// the rest of the process -- the correction never came back.
    #[test]
    fn resend_command_hands_over_a_band_illegal_at_the_live_rate() {
        // Legal at 48 kHz, above Nyquist at 44.1 kHz.
        let bands = [peaking(1_000.0, 6.0, 1.0), peaking(23_000.0, -3.0, 1.0)];
        let snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        assert!(
            validate_bands(&bands, 44_100.0).is_err(),
            "the premise: this set does NOT pass the user-facing check at 44.1 kHz"
        );

        let mut state = ResendState::default();
        let cmd = resend_command(&mut state, &snap, &bands).expect("the first stream must re-send");
        assert_eq!(state.last_rate, Some(44_100.0));
        match cmd {
            EngineCommand::SetCorrection(CorrectionConfig::Peq {
                bands: sets,
                design_rate,
            }) => {
                assert_eq!(sets, vec![bands.to_vec()], "every band goes over untouched");
                assert_eq!(design_rate, 44_100.0);
            }
            _ => panic!("the forwarder must never clear the whole set (D-10)"),
        }
    }

    /// The other half of the round trip: with the intent still in the
    /// engine, a snapshot that raises the refusal flag re-sends it rather
    /// than clearing, whatever the bands look like at that rate.
    #[test]
    fn resend_command_on_a_refusal_re_sends_rather_than_clearing() {
        let bands = [peaking(23_000.0, -3.0, 1.0)];
        let mut snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        snap.correction_rate_mismatch = Some(44_100.0);
        let mut state = ResendState {
            last_rate: Some(44_100.0),
            resent_for: None,
        };
        let cmd = resend_command(&mut state, &snap, &bands).expect("a refusal re-sends");
        assert!(matches!(cmd, EngineCommand::SetCorrection(_)));
    }

    /// ...ONCE. `correction_rate_mismatch` latches -- the engine re-sets it
    /// on every refused rebuild, and re-sending the same bands at the same
    /// rate is refused identically -- so answering on its LEVEL redesigns,
    /// refuses and logs on every published snapshot. R1-8's decaying
    /// `input_peak` makes that the full tick rate (4 Hz by default) for as
    /// long as audio plays.
    #[test]
    fn resend_command_answers_a_standing_refusal_only_once() {
        let bands = [peaking(23_000.0, -3.0, 1.0)];
        let mut snap = snapshot_with_stream(Some(stream_at(44_100.0)));
        snap.correction_rate_mismatch = Some(44_100.0);

        let mut state = ResendState::default();
        assert!(
            resend_command(&mut state, &snap, &bands).is_some(),
            "the first snapshot carrying the refusal must supply design intent"
        );
        for n in 1..=5 {
            assert!(
                resend_command(&mut state, &snap, &bands).is_none(),
                "snapshot {n} re-answered a refusal already answered"
            );
        }
    }

    /// ...and the suppression is scoped to that one refusal. Once the engine
    /// stops naming a rate the memory is dropped, so a LATER refusal -- the
    /// AirPods 44.1 -> 48 -> 44.1 kHz round trip -- is answered again.
    #[test]
    fn a_cleared_then_re_raised_refusal_is_answered_again() {
        let bands = [peaking(23_000.0, -3.0, 1.0)];
        let mut refused = snapshot_with_stream(Some(stream_at(44_100.0)));
        refused.correction_rate_mismatch = Some(44_100.0);
        let healthy = snapshot_with_stream(Some(stream_at(48_000.0)));

        let mut state = ResendState::default();
        assert!(resend_command(&mut state, &refused, &bands).is_some());
        assert!(resend_command(&mut state, &refused, &bands).is_none());

        // The engine installed the set at a legal rate by itself; no flag, so
        // nothing to send -- but the memory of the refusal must go.
        assert!(
            resend_command(&mut state, &healthy, &bands).is_none(),
            "a healthy snapshot is not a re-send trigger"
        );
        assert_eq!(state.resent_for, None, "the refusal memory must be dropped");

        assert!(
            resend_command(&mut state, &refused, &bands).is_some(),
            "a refusal raised again after a healthy snapshot is a new edge"
        );
    }

    /// A refusal that names a DIFFERENT rate is a different refusal, even
    /// with no healthy snapshot in between.
    #[test]
    fn a_refusal_at_another_rate_is_a_new_edge() {
        let bands = [peaking(23_000.0, -3.0, 1.0)];
        let mut at_44 = snapshot_with_stream(Some(stream_at(44_100.0)));
        at_44.correction_rate_mismatch = Some(44_100.0);
        let mut at_32 = snapshot_with_stream(Some(stream_at(32_000.0)));
        at_32.correction_rate_mismatch = Some(32_000.0);

        let mut state = ResendState::default();
        assert!(resend_command(&mut state, &at_44, &bands).is_some());
        assert!(resend_command(&mut state, &at_44, &bands).is_none());
        assert!(
            resend_command(&mut state, &at_32, &bands).is_some(),
            "32 kHz is a rate the forwarder has not supplied intent for"
        );
    }

    /// The flag clears while the user happens to have no EQ loaded. The
    /// memory must still be dropped -- the reset deliberately runs before
    /// the no-bands bail-out -- or the next refusal goes unanswered.
    #[test]
    fn the_refusal_memory_clears_even_with_no_bands() {
        let bands = [peaking(23_000.0, -3.0, 1.0)];
        let mut refused = snapshot_with_stream(Some(stream_at(44_100.0)));
        refused.correction_rate_mismatch = Some(44_100.0);
        let healthy = snapshot_with_stream(Some(stream_at(44_100.0)));

        let mut state = ResendState::default();
        assert!(resend_command(&mut state, &refused, &bands).is_some());
        assert!(resend_command(&mut state, &healthy, &[]).is_none());
        assert_eq!(state.resent_for, None);
        assert!(resend_command(&mut state, &refused, &bands).is_some());
    }

    /// It stays a pass-through of [`resend_decision`]'s verdict whenever the
    /// edge gate is not in play: no trigger, no command.
    #[test]
    fn resend_command_is_none_when_nothing_triggers() {
        let bands = [peaking(1_000.0, 3.0, 1.0)];
        let snap = snapshot_with_stream(Some(stream_at(48_000.0)));
        let mut seen = ResendState {
            last_rate: Some(48_000.0),
            resent_for: None,
        };
        assert!(resend_command(&mut seen, &snap, &bands).is_none());
        let mut fresh = ResendState::default();
        assert!(
            resend_command(&mut fresh, &snap, &[]).is_none(),
            "no bands, nothing to send"
        );
        assert_eq!(
            fresh.last_rate, None,
            "nothing sent, nothing to record as seen"
        );
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
    /// R1-1's `§ Tests` table, "Export/engine agreement" row: the number
    /// in the exported text equals `EngineState.auto_preamp_db`. This is the
    /// test that catches a preamp recomputed on a different grid -- the whole
    /// reason `build_correction` calls `ParametricEQ::preamp_db()` verbatim
    /// rather than re-deriving a peak of its own.
    ///
    /// Compared through the exporter's own formatter, not as raw numbers: the
    /// engine publishes an f32 and `export_autoeq_lines` rewrites any preamp
    /// in `(-0.05, 0]` as `0.0`, so the agreement that matters is the agreement
    /// of the rendered line.
    #[test]
    fn exported_preamp_equals_engine_state_auto_preamp_db() {
        let bands = vec![
            peaking(45.0, 9.0, 2.0),
            peaking(1_000.0, 12.0, 1.0),
            peaking(6_000.0, -4.0, 1.5),
        ];
        let rate = 48_000.0;

        let exported = ParametricEQ {
            bands: bands.clone(),
            sample_rate: rate,
        }
        .export_autoeq_format_with_preamp();
        let exported_preamp = exported.lines().next().expect("a Preamp line");
        assert_ne!(
            exported_preamp, "Preamp: 0.0 dB",
            "a boosting band set must export a real preamp, or this test is vacuous"
        );

        let config = design_correction(&bands, rate).expect("a valid band set designs");
        let (_, report) = paraeq_engine::controller::build_correction(&config, 2, 512, rate)
            .expect("and builds at the live rate");
        assert!(report.preamp_db < 0.0, "boosts must pull the output down");

        let engine_preamp = ParametricEQ {
            bands: Vec::new(),
            sample_rate: rate,
        }
        .export_autoeq_format_with_preamp_db(f64::from(report.preamp_db as f32));
        assert_eq!(
            engine_preamp, exported_preamp,
            "the engine applied a preamp the export does not name"
        );
    }
}
