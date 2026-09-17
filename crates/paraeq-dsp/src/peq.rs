//! Parametric EQ container + AutoEQ text export.
//! Oracle: prototype/paraeq/correction/parametric_eq.py
//!
//! Test tier: 1 + 2 + 3. Tier 1 is the oracle transcriptions —
//! `ParametricEQ::frequency_response`, `export_autoeq_format` and
//! `parse_autoeq`, pinned byte-for-byte against `fixtures/peq/two_band` and
//! `fixtures/autoeq_parser.json`. **Those stay frozen.** The realized-cascade
//! additions below declare their own tiers, one per function, because a `pub`
//! DSP function with no declared tier is exactly what CLAUDE.md's rule exists
//! to prevent:
//!
//! - [`sosfilt`] — **Tier 2**, delegate `scipy.signal.sosfilt(sos, x, zi=None)`
//!   (`fixtures/peq/sosfilt_offline`). It takes an EXPLICITLY supplied SOS array
//!   and no `self`, which is what makes it reachable from a fixture at all.
//! - [`ParametricEQ::realized_sos`] — **Tier 3.** The `biquad::IDENTITY`
//!   substitution for rows failing `biquad::is_stable` is OUR policy
//!   (`DIVERGENCES.md` #14, mirroring `build_iir`'s R1-3 funnel), not scipy's,
//!   and `biquad`'s designers are already Tier-1 pinned — so the fold has
//!   nothing left to delegate to and an analytic test is the honest oracle.
//! - [`ParametricEQ::realized_response`] — **Tier 3 (composition).** The
//!   magnitude kernel is already Tier-1 fixture-pinned through
//!   `frequency_response`; what this composes it with is the Tier-3 policy fold
//!   above, at a CALLER-SUPPLIED rate. Declaring it Tier 2 with a scipy
//!   reference it cannot consume would be the false "a delegate exists" claim
//!   `authority.rs` forbids. `frequency_response` is NOT refactored to share a
//!   kernel with it — that would edit a frozen function for no behavioural
//!   gain; two tests pin the agreement and the disagreement instead.
//! - [`ParametricEQ::apply_offline`] — **Tier 3 (composition).** Exactly
//!   `sosfilt(&self.realized_sos(rate_hz), x)`: a Tier-2-graded primitive
//!   composed with a Tier-3-declared policy fold. Both halves are graded; the
//!   composition itself has no independent delegate, so three tests pin it.
//! - [`ParametricEQ::preamp_grid`] — **Tier 3.** The grid is R1-1's own
//!   decision (quoted on [`ParametricEQ::preamp_db`]), so there is nothing to
//!   delegate to.

use crate::biquad;
use crate::DspError;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// snake_case is the WIRE FORMAT, not a style choice: the desktop UI's
// hand-written contract pins `"high_shelf" | "low_shelf" | "notch" | "peaking"`
// (desktop/ui/src/ipc/types.ts), the same spelling `as_str`/`from_str` and the
// AutoEQ fixtures use. Renaming these variants renames the JSON.
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum FilterType {
    HighShelf,
    LowShelf,
    Notch,
    Peaking,
}

impl FilterType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterType::HighShelf => "high_shelf",
            FilterType::LowShelf => "low_shelf",
            FilterType::Notch => "notch",
            FilterType::Peaking => "peaking",
        }
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<FilterType, DspError> {
        match s {
            "high_shelf" => Ok(FilterType::HighShelf),
            "low_shelf" => Ok(FilterType::LowShelf),
            "notch" => Ok(FilterType::Notch),
            "peaking" => Ok(FilterType::Peaking),
            other => Err(DspError::InvalidInput(format!(
                "unknown filter type {other}"
            ))),
        }
    }
    /// AutoEQ type tag (parametric_eq.py _AUTOEQ_FILTER_NAMES).
    pub fn autoeq_tag(&self) -> &'static str {
        match self {
            FilterType::HighShelf => "HSC",
            FilterType::LowShelf => "LSC",
            FilterType::Notch => "NO",
            FilterType::Peaking => "PK",
        }
    }
}

// PartialEq: paraeq-decide's DecisionSet compares whole plans (the spec's
// idempotence property — overriding a decision to the value auto already chose
// must change nothing but its `source`).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct EQBand {
    pub filter_type: FilterType,
    pub fc: f64,
    pub gain_db: f64,
    pub q: f64,
}

