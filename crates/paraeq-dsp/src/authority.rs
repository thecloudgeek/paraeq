//! Confidence-derived correction authority: a frequency-indexed ceiling on
//! what the corrector is allowed to do, plus the boost-Q cap that doubles as
//! the runtime stability guard.
//! Test tier: 3 — analytic physics/policy. There is no oracle: the prototype
//! has no authority concept at all, and a Python port written this afternoon
//! would launder a design guess into a golden fixture. What is pinned instead
//! (`tests/test_authority.rs`) is the closed form of every composition step,
//! the two worked `max_q_for_boost` values the spec states, and the
//! monotonicity/endpoint invariants of the excursion interpolation.
//! The same tier and the same missing oracle cover the path additions:
//! [`COUPLER_EXCURSION_DB`]'s endpoints (the room vector's shape up to
//! [`COUPLER_CUTOFF_HZ`], exactly zero above it), the [`AuthorityPolicy::q_ceiling`]
//! composition in [`max_q_for_boost_capped`] ([`COUPLER_Q_CEILING`] flat at 5.0;
//! [`ROOM_Q_CEILING`] log-linear from 10.0 at 200 Hz to 3.0 at 10 kHz), and
//! [`authority_band_mask`]. Each is a policy constant or one composition step,
//! closed form, and adds no fixture case.
//! The EGD gate (v1.1, [`EgdGate::Off`] by default) is Tier 3 against a
//! synthetic two-path IR whose excess group delay is known in closed form; see
//! `measurement-suite:590` for why it is off in v1. There is no oracle for it
//! either — no library computes "peak-to-peak over a 1/3-octave band", and the
//! threshold it compares against is a `[NEEDS DATA]` starting value, so a
//! fixture would pin an untuned guess. No fixture case.
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

/// Where the coupler path's excursion envelope stops. Above this frequency the
/// coupler is allowed no correction of either sign.
pub const COUPLER_CUTOFF_HZ: f64 = 10_000.0;

/// The coupler path's excursion envelope: [`DEFAULT_EXCURSION_DB`]'s shape up
/// to [`COUPLER_CUTOFF_HZ`], and **zero above it**.
///
/// `decision-engine-design.md` § Authority 1, verbatim: "On the coupler path,
/// `A_base(f) = 0` above 10 kHz."
///
/// **Why the fifth breakpoint looks like that.** The spec states a step, and a
/// step is what this is. [`validate_policy`] requires breakpoints strictly
/// increasing in frequency — two points at the same frequency would be a
/// second, unvalidated way to write a curve — so the step is spelled as a
/// [`COUPLER_CUTOFF_STEP`]-wide ramp instead. The readable alternative, a taper
/// from 2 dB at 10 kHz to 0 dB at 20 kHz, is wrong in the direction that
/// matters: it licenses correction across the octave the spec licenses none in.
pub const COUPLER_EXCURSION_DB: [(f64, f64); 5] = [
    (20.0, 10.0),
    (150.0, 10.0),
    (500.0, 2.0),
    (COUPLER_CUTOFF_HZ, 2.0),
    (COUPLER_CUTOFF_HZ * (1.0 + COUPLER_CUTOFF_STEP), 0.0),
];

/// The width of [`COUPLER_EXCURSION_DB`]'s step, as a fraction of
/// [`COUPLER_CUTOFF_HZ`]. One part per billion, which is the only number in
/// this file chosen against two numerical bounds rather than a spec sentence:
///
/// - **Narrow enough to be a step.** The standard analysis grid is 96 points
///   per octave, so its bins are ~72 Hz apart at 10 kHz. A 1e-5 Hz ramp cannot
///   contain one, and no curve this module builds can sample partway down it.
/// - **Wide enough to survive the logarithm.** [`excursion_db`] interpolates in
///   `ln f` and divides by `ln(f_hi) − ln(f_lo)`. At 10 kHz that difference
///   rounds to exactly zero below ~9e-16 relative, and the division then
///   returns NaN — a NaN ceiling compares false against every gain, i.e. reads
///   as "no limit" rather than "no authority". One part per billion clears
///   that floor by six orders of magnitude.
const COUPLER_CUTOFF_STEP: f64 = 1e-9;

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

