//! Target curves: load, interpolate (log-f not-a-knot spline + clamp),
//! anchor deviation layer, class-filtered closest-match, per-channel
//! correction, room target generator.
//!
//! Tiers: the prototype-ported surface (parse/interpolate/anchor/
//! closest-match scoring) is Tier 1, fixture-pinned — oracle:
//! prototype/paraeq/correction/target_curves.py. The class filter, the
//! `# classes:` header, [`compute_correction_multi`], and
//! [`build_room_target`] have NO prototype oracle and are Tier 3 (analytic
//! invariants), per docs/specs/2026-07-15-room-dsp-design.md
//! ("`targets.rs` — rework") and the 2026-07-15 decision-engine spec
//! ("Target selection").

use crate::spline::NakSpline;
use crate::{DspError, PerChannel};
use std::path::Path;

pub const ANCHOR_FREQS: [f64; 16] = [
    20.0, 32.0, 50.0, 80.0, 125.0, 200.0, 315.0, 500.0, 800.0, 1250.0, 2000.0, 3150.0, 5000.0,
    8000.0, 12500.0, 20000.0,
];

/// The four physical device kinds. Lives here because this is the only crate
/// every consumer may depend on; its SEMANTICS are owned by `paraeq-decide`
/// (it is `decide()`'s `bundle.class` input) and by the 2026-07-15
/// decision-engine spec, which pins these four variants and no coarser
/// `Room`: a room target is legal for BOTH room kinds.
///
/// Bookshelf and Floorstander are not an analysis branch — they differ in one
/// capture-safety number (`sweep_f_start_hz`), and the low corner that
/// actually distinguishes them is measured, never declared.
///
/// Naming DECIDED by the owner (docs/decisions/2026-07-22-owner-value-calls.md,
/// "TransducerClass naming"): the variants stay
/// `{Bookshelf, Floorstander, InEar, OverEar}` — precise about coupling
/// (`OverEar`/`InEar` is the acoustically meaningful distinction, where
/// "Headphone" is ambiguous) and parallel across the room pair and the
/// headphone pair. The wizard spec's `Headphone`/`Iem` are RETIRED as variant
/// names; they survive only as UI display strings, rendered by the wizard via
/// [`TransducerClass::display_name`].
///
/// `Ord` is declaration (alphabetical) order and carries no meaning — no class
/// is "greater" than another. It exists so a class is usable as an ordered key
/// and as a member of `paraeq-decide`'s `Domain::Choice` candidate set.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum TransducerClass {
    Bookshelf,
    Floorstander,
    InEar,
    OverEar,
}

impl TransducerClass {
    /// UI display string, exactly as decided in
    /// docs/decisions/2026-07-22-owner-value-calls.md ("TransducerClass
    /// naming"). The wizard renders these; nothing in DSP or policy code may
    /// branch on them.
    pub fn display_name(self) -> &'static str {
        match self {
            TransducerClass::Bookshelf => "Bookshelf speakers",
            TransducerClass::Floorstander => "Floorstanding speakers",
            TransducerClass::InEar => "In-ear monitors",
            TransducerClass::OverEar => "Over-ear headphones",
        }
    }
}

/// Every class, in declaration (alphabetical) order. What `# classes: any`
/// expands to.
const ALL_CLASSES: [TransducerClass; 4] = [
    TransducerClass::Bookshelf,
    TransducerClass::Floorstander,
    TransducerClass::InEar,
    TransducerClass::OverEar,
];

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(try_from = "RawTargetCurve"))]
pub struct TargetCurve {
    pub name: String,
    pub frequencies: Vec<f64>,
    pub gains_db: Vec<f64>,
    /// Raw `# category:` header text, retained for round-tripping unknown
    /// categories. [`TargetCurve::classes`] is the typed view.
    pub category: Option<String>,
    /// Classes this curve is legal for. A SET, not an `Option`: `flat` is
    /// legal for all four, `diffuse_field` for both headphone classes,
    /// `bk_1974` for both room classes. Parsed from the comma-separated
    /// `# classes:` header (which supersedes `# category:` — see
    /// [`parse_target_csv`] for the full rules); `any` expands to every
    /// class. Always sorted and deduplicated. EMPTY means legal for NO
    /// class — [`match_closest_target`] will never return such a curve.
    ///
    /// On deserialize the field defaults to empty: a serialized curve
    /// predating it deserializes as legal-nowhere, never legal-everywhere
    /// (see [`RawTargetCurve`]).
    pub classes: Vec<TransducerClass>,
    pub description: Option<String>,
    pub source: Option<String>,
}

