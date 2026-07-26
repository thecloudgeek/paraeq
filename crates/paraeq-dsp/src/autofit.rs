//! Greedy parametric-EQ auto-fit.
//!
//! Two entry points, deliberately separate:
//!
//! - [`auto_fit_parametric_eq`] — the coupler path. Oracle:
//!   `prototype/paraeq/correction/auto_fit.py`, Tier 1, golden-fixture pinned
//!   (`tests/test_autofit.rs`). **Byte-identical and frozen.** It has no gain
//!   limit, picks symmetrically, and hardcodes a 20–20000 Hz mask; that is
//!   correct parity with the oracle and wrong for a room, which is why the
//!   room does not use it.
//! - [`auto_fit_room`] — the room path. Tier 3 (analytic), additive, per
//!   channel, authority-limited. Pointed at a room curve the legacy function
//!   does exactly what Toole warns automated algorithms do: **it fills nulls.**
//!
//! # The autofit-shape reconciliation (cross-spec question 2, 2026-07-25)
//!
//! Three specs proposed three shapes for authority-limited fitting:
//! decision-engine § Authority 5 mutates `auto_fit_parametric_eq` in place;
//! engine-hardening adds a mono `auto_fit_parametric_eq_with_authority`;
//! room-dsp adds a per-channel `auto_fit_room`. Resolved in favour of
//! **room-dsp's `auto_fit_room`**, for three reasons that are not preferences:
//! mutation in place is incompatible with the frozen Tier-1 fixture ("fixtures
//! are sacred"), so it is out on the plan's own terms; between the two
//! additive shapes only the per-channel one matches the `PerChannel<T>` seam
//! that landed in Stage 3 *specifically* so the policy code would not be
//! written mono and rewritten (room-dsp § `PerChannel<T>`); and a mono
//! variant would then need a per-channel wrapper anyway, which is two
//! signatures owning one policy. The constants those three specs argue over
//! (`BOOST_WEIGHT`/`boost_ratio`, the narrow-dip threshold, the cut limit)
//! live in [`crate::authority::AuthorityPolicy`] — see its per-field
//! provenance notes; the shape decision here does not settle their values,
//! which stay OPEN \[OWNER\].

use crate::authority::{self, AuthorityCurve, Clamp};
use crate::logf::LogGrid;
use crate::peq::{EQBand, FilterType, ParametricEQ};
use crate::{DspError, PerChannel};

pub fn auto_fit_parametric_eq(
    correction_db: &[f64],
    freqs: &[f64],
    sample_rate: f64,
    max_bands: usize,
    min_gain_db: f64,
) -> Vec<EQBand> {
    let mut residual = correction_db.to_vec();
    let mask: Vec<bool> = freqs.iter().map(|f| (20.0..=20000.0).contains(f)).collect();
    let mut bands = Vec::new();
    for _ in 0..max_bands {
        let mut peak_idx = None;
        let mut peak_abs = f64::NEG_INFINITY;
        for i in 0..residual.len() {
            if mask[i] && residual[i].abs() > peak_abs {
                peak_abs = residual[i].abs();
                peak_idx = Some(i);
            }
        }
        let Some(idx) = peak_idx else { break };
        let peak_gain = residual[idx];
        if peak_gain.abs() < min_gain_db || freqs[idx] <= 0.0 {
            break;
        }
        let q = estimate_q(&residual, freqs, idx);
        let band = EQBand {
            filter_type: FilterType::Peaking,
            fc: freqs[idx],
            gain_db: peak_gain,
            q,
        };
        let response = ParametricEQ {
            bands: vec![band.clone()],
            sample_rate,
        }
        .frequency_response(freqs);
        for (r, resp) in residual.iter_mut().zip(&response) {
            let v = if resp.is_nan() { 0.0 } else { *resp };
            *r -= v;
        }
        bands.push(band);
    }
    bands
}

fn estimate_q(residual: &[f64], freqs: &[f64], peak_idx: usize) -> f64 {
    let f_center = freqs[peak_idx];
    let (lo_idx, hi_idx) = half_amplitude_crossings(residual, peak_idx);
    match (lo_idx.map(|i| freqs[i]), hi_idx.map(|i| freqs[i])) {
        (Some(lo), Some(hi)) if hi > lo && f_center > 0.0 => {
            (f_center / (hi - lo)).clamp(0.5, 20.0)
        }
        _ => 2.0,
    }
}

