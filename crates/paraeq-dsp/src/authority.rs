//! Confidence-derived correction authority: a frequency-indexed ceiling on
//! what the corrector is allowed to do, plus the boost-Q cap that doubles as
//! the runtime stability guard.
//! Test tier: 3 — analytic physics/policy. There is no oracle: the prototype
//! has no authority concept at all, and a Python port written this afternoon
//! would launder a design guess into a golden fixture. What is pinned instead
//! (`tests/test_authority.rs`) is the closed form of every composition step,
//! the two worked `max_q_for_boost` values the spec states, and the
//! monotonicity/endpoint invariants of the excursion interpolation.
//! Spec: docs/specs/2026-07-15-room-dsp-design.md, "`authority.rs` — new".
//!
//! # Why authority is confidence-derived, not threshold-derived
//!
//! "Full authority below 200 Hz" is unsafe as a blanket rule. An individual
//! room mode is a pole pair and *is* minimum-phase, but the measured response
//! is the **sum** of many modes plus direct sound, and summing minimum-phase
//! systems does not preserve minimum phase — real in-room responses are
//! mixed-phase throughout the modal region. REW's own help says "we cannot
//! simply say a response is minimum phase below some specific cutoff" and
//! documents non-minimum-phase regions at 44–56 Hz alongside minimum-phase
//! regions at 300–500 Hz.
//!
//! ParaEQ therefore derives authority from **σ(f)**, the inter-position
//! standard deviation ([`crate::fr::sigma_db`]), which measures the same thing
//! operationally: low σ means the feature is present at every position (a
//! property of the transducer, or of a mode dominating the whole listening
//! area — worth correcting); high σ means it is an interference artefact of
//! one microphone position (correcting it makes every other seat worse).
//! Because σ(f) rises toward the diffuse-field asymptote of 5.57 dB above the
//! Schroeder frequency, a σ-derived ceiling reproduces the ~200 Hz rule in an
//! untreated room **without hardcoding it**, and extends authority higher in a
//! treated one.
//!
//! [`crate::room::TransitionRange`] is display-only and is deliberately NOT an
//! input here. That seam is precisely where the refuted minimum-phase
//! reasoning would otherwise creep back in.

use crate::logf::LogGrid;
use crate::peq::EQBand;
use crate::DspError;

/// Trinnov's shipped excursion curve as `(freq_hz, max_abs_db)` breakpoints:
/// ±10 dB at or below 150 Hz, tapering to ±2 dB by 500 Hz, ±2 dB above.
/// Log-f interpolated between breakpoints by [`excursion_db`].
pub const DEFAULT_EXCURSION_DB: [(f64, f64); 4] =
    [(20.0, 10.0), (150.0, 10.0), (500.0, 2.0), (20000.0, 2.0)];

/// σ (dB) at or below which authority is FULL.
///
/// **Canonical.** `decision-engine-design.md` § Authority states outright that
/// the `σ_full = 1.0 / σ_none = 6.0` endpoints are "the canonical
/// `AuthorityPolicy` defaults, owned by room-dsp's `authority.rs` — `decide()`
/// consumes the `AuthorityCurve` that module produces and must not re-specify
/// different numbers." This constant is that single definition.
pub const SIGMA_FULL_DB: f64 = 1.0;

/// σ (dB) at or above which authority is ZERO. Sits just above the 5.57 dB
/// diffuse-field asymptote, so authority reaches zero only as a region becomes
/// fully statistical. See [`SIGMA_FULL_DB`] for the canonical-ownership note.
pub const SIGMA_NONE_DB: f64 = 6.0;

/// Boosts are capped at this fraction of the cut ceiling: cut peaks freely,
/// fill dips grudgingly.
///
/// **Cross-spec reconciliation (2026-07-25).** Three specs name this quantity:
/// room-dsp's `boost_ratio` (default 0.5), engine-hardening's `BOOST_WEIGHT`
/// (≈ 0.5, marked OPEN \[OWNER\]), and decision-engine § Authority 5, which
/// says room **auto mode** ships cut-only, i.e. `boost_ceiling` scaled to 0
/// "unless the user raises it in the drawer". Those are not in conflict once
/// the layers are separated: this crate owns the *mechanism* and ships the
/// 0.5 default; whether the room auto path passes `boost_ratio = 0.0` is
/// `decide()`'s policy call in Stage 6, expressible here as a value rather
/// than a second default. The numeric value itself remains OPEN \[OWNER\] —
/// it needs ears on real measurements — and it is a single named constant so
/// that ruling costs one line.
pub const DEFAULT_BOOST_RATIO: f64 = 0.5;