/// The path ceiling on boost Q. REW's gain-dependent cap
/// (`Q_max = 0.227·f₀/A`) is applied unconditionally on top of this and is
/// not a policy choice — `decide()` takes the min of the two.
///
/// Deliberately NOT `PartialOrd`. The spec gives `q_cap` the domain
/// `Range 1.0..=20.0` "on the ceiling" — a range over a scalar, not over this
/// enum — so a `Domain<QCapPolicy>::Range` cannot answer `contains` for the
/// room's own `LogLinear` value. An ordering here would answer it `false`
/// rather than leaving the ambiguity visible. OPEN for the owner: either
/// `q_cap`'s domain is `Choice` over the two path policies, or `QCapPolicy`
/// splits into a decided ceiling scalar plus a profile-owned shape.
///
/// **Moved here from `paraeq_decide::decisions` (Stage 6), and `paraeq-decide`
/// re-exports it** — the move Stage 5 already made for [`AuthorityCurve`], for
/// the same reason: `decision-engine-design.md` § Authority says `decide()`
/// "must not re-specify different numbers", and a policy owned by the crate
/// that composes it cannot drift from the crate that consumes it. The variants,
/// their fields and their externally-tagged wire form are unchanged, so
/// `{"Ceiling":5.0}` / `{"LogLinear":{…}}` still read and write identically and
/// no fixture or persisted profile moves with the type. The "min of the two" the
/// first paragraph names is [`max_q_for_boost_capped`]. The owner's `Choice`
/// option above is the one taken by
/// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` § D-C, applied on
/// the `paraeq-decide` side where the domain lives.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum QCapPolicy {
    /// A flat ceiling (coupler: 5.0). The drawer's `Range 1.0..=20.0` writes
    /// this variant.
    Ceiling(f64),
    /// Log-linear in frequency between two `(hz, q)` breakpoints. The room
    /// path: 10.0 @ 200 Hz → 3.0 @ 10 kHz.
    LogLinear { hi: (f64, f64), lo: (f64, f64) },
}

impl QCapPolicy {
    /// The path ceiling on boost Q at `f0`, interpolated in `log f` between the
    /// breakpoints and clamped to the end values outside them — the same
    /// interpolation law [`excursion_db`] and [`AuthorityCurve::at`] use.
    ///
    /// Linear in `log f`, not in `f`: a linear-`f` ramp hits both breakpoints
    /// and is wrong everywhere between them (at the geometric midpoint of the
    /// room ceiling it answers 9.13 where the spec's ramp answers 6.5, which is
    /// 2.6 Q of extra ringing licensed by an implementation detail).
    pub fn ceiling_at(&self, f0: f64) -> f64 {
        if !self.is_well_formed() {
            // [`validate_policy`] refuses a malformed ceiling, so this is a
            // direct-caller guard. Degrading to the global clamp says out loud
            // what a NaN would do silently: `f64::min` discards a NaN operand,
            // so the path would stop restricting anything either way.
            return *Q_CLAMP.end();
        }
        match *self {
            Self::Ceiling(q) => q,
            Self::LogLinear {
                hi: (f_hi, q_hi),
                lo: (f_lo, q_lo),
            } => {
                if !f0.is_finite() {
                    // A frequency that cannot be placed gets the TIGHTEST
                    // ceiling this policy names. The permissive NaN guard in
                    // [`AuthorityCurve::locate`] is deliberately the other way
                    // round; the fail-safe direction for a Q ceiling is down,
                    // because it is a ringing guard and the loose end of the
                    // room ramp is over three times the tight end.
                    return q_lo.min(q_hi);
                }
                if f0 <= f_lo {
                    return q_lo;
                }
                if f0 >= f_hi {
                    return q_hi;
                }
                let t = (f0.ln() - f_lo.ln()) / (f_hi.ln() - f_lo.ln());
                q_lo + t * (q_hi - q_lo)
            }
        }
    }