impl EQBand {
    pub fn to_sos(&self, sample_rate: f64) -> [f64; 6] {
        match self.filter_type {
            FilterType::HighShelf => biquad::high_shelf(self.fc, self.gain_db, self.q, sample_rate),
            FilterType::LowShelf => biquad::low_shelf(self.fc, self.gain_db, self.q, sample_rate),
            FilterType::Notch => biquad::notch(self.fc, self.q, sample_rate),
            FilterType::Peaking => biquad::peaking(self.fc, self.gain_db, self.q, sample_rate),
        }
    }
}

/// A parsed AutoEQ/EqualizerAPO ParametricEq preset: the preamp gain plus the
/// ON filter bands. Mirrors the oracle `ParsedPreset` (autoeq_db.py:48-51).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ParsedPreset {
    pub bands: Vec<EQBand>,
    pub preamp_db: f64,
}

fn preamp_re() -> &'static Regex {
    // autoeq_db.py:23 — `Preamp\s*:\s*([-\d.]+)\s*dB`, IGNORECASE, searched.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)Preamp\s*:\s*([-\d.]+)\s*dB").unwrap())
}

fn filter_re() -> &'static Regex {
    // autoeq_db.py:24-27 — only literal `ON` matches; the filter number and any
    // trailing text are ignored; IGNORECASE, searched (not anchored).
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)Filter\s+\d+\s*:\s*ON\s+(\w+)\s+Fc\s+([\d.]+)\s+Hz\s+Gain\s+([-\d.]+)\s+dB\s+Q\s+([\d.]+)",
        )
        .unwrap()
    })
}

/// AutoEQ short code → filter type. Accepts both EqualizerAPO shelf spellings
/// (LSC/HSC) and the legacy LS/HS aliases; unknown codes fall back to peaking.
/// Mirrors autoeq_db.py:30-37 + the `.get(short.upper(), "peaking")` default.
fn filter_type_from_code(code: &str) -> FilterType {
    match code.to_ascii_uppercase().as_str() {
        "HS" | "HSC" => FilterType::HighShelf,
        "LS" | "LSC" => FilterType::LowShelf,
        "NO" => FilterType::Notch,
        "PK" => FilterType::Peaking,
        _ => FilterType::Peaking,
    }
}

/// Parse AutoEQ ParametricEq text into a [`ParsedPreset`]. Infallible: zero
/// bands is a valid result (callers decide whether that's a warning).
///
/// Oracle: autoeq_db.py:82-110 `parse_parametric_eq`. Preamp defaults to 0.0
/// when absent; two Preamp lines are last-wins (each match overwrites and the
/// line is consumed via `continue`). Only `ON` filter lines are captured;
/// non-matching lines are skipped without error.
pub fn parse_autoeq(text: &str) -> ParsedPreset {
    let mut preamp_db = 0.0;
    let mut bands = Vec::new();
    for line in text.lines() {
        if let Some(caps) = preamp_re().captures(line) {
            preamp_db = caps[1].parse().unwrap_or(0.0);
            continue;
        }
        if let Some(caps) = filter_re().captures(line) {
            bands.push(EQBand {
                filter_type: filter_type_from_code(&caps[1]),
                fc: caps[2].parse().unwrap_or(0.0),
                gain_db: caps[3].parse().unwrap_or(0.0),
                q: caps[4].parse().unwrap_or(0.0),
            });
        }
    }
    ParsedPreset { bands, preamp_db }
}

pub struct ParametricEQ {
    pub bands: Vec<EQBand>,
    pub sample_rate: f64,
}

impl ParametricEQ {
    pub fn combined_sos(&self) -> Vec<[f64; 6]> {
        self.bands
            .iter()
            .map(|b| b.to_sos(self.sample_rate))
            .collect()
    }

