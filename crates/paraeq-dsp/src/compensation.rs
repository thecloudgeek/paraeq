//! Mic/jig calibration loading and application.
//! Test tiers: 1 — `apply_compensation` and the retained `(freqs, gains)` shims
//! stay pinned to prototype/paraeq/measurement/compensation.py, which carries
//! the same REW parsing rule (both halves landed in one commit; they must
//! agree row for row). 2 — fixtures/compensation cal cases, rendered FROM the
//! golden curve by generate_fixtures.py::gen_cal, so neither parser supplies
//! its own expectation. 3 — analytic invariants for metadata, `ignored_lines`
//! and `validate_cal`, which have no oracle at all.
//!
//! Agreeing "row for row" means the file grammar is pinned on both sides, not
//! inherited from whatever each language's stdlib happens to do. Where CPython
//! is the looser of the two the oracle is narrowed to this crate's rule, since
//! Unicode -- not CPython -- is the tie-breaker: see `columns` (C0 information
//! separators are not `White_Space`), `parse_number` (C-locale float alphabet)
//! and `universal_lines` (a bare `\r` ends a line).

use crate::DspError;
use std::path::Path;

/// A parsed calibration file: the curve plus everything else the file said.
#[derive(Clone, Debug)]
pub struct CalFile {
    /// `AGain =18dB` -> Some(18.0). Informational; NOT applied.
    pub again_db: Option<f64>,
    pub freqs: Vec<f64>,
    /// dB. NEVER normalized -- see `apply_compensation`.
    pub gains_db: Vec<f64>,
    /// Every line NOT loaded, verbatim, in file order. Blank lines are not
    /// recorded: they carry nothing to preserve.
    pub ignored_lines: Vec<String>,
    /// `Sens Factor =-0.421dB` -> Some(-0.421). Informational; NOT applied.
    pub sens_factor_db: Option<f64>,
    /// `SERNO: 7103798` -> Some("7103798").
    pub serial: Option<String>,
}

/// A point deviating from its neighbours' interpolant by more than this is an
/// [`CalWarningKind::Outlier`].
pub const DEFAULT_OUTLIER_DB: f64 = 1.0;

/// How far from 0 dB both neighbours must sit before an exact zero between
/// them reads as the vendor's bug rather than a curve crossing 0 dB.
const SUSPECT_ZERO_NEIGHBOUR_DB: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalWarning {
    pub freq_hz: f64,
    pub kind: CalWarningKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CalWarningKind {
    DuplicateFreq,
    NonMonotonicFreq,
    Outlier {
        neighbours_db: (f64, f64),
        value_db: f64,
    },
    /// Exact 0.0000 between non-zero neighbours -- the vendor's known bug.
    SuspectZero {
        neighbours_db: (f64, f64),
    },
}

/// Split on a comma OR a Unicode `White_Space` char: UMIK-1 rows are
/// tab-separated and ParaEQ CSV is comma-separated, and one parser now serves
/// both. Leading and trailing separators fall out as empty tokens, so the
/// caller owes this no `trim`.
///
/// CPython's `str.split()` additionally breaks on the C0 information separators
/// (U+001C-U+001F), which Unicode does NOT give `White_Space`; the oracle's
/// `_columns` excludes them so the two tokenizers agree.
fn columns(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|c| !c.is_empty())
}

