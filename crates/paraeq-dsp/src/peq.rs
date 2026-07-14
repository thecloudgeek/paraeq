//! Parametric EQ container + AutoEQ text export.
//! Oracle: prototype/paraeq/correction/parametric_eq.py

use crate::biquad;
use crate::DspError;
use regex::Regex;
use std::sync::OnceLock;

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

/// A parsed AutoEQ/EqualizerAPO ParametricEq preset: the preamp gain plus the
/// ON filter bands. Mirrors the oracle `ParsedPreset` (autoeq_db.py:48-51).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
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