    /// parametric_eq.py:79-90 — ParametricEQ.frequency_response:
    ///   if not self.bands: return np.zeros_like(freqs)
    ///   sos = self.combined_sos()
    ///   mag_db = biquad_frequency_response(sos, freqs, self.sample_rate)
    /// biquad_frequency_response computes a single cascaded |H| (product across
    /// all SOS sections via sosfreqz) and applies ONE +1e-10 floor to that
    /// product — not a per-band sum of per-band dB responses. That is exactly
    /// what `sos_frequency_response_db` does, so this is a direct call. The
    /// empty-bands special case is transcribed explicitly because the generic
    /// cascade (empty product = unity gain, then +1e-10 floor) would return
    /// 20*log10(1 + 1e-10) =/= 0.0 exactly, whereas Python special-cases it to
    /// return exact zeros.
    pub fn frequency_response(&self, freqs: &[f64]) -> Vec<f64> {
        if self.bands.is_empty() {
            return vec![0.0; freqs.len()];
        }
        biquad::sos_frequency_response_db(&self.combined_sos(), freqs, self.sample_rate)
    }

    /// Preamp for this band set, in dB: `-max(0, peak of the realized
    /// cascade)`. Never positive — a pure-cut EQ gets exactly 0.0, not a
    /// boost (DIVERGENCES.md #14: AutoEQ's `−max_gain` is signed; ours is
    /// clamped). No headroom constant — AutoEQ's ParametricEQ.txt convention
    /// is exactly `−max_gain` (its PREAMP_HEADROOM applies only to the
    /// GraphicEQ/FIR outputs). No oracle counterpart: the prototype hardcodes
    /// "Preamp: 0.0 dB" (2026-07-15 engine-hardening spec, R1-1).
    ///
    /// The peak is over the realized cascade — what the engine's stability
    /// funnel actually installs: rows failing [`biquad::is_stable`] are
    /// evaluated as identity, mirroring `build_iir`'s R1-3 substitution.
    /// (Without the mirror, one `q = 0`/NaN design turns the whole response
    /// NaN, the max fold discards NaN, and the surviving boosts get zero
    /// headroom — fail-unsafe in exactly the direction the preamp exists to
    /// prevent.) Not the per-band gain sum — overlapping boosts are
    /// superadditive, boosts against cuts subadditive. Grid (decided in
    /// R1-1): sorted union of a 1/48-octave log grid over
    /// [1.0, 0.499·sample_rate], every band's fc clamped into that range (a
    /// peaking band's maximum sits at its fc, so the union makes peaks
    /// exact regardless of grid density), and the two endpoints (shelf
    /// maxima sit at DC/Nyquist).
    pub fn preamp_db(&self) -> f64 {
        // sos_frequency_response_db's +1e-10 ADDITIVE magnitude floor lifts
        // a near-unity response by up to ~8.7e-10 dB, which would turn a
        // pure-cut cascade's preamp into a tiny negative number at the top
        // grid endpoint (|H| within ~1e-12 of 1.0 there). Peaks at or below
        // this ceiling are the bias, not a boost: exactly 0.0 by the
        // DIVERGENCES.md #14 convention.
        const BIAS_CEILING_DB: f64 = 1e-8;
        if self.bands.is_empty() {
            return 0.0;
        }
        // The grid and the realized fold both used to live inline here. They
        // are [`Self::preamp_grid`] and [`Self::realized_response`] now, for
        // one reason: the verification residual predicts the installed cascade
        // and MUST use the same two, or it reports an engine fault that is
        // really a prediction fault. Factored out, not copied, so they cannot
        // drift. Behaviour is unchanged — same grid, same substitution, same
        // rate, same fold.
        let peak = self
            .realized_response(&self.preamp_grid(), self.sample_rate)
            .into_iter()
            .fold(f64::NEG_INFINITY, f64::max);
        if peak > BIAS_CEILING_DB {
            -peak
        } else {
            0.0
        }
    }

    /// The analysis grid [`Self::preamp_db`] evaluates on — **Tier 3**, no
    /// oracle: the grid is R1-1's own decision, stated on `preamp_db`.
    ///
    /// The sorted, deduplicated union of a 1/48-octave log grid over
    /// `[1.0, 0.499·sample_rate]`, every band's `fc` clamped into that range,
    /// and the two endpoints.
    ///
    /// Public because a test — and B13's gate-2 recomputation — must be able to
    /// evaluate [`Self::realized_response`] on THE SAME grid `preamp_db` used.
    /// Without that, the two peaks differ by construction and any assertion
    /// relating them is a lottery on grid density.
    pub fn preamp_grid(&self) -> Vec<f64> {
        // .max(1.0) keeps the range non-empty (and fc's clamp valid) for
        // degenerate sample rates; unreachable for real audio rates.
        let f_max = (0.499 * self.sample_rate).max(1.0);
        let mut grid: Vec<f64> = Vec::new();
        for k in 0.. {
            let f = 2f64.powf(f64::from(k) / 48.0);
            if f >= f_max {
                break;
            }
            grid.push(f);
        }
        grid.push(f_max);
        // A peaking band's maximum sits at its fc, so unioning fc in makes the
        // peak exact regardless of grid density — the one feature a plain log
        // grid would lose.
        for band in &self.bands {
            grid.push(band.fc.clamp(1.0, f_max));
        }
        grid.sort_by(f64::total_cmp);
        grid.dedup();
        grid
    }

