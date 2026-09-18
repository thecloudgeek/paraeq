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
//!   channel, authority-limited, plus the residual-RMS stop and shelf
//!   emission, both Tier 3: the stop is asserted as a band count against a
//!   curve with a known residual, the shelf against the closed-form half-gain
//!   width. [`crate::biquad::low_shelf`] / [`crate::biquad::high_shelf`] are
//!   themselves Tier-1 fixture-pinned (`fixtures/biquad/matrix`), so the shelf
//!   work adds no new oracle need and **no new fixture case**. There is no
//!   delegate for any of it: the policy being tested is ParaEQ's own, and a
//!   Python port written to check it would launder a design guess into a
//!   golden fixture. Pointed at a room curve the legacy function does exactly
//!   what Toole warns automated algorithms do: **it fills nulls.**
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

/// A one-signed excursion at an end of `correction_range` must span at least
/// this many octaves before a shelf is emitted for it.
///
/// `decision-engine-design.md` § Decision table, `shelves`: "Emit a shelf where
/// a ≥0.5-octave one-signed excursion exists at either end of
/// `correction_range`". The span is measured to the excursion's own **half-gain
/// point**, which is not a convenience: an RBJ shelf sits at exactly half its
/// gain at `fc`, so the half-gain frequency IS the corner the shelf should be
/// placed at, and the same measurement answers both "is it broad enough" and
/// "where does it turn over".
pub const SHELF_MIN_WIDTH_OCT: f64 = 0.5;

/// Shelf Q is clamped into this range — `decision-engine-design.md` § Decision
/// table, `shelves`: "shelf Q clamped `[0.4, 0.7]`".
///
/// The ceiling is below RBJ's Butterworth 0.7071, and that is the whole point:
/// a shelf with Q above 0.7071 overshoots just outside its corner, re-creating
/// the bump it was emitted to remove. [`SHELF_Q_REQUEST`] therefore asks for
/// Butterworth and this clamp is what actually bites.
pub const SHELF_Q: std::ops::RangeInclusive<f64> = 0.4..=0.7;

/// The Q the room path asks for before [`SHELF_Q`]'s clamp: RBJ's
/// maximally-flat 0.7071 — the highest Q that does not overshoot, i.e. the
/// fastest transition the shape allows without inventing a new bump.
const SHELF_Q_REQUEST: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// The slice of `decide()`'s decision table [`auto_fit_room`] needs beyond the
/// authority curve: **where** to correct, **how flat** is flat enough, and
/// whether the ends may be corrected with a shelf.
///
/// Distinct from [`crate::authority::AuthorityPolicy`], which is the physics
/// *ceiling* (excursion envelope, confidence, boost ratio, Q cap). This is the
/// *goal*, and the two are deliberately different types so a tuning change to
/// one cannot be mistaken for a safety change to the other.
///
/// [`auto_fit_room`] takes it as an `Option` because both behaviours it gates
/// need a `correction_range` that no caller had to supply before it existed:
/// `None` is the Stage-5 behaviour — fit until the band budget or the
/// candidates run out — and is what a caller with no decision table (a test, a
/// direct probe) should pass. `decide()` always passes `Some`.
///
/// Spec: `decision-engine-design.md` § Decision table, rows `correction_range`,
/// `flatness_target_db` ("3.0 room / 1.0 coupler"), `max_filters` ("Greedy
/// worst-first; stop when residual RMS over the authority band
/// `< flatness_target_db` or the cap is hit") and `shelves`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomFitPolicy {
    /// `(low_hz, high_hz)`, **both edges inclusive** — the band the fit works
    /// in and, intersected with where the curve has any authority at all, the
    /// band the stop is graded over.
    pub correction_range: (f64, f64),
    /// The residual RMS, in dB over the authority band, at or below which the
    /// fit is finished. Must be finite and positive.
    pub flatness_target_db: f64,
    /// Whether a broad one-signed excursion at either end of
    /// `correction_range` may be corrected with a shelf rather than left to
    /// the peaking loop. `Choice: true, false`, default true.
    pub shelves: bool,
}

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