/// Wire shape [`TargetCurve`] deserializes through (`serde(try_from)`): the
/// raw fields land here, then [`validate_curve_data`]'s invariants run before
/// a `TargetCurve` exists. Deserialization is otherwise a third construction
/// path — after [`parse_target_csv`] and struct literals — and the only one
/// fed by untrusted wire data (`MeasurementBundle` is "the bug-report
/// attachment"), so it must not bypass the no-panic guarantee
/// [`TargetCurve::interpolate`] documents.
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct RawTargetCurve {
    name: String,
    frequencies: Vec<f64>,
    gains_db: Vec<f64>,
    category: Option<String>,
    /// Missing -> empty -> legal for NO class, never legal-everywhere.
    #[serde(default)]
    classes: Vec<TransducerClass>,
    description: Option<String>,
    source: Option<String>,
}

#[cfg(feature = "serde")]
impl TryFrom<RawTargetCurve> for TargetCurve {
    type Error = String;

    fn try_from(raw: RawTargetCurve) -> Result<Self, Self::Error> {
        let curve = TargetCurve {
            category: raw.category,
            classes: raw.classes,
            description: raw.description,
            frequencies: raw.frequencies,
            gains_db: raw.gains_db,
            name: raw.name,
            source: raw.source,
        };
        validate_curve_data(&curve).map_err(|e| e.to_string())?;
        Ok(curve)
    }
}

fn log10_floored(freqs: &[f64]) -> Vec<f64> {
    freqs.iter().map(|f| f.max(0.1).log10()).collect()
}

impl TargetCurve {
    /// Interpolate the curve at `query` frequencies (log-f spline, clamped
    /// to the end gains outside the curve's range).
    ///
    /// # Panics
    ///
    /// Panics if `frequencies` are not strictly increasing or have fewer
    /// than 2 points. Curves from [`parse_target_csv`] are validated at
    /// parse time and can never trip this; a hand-constructed `TargetCurve`
    /// that does is a programmer error.
    pub fn interpolate(&self, query: &[f64]) -> Vec<f64> {
        let s = NakSpline::new(&log10_floored(&self.frequencies), &self.gains_db)
            .expect("target curve freqs must be strictly increasing");
        let (first, last) = (self.frequencies[0], *self.frequencies.last().unwrap());
        let (g0, gn) = (self.gains_db[0], *self.gains_db.last().unwrap());
        query
            .iter()
            .map(|&q| {
                if q < first {
                    g0
                } else if q > last {
                    gn
                } else {
                    s.eval(q.max(0.1).log10())
                }
            })
            .collect()
    }
}

/// Parse a `# classes:` header value: comma-separated variant names, matched
/// case-insensitively; `any` expands to all four classes. An unknown token
/// (including an empty one from a stray comma) is a PARSE ERROR, not a silent
/// drop — a typo'd header must not quietly change which devices a curve is
/// legal for.
fn parse_classes_header(raw: &str) -> Result<Vec<TransducerClass>, DspError> {
    let mut classes: Vec<TransducerClass> = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        let expanded: &[TransducerClass] = match token.to_lowercase().as_str() {
            "any" => &ALL_CLASSES,
            "bookshelf" => &[TransducerClass::Bookshelf],
            "floorstander" => &[TransducerClass::Floorstander],
            "inear" => &[TransducerClass::InEar],
            "overear" => &[TransducerClass::OverEar],
            _ => {
                return Err(DspError::Parse(format!(
                    "unknown class token {token:?} in '# classes: {raw}' \
                     (expected Bookshelf, Floorstander, InEar, OverEar, or any)"
                )))
            }
        };
        for c in expanded {
            if !classes.contains(c) {
                classes.push(*c);
            }
        }
    }
    classes.sort();
    Ok(classes)
}

/// The data invariants [`TargetCurve::interpolate`] relies on (>= 2 knots,
/// strictly increasing, finite, lengths in lockstep), enforced on every
/// construction path that crosses a trust boundary: the CSV parser and — via
/// `serde(try_from)` — deserialization, which is otherwise a third
/// constructor that would bypass parse-time validation (a curve arriving in
/// a serialized `MeasurementBundle` must not be able to panic `interpolate`).
fn validate_curve_data(curve: &TargetCurve) -> Result<(), DspError> {
    if curve.frequencies.len() < 2 {
        return Err(DspError::Parse(format!(
            "target curve needs at least 2 data points, got {}",
            curve.frequencies.len()
        )));
    }
    if curve.frequencies.len() != curve.gains_db.len() {
        return Err(DspError::Parse(format!(
            "target curve has {} frequencies but {} gains",
            curve.frequencies.len(),
            curve.gains_db.len()
        )));
    }
    if curve
        .frequencies
        .iter()
        .chain(curve.gains_db.iter())
        .any(|v| !v.is_finite())
    {
        return Err(DspError::Parse(
            "target curve contains non-finite values".into(),
        ));
    }
    if curve.frequencies.windows(2).any(|w| w[1] <= w[0]) {
        return Err(DspError::Parse(
            "target curve frequencies must be strictly increasing".into(),
        ));
    }
    Ok(())
}

