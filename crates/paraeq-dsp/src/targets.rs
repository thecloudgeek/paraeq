//! Target curves: load, interpolate (log-f not-a-knot spline + clamp),
//! anchor deviation layer, closest-match. Oracle: prototype/paraeq/correction/target_curves.py

use crate::spline::NakSpline;
use crate::DspError;
use std::path::Path;

pub const ANCHOR_FREQS: [f64; 16] = [
    20.0, 32.0, 50.0, 80.0, 125.0, 200.0, 315.0, 500.0, 800.0, 1250.0, 2000.0, 3150.0, 5000.0,
    8000.0, 12500.0, 20000.0,
];

#[derive(Clone, Debug)]
pub struct TargetCurve {
    pub name: String,
    pub frequencies: Vec<f64>,
    pub gains_db: Vec<f64>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub source: Option<String>,
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

pub fn parse_target_csv(content: &str, fallback_name: &str) -> Result<TargetCurve, DspError> {
    let mut curve = TargetCurve {
        name: fallback_name.to_string(),
        frequencies: Vec::new(),
        gains_db: Vec::new(),
        category: None,
        description: None,
        source: None,
    };
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
    if curve.frequencies.len() < 2 {
        return Err(DspError::Parse(format!(
            "target CSV needs at least 2 data rows, got {}",
            curve.frequencies.len()
        )));
    }
    if curve.frequencies.windows(2).any(|w| w[1] <= w[0]) {
        return Err(DspError::Parse(
            "target CSV frequencies must be strictly increasing".into(),
        ));
    }
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
        description: base.description.clone(),
        source: base.source.clone(),
    }
}

pub fn match_closest_target<'a>(
    measured_freqs: &[f64],
    measured_db: &[f64],
    targets: &'a [TargetCurve],
) -> &'a TargetCurve {
    let mut best: Option<(&TargetCurve, f64)> = None;
    for t in targets {
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
    best.expect("targets list must be non-empty").0
}