    /// Positive requirements throughout, so a NaN refuses rather than passing:
    /// a non-finite ceiling compares false against every Q, which reads as "no
    /// limit" rather than "no authority".
    fn is_well_formed(&self) -> bool {
        match *self {
            Self::Ceiling(q) => q.is_finite() && q > 0.0,
            Self::LogLinear {
                hi: (f_hi, q_hi),
                lo: (f_lo, q_lo),
            } => {
                f_lo.is_finite()
                    && f_lo > 0.0
                    && f_hi.is_finite()
                    && f_hi > f_lo
                    && q_lo.is_finite()
                    && q_lo > 0.0
                    && q_hi.is_finite()
                    && q_hi > 0.0
            }
        }
    }
}

/// The coupler path's Q ceiling: a flat 5.0.
/// `decision-engine-design.md` § Decision table, `q_cap`: "the path ceiling:
/// 5.0 coupler; room `10.0 @ 200 Hz → 3.0 @ 10 kHz` log-linear. Take the min."
pub const COUPLER_Q_CEILING: QCapPolicy = QCapPolicy::Ceiling(5.0);

/// The room path's Q ceiling: 10.0 at 200 Hz falling log-linearly to 3.0 at
/// 10 kHz. See [`COUPLER_Q_CEILING`] for the spec sentence both come from.
pub const ROOM_Q_CEILING: QCapPolicy = QCapPolicy::LogLinear {
    hi: (10_000.0, 3.0),
    lo: (200.0, 10.0),
};

/// Half the width of the EGD test band, in octaves. A 1/3-octave band centred
/// at `f_c` spans `f_c·2^(−1/6) .. f_c·2^(+1/6)`, i.e. `0.2315·f_c` wide.
///
/// Numerically equal to [`DEFAULT_MIN_DIP_WIDTH_OCT`] and semantically
/// unrelated to it: that one is the *narrow-dip veto* width, this one is the
/// *analysis window* the EGD flatness test averages over. Two spec sentences,
/// two constants, so a ruling on either moves only its own.
const EGD_BAND_HALF_OCT: f64 = 1.0 / 6.0;

/// The EGD flatness threshold, as a fraction of the centre frequency's period:
/// a band is flat when its peak-to-peak excess group delay is under
/// `EGD_FLATNESS_PERIODS / f_c` seconds.
///
/// `decision-engine-design.md` § Authority 2, verbatim: "peak-to-peak EGD
/// deviation within the band `< 1/(4·f_c)` (a quarter period — self-scaling:
/// 5 ms at 50 Hz, 0.5 ms at 500 Hz)". `0.25` is that `1/4`, named once so the
/// owner's ruling costs one line.
///
/// **OPEN \[OWNER + NEEDS DATA\].** `docs/decisions/2026-07-21-decision-engine-open-questions.md`
/// § Q4 ships this as a *starting value*: the framing is settled (a quarter
/// period is a 90° excess-phase cap; 180° can invert polarity within the band,
/// 1/8 rejects real modal peaks whose EGD is never perfectly zero), but the
/// exact multiplier "needs rooms". The retune signal, from that section:
///
/// - **Loosen toward `1/(2·f_c)`** if the gate keeps vetoing bass peaks that
///   are otherwise obviously correctable (low σ, clean single-pole shape) and a
///   re-measure after a manual boost shows the multi-position average improved
///   with no headroom-limiter trip.
/// - **Tighten toward `1/(8·f_c)`** if a passing band, once boosted, fails to
///   lift the multi-position average, trips the excursion limiter, or adds
///   audible ringing.
/// - **Cross-check against σ(f):** "in an untreated room the EGD gate and the
///   σ(f) ceiling should flag the same nulls; systematic disagreement is the
///   signal to retune."
///
/// That cross-check is why [`egd_flat_mask`] ships in v1 even though
/// [`EgdGate`] does not — the owner cannot observe the disagreement without the
/// mask. What is deferred is *acting* on it.
pub const EGD_FLATNESS_PERIODS: f64 = 0.25;