    /// The cascade the engine ACTUALLY INSTALLS, designed at `rate_hz` —
    /// **Tier 3**, the `biquad::IDENTITY` substitution being our own policy.
    ///
    /// Rows failing [`biquad::is_stable`] are replaced by [`biquad::IDENTITY`],
    /// mirroring `build_iir`'s R1-3 stability funnel, so one `q = 0`/NaN design
    /// leaves the rest of the cascade running instead of turning the whole
    /// response NaN.
    ///
    /// **`rate_hz` is an argument, not `self.sample_rate`, and that is the
    /// point.** R1-6 re-designs at the LIVE stream rate;
    /// `CorrectionPlan::design_rate` is "Provenance only — never the rate the
    /// engine designs at". Predicting at `design_rate` while the engine runs at
    /// another rate manufactures a bilinear-warp residual at HF and blames the
    /// chain for it. [`Self::combined_sos`] hardcodes `self.sample_rate` and is
    /// left alone; this is the variant that takes the rate from its caller.
    pub fn realized_sos(&self, rate_hz: f64) -> Vec<[f64; 6]> {
        self.bands
            .iter()
            .map(|b| {
                let row = b.to_sos(rate_hz);
                if biquad::is_stable(&row) {
                    row
                } else {
                    biquad::IDENTITY
                }
            })
            .collect()
    }

    /// Magnitude response of [`Self::realized_sos`] on `freqs`, dB — **Tier 3
    /// (composition)**.
    ///
    /// The same shape [`Self::frequency_response`] has, differing ONLY in where
    /// the sections come from (realized, at a caller-supplied rate) and
    /// therefore in what it answers: `frequency_response` is the cascade AS
    /// DESIGNED, this is the cascade AS INSTALLED. With every row stable and
    /// `rate_hz == self.sample_rate` the two agree, and a test says so; with one
    /// unstable row they must NOT, and a second test says that.
    ///
    /// This is `H(f)` in the verification level book — the installed bands'
    /// realized cascade magnitude, preamp EXCLUDED. The preamp is added by the
    /// caller as a constant offset; a safety interlock may not be conditioned on
    /// the hypothesis it is testing.
    ///
    /// `frequency_response` is deliberately NOT refactored into a shared kernel:
    /// it is Tier-1 fixture-pinned, and editing frozen code to host a parameter
    /// it never varies buys nothing a test does not.
    pub fn realized_response(&self, freqs: &[f64], rate_hz: f64) -> Vec<f64> {
        if self.bands.is_empty() {
            // Same explicit special case `frequency_response` carries: the
            // generic empty cascade is unity gain plus the +1e-10 floor, i.e.
            // 20*log10(1 + 1e-10) != 0.0 exactly.
            return vec![0.0; freqs.len()];
        }
        biquad::sos_frequency_response_db(&self.realized_sos(rate_hz), freqs, rate_hz)
    }

    /// Filter `x` through the realized cascade at `rate_hz`, zero initial state
    /// — **Tier 3 (composition)**, exactly
    /// `sosfilt(&self.realized_sos(rate_hz), x)`.
    ///
    /// The correction-aware render: what a buffer sounds like after the
    /// correction the engine is running. B13's verification marker template
    /// needs it so the marker it correlates against is the marker the corrected
    /// chain will actually emit.
    ///
    /// Offline and stateless by construction — no `zi` survives a call — so the
    /// output depends on the input alone and never on what was rendered before
    /// it. This is NOT the realtime path; `paraeq-engine` owns that and keeps
    /// its own state.
    pub fn apply_offline(&self, x: &[f64], rate_hz: f64) -> Vec<f64> {
        sosfilt(&self.realized_sos(rate_hz), x)
    }