/// The first index either side of `peak_idx` where `|residual|` falls below
/// half the peak's magnitude — the oracle's half-amplitude crossing search,
/// extracted verbatim so [`estimate_q`] and [`auto_fit_room`]'s narrow-dip
/// veto measure the same feature the same way.
///
/// Two oracle quirks are preserved deliberately, because `estimate_q` is
/// golden-fixture pinned through [`auto_fit_parametric_eq`]:
/// the downward scan stops at index 1 (index 0 is never tested), and a search
/// that reaches an edge without crossing returns `None` rather than the edge.
fn half_amplitude_crossings(residual: &[f64], peak_idx: usize) -> (Option<usize>, Option<usize>) {
    let half = 0.5 * residual[peak_idx].abs();
    let mut lo = None;
    let mut i = peak_idx;
    while i > 1 {
        i -= 1;
        if residual[i].abs() < half {
            lo = Some(i);
            break;
        }
    }
    let mut hi = None;
    for (j, v) in residual.iter().enumerate().skip(peak_idx + 1) {
        if v.abs() < half {
            hi = Some(j);
            break;
        }
    }
    (lo, hi)
}

/// What one channel's room fit produced: the bands, every clamp and veto that
/// shaped them, and the count of bands the stability funnel dropped.
///
/// room-dsp's signature returns a bare `PerChannel<Vec<EQBand>>`. That is a
/// strict information loss the same spec forbids elsewhere: `clamp_band` is
/// specified to report "every change so the Advanced drawer can explain WHY
/// (each decision visible + overridable)", and `auto_fit_room` is
/// `clamp_band`'s only production caller — a bare `Vec<EQBand>` return makes
/// that reporting unreachable. The spec's return value is [`Self::bands`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomFitReport {
    /// The realized bands, in the order the greedy loop picked them.
    pub bands: Vec<EQBand>,
    /// Every clamp and veto, in the order they happened.
    pub clamps: Vec<Clamp>,
    /// Bands the Jury retry funnel could not stabilize (`authority.band_dropped`).
    /// With the Q cap applied this should always be 0; a nonzero value is a
    /// bug report, not a tuning outcome.
    pub dropped: usize,
}

/// Authority-limited, per-channel greedy fit for the room path.
///
/// Four things differ from [`auto_fit_parametric_eq`], all of them because a
/// room curve is not a coupler curve.
///
/// **Sign convention first, because everything below depends on it.**
/// `targets::compute_correction(measured, target) = target − measured`, so
/// `residual > 0` means *boost* (the measurement sits **below** target — a
/// measured **dip** — dangerous) and `residual < 0` means *cut* (a measured
/// **peak** — safe).
///
/// 1. **Asymmetric picking.** Score `|residual|` for a cut and
///    `boost_ratio · residual` for a boost, so a peak always outranks a dip of
///    the same size. `boost_ratio` is read off the curve as
///    `max_boost_db / max_cut_db` at the bin, which is the same ratio
///    [`crate::authority::build_authority`] applied — so a curve built
///    cut-only (ratio 0) also *ranks* every boost last, rather than ranking it
///    highly and then clamping it to nothing.
/// 2. **Narrow-dip veto.** A positive residual whose half-amplitude width is
///    under [`AuthorityCurve::min_dip_width_oct`] is refused outright and its
///    whole support is struck from the candidate set — narrow dips are
///    destructive interference, non-minimum-phase, and boosting one burns
///    headroom and excursion to produce the same cancellation, louder.
/// 3. **Gain and Q clamps.** Every band goes through
///    [`crate::authority::clamp_band`] and then the Jury retry funnel
///    ([`crate::authority::stabilize_band`]).
/// 4. **Mask.** `grid.f_min()..=grid.f_max()` intersected with
///    `min_valid_freq_hz` — never fit a band below the frequency the gate can
///    resolve. This is the link that makes "gated + spatially averaged" an
///    honest claim rather than a slogan: `gating::GateReport` publishes its
///    own limit and the corrector obeys it.
///
/// `min_valid_freq_hz` is a seventh parameter the spec's signature omits even
/// though its own item 4 requires it; pass `GateReport::min_valid_freq_hz`, or
/// `0.0` on an ungated path.
///
/// # Errors
///
/// `InvalidInput` when a channel's length does not match the grid, when any
/// correction value is non-finite (a NaN residual wins every comparison it
/// should lose once negated), when `min_valid_freq_hz` is not finite, or when
/// `sample_rate` is not positive and finite.
pub fn auto_fit_room(
    correction: &PerChannel<Vec<f64>>,
    grid: &LogGrid,
    sample_rate: f64,
    authority: &AuthorityCurve,
    max_bands: usize,
    min_gain_db: f64,
    min_valid_freq_hz: f64,
) -> Result<PerChannel<RoomFitReport>, DspError> {
    if !(sample_rate.is_finite() && sample_rate > 0.0) {
        return Err(DspError::InvalidInput(format!(
            "auto_fit_room: sample_rate must be finite and > 0, got {sample_rate}"
        )));
    }
    if !min_valid_freq_hz.is_finite() {
        return Err(DspError::InvalidInput(format!(
            "auto_fit_room: min_valid_freq_hz must be finite, got {min_valid_freq_hz}"
        )));
    }
    if !(min_gain_db.is_finite() && min_gain_db >= 0.0) {
        return Err(DspError::InvalidInput(format!(
            "auto_fit_room: min_gain_db must be finite and >= 0, got {min_gain_db}"
        )));
    }
    correction.try_map(|channel| {
        fit_one_channel(
            channel,
            grid,
            sample_rate,
            authority,
            max_bands,
            min_gain_db,
            min_valid_freq_hz,
        )
    })
}