/// Whether the excess-group-delay gate is armed. **Ships [`EgdGate::Off`].**
///
/// `measurement-suite-design.md` § Out of Scope, verbatim: "**Excess-group-delay
/// authority masking** — **v1.1**, not v1. It is the sharpest differentiator,
/// and it is still deferred: three cheaper guards cover the same failure in v1
/// (RMS averaging refuses nulls structurally, σ(f) flags them as
/// position-dependent, asymmetric cut/boost never fills them). Clean seam:
/// `fir.rs`'s `minimum_phase_homomorphic` already provides the reference."
///
/// Five specs say the *masking* is v1.1 and one says v1; the decision record
/// (`2026-07-21`, § Q4) is both the majority side and the later document:
/// "σ(f) (Q5) is the **primary** shipping authority; EGD is the deferred
/// refinement, and the two are belt-and-braces." So the **trace** ships in v1
/// as Evidence ([`crate::fr::excess_group_delay_s`], reaching `paraeq-decide`
/// as `Analysis::excess_group_delay_s`), the **gate** ships in v1.1, and this
/// flag — not the presence of a mask argument — is what separates them.
///
/// The flag lives on the policy rather than being implied by
/// [`build_authority_gated`]'s mask argument on purpose: a caller that computes
/// the mask for the owner's σ-vs-EGD cross-check must be able to compute it
/// *without* changing what the corrector does.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum EgdGate {
    /// v1: the mask is ignored, `build_authority_gated` is `build_authority`.
    #[default]
    Off,
    /// v1.1: bands the mask marks non-flat get `max_boost_db = 0`.
    /// `periods` is the flatness threshold — [`EGD_FLATNESS_PERIODS`] is the
    /// shipped starting value, and it is carried here rather than read from the
    /// constant so that the owner's retune is a policy value, not a recompile.
    On { periods: f64 },
}

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
    /// The path ceiling on boost Q, composed with the gain-dependent cap by
    /// [`max_q_for_boost_capped`]. Default [`ROOM_Q_CEILING`].
    ///
    /// Last of the v1 fields, because those are in the spec's composition order
    /// and the Q ceiling is the last step of it.
    pub q_ceiling: QCapPolicy,
    /// Whether [`build_authority_gated`] applies the EGD mask. Default
    /// [`EgdGate::Off`] — the v1 contract.
    ///
    /// After [`Self::q_ceiling`] because it is not part of the v1 composition
    /// order at all: it is the v1.1 addition, appended so every existing
    /// `..Self::default()` literal still reads top to bottom in the spec's
    /// order.
    pub egd_gate: EgdGate,
}

impl Default for AuthorityPolicy {
    /// The room path. [`AuthorityPolicy::room`] says so by name; this impl
    /// exists because the struct-update syntax in every caller needs it.
    fn default() -> Self {
        Self {
            excursion: DEFAULT_EXCURSION_DB.to_vec(),
            sigma_full_db: SIGMA_FULL_DB,
            sigma_none_db: SIGMA_NONE_DB,
            boost_ratio: DEFAULT_BOOST_RATIO,
            min_dip_width_oct: DEFAULT_MIN_DIP_WIDTH_OCT,
            q_ceiling: ROOM_Q_CEILING,
            egd_gate: EgdGate::Off,
        }
    }
}

impl AuthorityPolicy {
    /// The coupler path: [`COUPLER_EXCURSION_DB`] (zero above
    /// [`COUPLER_CUTOFF_HZ`]) and [`COUPLER_Q_CEILING`].
    ///
    /// Named constructors rather than a `for_path(CouplingPath)`: `CouplingPath`
    /// lives in `paraeq-decide`, and this crate may not depend on it. The two
    /// paths differ in exactly these two fields, so `decide()` picks a
    /// constructor instead of re-specifying numbers the spec says it must not.
    pub fn coupler() -> Self {
        Self {
            excursion: COUPLER_EXCURSION_DB.to_vec(),
            q_ceiling: COUPLER_Q_CEILING,
            ..Self::default()
        }
    }

