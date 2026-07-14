//! Parametric EQ container + AutoEQ text export.
//! Oracle: prototype/paraeq/correction/parametric_eq.py

use crate::biquad;
use crate::DspError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
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
    /// DIVERGENCE (see DIVERGENCES.md): the prototype hardcodes the preamp
    /// line as a fixed literal ("Preamp: 0.0 dB"); Rust takes the real
    /// preamp value and writes it. Each filter line: 1-based index, AutoEQ
    /// tag, Fc rounded to 0 decimals, Gain to 1 decimal, Q to 3 decimals.
    /// Lines are joined with "\n" and there is no trailing newline.
    pub fn export_autoeq_format(&self, preamp_db: f64) -> String {
        let mut lines = vec![format!("Preamp: {:.1} dB", preamp_db)];
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