/// How far the realized cascade may exceed the envelope before a band is
/// refused, in dB.
///
/// **This is a physical tolerance, not a float-equality epsilon**, and it
/// used to be the latter (`1e-9`) — a defect that cost two of the four
/// transducer paths their entire correction. `COUPLER_EXCURSION_DB` is
/// EXACTLY zero above 10 kHz, which is the decision-engine spec's own
/// "On the coupler path, `A_base(f) = 0` above 10 kHz"; and every
/// realizable IIR band has a non-zero magnitude at every frequency, so a
/// 1.5 kHz peaking filter's tail at 13 kHz (measured: 8.8e-3 dB) exceeded a
/// 1e-9 ceiling by seven orders of magnitude. `fits` was therefore false for
/// every candidate on the coupler path, the bisection shrank each one below
/// `min_gain_db`, and `auto_fit_room` returned **zero bands for every in-ear
/// and over-ear measurement**.
///
/// The envelope bounds the CORRECTION; a skirt is not a correction. No band
/// is ever PLACED in a zero-envelope region — `admissible` refuses it,
/// because `max_cut.max(max_boost) >= min_gain_db` is false there — so what
/// this forgives is only what a filter placed elsewhere leaves behind.
///
/// 0.1 dB is the resolution at which this product can state a gain at all:
/// `ParametricEQ::export_autoeq_format` and every rationale render gains to
/// one decimal, so an overshoot under it is below the smallest number the
/// app can name, and far below the ~0.5 dB narrowband JND. It bounds the
/// CASCADE, not one band, so the accumulated tails of a ten-band fit are
/// bounded by it too — a fit whose skirts really do add up to 0.1 dB in a
/// no-correction band stops placing filters, which is the ceiling working
/// rather than failing.
pub const CEILING_SLOP_DB: f64 = 0.1;