    /// parametric_eq.py:92-106 — ParametricEQ.export_autoeq_format:
    ///   lines = ["Preamp: 0.0 dB"]
    ///   for i, band in enumerate(self.bands, 1):
    ///       filter_name = _AUTOEQ_FILTER_NAMES[band.filter_type]
    ///       lines.append(
    ///           f"Filter {i}: ON {filter_name} Fc {band.fc:.0f} Hz "
    ///           f"Gain {band.gain_db:.1f} dB Q {band.q:.3f}"
    ///       )
    ///   text = "\n".join(lines)
    ///   return text
    /// Preamp is a fixed literal (not computed). Each filter line: 1-based
    /// index, AutoEQ tag, Fc rounded to 0 decimals, Gain to 1 decimal, Q to 3
    /// decimals. Lines are joined with "\n" and there is no trailing newline.
    pub fn export_autoeq_format(&self) -> String {
        self.export_autoeq_lines(0.0)
    }

    /// [`Self::export_autoeq_format`] with the **computed** preamp
    /// ([`Self::preamp_db`]) in the header instead of the oracle's `0.0`
    /// literal. This is the export a user should actually load into other EQ
    /// software, and the one ParaEQ's own UI must offer.
    ///
    /// Additive rather than a change to `export_autoeq_format`, and the reason
    /// is not timidity: `export_autoeq_format` is pinned byte-for-byte against
    /// `fixtures/peq/two_band`'s `autoeq_export` scalar
    /// (`test_peq.rs::autoeq_export_matches_oracle_exactly`), which is a Tier-1
    /// frozen fixture. Emitting a real preamp from it would require
    /// regenerating a fixture the rescope plan forbids regenerating, to encode
    /// a value the oracle does not compute. So the oracle-parity function keeps
    /// its literal and this one carries the correction.
    ///
    /// The number is `−max(0, peak of the realized cascade)` — see
    /// [`Self::preamp_db`] for why "realized" is load-bearing (after the Q cap,
    /// the excursion clamp and band-dropping, the filters that run are not the
    /// filters autofit first proposed) and why there is no headroom constant.
    /// Formatted to one decimal, matching AutoEQ's own `ParametricEQ.txt`.
    ///
    /// **A preamp that lives only in exported text protects other people's EQ
    /// software and not ParaEQ.** The engine-side half of this is the
    /// controller's, not the Tauri backend's: the computed number is carried as
    /// a `preamp_lin` field **inside** `paraeq_engine::Correction` and applied
    /// on the **corrected path only**, so it swaps atomically with the
    /// correction it protects and leaves the pass-through path un-attenuated.
    ///
    /// It is deliberately **not** sent as a second
    /// `EngineCommand::SetGainDb(preamp_db)` alongside `SetCorrection`, which is
    /// what an earlier draft of the decision-engine spec and of this comment
    /// said. `gain_bits` is read once and applied on *both* chain paths, so that
    /// carrier would leave the bypassed side quieter than the corrected side by
    /// the whole preamp — up to ~10 dB of silent bias in the product's headline
    /// A/B control — and the correction and the gain would swap
    /// non-atomically, opening a window of un-preamped boost. The user's manual
    /// trim keeps `SetGainDb` (`engine_set_preamp_db` in
    /// `desktop/src-tauri/src/commands.rs`, both paths, persisted); the two
    /// compose, multiplying in linear and adding in dB. See
    /// `docs/specs/2026-07-15-engine-hardening-design.md` R1-1 and
    /// `docs/decisions/2026-09-16-post-merge-and-stage6-calls.md` section D-1.
    pub fn export_autoeq_format_with_preamp(&self) -> String {
        self.export_autoeq_lines(self.preamp_db())
    }