/// A token is a number only if it parses AND is finite. `f64::from_str` accepts
/// "NaN"/"inf" and returns `inf` on overflow, so without this a prose line could
/// present itself as a data row and a NaN -- or `1e400` -- would ride into the
/// curve.
///
/// The agreed grammar is the C-locale float: ASCII only, no `_` digit
/// separators. Python's `float()` accepts `1_000` and Arabic-Indic digits and
/// this does not, so the oracle's `_parse_number` gates on the ASCII float
/// alphabet before calling `float()`.
fn parse_number(token: &str) -> Option<f64> {
    token.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `key` `=` number, per the spec's `([-+0-9.]+)` capture. Hand-rolled rather
/// than pulling `regex` into a pure-math crate for three patterns.
fn scan_number(line: &str, key: &str) -> Option<f64> {
    let rest = line.split_once(key)?.1.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '+' || c == '-' || c == '.'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `SERNO:` then the next whitespace-delimited token.
fn scan_serial(line: &str) -> Option<String> {
    let rest = line.split_once("SERNO:")?.1.trim_start();
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string()).filter(|s| !s.is_empty())
}

/// `\r\n`, `\r` and `\n` all end a line. The oracle iterates a text-mode file,
/// whose universal-newline translation ends a line at a bare `\r`; `str::lines`
/// splits on `\n` only, so a CR-only file would reach `columns` as ONE line and
/// -- `'\r'` being whitespace -- tokenize into a single silently truncated row.
/// The empty piece a `\r\n` leaves behind is a blank line, which neither parser
/// records.
fn universal_lines(content: &str) -> impl Iterator<Item = &str> {
    content.split('\n').flat_map(|line| line.split('\r'))
}

/// Parse a calibration file by REW's own rule: **only lines which begin with a
/// number are loaded, others are ignored.** That single rule handles EARS (two
/// quoted headers), UMIK-1 0-degree (one header), UMIK-1 90-degree (two),
/// unquoted legacy files, and `*`- or `#`-comments, with no format dispatch.
///
/// Columns 0 and 1 are frequency (Hz) and gain (dB); column 2 (phase) is
/// ignored where present. Metadata is recorded, never applied -- Sens Factor is
/// an absolute-SPL constant and ParaEQ measures *relative* response.
///
/// Errors only on a file with no data rows, or a row whose first column is a
/// number but whose second is not (malformed data, not a header).
pub fn parse_cal(content: &str) -> Result<CalFile, DspError> {
    // A UTF-8 BOM is an encoding marker, not content, and U+FEFF is not
    // White_Space -- so it rides into the first token, a headerless file's first
    // row parses as a header, and the curve edge-holds from the SECOND row down.
    // Silent, and worst exactly where correction authority is highest. Stripped
    // once, here at the content boundary; the oracle reads utf-8-sig for this.
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);

    let mut freqs = Vec::new();
    let mut gains_db = Vec::new();
    let mut ignored_lines = Vec::new();

    for line in universal_lines(content) {
        let mut cols = columns(line);
        let Some(first) = cols.next() else {
            continue; // blank
        };
        let Some(freq) = parse_number(first) else {
            ignored_lines.push(line.to_string()); // header, comment, prose
            continue;
        };
        let gain = cols
            .next()
            .and_then(parse_number)
            .ok_or_else(|| DspError::Parse(format!("malformed cal row: {}", line.trim())))?;
        freqs.push(freq);
        gains_db.push(gain);
    }

    if freqs.is_empty() {
        return Err(DspError::Parse(
            "no data rows in cal file: no line begins with a number".to_string(),
        ));
    }

    // Metadata lives in the header lines, quotes stripped so `SERNO: 7103798"`
    // does not capture the closing quote into the serial.
    let stripped: Vec<String> = ignored_lines.iter().map(|l| l.replace('"', "")).collect();
    Ok(CalFile {
        again_db: stripped.iter().find_map(|l| scan_number(l, "AGain")),
        freqs,
        gains_db,
        ignored_lines,
        sens_factor_db: stripped.iter().find_map(|l| scan_number(l, "Sens Factor")),
        serial: stripped.iter().find_map(|l| scan_serial(l)),
    })
}

/// Thin filesystem wrapper around [`parse_cal`].
pub fn load_cal(path: &Path) -> Result<CalFile, DspError> {
    parse_cal(&std::fs::read_to_string(path)?)
}

#[deprecated(note = "use parse_cal; this discards the cal file's metadata")]
pub fn parse_compensation(content: &str) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let cal = parse_cal(content)?;
    Ok((cal.freqs, cal.gains_db))
}

#[deprecated(note = "use load_cal; this discards the cal file's metadata")]
pub fn load_compensation(path: &Path) -> Result<(Vec<f64>, Vec<f64>), DspError> {
    let cal = load_cal(path)?;
    Ok((cal.freqs, cal.gains_db))
}