    /// The room path: [`DEFAULT_EXCURSION_DB`] and [`ROOM_Q_CEILING`]. Equal to
    /// [`AuthorityPolicy::default`]; see [`AuthorityPolicy::coupler`].
    pub fn room() -> Self {
        Self::default()
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
        // `j - 1` underflows. It resolves to the first bin — which is the most
        // PERMISSIVE answer on this curve, since the excursion envelope is
        // widest at the lowest frequency. That is acceptable only because it
        // is unreachable in practice: `clamp_band` refuses a non-finite band
        // before calling this, and `auto_fit_room` only ever queries grid
        // frequencies. It is a panic guard, not a safety policy — do not start
        // relying on it as one.
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

impl AuthorityAt {
    /// Whether the σ gate — rather than the excursion envelope — is what
    /// lowered the ceiling here.
    ///
    /// The attribution rule, in one place, because two callers need it and a
    /// second copy is a second answer: [`clamp_band`] and
    /// [`crate::autofit::auto_fit_room`]'s cumulative-headroom clamp both label
    /// their `Clamp`s with it. Compared with a relative tolerance because both
    /// sides are products of interpolations, so exact equality would
    /// misattribute on roundoff.
    pub fn throttled_by_sigma(&self) -> bool {
        self.max_cut_db < self.excursion_db * (1.0 - 1e-12)
    }

    /// Whether the envelope licenses ANY correction here, of either sign.
    ///
    /// The band predicate, named. It is the second half of
    /// [`authority_band_mask`]'s definition — the spec's
    /// `{ f : authority.at(f).max_boost_db > 0 || max_cut_db > 0 }` — and it
    /// exists as a method because a second caller needs the per-frequency
    /// question without a range:
    /// `autofit::cut_limit_db` has to tell "a region we declined to correct"
    /// apart from "a region whose ceiling happens to be low", and re-spelling
    /// the disjunction there would be the second copy
    /// `the_authority_band_mask_is_the_same_function_the_gate_and_the_stop_both_use`
    /// exists to prevent.
    pub fn licenses_correction(&self) -> bool {
        self.max_boost_db > 0.0 || self.max_cut_db > 0.0
    }

    /// The `Clamp` variant describing a gain change at this frequency.
    pub fn gain_clamp(&self, from: f64, to: f64) -> Clamp {
        if self.throttled_by_sigma() {
            Clamp::GainToSigma {
                from,
                to,
                sigma_db: self.sigma_db,
            }
        } else {
            Clamp::GainToExcursion { from, to }
        }
    }
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
    // The v1 path, and the whole of it: no evidence, therefore no gate, and
    // `build_authority_gated` with `None` is this function line for line.
    build_authority_gated(grid, sigma_db, None, policy)
}

/// [`build_authority`] plus the **v1.1** excess-group-delay gate.
///
/// `egd_flat` is [`egd_flat_mask`]'s answer on the same grid: `true` where the
/// region is flat enough in excess group delay to be treated as locally
/// minimum-phase, and therefore boostable. The mask is applied **iff**
/// `policy.egd_gate` is [`EgdGate::On`] **and** `egd_flat` is `Some`; where it
/// applies and the bin is `false`, `max_boost_db` becomes `0.0` and `max_q` is
/// recomputed at that zero boost. `max_cut_db` is never touched — the spec
/// gates boosts only (`decision-engine-design.md` § Authority 2, "Excess-group-delay
/// gate (**boost only**) … Bands failing the test get `boost_ceiling = 0`").
///
/// **Two independent conditions, on purpose.**
///
/// - A mask alone can never arm the gate. That is [`EgdGate`]'s whole job, and
///   it is why the flag lives on the policy: the owner's σ-vs-EGD cross-check
///   (`2026-07-21` § Q4) computes the mask on every bundle in v1 and must not
///   change a single ceiling by doing so.
/// - Missing evidence never silently vetoes. `None` is an all-true mask, not an
///   all-false one. σ(f) is the primary shipping authority, and a fail-closed
///   EGD arm would zero boost on every bundle whose IR could not be derotated —
///   a silent, global behaviour change caused by *absent* data.
///
/// With `None` or an all-`true` mask the result is **bit-for-bit**
/// [`build_authority`]'s, which `tests/test_authority.rs` pins with `to_bits()`
/// rather than `==`.
///
/// # Errors
///
/// [`build_authority`]'s errors, plus `InvalidInput` when `egd_flat` is `Some`
/// and does not match the grid. That length is checked in **both** gate states:
/// a mask of the wrong shape is a caller bug either way, and refusing it while
/// the gate is `Off` is what stops it lying dormant until the owner arms the
/// flag in v1.1.
pub fn build_authority_gated(
    grid: &LogGrid,
    sigma_db: &[f64],
    egd_flat: Option<&[bool]>,
    policy: &AuthorityPolicy,
) -> Result<AuthorityCurve, DspError> {
    validate_policy(policy)?;
    if sigma_db.len() != grid.len() {
        // The message names `build_authority` in both entry points: that is the
        // shipped constructor, and this function is the same constructor behind
        // a flag, not a second one.
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
    if let Some(mask) = egd_flat {
        if mask.len() != grid.len() {
            return Err(DspError::InvalidInput(format!(
                "build_authority_gated: {} EGD mask values for a {}-point grid",
                mask.len(),
                grid.len()
            )));
        }
    }
    // Resolved once, outside the loop, so the two conditions are visibly ANDed
    // in one place rather than re-tested per bin.
    let gate: Option<&[bool]> = match policy.egd_gate {
        EgdGate::Off => None,
        EgdGate::On { .. } => egd_flat,
    };
    let span = policy.sigma_none_db - policy.sigma_full_db;
    let freqs = grid.freqs().to_vec();
    let mut excursion = Vec::with_capacity(freqs.len());
    let mut max_boost_db = Vec::with_capacity(freqs.len());
    let mut max_cut_db = Vec::with_capacity(freqs.len());
    let mut max_q = Vec::with_capacity(freqs.len());
    for (i, (&f, &sigma)) in freqs.iter().zip(sigma_db).enumerate() {
        let e = excursion_db(&policy.excursion, f);
        let w = ((policy.sigma_none_db - sigma) / span).clamp(0.0, 1.0);
        let cut = e * w;
        let mut boost = policy.boost_ratio * cut;
        if let Some(mask) = gate {
            if !mask[i] {
                boost = 0.0;
            }
        }
        excursion.push(e);
        max_cut_db.push(cut);
        // Recomputed from the gated boost, not the ungated one, so the curve's
        // documented invariant `max_q[i] == max_q_for_boost(freqs[i],
        // max_boost_db[i])` survives the gate. A zeroed boost gets the LOOSEST
        // cap, which costs nothing: no boost can be placed there to use it.
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

/// Which bins of `grid` sit in a region flat enough in excess group delay to be
/// treated as locally minimum-phase — **the evidence the v1.1 gate consumes,
/// computed in v1.**
///
/// `decision-engine-design.md` § Authority 2, verbatim: "The test, per
/// 1/3-octave band centred at `f_c`: peak-to-peak EGD deviation within the band
/// `< 1/(4·f_c)` (a quarter period — self-scaling: 5 ms at 50 Hz, 0.5 ms at
/// 500 Hz)." `periods` is that `1/4`; see [`EGD_FLATNESS_PERIODS`].
///
/// # What the caller must do first
///
/// `egd_s` is the excess-group-delay trace **on `grid`**, one value per bin, in
/// seconds. [`crate::fr::excess_group_delay_s`] produces it on the LINEAR rfft
/// axis — "Resampling onto a log grid happens afterwards, in the caller, never
/// inside the derivative" — so the caller bridges the two with
/// [`crate::logf::resample_db_to_log_grid`], the crate's one resampler
/// (`Prefilter::None` is plain `np.interp`; the `_db` in its name is a
/// misnomer here, it resamples any scalar trace). **No resampler is added
/// here**: a second one would be a second answer to "what is the EGD at this
/// bin", which is exactly the drift `logf` exists to prevent.
///
/// # Why a `Vec<bool>` and not a `Result`
///
/// It follows [`authority_band_mask`]: a mask that cannot be computed grades
/// **nothing** as flat rather than grading everything. All-`false` is returned
/// when `egd_s` does not match the grid or `periods` is not finite and
/// positive, and one bin is `false` when any EGD sample in its band is
/// non-finite. That is the opposite of [`build_authority_gated`]'s `None` —
/// deliberately, and the two are not in tension: `None` there means *no
/// evidence was offered*, while a malformed slice here means *evidence was
/// offered and is unusable*. The first must not veto; the second must not
/// license.
pub fn egd_flat_mask(grid: &LogGrid, egd_s: &[f64], periods: f64) -> Vec<bool> {
    let freqs = grid.freqs();
    if egd_s.len() != freqs.len() || !(periods.is_finite() && periods > 0.0) {
        return vec![false; freqs.len()];
    }
    let lo_ratio = 2f64.powf(-EGD_BAND_HALF_OCT);
    let hi_ratio = 2f64.powf(EGD_BAND_HALF_OCT);
    freqs
        .iter()
        .map(|&f_c| {
            if !f_c.is_finite() || f_c <= 0.0 {
                return false;
            }
            // `LogGrid` is strictly increasing by construction, so the band is
            // one contiguous slice and a binary search finds its ends.
            let first = freqs.partition_point(|&f| f < f_c * lo_ratio);
            let last = freqs.partition_point(|&f| f <= f_c * hi_ratio);
            let band = &egd_s[first..last];
            if band.iter().any(|e| !e.is_finite()) {
                return false;
            }
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for &e in band {
                if e < lo {
                    lo = e;
                }
                if e > hi {
                    hi = e;
                }
            }
            // `f_c` is one of the band's own members, so an empty band is
            // impossible and this subtraction is always a real peak-to-peak.
            // The comparison is written positively: a NaN that somehow reached
            // it answers `false`, i.e. "not flat", never "flat".
            hi - lo < periods / f_c
        })
        .collect()
}

/// Which bins of `grid` are inside **the authority band**, the one definition
/// of that phrase.
///
/// `decision-engine-design.md` § Refusal table, verbatim: the band is
/// `correction_range ∩ { f : authority.at(f).max_boost_db > 0 || max_cut_db > 0 }`,
/// taken on the analysis grid. Both edges of `correction_range` are inclusive,
/// matching the `Range` domains the spec's decision table gives it.
///
/// **This function exists so the phrase has one meaning.** Two consumers need
/// it — `autofit`'s residual-RMS stopping criterion and `decide()`'s
/// verification gate — and two spec sections use the phrase without defining
/// it (`decision-engine-design.md` § Decision table, `max_filters`, and the
/// refusal row above). A second open-coded filter would be a second answer to
/// "did the correction land", which is the one question the product refuses on.
/// `tests/test_authority.rs` enforces the single copy by scanning the
/// workspace's library sources for the predicate.
///
/// Returns all-`false` for a `correction_range` that is non-finite or inverted,
/// and `false` for any non-finite grid frequency: a band that cannot be placed
/// grades nothing, rather than grading everything. There is no `Result` because
/// the callers' alternative to an empty band is a refusal they already emit.
pub fn authority_band_mask(
    grid: &[f64],
    curve: &AuthorityCurve,
    correction_range: (f64, f64),
) -> Vec<bool> {
    let (low_hz, high_hz) = correction_range;
    if !(low_hz.is_finite() && high_hz.is_finite() && low_hz <= high_hz) {
        return vec![false; grid.len()];
    }
    grid.iter()
        .map(|&f| {
            if !f.is_finite() || f < low_hz || f > high_hz {
                return false;
            }
            curve.at(f).licenses_correction()
        })
        .collect()
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

/// The whole Q ceiling on a boost: the gain-dependent [`max_q_for_boost`] cap,
/// the path ceiling, and the global [`Q_CLAMP`] — the min of the three.
///
/// `decision-engine-design.md` § Decision table, `q_cap`: "Boosts only:
/// `Q_max = 0.227·f₀/A`, `A = 10^(G/40)`. **And** the path ceiling: 5.0 coupler;
/// room `10.0 @ 200 Hz → 3.0 @ 10 kHz` log-linear. Take the min." Until this
/// function existed the path ceiling was applied nowhere, and the only
/// alternative was `decide()` re-specifying numbers the same spec forbids it to
/// re-specify.
///
/// **Additive, and [`clamp_band`] is deliberately not rewritten to call it.**
/// `clamp_band` is the Stage-5 per-band clamp: it is handed a curve, not a
/// policy, so it has no path to apply and composes the other two terms exactly
/// as this function does. Routing it through here would change the Q of every
/// band the shipped path already clamps — a behaviour change dressed as a
/// refactor. `tests/test_authority.rs` pins the two compositions equal wherever
/// the path ceiling is not the binding term, so they cannot drift apart while
/// waiting for the caller that passes the policy down.
pub fn max_q_for_boost_capped(f0: f64, gain_db: f64, q_ceiling: &QCapPolicy) -> f64 {
    max_q_for_boost(f0, gain_db)
        .min(q_ceiling.ceiling_at(f0))
        .clamp(*Q_CLAMP.start(), *Q_CLAMP.end())
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
///
/// **The derive is pre-freeze, not a convenience.** `CorrectionPlan` gains
/// `clamps: Vec<Vec<Clamp>>` (base plan B6 item 4) and derives
/// `Deserialize`/`Serialize` unconditionally, and `paraeq-decide` turns this
/// crate's `serde` feature on unconditionally — so from that point on the
/// variant set and the field names of this enum ARE a wire format, and every
/// `fixtures/decide/<case>/expected.json` moves when either changes. Both the
/// derive and [`Self::BelowMinGain`] land in one commit, before the fixture
/// freeze, for that reason.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum Clamp {
    /// Emitted by [`crate::autofit::auto_fit_room`], not by [`clamp_band`]:
    /// the best feature left in the residual was smaller than `min_gain_db`,
    /// so no band was placed on it at all.
    ///
    /// `decision-engine-design.md` § Decision table, `max_filters`: "Drop any
    /// band with `|gain| < flatness/2`", and `min_gain_db` is that
    /// `flatness/2`. Without this variant the drop is **silent**, which
    /// contradicts [`clamp_band`]'s own premise that every change is
    /// explainable — the drawer needs to be able to say "we found a 1.2 dB
    /// bump and left it alone because you asked for 3 dB flat". `fc` and
    /// `gain_db` are what was found, not what was emitted.
    BelowMinGain { fc: f64, gain_db: f64 },
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
        clamps.push(at.gain_clamp(band.gain_db, gain));
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
    if !policy.q_ceiling.is_well_formed() {
        return Err(DspError::InvalidInput(format!(
            "AuthorityPolicy: q_ceiling needs finite positive Q and, for \
             LogLinear, strictly increasing finite positive breakpoints, got {:?}",
            policy.q_ceiling
        )));
    }
    // Unreachable on the v1 path: `EgdGate::Off` carries no number, so this arm
    // can only fire for a policy that armed the v1.1 gate. Positive requirement
    // like every other check here — a NaN threshold would make every
    // peak-to-peak comparison false, i.e. veto EVERY boost, which is the one
    // failure the gate is least likely to be blamed for.
    if let EgdGate::On { periods } = policy.egd_gate {
        if !(periods.is_finite() && periods > 0.0) {
            return Err(DspError::InvalidInput(format!(
                "AuthorityPolicy: egd_gate periods must be finite and > 0, got {periods}"
            )));
        }
    }
    Ok(())
}

/// Re-exported so `autofit.rs` and downstream crates can reach the identity
/// row without importing `biquad` for one constant.
pub use crate::biquad::IDENTITY;