    /// [`Self::export_autoeq_format`] with a **caller-supplied** preamp in the
    /// header — neither the oracle's `0.0` literal nor the cascade-derived
    /// [`Self::preamp_db`], but whatever number the caller hands over.
    ///
    /// **No production caller today.** It was written for the desktop's
    /// `eq_export_autoeq`, which used to export the number the user typed and
    /// the engine was running at (`SetGainDb`). R1-1 repointed that command to
    /// [`Self::export_autoeq_format_with_preamp`]: the exported preamp is now
    /// the CASCADE-DERIVED number, because engine-hardening `:117` requires
    /// the exported text to equal `EngineState.auto_preamp_db`, so other
    /// people's EQ software gets the headroom ParaEQ absorbs internally. This
    /// variant is kept for the caller that supplies its own number (the
    /// daemon seam); every remaining call site is a test.
    ///
    /// Shares the `-0.0` guard with the other two exports, and only that: a
    /// preamp in `(-0.05, 0]` is zero to the precision the format carries, so
    /// it prints as `0.0 dB`, never `-0.0 dB`. Every other value — positive as
    /// well as negative — is written verbatim, so any value handed to it across
    /// the desktop's accepted range (`PREAMP_MIN_DB..=PREAMP_MAX_DB`, i.e.
    /// -30..=+10) survives a FORMAT-level export/parse round trip
    /// (`test_autoeq_parse.rs::export_then_parse_roundtrip`).
    ///
    /// That is a claim about this function's text, not about the app. At app
    /// level a round trip is NOT level-neutral: `eq_import_autoeq` routes a
    /// parsed `Preamp:` line to the user's trim while the engine derives its
    /// own headroom from the same bands, so the two compose. See
    /// `DIVERGENCES.md` #19.
    pub fn export_autoeq_format_with_preamp_db(&self, preamp_db: f64) -> String {
        self.export_autoeq_lines(preamp_db)
    }

    /// The shared body of the three public exports; the preamp is the only
    /// thing they differ in, so it is the only parameter. Line format is
    /// transcribed on [`Self::export_autoeq_format`].
    fn export_autoeq_lines(&self, preamp_db: f64) -> String {
        // A preamp in (-0.05, 0] rounds to zero at one decimal and would print
        // as "-0.0 dB", which reads as a defect. It is zero to the precision
        // the format carries, so print it as zero. The upper bound is
        // load-bearing: `export_autoeq_format_with_preamp_db` takes an
        // arbitrary caller-supplied value, and a *positive* preamp must be
        // written verbatim, not swallowed.
        let shown = if preamp_db > -0.05 && preamp_db <= 0.0 {
            0.0
        } else {
            preamp_db
        };
        let mut lines = vec![format!("Preamp: {shown:.1} dB")];
        for (i, band) in self.bands.iter().enumerate() {
            lines.push(format!(
                "Filter {}: ON {} Fc {:.0} Hz Gain {:.1} dB Q {:.3}",
                i + 1,
                band.filter_type.autoeq_tag(),
                band.fc,
                band.gain_db,
                band.q
            ));
        }
        lines.join("\n")
    }
}

/// Zero-state cascaded direct-form-II-transposed filtering of `x` by an
/// explicitly supplied SOS array — **Tier 2**, delegate
/// `scipy.signal.sosfilt(sos, x, zi=None)` (`fixtures/peq/sosfilt_offline`).
///
/// A free function taking the sections as an ARGUMENT, with the same signature
/// shape scipy's has and no `self`. That is deliberate: its caller
/// [`ParametricEQ::apply_offline`] derives its sections from
/// `realized_sos(rate_hz)`, so a fixture's arbitrary SOS array could never
/// reach the filtering through that door — a fixture hung on `apply_offline`
/// would have graded the policy fold and nothing about the arithmetic.
///
/// Per section, following scipy's `_sosfilt` exactly:
///
/// ```text
///   y[n] = b0·x[n] + z0
///   z0   = b1·x[n] − a1·y[n] + z1
///   z1   = b2·x[n] − a2·y[n]
/// ```
///
/// `a0` (column 3) is IGNORED, as scipy ignores it: the rows are assumed
/// already normalized, which is how `biquad.rs` emits them (`row()` divides
/// through by `a0`). A row with `a0 != 1` therefore means something different
/// here than a reader might expect, and the same different thing it means to
/// scipy.
///
/// Zero initial state on every call. No filter state survives, so the same
/// input always produces bit-identical output.
pub fn sosfilt(sos: &[[f64; 6]], x: &[f64]) -> Vec<f64> {
    let mut y = x.to_vec();
    for row in sos {
        let (b0, b1, b2, a1, a2) = (row[0], row[1], row[2], row[4], row[5]);
        let (mut z0, mut z1) = (0.0f64, 0.0f64);
        for v in &mut y {
            let xn = *v;
            let yn = b0 * xn + z0;
            z0 = b1 * xn - a1 * yn + z1;
            z1 = b2 * xn - a2 * yn;
            *v = yn;
        }
    }
    y
}