/// How far the realized cascade may CUT inside a region the envelope licenses
/// nothing in, in dB.
///
/// A region with `max_cut_db == max_boost_db == 0` is one the corrector may not
/// TARGET — `admissible` already refuses to place a band there — and that is a
/// different claim from "no filter placed anywhere else may have a magnitude
/// here". No realizable filter bank has a magnitude of exactly 0 dB at any
/// frequency, so the second claim forbids every filter: measured on the coupler
/// path, a −2 dB peaking band at 8 kHz (Q 2.6, inside an envelope that licenses
/// 2 dB there) leaves 0.65 dB at 10 kHz, where the coupler envelope steps to
/// zero. Under a `CEILING_SLOP_DB` bound that band shrinks to −0.31 dB, falls
/// under `min_gain_db` and is struck — so the envelope licensed a correction at
/// 8 kHz that no realizable filter could deliver, which is a policy
/// contradiction rather than a safety property.
///
/// **Only the CUT half is loosened, and that asymmetry is the whole design.**
/// What the envelope guards is the corrector DRIVING the system: excursion,
/// filling a null, a resonance that rings longer than the problem it fixes —
/// every one of those is a BOOST, and boosts stay bounded by
/// `max_boost_db + CEILING_SLOP_DB` everywhere, which in a no-authority region
/// is 0.1 dB. A cut that leaks out of a neighbouring filter cannot damage a
/// driver, cannot fill a null and cannot ring. `docs/decisions/...` § D-E puts
/// it in one line: "cuts are free, boosts cost headroom and can damage
/// drivers."
///
/// 1.0 dB, because that is the tightest `flatness_target_db` any path ships
/// (the coupler's) — an unintended cut smaller than the error the fit is
/// content to leave behind is not a correction anyone is entitled to notice.
pub const NO_AUTHORITY_CUT_LEAK_DB: f64 = 1.0;

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
    ///
    /// **Stability only.** A candidate the ceiling had no headroom left for is
    /// NOT counted here: that is a tuning outcome the envelope is supposed to
    /// produce, and it is reported through [`Self::clamps`] as the
    /// `GainToExcursion` entry naming the gain that was asked for. A nonzero
    /// `dropped` is a bug report; a nonzero clamp count is a correction being
    /// shaped.
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
/// # `max_bands`, `min_gain_db` and `fit_policy`
///
/// `max_bands` is `decisions.max_filters` — a **cap**, not a target. With a
/// `fit_policy` the fit stops as soon as the residual RMS over the authority
/// band falls below `flatness_target_db`, so the cap binds only on a curve the
/// budget cannot flatten (`decision-engine-design.md` § Decision table,
/// `max_filters`). Shelves are charged against the same budget: `max_bands` is
/// a limit on **filters**, not on peaking filters.
///
/// `min_gain_db` is the same row's drop rule — "Drop any band with
/// `|gain| < flatness/2`". The *binding* `min_gain_db = flatness_target_db / 2`
/// is `decide()`'s to make and is tested there; this function owns the
/// mechanism, and a feature refused by it is reported as
/// [`Clamp::BelowMinGain`] rather than dropped silently.
///
/// # Errors
///
/// `InvalidInput` when a channel's length does not match the grid, when any
/// correction value is non-finite (a NaN residual wins every comparison it
/// should lose once negated), when `min_valid_freq_hz` is not finite, when
/// `sample_rate` is not positive and finite, or when a `fit_policy` is present
/// and malformed (a non-finite or inverted `correction_range`, a
/// `correction_range` that starts at or below 0 Hz, or a
/// `flatness_target_db` that is not finite and positive). The policy is
/// validated rather than tolerated because
/// [`crate::authority::authority_band_mask`] answers an empty band for a range
/// it cannot place — and an empty band can never say "flat enough", so a
/// tolerated one would silently disable the stop instead of refusing.
// The argument list is the spec's own signature (room-dsp § `autofit.rs`
// changes) plus the two parameters its own items require but its signature
// omits — `min_valid_freq_hz` and the decision-table slice. Bundling them into
// one struct would hide which of them are safety inputs and which are goals,
// which is the distinction `RoomFitPolicy`'s doc exists to keep visible.
#[allow(clippy::too_many_arguments)]
pub fn auto_fit_room(
    correction: &PerChannel<Vec<f64>>,
    grid: &LogGrid,
    sample_rate: f64,
    authority: &AuthorityCurve,
    max_bands: usize,
    min_gain_db: f64,
    min_valid_freq_hz: f64,
    fit_policy: Option<&RoomFitPolicy>,
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
    if let Some(policy) = fit_policy {
        let (low_hz, high_hz) = policy.correction_range;
        if !(low_hz.is_finite() && high_hz.is_finite() && low_hz > 0.0 && low_hz <= high_hz) {
            return Err(DspError::InvalidInput(format!(
                "auto_fit_room: correction_range must be finite with 0 < low <= high, \
                 got ({low_hz}, {high_hz})"
            )));
        }
        if !(policy.flatness_target_db.is_finite() && policy.flatness_target_db > 0.0) {
            return Err(DspError::InvalidInput(format!(
                "auto_fit_room: flatness_target_db must be finite and > 0, got {}",
                policy.flatness_target_db
            )));
        }
    }
    let ctx = FitContext {
        authority,
        grid,
        min_gain_db,
        sample_rate,
    };
    correction
        .try_map(|channel| fit_one_channel(channel, &ctx, max_bands, min_valid_freq_hz, fit_policy))
}

/// What every step of one fit shares and none of them changes: the ceiling,
/// the grid, the drop floor and the design rate.
///
/// It exists so the band path — clamp, stabilize, shrink, commit — can be one
/// function called from both the shelf pre-pass and the greedy loop without a
/// nine-argument signature. Nothing here is per-band or per-channel.
struct FitContext<'a> {
    authority: &'a AuthorityCurve,
    grid: &'a LogGrid,
    min_gain_db: f64,
    sample_rate: f64,
}