fn fit_one_channel(
    correction_db: &[f64],
    grid: &LogGrid,
    sample_rate: f64,
    authority: &AuthorityCurve,
    max_bands: usize,
    min_gain_db: f64,
    min_valid_freq_hz: f64,
) -> Result<RoomFitReport, DspError> {
    let freqs = grid.freqs();
    if correction_db.len() != freqs.len() {
        return Err(DspError::InvalidInput(format!(
            "auto_fit_room: {} correction points for a {}-point grid",
            correction_db.len(),
            freqs.len()
        )));
    }
    if correction_db.iter().any(|v| !v.is_finite()) {
        return Err(DspError::InvalidInput(
            "auto_fit_room: correction contains non-finite values".into(),
        ));
    }
    let mut report = RoomFitReport::default();
    if freqs.is_empty() {
        return Ok(report);
    }

    // The mask, and with it the one-time admissibility precompute. A bin whose
    // ceiling cannot even reach `min_gain_db` in either direction can never
    // produce a band, so striking it here is not an optimization but the
    // termination argument: the candidate loop below only ever *removes*
    // candidates, and this removes the whole zero-authority case at once
    // (sigma = 10 dB everywhere -> no bands, in one pass rather than 957).
    let f_lo = grid.f_min().max(min_valid_freq_hz);
    let band_range = f_lo..=grid.f_max();
    let mut admissible: Vec<bool> = freqs
        .iter()
        .map(|&f| {
            if !band_range.contains(&f) || f <= 0.0 {
                return false;
            }
            let at = authority.at(f);
            at.max_cut_db.max(at.max_boost_db) >= min_gain_db
        })
        .collect();

    let mut residual = correction_db.to_vec();
    for _ in 0..max_bands {
        let Some((idx, gain, q)) = pick_candidate(
            &residual,
            freqs,
            authority,
            &mut admissible,
            min_gain_db,
            &mut report,
        ) else {
            break;
        };
        let requested = EQBand {
            filter_type: FilterType::Peaking,
            fc: freqs[idx],
            gain_db: gain,
            q,
        };
        let (clamped, clamps) = authority::clamp_band(&requested, authority);
        report.clamps.extend(clamps);
        let Some((realized, _sos)) = authority::stabilize_band(&clamped, sample_rate) else {
            // The Jury funnel gave up. Strike the bin so the loop advances,
            // count it, and keep fitting the rest of the curve — dropping one
            // band is a defect to report, not a reason to abandon the fit.
            report.dropped += 1;
            admissible[idx] = false;
            continue;
        };
        // Subtract what will actually run, not what was asked for: the greedy
        // loop's bookkeeping must track the realized cascade or every
        // subsequent pick is fitting a residual that does not exist.
        let response = ParametricEQ {
            bands: vec![realized.clone()],
            sample_rate,
        }
        .frequency_response(freqs);
        for (r, resp) in residual.iter_mut().zip(&response) {
            let v = if resp.is_finite() { *resp } else { 0.0 };
            *r -= v;
        }
        report.bands.push(realized);
    }
    Ok(report)
}