/// Never fill a dip narrower than this, at any gain. 1/6 octave.
///
/// **Cross-spec reconciliation (2026-07-25).** room-dsp types the veto as a
/// width (`min_dip_width_oct`, default 1/6 octave) and decision-engine
/// § Authority 4 words it identically ("admissible only where the positive
/// excursion is ≥ 1/6 octave wide at half its peak value"). engine-hardening
/// words the same veto as a Q threshold — "reject any positive-gain candidate
/// whose estimated Q exceeds ... `Q > 3.0`" — and itself records that the two
/// are *the same decision* which "must reconcile to one definition together".
/// Two of the three specs agree on 1/6 octave and the API is typed as a width,
/// so the width wins. The alternative is materially stricter, not a rounding
/// difference: see [`width_oct_for_q`] — Q = 3 is 0.479 octave, Q = 8.64 is
/// 1/6. Still OPEN \[OWNER\] (an ears call); bounded meanwhile by the fact
/// that any dip that survives the veto is still gain-limited to
/// `boost_ratio · e(f) · w(f)` — at most 5 dB below 150 Hz, 1 dB above 500 Hz.
pub const DEFAULT_MIN_DIP_WIDTH_OCT: f64 = 1.0 / 6.0;

/// REW's boost-Q cap constant: `Q_max = BOOST_Q_CONSTANT · f0 / A`, derived
/// from a 500 ms T60 rule and verified against this crate's own `biquad.rs`
/// coefficients.
pub const BOOST_Q_CONSTANT: f64 = 0.227;

/// The global Q clamp `autofit.rs` has always applied (`autofit.rs:71`), kept
/// as the outer bound on the room path too: the boost-Q cap is a *further*
/// restriction on boosts, never a licence to exceed this.
pub const Q_CLAMP: std::ops::RangeInclusive<f64> = 0.5..=20.0;

/// The policy inputs to [`build_authority`]. Every field is a named constant
/// with a documented provenance, so an owner ruling changes data, not code.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorityPolicy {
    /// Trinnov's shipped excursion curve as `(freq_hz, max_abs_db)`
    /// breakpoints, log-f interpolated. Must be non-empty and strictly
    /// increasing in frequency. Default [`DEFAULT_EXCURSION_DB`].
    pub excursion: Vec<(f64, f64)>,
    /// σ (dB) at or below which authority is FULL. Default [`SIGMA_FULL_DB`].
    pub sigma_full_db: f64,
    /// σ (dB) at or above which authority is ZERO. Default [`SIGMA_NONE_DB`].
    pub sigma_none_db: f64,
    /// Boosts capped at this fraction of the cut ceiling. Default
    /// [`DEFAULT_BOOST_RATIO`].
    pub boost_ratio: f64,
    /// Never fill a dip narrower than this. Default
    /// [`DEFAULT_MIN_DIP_WIDTH_OCT`].
    pub min_dip_width_oct: f64,
}

impl Default for AuthorityPolicy {
    fn default() -> Self {
        Self {
            excursion: DEFAULT_EXCURSION_DB.to_vec(),
            sigma_full_db: SIGMA_FULL_DB,
            sigma_none_db: SIGMA_NONE_DB,
            boost_ratio: DEFAULT_BOOST_RATIO,
            min_dip_width_oct: DEFAULT_MIN_DIP_WIDTH_OCT,
        }
    }
}

/// The per-frequency ceiling, on the grid [`build_authority`] was given.
///
/// The four spec'd vectors (`freqs`, `max_boost_db`, `max_cut_db`, `max_q`)
/// are the contract. `excursion_db` and `sigma_db` are the **attribution
/// record**: they carry the two composition inputs so [`clamp_band`] can say
/// *which* gate bit — the Advanced drawer's whole premise is that every
/// decision is visible and overridable, and "your +6 dB became +1 dB" is not
/// an explanation without saying whether the excursion envelope or the
/// seat-to-seat disagreement did it.
///
/// Constructible only through [`build_authority`]: the fields are private and
/// there is no `Default`, so a curve that skipped validation (mismatched
/// lengths, a σ that was never checked finite, boosts above cuts) cannot be
/// handed to [`clamp_band`] or to the autofit. Deserialization — otherwise a
/// second, unvalidated constructor — routes through
/// [`RawAuthorityCurve`]'s `TryFrom`, the same guard `targets::TargetCurve`
/// uses.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(try_from = "RawAuthorityCurve"))]
pub struct AuthorityCurve {
    excursion_db: Vec<f64>,
    freqs: Vec<f64>,
    max_boost_db: Vec<f64>,
    max_cut_db: Vec<f64>,
    max_q: Vec<f64>,
    min_dip_width_oct: f64,
    sigma_db: Vec<f64>,
}