fn fit_one_channel(
    correction_db: &[f64],
    ctx: &FitContext,
    max_bands: usize,
    min_valid_freq_hz: f64,
    fit_policy: Option<&RoomFitPolicy>,
) -> Result<RoomFitReport, DspError> {
    let (authority, grid, min_gain_db) = (ctx.authority, ctx.grid, ctx.min_gain_db);
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
    // The realized cascade so far, in dB, on the grid. THE authority ceiling is
    // a property of the cascade, not of any one band: clamping each band
    // individually lets a greedy loop place band after band at the same
    // frequency, each inside the ceiling, summing to far outside it. Measured
    // before this was added: a −24 dB modal peak at 60 Hz produced a realized
    // −22.9 dB against a ±10 dB envelope, plus two spurious boosts on the
    // shoulders it over-cut. The envelope is a driver-excursion limit, so
    // exceeding it 2.3× is exactly the failure it exists to prevent.
    let mut applied = vec![0.0f64; freqs.len()];

    // THE authority band, called rather than re-derived: `authority_band_mask`
    // is the single definition of the phrase, shared with `decide()`'s
    // verification gate, and `tests/test_authority.rs` fails if a second copy
    // of the predicate appears anywhere in the workspace's library sources.
    let band_mask =
        fit_policy.map(|p| authority::authority_band_mask(freqs, authority, p.correction_range));
    let flat_enough = |residual: &[f64]| match (fit_policy, band_mask.as_ref()) {
        (Some(policy), Some(mask)) => {
            residual_rms_db(residual, mask).is_some_and(|rms| rms < policy.flatness_target_db)
        }
        // No policy is no target, so nothing is ever "flat enough" and the fit
        // runs to its band budget — the pre-B4 behaviour, unchanged.
        _ => false,
    };

    // Shelves first, and before the greedy loop rather than inside it: a broad
    // end tilt is one filter's work, and the loop would otherwise spend six
    // peaking bands walking up it before the shelf could be considered. The
    // spec's own sentence for the row is "a shelf fixes it with one filter
    // instead of six".
    if let Some(policy) = fit_policy {
        if policy.shelves && !flat_enough(&residual) {
            emit_shelves(
                policy.correction_range,
                ctx,
                &admissible,
                &mut applied,
                &mut residual,
                &mut report,
            );
        }
    }

    // Shelves are filters, so they are charged against `max_filters` too.
    let budget = max_bands.saturating_sub(report.bands.len());
    for _ in 0..budget {
        if flat_enough(&residual) {
            break;
        }
        let Some((idx, gain, q)) = pick_candidate(
            &residual,
            &applied,
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
        match commit_band(&requested, ctx, &mut applied, &mut residual, &mut report) {
            Commit::Emitted => {}
            Commit::Unstable => {
                // The Jury funnel gave up. Strike the bin so the loop
                // advances, count it, and keep fitting the rest of the curve —
                // dropping one band is a defect to report, not a reason to
                // abandon the fit.
                report.dropped += 1;
                admissible[idx] = false;
            }
            Commit::NoHeadroom => {
                // No usable gain survives here — the ceiling is already spent
                // at this feature. Strike the bin rather than emitting a
                // filter that does nothing. The budget iteration IS spent, so
                // a curve whose ceiling is exhausted ends the fit early rather
                // than walking the whole grid one struck bin at a time; that
                // is what `a_spent_ceiling_does_not_burn_the_band_budget`
                // pins.
                //
                // **Deliberately NOT counted in `report.dropped`.** That field
                // is the Jury/stability funnel's count, and its own doc says a
                // nonzero value "is a bug report, not a tuning outcome" — an
                // unstable design is a defect, while a spent ceiling is the
                // envelope doing exactly what it exists to do. Counting the two
                // together would make every healthy ceiling-limited fit look
                // like a stability failure. The reporting a NoHeadroom strike
                // owes the drawer is already there: `commit_band` pushes
                // `clamp_band`'s own clamps BEFORE it can answer `NoHeadroom`,
                // so the `GainToExcursion` entry naming the requested gain and
                // the ceiling is in `report.clamps` either way.
                admissible[idx] = false;
            }
        }
    }
    Ok(report)
}

/// The residual RMS over the authority band, in dB — `None` when the band is
/// empty, because a band with no bins grades nothing and must not be read as
/// "flat".
///
/// A plain mean of squares over grid bins **is** the octave-weighted mean the
/// spec asks for, and only because the analysis grid is uniform in `log f`
/// ([`LogGrid`]): every bin covers the same fraction of an octave, so no
/// explicit weight is needed. On a linear grid the same code would be
/// treble-weighted, and wrong.
fn residual_rms_db(residual: &[f64], band: &[bool]) -> Option<f64> {
    let mut sum_sq = 0.0;
    let mut n = 0usize;
    for (r, in_band) in residual.iter().zip(band) {
        if *in_band {
            sum_sq += r * r;
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    Some((sum_sq / n as f64).sqrt())
}

/// What happened to a band on its way from "requested" to "installed".
enum Commit {
    /// Clamped, stable, and inside the cascade ceiling: it is in the report.
    Emitted,
    /// The Jury retry funnel could not stabilize it.
    Unstable,
    /// Nothing at or above `min_gain_db` fits under what the cascade has
    /// already spent here.
    NoHeadroom,
}

/// Clamp one requested band to the curve, stabilize it, shrink it into the
/// cascade, and — if anything survives — commit it to `report`, `residual` and
/// `applied`.
///
/// Extracted so the shelf pre-pass and the greedy loop cannot drift: every
/// band `auto_fit_room` emits goes through the same four gates in the same
/// order, whatever picked it.
fn commit_band(
    requested: &EQBand,
    ctx: &FitContext,
    applied: &mut [f64],
    residual: &mut [f64],
    report: &mut RoomFitReport,
) -> Commit {
    let (clamped, clamps) = authority::clamp_band(requested, ctx.authority);
    report.clamps.extend(clamps);
    let Some((stable, _sos)) = authority::stabilize_band(&clamped, ctx.sample_rate) else {
        return Commit::Unstable;
    };
    // THE cascade gate. Everything above bounds one band; this bounds the sum,
    // which is the only thing the driver and the headroom budget actually see.
    let Some((realized, response)) = shrink_into_cascade(
        &stable,
        applied,
        ctx.authority,
        ctx.grid.freqs(),
        ctx.sample_rate,
        ctx.min_gain_db,
    ) else {
        return Commit::NoHeadroom;
    };
    if realized.gain_db != stable.gain_db {
        report.clamps.push(
            ctx.authority
                .at(requested.fc)
                .gain_clamp(stable.gain_db, realized.gain_db),
        );
    }
    // Subtract what will actually run, not what was asked for: the greedy
    // loop's bookkeeping must track the realized cascade or every subsequent
    // pick is fitting a residual that does not exist.
    for ((r, a), resp) in residual.iter_mut().zip(applied.iter_mut()).zip(&response) {
        *r -= resp;
        *a += resp;
    }
    report.bands.push(realized);
    Commit::Emitted
}

/// Emit at most one shelf at each end of `correction_range`.
///
/// `decision-engine-design.md` § Decision table, `shelves`: "Emit a shelf where
/// a ≥0.5-octave one-signed excursion exists at either end of
/// `correction_range`; shelf Q clamped `[0.4, 0.7]`."
fn emit_shelves(
    correction_range: (f64, f64),
    ctx: &FitContext,
    admissible: &[bool],
    applied: &mut [f64],
    residual: &mut [f64],
    report: &mut RoomFitReport,
) {
    for filter_type in [FilterType::LowShelf, FilterType::HighShelf] {
        let Some(shelf) = shelf_at_end(filter_type, correction_range, ctx, residual, admissible)
        else {
            continue;
        };
        match commit_band(&shelf, ctx, applied, residual, report) {
            Commit::Emitted => {}
            // Same meaning as in the greedy loop: a design the Jury funnel
            // could not stabilize is a defect to report.
            Commit::Unstable => report.dropped += 1,
            // Nothing to strike and nothing to report: the tilt is still in
            // the residual, so the peaking loop gets its turn at it under the
            // same ceiling. No budget is spent, because no filter was placed.
            Commit::NoHeadroom => {}
        }
    }
}

/// The shelf one end of `correction_range` asks for, if any.
///
/// The excursion's **asymptote** is the residual at the end bin itself — that
/// is what a shelf's gain parameter means — and its **corner** is the first bin
/// walking inward where the residual either changes sign or falls to half that
/// asymptote, because an RBJ shelf sits at exactly half its gain at `fc`. The
/// span between the two is the "one-signed excursion" the spec measures against
/// [`SHELF_MIN_WIDTH_OCT`].
///
/// A run that is broad enough but whose asymptote is below `min_gain_db` yields
/// no shelf and **no report**: the feature is still in the residual, so the
/// greedy loop sees it and, if it is the largest thing left, reports it as
/// [`Clamp::BelowMinGain`] there. One emission site, one meaning — reporting
/// here as well would announce every silent grid tail as a decision.
fn shelf_at_end(
    filter_type: FilterType,
    correction_range: (f64, f64),
    ctx: &FitContext,
    residual: &[f64],
    admissible: &[bool],
) -> Option<EQBand> {
    let freqs = ctx.grid.freqs();
    let (low_hz, high_hz) = correction_range;
    let in_band = |i: usize| admissible[i] && freqs[i] >= low_hz && freqs[i] <= high_hz;
    let from_low = matches!(filter_type, FilterType::LowShelf);
    let end = if from_low {
        (0..freqs.len()).find(|&i| in_band(i))?
    } else {
        (0..freqs.len()).rev().find(|&i| in_band(i))?
    };
    let asymptote = residual[end];
    if asymptote == 0.0 {
        // No excursion at all, and `0.0.signum()` is +1.0 — so this has to be
        // tested rather than left to the sign walk below.
        return None;
    }
    let half = 0.5 * asymptote.abs();
    let mut corner = end;
    loop {
        let next = if from_low {
            if corner + 1 >= freqs.len() {
                break;
            }
            corner + 1
        } else {
            if corner == 0 {
                break;
            }
            corner - 1
        };
        if !in_band(next) {
            break;
        }
        let r = residual[next];
        if r * asymptote <= 0.0 || r.abs() < half {
            break;
        }
        corner = next;
    }
    let width_oct = if from_low {
        (freqs[corner] / freqs[end]).log2()
    } else {
        (freqs[end] / freqs[corner]).log2()
    };
    if width_oct < SHELF_MIN_WIDTH_OCT || asymptote.abs() < ctx.min_gain_db {
        return None;
    }
    Some(EQBand {
        filter_type,
        fc: freqs[corner],
        gain_db: asymptote,
        q: SHELF_Q_REQUEST.clamp(*SHELF_Q.start(), *SHELF_Q.end()),
    })
}

/// What is left of the ceiling at a bin, given `applied` dB of realized
/// cascade already there.
///
/// `applied > 0` is boost already spent, so it eats boost headroom and *frees*
/// cut headroom, and vice versa — which is why the cut term adds. Both are
/// floored at zero.
fn remaining_headroom(at: &authority::AuthorityAt, applied: f64) -> (f64, f64) {
    (
        (at.max_boost_db - applied).max(0.0),
        (at.max_cut_db + applied).max(0.0),
    )
}

/// The most the cascade may CUT at one frequency: the envelope's own ceiling
/// plus [`CEILING_SLOP_DB`] where the envelope licenses something, and
/// [`NO_AUTHORITY_CUT_LEAK_DB`] where it licenses nothing.
///
/// One function because two call sites must not disagree about what "inside the
/// envelope" means, and because the asymmetry needs a name to be argued with.
fn cut_limit_db(at: &authority::AuthorityAt) -> f64 {
    if at.licenses_correction() {
        at.max_cut_db + CEILING_SLOP_DB
    } else {
        NO_AUTHORITY_CUT_LEAK_DB
    }
}

/// Reduce `band`'s gain until adding it keeps the **whole cascade** inside the
/// authority curve at every bin, and return it with its realized response.
///
/// This is the gate the per-band clamp cannot be. `clamp_band` bounds one
/// filter at one frequency; the excursion envelope is a driver-excursion limit
/// on what the *driver* sees, which is the sum. Two things defeat a per-band
/// reading, and both were measured before this existed:
///
/// - **Restacking.** A residual deeper than the ceiling stays the top-scoring
///   candidate after being partially corrected, so the greedy loop places band
///   after band at the same frequency, each individually legal. A −24 dB modal
///   peak at 60 Hz realized **−22.9 dB** against a ±10 dB envelope.
/// - **Skirts.** Bounding only each band's own centre bin still lets
///   neighbouring bands' skirts pile up in between; the same case then realized
///   **−14.6 dB**. Only checking every bin catches this.
///
/// Bisection on a scale factor is valid because a peaking filter's dB response
/// is monotone in its gain parameter at every frequency, so feasibility is
/// monotone in the scale. `scale = 0` is feasible by construction (a 0 dB band
/// contributes nothing and `applied` is inside the curve by induction), which
/// is what makes the search total. Returns `None` when nothing at or above
/// `min_gain_db` fits.
fn shrink_into_cascade(
    band: &EQBand,
    applied: &[f64],
    authority: &AuthorityCurve,
    freqs: &[f64],
    sample_rate: f64,
    min_gain_db: f64,
) -> Option<(EQBand, Vec<f64>)> {
    /// Enough halvings to land within ~0.4% of the true limit on a 20 dB band.
    const BISECTION_STEPS: usize = 12;

    let response_at = |gain_db: f64| -> Vec<f64> {
        ParametricEQ {
            bands: vec![EQBand {
                gain_db,
                ..band.clone()
            }],
            sample_rate,
        }
        .frequency_response(freqs)
        .into_iter()
        // A non-finite section response contributes nothing rather than
        // poisoning the cascade — the same substitution `preamp_db` makes.
        .map(|v| if v.is_finite() { v } else { 0.0 })
        .collect()
    };
    let fits = |response: &[f64]| -> bool {
        freqs.iter().zip(applied).zip(response).all(|((&f, a), r)| {
            let at = authority.at(f);
            let total = a + r;
            total <= at.max_boost_db + CEILING_SLOP_DB && total >= -cut_limit_db(&at)
        })
    };

    let full = response_at(band.gain_db);
    if fits(&full) {
        return Some((band.clone(), full));
    }
    // Largest feasible scale in (0, 1). `lo` is always feasible, `hi` never.
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut best: Option<(f64, Vec<f64>)> = None;
    for _ in 0..BISECTION_STEPS {
        let mid = 0.5 * (lo + hi);
        let gain = band.gain_db * mid;
        let response = response_at(gain);
        if fits(&response) {
            lo = mid;
            best = Some((gain, response));
        } else {
            hi = mid;
        }
    }
    let (gain, response) = best?;
    if gain.abs() < min_gain_db {
        return None;
    }
    Some((
        EQBand {
            gain_db: gain,
            ..band.clone()
        },
        response,
    ))
}

/// Pick the next admissible candidate, applying the narrow-dip veto and
/// striking anything it refuses.
///
/// Returns `(index, requested_gain_db, estimated_q)`, or `None` when nothing
/// admissible clears `min_gain_db`. Every iteration either returns or strikes
/// at least one bin, so the loop is bounded by the grid size.
fn pick_candidate(
    residual: &[f64],
    applied: &[f64],
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
            // The drop rule, made visible. `decision-engine-design.md`
            // § Decision table, `max_filters`: "Drop any band with
            // `|gain| < flatness/2`" — and `min_gain_db` IS that flatness/2.
            // The best thing left is too small to be worth a filter, so the
            // fit is finished; reporting it is what lets the drawer say "we
            // found a 1.2 dB bump and left it alone because you asked for
            // 3 dB flat" instead of dropping it in silence, which is the one
            // option `clamp_band`'s premise — report every change — forbids.
            // The reported gain is what was FOUND (the raw residual), not the
            // asymmetric score that ranked it.
            report.clamps.push(Clamp::BelowMinGain {
                fc: freqs[idx],
                gain_db: residual[idx],
            });
            return None;
        }
        let gain = residual[idx];
        // Spent authority: this bin's ceiling has already been used up by the
        // bands placed so far, so nothing useful can be added here. Strike it
        // — without this the greedy loop re-picks the same bin every iteration
        // (the residual barely moves once the clamp bites) and burns the whole
        // band budget producing near-zero-gain filters.
        let at = authority.at(freqs[idx]);
        let (remaining_boost, remaining_cut) = remaining_headroom(&at, applied[idx]);
        let remaining = if gain < 0.0 {
            remaining_cut
        } else {
            remaining_boost
        };
        if remaining < min_gain_db {
            admissible[idx] = false;
            continue;
        }
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