/// Pick the next admissible candidate, applying the narrow-dip veto and
/// striking anything it refuses.
///
/// Returns `(index, requested_gain_db, estimated_q)`, or `None` when nothing
/// admissible clears `min_gain_db`. Every iteration either returns or strikes
/// at least one bin, so the loop is bounded by the grid size.
fn pick_candidate(
    residual: &[f64],
    freqs: &[f64],
    authority: &AuthorityCurve,
    admissible: &mut [bool],
    min_gain_db: f64,
    report: &mut RoomFitReport,
) -> Option<(usize, f64, f64)> {
    loop {
        let mut best: Option<(usize, f64)> = None;
        for (i, &r) in residual.iter().enumerate() {
            if !admissible[i] {
                continue;
            }
            // Asymmetric score: a cut is worth its full magnitude, a boost only
            // `boost_ratio` of it, so a peak always outranks a dip of the same
            // size. The ratio comes from the curve so a cut-only curve ranks
            // boosts last rather than ranking them first and clamping to zero.
            let score = if r < 0.0 {
                -r
            } else {
                r * boost_ratio_at(authority, freqs[i])
            };
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((i, score));
            }
        }
        // `score` is finite by construction: the caller validated every
        // residual finite and `min_gain_db` finite, and `boost_ratio_at` is a
        // ratio of validated ceilings.
        let (idx, score) = best?;
        if score < min_gain_db {
            return None;
        }
        let gain = residual[idx];
        if gain > 0.0 {
            // Narrow-dip veto (a pick-time refusal, not a gain clamp): measure
            // the dip's half-amplitude width and refuse anything narrower than
            // the policy allows. A crossing that never happens means the
            // feature runs to the edge of the band — broad, not narrow — so a
            // missing crossing is not a veto.
            let (lo, hi) = half_amplitude_crossings(residual, idx);
            if let (Some(lo), Some(hi)) = (lo, hi) {
                let width_oct = (freqs[hi] / freqs[lo]).log2();
                if width_oct < authority.min_dip_width_oct() {
                    report.clamps.push(Clamp::DipRefused { width_oct });
                    let (from, to) = refused_span(residual, lo, hi);
                    for slot in admissible.iter_mut().take(to + 1).skip(from) {
                        *slot = false;
                    }
                    continue;
                }
            }
        }
        return Some((idx, gain, estimate_q(residual, freqs, idx)));
    }
}

/// The bins a narrow-dip veto strikes: the half-amplitude support `[lo, hi]`,
/// extended outward through the feature's own basin — for as long as the
/// residual keeps *falling* as you move away from the dip.
///
/// Striking only `[lo, hi]` is not enough, and the failure is not theoretical:
/// the bins just outside a vetoed dip still carry half its amplitude, so the
/// next pass picks a shoulder, measures *its* half-amplitude crossings — which
/// find nothing on the inward side, because the residual only rises toward the
/// dip's centre — concludes the feature is broad, and fills the very null the
/// veto just refused. Extending through the basin removes the shoulders with
/// the dip they belong to.
///
/// The walk stops at the first local minimum, which is what keeps the rule
/// from over-reaching: a narrow spike sitting on a genuinely broad positive
/// region strikes only the spike, and the broad region is picked, measured
/// broad, and corrected on a later pass.
fn refused_span(residual: &[f64], lo: usize, hi: usize) -> (usize, usize) {
    let mut from = lo;
    while from > 0 && residual[from - 1] < residual[from] {
        from -= 1;
    }
    let mut to = hi;
    while to + 1 < residual.len() && residual[to + 1] < residual[to] {
        to += 1;
    }
    (from, to)
}

/// `max_boost / max_cut` at `f` — the boost weight the curve was built with,
/// recovered rather than re-specified. Falls back to 0.0 where there is no cut
/// authority at all (nothing to take a ratio of, and nothing to boost either).
fn boost_ratio_at(authority: &AuthorityCurve, freq_hz: f64) -> f64 {
    let at = authority.at(freq_hz);
    if at.max_cut_db > 0.0 {
        at.max_boost_db / at.max_cut_db
    } else {
        0.0
    }
}