impl AuthorityCurve {
    /// The excursion envelope `e(f)` before the confidence weight — the
    /// attribution record, not part of the spec's four-field contract.
    pub fn excursion_db(&self) -> &[f64] {
        &self.excursion_db
    }

    pub fn freqs(&self) -> &[f64] {
        &self.freqs
    }

    pub fn len(&self) -> usize {
        self.freqs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.freqs.is_empty()
    }

    pub fn max_boost_db(&self) -> &[f64] {
        &self.max_boost_db
    }

    pub fn max_cut_db(&self) -> &[f64] {
        &self.max_cut_db
    }

    /// The narrow-dip veto width, carried through from the policy.
    ///
    /// It rides on the curve rather than being a seventh argument to
    /// [`crate::autofit::auto_fit_room`] because the veto is part of "what the
    /// corrector is allowed to do at this frequency", which is exactly what
    /// this type is. The spec's `auto_fit_room` signature takes the curve and
    /// not the policy, so without this the only place the veto could come
    /// from would be a duplicated default.
    pub fn min_dip_width_oct(&self) -> f64 {
        self.min_dip_width_oct
    }

    /// `max_q_for_boost(f, max_boost(f))` per bin — for display and for the
    /// drawer's Q control. [`clamp_band`] recomputes at the band's exact `fc`
    /// and its *clamped* gain rather than reading this, because interpolating
    /// a cap is not the same as capping at the interpolated point.
    pub fn max_q(&self) -> &[f64] {
        &self.max_q
    }

    /// The σ(f) the curve was built from — the attribution record.
    pub fn sigma_db(&self) -> &[f64] {
        &self.sigma_db
    }

    /// The ceiling at an arbitrary frequency: linear interpolation in `log f`
    /// between grid bins, clamped to the end values outside the grid.
    ///
    /// Clamping rather than zeroing outside the grid is deliberate and is the
    /// safe direction *only* because the autofit's frequency mask already
    /// refuses to place a band outside `[f_min, f_max] ∩ min_valid_freq_hz`
    /// (see [`crate::autofit::auto_fit_room`]). A band that reaches this
    /// function is in-grid by construction; the clamp exists so a caller
    /// probing an edge frequency gets the edge ceiling instead of a panic.
    pub fn at(&self, freq_hz: f64) -> AuthorityAt {
        let (i, j, t) = self.locate(freq_hz);
        let lerp = |v: &[f64]| v[i] + t * (v[j] - v[i]);
        AuthorityAt {
            excursion_db: lerp(&self.excursion_db),
            max_boost_db: lerp(&self.max_boost_db),
            max_cut_db: lerp(&self.max_cut_db),
            sigma_db: lerp(&self.sigma_db),
        }
    }

    /// `(lo_index, hi_index, fraction)` for linear interpolation in `log f`.
    /// A single-bin curve degenerates to `(0, 0, 0.0)`.
    fn locate(&self, freq_hz: f64) -> (usize, usize, f64) {
        let n = self.freqs.len();
        // The `is_finite` arm is load-bearing, not defensive noise: a NaN
        // reaches `partition_point` with every predicate false, `j` is 0, and
        // `j - 1` underflows. Clamping NaN to the first bin instead is the
        // conservative direction (the lowest frequency has the *most*
        // authority, so a caller that got here with a NaN sees a real ceiling
        // rather than a panic — and `clamp_band` refuses non-finite bands
        // before they ever reach here).
        if n == 1 || !freq_hz.is_finite() || freq_hz <= self.freqs[0] {
            return (0, 0, 0.0);
        }
        if freq_hz >= self.freqs[n - 1] {
            return (n - 1, n - 1, 0.0);
        }
        let j = self.freqs.partition_point(|&f| f <= freq_hz);
        let i = j - 1;
        let (lo, hi) = (self.freqs[i].ln(), self.freqs[j].ln());
        // The grid is strictly increasing, so hi > lo and this cannot divide
        // by zero.
        (i, j, (freq_hz.ln() - lo) / (hi - lo))
    }
}

