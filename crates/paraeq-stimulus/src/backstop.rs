//! The child's own level interlock, and its own MS-4 emit guard.
//!
//! This check measures **windows of the whole file**. It knows nothing about
//! the parent's level book — not the span the parent levels, not the parent's
//! level newtype, not the verification level — all of which live in
//! `crates/paraeq-measure/src/level.rs` and `crates/paraeq-measure/src/verify.rs`
//! and are deliberately unreachable from here. The parent's number and this one
//! may legitimately differ: a 5.5 s sweep inside a 7.7 s bracketed file reads
//! `10*log10(5.5/7.7) = -1.46 dB` low on a whole-file RMS, which is exactly why
//! this is a max-over-windows check and not an RMS.
//!
//! That difference is the whole argument for the window. A whole-file check is
//! defeated by padding: add lead-in silence and the same hot sweep reads
//! quieter, by arbitrarily much. The interlock a caller can beat by making the
//! file longer is not an interlock.
//!
//! The ceiling itself is NOT declared here. It is imported, so the child cannot
//! drift away from the parent's number by being forgotten.

use paraeq_coreaudio::ABSOLUTE_MAX_DBFS_RMS;

use crate::protocol::{ExitCode, Refusal};

/// Window hop, milliseconds. Small enough that a hot burst cannot hide between
/// two windows.
pub const BACKSTOP_HOP_MS: f64 = 100.0;

/// Window length, milliseconds. Comfortably more than two cycles at the 20 Hz
/// sweep start, so the lowest content the file can carry still fills a window
/// rather than reading as a fraction of one.
pub const BACKSTOP_WINDOW_MS: f64 = 400.0;

/// The MS-4 guard, in f32.
///
/// NOT a call to the parent's `emit_guard`, which is `&mut [f64]`
/// (`crates/paraeq-measure/src/stimulus.rs`) and cannot take this crate's
/// blocks; it is not re-exported for that reason. ORDER IS LOAD-BEARING and is
/// copied from that function verbatim in shape: non-finites are ZEROED FIRST,
/// then finite samples are clamped — because `NaN.clamp(-1.0, 1.0)` is NaN by
/// documented Rust behaviour (`GuardCounts`' own doc,
/// `crates/paraeq-measure/src/stimulus.rs`). Counts are reported in the child's
/// `done` line; the parent maps them to its own emit warnings.
///
/// This doc names parent FILES on purpose — that pointer is the anti-drift
/// device, and naming a file is not a dependency. It names no parent
/// identifier.
///
/// Returns `(clamped, sanitized)`.
pub fn guard_block(block: &mut [f32]) -> (u64, u64) {
    let mut clamped = 0u64;
    let mut sanitized = 0u64;
    for v in block.iter_mut() {
        if !v.is_finite() {
            *v = 0.0;
            sanitized += 1;
        } else if v.abs() > 1.0 {
            *v = v.clamp(-1.0, 1.0);
            clamped += 1;
        }
    }
    (clamped, sanitized)
}

/// The largest windowed RMS in the file, in dBFS (full scale = 1.0).
///
/// Windows are `BACKSTOP_WINDOW_MS` long and start every `BACKSTOP_HOP_MS`. A
/// file shorter than one window is measured whole rather than skipped: a short
/// file is still a file that can be hot.
///
/// Returns `f64::NEG_INFINITY` for an empty or silent buffer, which compares
/// correctly against any ceiling.
pub fn max_window_rms_dbfs(samples: &[f32], sample_rate_hz: u32) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let rate = f64::from(sample_rate_hz.max(1));
    let window = ((BACKSTOP_WINDOW_MS / 1000.0) * rate).round().max(1.0) as usize;
    let hop = ((BACKSTOP_HOP_MS / 1000.0) * rate).round().max(1.0) as usize;
    let window = window.min(samples.len());

    let mut worst = f64::NEG_INFINITY;
    let mut start = 0usize;
    loop {
        let end = (start + window).min(samples.len());
        let slice = &samples[start..end];
        let mean_square = slice
            .iter()
            .map(|&v| f64::from(v) * f64::from(v))
            .sum::<f64>()
            / slice.len() as f64;
        if mean_square > 0.0 {
            let dbfs = 10.0 * mean_square.log10();
            if dbfs > worst {
                worst = dbfs;
            }
        }
        if end == samples.len() {
            break;
        }
        start += hop;
    }
    worst
}

/// Refuse a file that is too hot, BEFORE any device is opened.
///
/// Three independent bounds, because they fail differently: a non-finite sample
/// makes the other two blind, the windowed RMS catches a file levelled above
/// the ceiling, and the peak catches a file whose average is fine but which
/// contains a full-scale transient.
///
/// **Non-finite first, and it is a refusal rather than a repair.** Both of the
/// other bounds are NaN-blind: one NaN makes a window's `mean_square` NaN, and
/// `NaN > 0.0` is false, so the window is SKIPPED rather than failing; the peak
/// fold drops non-finites the same way. A file carrying one NaN per 400 ms hop
/// therefore cleared both bounds while holding ±1.0 content at about 0 dBFS —
/// the exact file this interlock exists to stop. `guard_block` would zero those
/// samples at emit time, but by then the device is open and the level was never
/// measured. A non-finite sample in a file the parent generated is a bug
/// upstream, not a blemish to sanitize past.
pub fn check(samples: &[f32], sample_rate_hz: u32) -> Result<(), Refusal> {
    let non_finite = samples.iter().filter(|v| !v.is_finite()).count();
    if non_finite > 0 {
        return Err(Refusal::new(
            ExitCode::BackstopRefused,
            format!(
                "{non_finite} of {} samples are not finite — the level bounds below cannot \
                 measure a file that contains one, so this is refused rather than played",
                samples.len()
            ),
        ));
    }
    let peak = samples.iter().fold(
        0.0f32,
        |acc, &v| {
            if v.is_finite() {
                acc.max(v.abs())
            } else {
                acc
            }
        },
    );
    if peak > 1.0 {
        return Err(Refusal::new(
            ExitCode::BackstopRefused,
            format!("peak {peak} exceeds full scale"),
        ));
    }
    let loudest = max_window_rms_dbfs(samples, sample_rate_hz);
    if loudest > ABSOLUTE_MAX_DBFS_RMS {
        return Err(Refusal::new(
            ExitCode::BackstopRefused,
            format!(
                "loudest {BACKSTOP_WINDOW_MS:.0} ms window is {loudest:.3} dBFS RMS, above the \
                 absolute maximum {ABSOLUTE_MAX_DBFS_RMS:.3} dBFS RMS"
            ),
        ));
    }
    Ok(())
}