/// Back-compat class legality for a curve WITHOUT a `# classes:` header
/// (decision-engine spec, "Target selection"): `over-ear → {OverEar}`,
/// `in-ear → {InEar}`, `reference → {InEar, OverEar}` — deliberately NOT
/// room, because `reference` curves like diffuse_field are head-measured and
/// carry ear gain. Any other (or missing) category maps to the EMPTY set:
/// legal for no class. Never `any` — a curve that declares nothing must not
/// silently become legal everywhere.
fn classes_from_category(category: Option<&str>) -> Vec<TransducerClass> {
    match category.map(|c| c.trim().to_lowercase()).as_deref() {
        Some("in-ear") => vec![TransducerClass::InEar],
        Some("over-ear") => vec![TransducerClass::OverEar],
        Some("reference") => vec![TransducerClass::InEar, TransducerClass::OverEar],
        _ => Vec::new(),
    }
}

pub fn parse_target_csv(content: &str, fallback_name: &str) -> Result<TargetCurve, DspError> {
    let mut curve = TargetCurve {
        name: fallback_name.to_string(),
        frequencies: Vec::new(),
        gains_db: Vec::new(),
        category: None,
        classes: Vec::new(),
        description: None,
        source: None,
    };
    let mut classes_raw: Option<String> = None;
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(comment) = t.strip_prefix('#') {
            if let Some((key, value)) = comment.split_once(':') {
                let value = value.trim();
                if !value.is_empty() {
                    match key.trim().to_lowercase().as_str() {
                        "category" => curve.category = Some(value.to_string()),
                        "classes" => classes_raw = Some(value.to_string()),
                        "description" => curve.description = Some(value.to_string()),
                        "name" => curve.name = value.to_string(),
                        "source" => curve.source = Some(value.to_string()),
                        _ => {}
                    }
                }
            }
            continue;
        }
        let (f, g) = t
            .split_once(',')
            .ok_or_else(|| DspError::Parse(format!("bad target row: {t}")))?;
        let freq: f64 = f
            .trim()
            .parse()
            .map_err(|e| DspError::Parse(format!("{t}: {e}")))?;
        let gain: f64 = g
            .trim()
            .parse()
            .map_err(|e| DspError::Parse(format!("{t}: {e}")))?;
        // f64::parse accepts "NaN"/"inf", and NaN would slip through the
        // strictly-increasing check below (NaN comparisons are all false).
        if !freq.is_finite() || !gain.is_finite() {
            return Err(DspError::Parse(format!(
                "non-finite value in target row: {t}"
            )));
        }
        curve.frequencies.push(freq);
        curve.gains_db.push(gain);
    }
    // Validate at parse time so a parsed curve can never panic later inside
    // `interpolate` (the spline requires >= 2 strictly increasing knots).
    validate_curve_data(&curve)?;
    // Class legality (decision-engine spec, "Target selection"):
    //  - `# classes:` supersedes `# category:` when present; unknown tokens
    //    are parse errors (see `parse_classes_header`).
    //  - Without it, the back-compat category map applies — and a curve
    //    declaring neither header (or an unmapped category) is legal for NO
    //    class, never `any` (see `classes_from_category`). An empty-valued
    //    `# classes:` header is skipped by the metadata loop above and lands
    //    here too.
    curve.classes = match classes_raw {
        Some(raw) => parse_classes_header(&raw)?,
        None => classes_from_category(curve.category.as_deref()),
    };
    Ok(curve)
}

pub fn load_target_csv(path: &Path) -> Result<TargetCurve, DspError> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("target");
    parse_target_csv(&std::fs::read_to_string(path)?, stem)
}

pub fn list_targets(dir: &Path) -> Result<Vec<TargetCurve>, DspError> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("csv"))
        .collect();
    paths.sort();
    paths.iter().map(|p| load_target_csv(p)).collect()
}

pub fn compute_correction(measured_db: &[f64], target_db: &[f64]) -> Vec<f64> {
    target_db
        .iter()
        .zip(measured_db)
        .map(|(t, m)| t - m)
        .collect()
}

