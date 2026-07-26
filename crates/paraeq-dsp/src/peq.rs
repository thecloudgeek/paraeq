//! Parametric EQ container + AutoEQ text export.
//! Oracle: prototype/paraeq/correction/parametric_eq.py

use crate::biquad;
use crate::DspError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
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
        for band in &self.bands {
            grid.push(band.fc.clamp(1.0, f_max));
        }
        grid.sort_by(f64::total_cmp);
        grid.dedup();
        let sos: Vec<[f64; 6]> = self
            .combined_sos()
            .into_iter()
            .map(|row| {
                if biquad::is_stable(&row) {
                    row
                } else {
                    biquad::IDENTITY
                }
            })
            .collect();
        let peak = biquad::sos_frequency_response_db(&sos, &grid, self.sample_rate)
            .into_iter()
            .fold(f64::NEG_INFINITY, f64::max);
        if peak > BIAS_CEILING_DB {
            -peak
        } else {
            0.0
        }
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
    /// software and not ParaEQ.** The engine-side half of this — sending
    /// `SetGainDb(preamp_db)` alongside `SetCorrection` so the gain stage at
    /// `chain.rs` carries it — is the Tauri backend's, and is sequenced
    /// post-merge with the rest of the desktop wiring.
    pub fn export_autoeq_format_with_preamp(&self) -> String {
        self.export_autoeq_lines(self.preamp_db())
    }

    /// The shared body of the two public exports; the preamp is the only thing
    /// they differ in, so it is the only parameter. Line format is transcribed
    /// on [`Self::export_autoeq_format`].
    fn export_autoeq_lines(&self, preamp_db: f64) -> String {
        // A preamp between -0.05 and 0 dB rounds to zero at one decimal and
        // would print as "-0.0 dB", which reads as a defect. It is zero to the
        // precision the format carries, so print it as zero.
        let shown = if preamp_db > -0.05 { 0.0 } else { preamp_db };
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