/// Wire shape [`AuthorityCurve`] deserializes through (`serde(try_from)`).
/// Without it, `Deserialize` would be a second constructor that skips
/// [`build_authority`]'s validation — and a curve with a NaN ceiling compares
/// false against every gain, i.e. silently unbounded authority.
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct RawAuthorityCurve {
    excursion_db: Vec<f64>,
    freqs: Vec<f64>,
    max_boost_db: Vec<f64>,
    max_cut_db: Vec<f64>,
    max_q: Vec<f64>,
    min_dip_width_oct: f64,
    sigma_db: Vec<f64>,
}

#[cfg(feature = "serde")]
impl TryFrom<RawAuthorityCurve> for AuthorityCurve {
    type Error = String;

    fn try_from(raw: RawAuthorityCurve) -> Result<Self, Self::Error> {
        let n = raw.freqs.len();
        if n == 0 {
            return Err("AuthorityCurve: empty curve".into());
        }
        let lengths = [
            raw.excursion_db.len(),
            raw.max_boost_db.len(),
            raw.max_cut_db.len(),
            raw.max_q.len(),
            raw.sigma_db.len(),
        ];
        if lengths.iter().any(|&len| len != n) {
            return Err(format!(
                "AuthorityCurve: {n} freqs but component lengths {lengths:?}"
            ));
        }
        if raw.freqs.windows(2).any(|w| w[1] <= w[0]) {
            return Err("AuthorityCurve: freqs must be strictly increasing".into());
        }
        // Positive requirements: a NaN ceiling would compare false against
        // every gain, which reads as "no limit", not as "no authority".
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        if !(finite(&raw.freqs)
            && finite(&raw.excursion_db)
            && finite(&raw.max_boost_db)
            && finite(&raw.max_cut_db)
            && finite(&raw.max_q)
            && finite(&raw.sigma_db))
        {
            return Err("AuthorityCurve: every component must be finite".into());
        }
        let nonneg = |v: &[f64]| v.iter().all(|&x| x >= 0.0);
        if !(nonneg(&raw.max_boost_db) && nonneg(&raw.max_cut_db) && nonneg(&raw.sigma_db)) {
            return Err(
                "AuthorityCurve: ceilings and sigma are magnitudes and cannot be negative".into(),
            );
        }
        if !(raw.min_dip_width_oct.is_finite() && raw.min_dip_width_oct >= 0.0) {
            return Err(format!(
                "AuthorityCurve: min_dip_width_oct must be finite and >= 0, got {}",
                raw.min_dip_width_oct
            ));
        }
        Ok(Self {
            excursion_db: raw.excursion_db,
            freqs: raw.freqs,
            max_boost_db: raw.max_boost_db,
            max_cut_db: raw.max_cut_db,
            max_q: raw.max_q,
            min_dip_width_oct: raw.min_dip_width_oct,
            sigma_db: raw.sigma_db,
        })
    }
}

/// [`AuthorityCurve::at`]'s answer: the ceiling and the two attribution
/// inputs at one frequency.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuthorityAt {
    pub excursion_db: f64,
    pub max_boost_db: f64,
    pub max_cut_db: f64,
    pub sigma_db: f64,
}