pub fn build_anchor_target(
    base: &TargetCurve,
    anchor_freqs: &[f64],
    offsets_db: &[f64],
    name: &str,
) -> TargetCurve {
    let mut grid: Vec<f64> = base
        .frequencies
        .iter()
        .chain(anchor_freqs)
        .copied()
        .collect();
    grid.sort_by(|a, b| a.total_cmp(b));
    grid.dedup();
    let log_anchor: Vec<f64> = anchor_freqs.iter().map(|f| f.log10()).collect();
    let dev_spline = NakSpline::new(&log_anchor, offsets_db).expect("anchors strictly increasing");
    let (a_first, a_last) = (anchor_freqs[0], *anchor_freqs.last().unwrap());
    let (o_first, o_last) = (offsets_db[0], *offsets_db.last().unwrap());
    let deviation: Vec<f64> = grid
        .iter()
        .map(|&f| {
            if f < a_first {
                o_first
            } else if f > a_last {
                o_last
            } else {
                dev_spline.eval(f.log10())
            }
        })
        .collect();
    let base_gains = base.interpolate(&grid);
    TargetCurve {
        name: name.to_string(),
        gains_db: base_gains
            .iter()
            .zip(&deviation)
            .map(|(b, d)| b + d)
            .collect(),
        frequencies: grid,
        category: base.category.clone(),
        classes: base.classes.clone(),
        description: base.description.clone(),
        source: base.source.clone(),
    }
}

/// Closest curve to the measurement by the standard deviation of the
/// level-invariant residual — considering ONLY curves legal for `class`.
///
/// The REQUIRED leading class argument is the compiler-enforced fix for the
/// category error: an unfiltered match would happily return
/// `harman_oe_2018` (+8.3 dB of synthesized ear gain at 3 kHz) for a
/// loudspeaker measurement, double-applying what the listener's own outer
/// ear already delivers acoustically. `Err` if no candidate is legal for
/// `class` — a wrong curve is never a fallback.
pub fn match_closest_target<'a>(
    class: TransducerClass,
    measured_freqs: &[f64],
    measured_db: &[f64],
    targets: &'a [TargetCurve],
) -> Result<&'a TargetCurve, DspError> {
    // Without these, the residual zip would silently truncate to the shorter
    // slice, and an empty measurement would score every candidate NaN (0/0)
    // yet still return the first class-legal curve as Ok.
    if measured_freqs.is_empty() {
        return Err(DspError::InvalidInput(
            "match_closest_target: empty measurement".into(),
        ));
    }
    if measured_freqs.len() != measured_db.len() {
        return Err(DspError::InvalidInput(format!(
            "match_closest_target: {} freqs vs {} dB values",
            measured_freqs.len(),
            measured_db.len()
        )));
    }
    let mut best: Option<(&TargetCurve, f64)> = None;
    for t in targets.iter().filter(|t| t.classes.contains(&class)) {
        let interp = t.interpolate(measured_freqs);
        let residual: Vec<f64> = interp.iter().zip(measured_db).map(|(i, m)| i - m).collect();
        let mean = residual.iter().sum::<f64>() / residual.len() as f64;
        let score = (residual.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
            / residual.len() as f64)
            .sqrt();
        if best.is_none() || score < best.unwrap().1 {
            best = Some((t, score));
        }
    }
    best.map(|(t, _)| t).ok_or_else(|| {
        DspError::InvalidInput(format!(
            "no target curve is legal for {class:?} ({} candidate(s) checked)",
            targets.len()
        ))
    })
}

/// Per-channel [`compute_correction`] against ONE shared mono target.
///
/// The target being mono is the point: correcting every channel to a common
/// target is what removes L/R imbalance (and why cal curves must never be
/// normalized — the cal file's per-channel offset is the truth the shared
/// target corrects against). `Err` if any channel's length differs from the
/// target's — the mono zip would silently truncate instead.
pub fn compute_correction_multi(
    measured: &PerChannel<Vec<f64>>,
    target_db: &[f64],
) -> Result<PerChannel<Vec<f64>>, DspError> {
    measured.try_map(|ch| {
        if ch.len() != target_db.len() {
            return Err(DspError::InvalidInput(format!(
                "channel length {} != target length {}",
                ch.len(),
                target_db.len()
            )));
        }
        Ok(compute_correction(ch, target_db))
    })
}