/// Deviation of point `i` from the linear interpolation of its two neighbours,
/// taken on the LOG-frequency axis because cal grids are log-spaced: linear-in-f
/// would skew every prediction toward the upper neighbour and manufacture
/// outliers on a smooth curve. `None` where the neighbours cannot define an
/// interpolant (non-positive or non-increasing frequencies -- already reported
/// as a frequency defect in their own right).
fn neighbour_deviation_db(freqs: &[f64], gains_db: &[f64], i: usize) -> Option<f64> {
    let (fa, fb, fc) = (freqs[i - 1], freqs[i], freqs[i + 1]);
    if fa <= 0.0 || fc <= fa {
        return None;
    }
    let t = (fb.ln() - fa.ln()) / (fc.ln() - fa.ln());
    let predicted = gains_db[i - 1] + t * (gains_db[i + 1] - gains_db[i - 1]);
    Some((gains_db[i] - predicted).abs())
}

/// Warn about a cal file's defects at [`DEFAULT_OUTLIER_DB`].
pub fn validate_cal(cal: &CalFile) -> Vec<CalWarning> {
    validate_cal_with_threshold(cal, DEFAULT_OUTLIER_DB)
}

/// Warnings are **returned, not raised**: a cal file is user data, ParaEQ must
/// not refuse to load it, and auto-repair is offered by the wizard, never
/// silent.
pub fn validate_cal_with_threshold(cal: &CalFile, outlier_db: f64) -> Vec<CalWarning> {
    let (freqs, gains_db) = (&cal.freqs, &cal.gains_db);
    let mut warnings = Vec::new();

    for i in 1..freqs.len() {
        let kind = if freqs[i] == freqs[i - 1] {
            CalWarningKind::DuplicateFreq
        } else if freqs[i] < freqs[i - 1] {
            CalWarningKind::NonMonotonicFreq
        } else {
            continue;
        };
        warnings.push(CalWarning {
            freq_hz: freqs[i],
            kind,
        });
    }

    // A defect poisons its own neighbours' predictions -- the 7005770 zero
    // pulls both adjacent points ~1.6 dB off their interpolants -- so a maximal
    // RUN of over-threshold points is one defect, reported at its worst point.
    // Per-point reporting would raise three warnings for one bad sample and
    // invite the wizard to interpolate over two good ones.
    let over = |i: usize| neighbour_deviation_db(freqs, gains_db, i).filter(|d| *d > outlier_db);
    let mut i = 1;
    while i + 1 < freqs.len() {
        let Some(dev) = over(i) else {
            i += 1;
            continue;
        };
        let (mut worst, mut worst_dev) = (i, dev);
        i += 1;
        while i + 1 < freqs.len() {
            let Some(dev) = over(i) else { break };
            if dev > worst_dev {
                (worst, worst_dev) = (i, dev);
            }
            i += 1;
        }
        let neighbours_db = (gains_db[worst - 1], gains_db[worst + 1]);
        // Exact zero earns its own kind: it is the vendor's specific failure
        // signature, and it deserves its own message. Gated behind the outlier
        // run, so a curve that legitimately crosses 0 dB stays silent.
        let kind = if gains_db[worst] == 0.0
            && neighbours_db.0.abs() > SUSPECT_ZERO_NEIGHBOUR_DB
            && neighbours_db.1.abs() > SUSPECT_ZERO_NEIGHBOUR_DB
        {
            CalWarningKind::SuspectZero { neighbours_db }
        } else {
            CalWarningKind::Outlier {
                neighbours_db,
                value_db: gains_db[worst],
            }
        };
        warnings.push(CalWarning {
            freq_hz: freqs[worst],
            kind,
        });
    }
    warnings
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
///
/// The curve is applied **as-is**. Never normalize it to 0 dB at a reference
/// frequency: the EARS jig encodes a real 2.1 dB L/R capsule offset IN the
/// curve, and erasing it would bake that channel imbalance into every
/// correction ParaEQ generates. A shared mono target corrects imbalance only if
/// the cal curve still carries it.
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