/// Compose the ceiling. Order matters and is the spec's:
///
/// 1. **Excursion ceiling** `e(f)` — log-f interpolation of the Trinnov
///    breakpoints.
/// 2. **Confidence weight** `w(f) = clamp((σ_none − σ(f)) / (σ_none − σ_full), 0, 1)`.
/// 3. `max_cut(f) = e(f) · w(f)`.
/// 4. `max_boost(f) = boost_ratio · e(f) · w(f)` — asymmetric by construction.
/// 5. `max_q(f) = max_q_for_boost(f, max_boost(f))`.
///
/// Step 6 of the spec's list — the narrow-dip refusal — is deliberately absent:
/// it is a **pick-time veto**, not a gain clamp, and lives in
/// [`crate::autofit::auto_fit_room`] where the residual's shape is visible.
///
/// # Errors
///
/// `InvalidInput` when `sigma_db` does not match the grid, when any σ is
/// non-finite or negative (a standard deviation cannot be either, so this is a
/// caller bug that would otherwise silently produce an unbounded ceiling), or
/// when the policy is malformed. Validation is total on purpose: this function
/// is the only constructor of the type the autofit trusts.
pub fn build_authority(
    grid: &LogGrid,
    sigma_db: &[f64],
    policy: &AuthorityPolicy,
) -> Result<AuthorityCurve, DspError> {
    validate_policy(policy)?;
    if sigma_db.len() != grid.len() {
        return Err(DspError::InvalidInput(format!(
            "build_authority: {} sigma values for a {}-point grid",
            sigma_db.len(),
            grid.len()
        )));
    }
    // Positive requirement: NaN passes no comparison, so it lands here rather
    // than propagating into a ceiling that compares false against every gain.
    if !sigma_db.iter().all(|s| s.is_finite() && *s >= 0.0) {
        return Err(DspError::InvalidInput(
            "build_authority: sigma must be finite and non-negative".into(),
        ));
    }
    let span = policy.sigma_none_db - policy.sigma_full_db;
    let freqs = grid.freqs().to_vec();
    let mut excursion = Vec::with_capacity(freqs.len());
    let mut max_boost_db = Vec::with_capacity(freqs.len());
    let mut max_cut_db = Vec::with_capacity(freqs.len());
    let mut max_q = Vec::with_capacity(freqs.len());
    for (&f, &sigma) in freqs.iter().zip(sigma_db) {
        let e = excursion_db(&policy.excursion, f);
        let w = ((policy.sigma_none_db - sigma) / span).clamp(0.0, 1.0);
        let cut = e * w;
        let boost = policy.boost_ratio * cut;
        excursion.push(e);
        max_cut_db.push(cut);
        max_q.push(max_q_for_boost(f, boost));
        max_boost_db.push(boost);
    }
    Ok(AuthorityCurve {
        excursion_db: excursion,
        freqs,
        max_boost_db,
        max_cut_db,
        max_q,
        min_dip_width_oct: policy.min_dip_width_oct,
        sigma_db: sigma_db.to_vec(),
    })
}

/// Log-f linear interpolation of the excursion breakpoints, clamped to the end
/// values outside their range.
///
/// Worked check from the spec: `excursion_db(DEFAULT_EXCURSION_DB, 300.0)` is
/// `10 − 8·(log₁₀300 − log₁₀150)/(log₁₀500 − log₁₀150) = 5.394 dB`.
///
/// # Panics
///
/// If `breakpoints` is empty. [`build_authority`] rejects that at
/// [`validate_policy`]; this is a direct-caller bug.
pub fn excursion_db(breakpoints: &[(f64, f64)], freq_hz: f64) -> f64 {
    assert!(
        !breakpoints.is_empty(),
        "excursion_db: empty breakpoint list"
    );
    let n = breakpoints.len();
    if freq_hz <= breakpoints[0].0 {
        return breakpoints[0].1;
    }
    if freq_hz >= breakpoints[n - 1].0 {
        return breakpoints[n - 1].1;
    }
    let j = breakpoints.partition_point(|&(f, _)| f <= freq_hz);
    let (f_lo, db_lo) = breakpoints[j - 1];
    let (f_hi, db_hi) = breakpoints[j];
    let t = (freq_hz.ln() - f_lo.ln()) / (f_hi.ln() - f_lo.ln());
    db_lo + t * (db_hi - db_lo)
}

/// REW's gain-dependent boost-Q cap: `Q_max = 0.227 · f0 / A`, `A = 10^(G/40)`.
///
/// Derived from a 500 ms T60 rule — a boost narrower than this rings longer
/// than the room does, so it adds decay rather than removing it. Doubles as
/// the design-path stability guard: with this cap applied, the Jury test
/// ([`is_stable`]) never fires.
///
/// Worked values from the spec: `max_q_for_boost(100.0, 6.0) ≈ 16.07`
/// (`A = 10^0.15 = 1.4125`), `max_q_for_boost(100.0, 0.0) = 22.7`.
///
/// A non-positive `gain_db` (a cut) is not this function's business — cuts
/// keep the plain [`Q_CLAMP`] — but the formula is still evaluated rather than
/// special-cased, so a caller that passes one gets a *looser* cap (A < 1),
/// never a silently tighter one it did not ask for.
pub fn max_q_for_boost(f0: f64, gain_db: f64) -> f64 {
    let a = 10f64.powf(gain_db / 40.0);
    BOOST_Q_CONSTANT * f0 / a
}