/// The FIXED design rate for the room target's shelf. Deliberate: a target
/// is a *curve*, not a filter, so no engine sample rate belongs here.
/// Bilinear warping of a 105 Hz corner at 48 kHz is ~0.0002% — utterly
/// negligible — and the shelf is flat by 20 kHz where warping would matter.
const ROOM_SHELF_DESIGN_RATE_HZ: f64 = 48_000.0;

/// Parameters for the generated domestic room ("house") target: an RBJ
/// low-shelf bass lift plus a linear-in-log-f downward tilt about
/// `pivot_hz`. Defaults are the domestic consensus starting point
/// (105 Hz / +4 dB / Q 0.71 shelf, −0.9 dB/oct tilt, 1 kHz pivot).
///
/// # The SMPTE X-curve is explicitly REFUSED
///
/// Not an omission — a documented refusal, so a future session does not
/// "helpfully" add it. The X-curve (SMPTE ST 202) is scoped to rooms above
/// 125 m³ (dubbing stages, cinemas), and its −3 dB/oct is 3× the domestic
/// consensus (−0.9 dB/oct). Bundling it as a selectable domestic room target
/// would be a category error of exactly the kind [`TransducerClass`] exists
/// to prevent — which is also why `tilt_db_per_oct` is domain-checked to
/// `−1.5..=0.0`. Users can import a CSV; ParaEQ will not bundle it or
/// suggest it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomTargetSpec {
    pub shelf_hz: f64,
    pub shelf_gain_db: f64,
    pub shelf_q: f64,
    /// Valid domain `-1.5..=0.0`; [`build_room_target`] refuses anything
    /// outside it (see the X-curve refusal above).
    pub tilt_db_per_oct: f64,
    pub pivot_hz: f64,
}

impl Default for RoomTargetSpec {
    fn default() -> Self {
        Self {
            shelf_hz: 105.0,
            shelf_gain_db: 4.0,
            shelf_q: 0.71,
            tilt_db_per_oct: -0.9,
            pivot_hz: 1000.0,
        }
    }
}

/// Generate the room target on `freqs`:
/// `gains_db[i] = shelf_db(f_i) + tilt_db_per_oct · log2(f_i / pivot_hz)`,
/// where `shelf_db` reuses the existing fixture-pinned RBJ designer
/// ([`crate::biquad::low_shelf`] evaluated via
/// [`crate::biquad::sos_frequency_response_db`]) at the fixed
/// [`ROOM_SHELF_DESIGN_RATE_HZ`] rather than introducing a second closed
/// form. The result is legal for BOTH room classes and nothing else.
///
/// `Err` if `tilt_db_per_oct` is outside `-1.5..=0.0`, or if `freqs` is not
/// a strictly increasing positive grid of ≥ 2 points (the [`TargetCurve`]
/// invariant that keeps `interpolate` panic-free).
pub fn build_room_target(spec: &RoomTargetSpec, freqs: &[f64]) -> Result<TargetCurve, DspError> {
    if !(-1.5..=0.0).contains(&spec.tilt_db_per_oct) {
        return Err(DspError::InvalidInput(format!(
            "tilt_db_per_oct {} outside -1.5..=0.0 (the domestic consensus \
             domain; the steeper SMPTE X-curve is deliberately refused)",
            spec.tilt_db_per_oct
        )));
    }
    if freqs.len() < 2 {
        return Err(DspError::InvalidInput(format!(
            "room target grid needs >= 2 points, got {}",
            freqs.len()
        )));
    }
    if freqs[0] <= 0.0 || freqs.windows(2).any(|w| w[1] <= w[0]) {
        return Err(DspError::InvalidInput(
            "room target grid must be positive and strictly increasing".into(),
        ));
    }
    let shelf = crate::biquad::sos_frequency_response_db(
        &[crate::biquad::low_shelf(
            spec.shelf_hz,
            spec.shelf_gain_db,
            spec.shelf_q,
            ROOM_SHELF_DESIGN_RATE_HZ,
        )],
        freqs,
        ROOM_SHELF_DESIGN_RATE_HZ,
    );
    let gains_db = freqs
        .iter()
        .zip(&shelf)
        .map(|(&f, s)| s + spec.tilt_db_per_oct * (f / spec.pivot_hz).log2())
        .collect();
    Ok(TargetCurve {
        name: format!(
            "Room ({:+.1} dB/oct, shelf {:+.1} dB @ {:.0} Hz)",
            spec.tilt_db_per_oct, spec.shelf_gain_db, spec.shelf_hz
        ),
        frequencies: freqs.to_vec(),
        gains_db,
        category: Some("room".to_string()),
        classes: vec![TransducerClass::Bookshelf, TransducerClass::Floorstander],
        description: None,
        source: None,
    })
}
