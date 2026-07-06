//! Mic/jig calibration loading and application.
//! Oracle: prototype/paraeq/measurement/compensation.py

use crate::DspError;

/// Parse a compensation file's contents, auto-detecting the format from the
/// first line: miniDSP/REW-style files start with a quoted header string,
/// everything else is treated as ParaEQ CSV.
pub fn parse_compensation(content: &str) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let first = content.lines().next().unwrap_or("");
    if first.trim_start().starts_with('"') {
        parse_minidsp(content)
    } else {
        parse_paraeq_csv(content)
    }
}

/// ParaEQ CSV: `freq,gain` rows. Blank lines and `#`-comments are skipped.
fn parse_paraeq_csv(content: &str) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut parts = t.split(',');
        let (f, g) = (parts.next(), parts.next());
        match (f, g) {
            (Some(f), Some(g)) => {
                freqs.push(
                    f.trim()
                        .parse::<f64>()
                        .map_err(|e| DspError::Parse(format!("{t}: {e}")))?,
                );
                gains.push(
                    g.trim()
                        .parse::<f64>()
                        .map_err(|e| DspError::Parse(format!("{t}: {e}")))?,
                );
            }
            _ => return Err(DspError::Parse(format!("bad CSV row: {t}"))),
        }
    }
    Ok((freqs, gains))
}

/// miniDSP/REW-style: 2 leading header lines, `*`-comment lines, then
/// whitespace-separated `Freq(Hz) SPL(dB) Phase(degrees)` rows. Phase is
/// dropped. Mirrors `np.loadtxt(skiprows=2, comments="*", usecols=(0, 1))`.
fn parse_minidsp(content: &str) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let mut freqs = Vec::new();
    let mut gains = Vec::new();
    for line in content.lines().skip(2) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('*') {
            continue;
        }
        let mut cols = t.split_whitespace();
        let (f, g) = (cols.next(), cols.next());
        match (f, g) {
            (Some(f), Some(g)) => {
                freqs.push(
                    f.parse::<f64>()
                        .map_err(|e| DspError::Parse(format!("{t}: {e}")))?,
                );
                gains.push(
                    g.parse::<f64>()
                        .map_err(|e| DspError::Parse(format!("{t}: {e}")))?,
                );
            }
            _ => return Err(DspError::Parse(format!("bad miniDSP row: {t}"))),
        }
    }
    Ok((freqs, gains))
}

/// Thin filesystem wrapper around [`parse_compensation`].
pub fn load_compensation(path: &std::path::Path) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    parse_compensation(&std::fs::read_to_string(path)?)
}

/// np.interp semantics: linear inside the range, edge-hold outside it.
/// `xp` must be sorted increasing.
pub(crate) fn linear_interp_edge_hold(x: &[f64], xp: &[f64], fp: &[f64]) -> Vec<f64> {
    x.iter()
        .map(|&q| {
            if q <= xp[0] {
                fp[0]
            } else if q >= xp[xp.len() - 1] {
                fp[fp.len() - 1]
            } else {
                let j = xp.partition_point(|&v| v <= q) - 1;
                let t = (q - xp[j]) / (xp[j + 1] - xp[j]);
                fp[j] + t * (fp[j + 1] - fp[j])
            }
        })
        .collect()
}

/// Subtract a mic/jig compensation curve (interpolated onto `freqs_fft`)
/// from a measured magnitude spectrum.
pub fn apply_compensation(
    magnitude_db: &[f64],
    freqs_fft: &[f64],
    comp_freqs: &[f64],
    comp_gains_db: &[f64],
) -> Vec<f64> {
    let interp = linear_interp_edge_hold(freqs_fft, comp_freqs, comp_gains_db);
    magnitude_db
        .iter()
        .zip(&interp)
        .map(|(m, c)| m - c)
        .collect()
}