/// The half-amplitude width in octaves of a resonance of quality `q`.
///
/// `f_hi·f_lo = f0²` and `f_hi − f_lo = f0/q` give `√r − 1/√r = 1/q` with
/// `r = f_hi/f_lo`, hence `r = ((1/q + √(1/q² + 4))/2)²` and the width is
/// `log2(r)`. Present because the narrow-dip veto is specified as a width in
/// one place and as a Q threshold in another (see
/// [`DEFAULT_MIN_DIP_WIDTH_OCT`]), and the conversion between the two must be
/// exact and in one place rather than approximated in a comment.
///
/// `width_oct_for_q(3.0) ≈ 0.4787`; `width_oct_for_q(8.6444) ≈ 1/6`.
pub fn width_oct_for_q(q: f64) -> f64 {
    let inv = 1.0 / q;
    let root_r = 0.5 * (inv + (inv * inv + 4.0).sqrt());
    (root_r * root_r).log2()
}

/// Jury stability test on a designed SOS row: `|a2| < 1 && |a1| < a2 + 1`.
///
/// Re-exported under the name the spec gives it. The implementation lives in
/// [`crate::biquad`] (it landed in Stage 1 as R1-3's install-boundary guard and
/// `peq::preamp_db` already depends on it); a second copy here would be a
/// second place for the two to disagree.
pub use crate::biquad::is_stable;

/// What [`clamp_band`] changed, and why. Every variant is rendered by the
/// Advanced drawer as an explanation, so each carries the numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Clamp {
    /// The gain hit the Trinnov excursion envelope: confidence was full here,
    /// the physics ceiling was not.
    GainToExcursion { from: f64, to: f64 },
    /// The gain hit the σ-weighted ceiling: the positions disagree at this
    /// frequency, so the correction was throttled below what the excursion
    /// envelope alone would have allowed.
    GainToSigma { from: f64, to: f64, sigma_db: f64 },
    /// Q hit the boost-Q cap (or the global [`Q_CLAMP`] ceiling).
    QToBoostCap { from: f64, to: f64 },
    /// Emitted by [`crate::autofit::auto_fit_room`], not by [`clamp_band`]:
    /// the narrow-dip refusal is a **pick-time veto** that needs the
    /// residual's shape, which a single band no longer carries.
    DipRefused { width_oct: f64 },
}

/// Clamp one band to the curve, reporting every change.
///
/// - Gain is clamped into `[−max_cut(fc), +max_boost(fc)]`. Attribution: if
///   the confidence weight was below 1 at `fc` the σ gate is what bit
///   ([`Clamp::GainToSigma`]), otherwise the excursion envelope did
///   ([`Clamp::GainToExcursion`]). "Below 1" is read off the recorded
///   `excursion_db` vs `max_cut_db`, not recomputed from a policy this
///   function was not given.
/// - Q is clamped to [`Q_CLAMP`] always, and additionally to
///   `max_q_for_boost(fc, gain)` when the **clamped** gain is a boost. Using
///   the clamped gain is the point: a +12 dB request clamped to +2 dB gets
///   +2 dB's Q ceiling, which is looser — capping against the request would
///   restrict a band the corrector never asked for.
/// - A non-finite `fc`, `gain_db` or `q` is not clamped into range; the band
///   is returned with `gain_db = 0.0` and reported as a full clamp to zero.
///   Clamping NaN yields NaN by documented Rust behaviour, and a NaN
///   coefficient is permanently broken audio.
pub fn clamp_band(band: &EQBand, authority: &AuthorityCurve) -> (EQBand, Vec<Clamp>) {
    let mut out = band.clone();
    let mut clamps = Vec::new();
    if !band.fc.is_finite() || !band.gain_db.is_finite() || !band.q.is_finite() {
        out.gain_db = 0.0;
        out.q = *Q_CLAMP.start();
        clamps.push(Clamp::GainToExcursion {
            from: band.gain_db,
            to: 0.0,
        });
        return (out, clamps);
    }
    let at = authority.at(band.fc);
    let gain = band.gain_db.clamp(-at.max_cut_db, at.max_boost_db);
    if gain != band.gain_db {
        // The σ gate bit iff it actually reduced the envelope here. Compared
        // with a relative tolerance because both sides are products of
        // interpolations, so exact equality would misattribute on roundoff.
        let throttled = at.max_cut_db < at.excursion_db * (1.0 - 1e-12);
        clamps.push(if throttled {
            Clamp::GainToSigma {
                from: band.gain_db,
                to: gain,
                sigma_db: at.sigma_db,
            }
        } else {
            Clamp::GainToExcursion {
                from: band.gain_db,
                to: gain,
            }
        });
    }
    out.gain_db = gain;

    let mut q_ceiling = *Q_CLAMP.end();
    if gain > 0.0 {
        q_ceiling = q_ceiling.min(max_q_for_boost(band.fc, gain));
    }
    let q = band
        .q
        .clamp(*Q_CLAMP.start(), q_ceiling.max(*Q_CLAMP.start()));
    if q != band.q {
        clamps.push(Clamp::QToBoostCap {
            from: band.q,
            to: q,
        });
    }
    out.q = q;
    (out, clamps)
}

/// Design `band` and back its Q off until the result passes the Jury test.
///
/// The spec's belt and braces: "After `EQBand::to_sos`, assert `is_stable`; on
/// failure back Q off by 10% and retry up to 8 times, then drop the band."
/// With the Q cap applied this should never fire — if it does, that is a bug,
/// and the returned `None` is what `authority.band_dropped` logs.
///
/// Returns the surviving band and its SOS row, or `None` if eight retries did
/// not produce a stable design.
pub fn stabilize_band(band: &EQBand, sample_rate: f64) -> Option<(EQBand, [f64; 6])> {
    const MAX_RETRIES: usize = 8;
    const BACKOFF: f64 = 0.9;
    let mut candidate = band.clone();
    for _ in 0..=MAX_RETRIES {
        let sos = candidate.to_sos(sample_rate);
        if is_stable(&sos) {
            return Some((candidate, sos));
        }
        candidate.q *= BACKOFF;
    }
    None
}

fn validate_policy(policy: &AuthorityPolicy) -> Result<(), DspError> {
    if policy.excursion.is_empty() {
        return Err(DspError::InvalidInput(
            "AuthorityPolicy: excursion needs at least one breakpoint".into(),
        ));
    }
    let bad_point = policy
        .excursion
        .iter()
        .any(|&(f, db)| !f.is_finite() || f <= 0.0 || !db.is_finite() || db < 0.0);
    if bad_point {
        return Err(DspError::InvalidInput(
            "AuthorityPolicy: excursion breakpoints need finite f > 0 and finite dB >= 0".into(),
        ));
    }
    if policy.excursion.windows(2).any(|w| w[1].0 <= w[0].0) {
        return Err(DspError::InvalidInput(
            "AuthorityPolicy: excursion breakpoints must be strictly increasing in frequency"
                .into(),
        ));
    }
    // Positive requirements throughout, so NaN refuses rather than passing.
    let sigma_ok = policy.sigma_full_db.is_finite()
        && policy.sigma_none_db.is_finite()
        && policy.sigma_full_db >= 0.0
        && policy.sigma_none_db > policy.sigma_full_db;
    if !sigma_ok {
        return Err(DspError::InvalidInput(format!(
            "AuthorityPolicy: need 0 <= sigma_full_db < sigma_none_db, got {} and {}",
            policy.sigma_full_db, policy.sigma_none_db
        )));
    }
    if !(policy.boost_ratio.is_finite() && (0.0..=1.0).contains(&policy.boost_ratio)) {
        return Err(DspError::InvalidInput(format!(
            "AuthorityPolicy: boost_ratio must be in 0.0..=1.0, got {}",
            policy.boost_ratio
        )));
    }
    if !(policy.min_dip_width_oct.is_finite() && policy.min_dip_width_oct >= 0.0) {
        return Err(DspError::InvalidInput(format!(
            "AuthorityPolicy: min_dip_width_oct must be finite and >= 0, got {}",
            policy.min_dip_width_oct
        )));
    }
    Ok(())
}

/// Re-exported so `autofit.rs` and downstream crates can reach the identity
/// row without importing `biquad` for one constant.
pub use crate::biquad::IDENTITY;
