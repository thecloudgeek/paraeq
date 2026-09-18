//! The analysis stage: pure ORCHESTRATION over `paraeq-dsp`, no new math.
//!
//! Every step below is a call into a module that already owns it, in the order
//! the plan fixes: gate each position's IR (`window_type` + the L/R windows) →
//! `fr::complex_spectrum` → `fr::derotate` → `logf::resample_complex_to_log_grid`
//! → `fdw::apply_fdw` → magnitude dB → `compensation::apply_compensation` →
//! `fr::smooth` → `fr::align_spl` → `fr::average_measurements_rms` /
//! `fr::average_measurements` → `fr::sigma_db` → `fr::excess_group_delay_s`.
//! Nothing here decides anything; it produces the curves the decisions are read
//! off (`Analysis` is documented "products, not decisions").
//!
//! **Deconvolution is not a stage here, and that is not an omission.**
//! `MeasurementBundle` carries an already-deconvolved [`ImpulseResponse`] per
//! position — "`deconvolution::deconvolve` must return this instead of a bare
//! `Vec<f64>`" — and no raw recording. The capture layer deconvolves; `decide()`
//! starts at the gate.
//!
//! **Purity.** No filesystem, no clock, no randomness, and no logging: the same
//! bundle produces the same curves forever, which is what makes a stored bundle
//! plus its expected `DecisionSet` a fixture.
//!
//! **Totality.** `decide()` has no `Result`, so this stage has none either. A
//! bundle whose positions cannot be analysed — no positions at all, a ragged
//! channel count, an empty channel, a peak index outside its own samples, a
//! non-finite sample — yields the EMPTY products rather than a fabricated
//! curve, and the diagnosis is the refusal table's job (B7c). Fabricating a
//! flat curve for an unanalysable position would be exactly the "second, lying
//! source of truth about what the app did" the design exists to prevent.

use crate::bundle::MeasurementBundle;
use crate::decisions::{AuthorityPreset, WindowType};
use crate::profile::{AveragingMode, CouplingPath, PathProfile, SmoothingMode};
use paraeq_dsp::authority::{
    build_authority_gated, AuthorityCurve, AuthorityPolicy, EgdGate, SIGMA_NONE_DB,
};
use paraeq_dsp::compensation::apply_compensation;
use paraeq_dsp::fdw::{apply_fdw, FdwSpec};
use paraeq_dsp::fr::{
    align_spl, average_measurements, average_measurements_rms, complex_spectrum, derotate,
    excess_group_delay_s, sigma_db, smooth, AlignedSet,
};
use paraeq_dsp::gating::{apply_gate, GateSpec, ImpulseResponse as GatedIr, SweepParams};
use paraeq_dsp::logf::{resample_complex_to_log_grid, resample_db_to_log_grid, LogGrid, Prefilter};
use paraeq_dsp::peq::{EQBand, ParametricEQ};
use paraeq_dsp::window::{WindowKind, WindowSpec};

/// The anti-comb prefilter fraction the log resample runs at: REW's finest
/// variable-smoothing setting, so the lowpass-before-downsample cannot erase
/// anything the `smoothing` decision would have kept.
const RESAMPLE_PREFILTER: Prefilter = Prefilter::AntiComb { fraction: 48 };

/// The additive magnitude floor, matching `biquad::sos_frequency_response_db`'s
/// `+1e-10`, so a bin that came out exactly zero maps to a finite −200 dB
/// rather than to `-inf` — which `fr::smooth` and `fr::align_spl` both refuse,
/// and which `serde_json` cannot round-trip.
const MAGNITUDE_FLOOR: f64 = 1e-10;

/// The post-peak half of the shipped IR store window, milliseconds.
///
/// MIRRORED from `crates/paraeq-measure/src/store.rs`, which owns it:
/// `paraeq-decide` may not depend on `paraeq-measure`, and § D-K derives the
/// `fdw_post_cycles` domain ceiling from this number — "**derived at runtime**
/// as `STORE_POST_MS/1000 · grid.f_min()`". If the store window moves, this
/// constant and that ceiling move with it. Same mirroring posture, and the same
/// reason, as `PathProfile::sensitivity_envelope_spl_per_dbfs`.
pub(crate) const STORE_POST_MS: f64 = 1500.0;

/// The `fdw_post_cycles` domain ceiling on `grid`, § D-K.
///
/// Derived at runtime rather than spelled as a literal, because BOTH bounds are
/// properties of things that can move: "`STORE_POST_MS/1000 · grid.f_min()` and
/// additionally floored by the grid's `ppo/1.849` limit (`fdw::apply_fdw` errors
/// when `sigma_bins < 1.0`)". On the shipped standard grid that is
/// `min(1.5 · 20, 96/1.849) = min(30.0, 51.9) = 30.0` — the store window binds,
/// not the grid. The spec's former ceiling of 61.0 was unattainable under
/// either bound, and this is a narrowing to what the implementation can honour
/// rather than a policy change.
pub(crate) fn fdw_post_ceiling(grid: &LogGrid) -> f64 {
    let store_bound = STORE_POST_MS / 1000.0 * grid.f_min();
    let grid_bound = f64::from(grid.points_per_octave()) / 1.849;
    store_bound.min(grid_bound)
}

/// What the analysis stage needs from the decisions that precede it — the
/// `Reanalyze` tier's own list, in the spec's words: "window → FDW →
/// compensation → smoothing → Align SPL → averaging → σ(f)".
pub(crate) struct AnalysisSettings {
    pub align_spl_band: (f64, f64),
    pub averaging: AveragingMode,
    /// `None` on the coupler path: "Coupler: off", carried as
    /// `GatingMode::None` on the profile. The `fdw_*` decisions still exist and
    /// still have values — the profile decides whether the pass runs, not the
    /// cycle count.
    pub fdw: Option<FdwSpec>,
    pub left_window_ms: f64,
    /// The DECIDED right window. The gate actually applied is this bounded by
    /// the post-peak data the recording holds.
    ///
    /// `apply_gate` clamps the LEFT window itself ("The left clamp is
    /// MANDATORY") but REFUSES a right gate longer than the whole recording —
    /// "a programmer error and, unchecked, an unbounded allocation". It is the
    /// caller's job not to ask, and refusing to analyse a short recording at all
    /// would be a worse answer than windowing the data that is there. The
    /// shipped store window is `[peak − 100 ms, peak + 1500 ms]`, so on a real
    /// capture this bound never binds — not even for the coupler path's
    /// ungated 1000 ms. `GateReport` reports the effective duration either way,
    /// which is what the resolution limit is computed from.
    pub right_window_ms: f64,
    pub smoothing: SmoothingMode,
    pub window: WindowSpec,
}

/// The curves `Analysis` publishes, plus the two facts the rules need about the
/// shape of what was analysed.
pub(crate) struct AnalysisProducts {
    /// Per channel, on `grid`.
    pub averaged_db: Vec<Vec<f64>>,
    /// The capture's channel count, 0 when nothing could be analysed.
    pub channels: usize,
    /// On `grid`, in seconds. **Evidence only in v1** — an input to no
    /// decision.
    pub excess_group_delay_s: Vec<f64>,
    /// Aligned, smoothed, channel-collapsed, index-parallel to
    /// `MeasurementBundle::positions` — empty when nothing could be analysed.
    pub per_position_db: Vec<Vec<f64>>,
    /// On `grid`. One pass over `per_position_db`, exactly as the field's own
    /// doc says.
    pub sigma_db: Vec<f64>,
}

/// The shape facts the phase-1 rules read before any curve exists: how long the
/// capture gives the left window, how much post-peak data the right window can
/// actually reach, and how many channels there are.
pub(crate) struct Geometry {
    /// Whether [`analyze`] will produce curves at all. See the module header.
    pub analyzable: bool,
    pub channels: usize,
    /// `t_peak` in ms — the SHORTEST across positions, because one window has
    /// to fit every position and the earliest arrival is the binding one.
    pub peak_time_ms: f64,
    /// Post-peak data held, ms — again the shortest across positions, and for
    /// the same reason. This is what bounds the right gate that is actually
    /// APPLIED; see [`AnalysisSettings::right_window_ms`].
    pub post_peak_ms: f64,
    /// The IRs' rate. Falls back to `capture.sweep_rate` when there are no
    /// positions; a mismatch between the two is `SweepRateMismatch` (B7c).
    pub sample_rate: f64,
}

/// Measure the capture without touching a sample of it.
pub(crate) fn geometry(bundle: &MeasurementBundle) -> Geometry {
    let fallback_rate = f64::from(bundle.capture.sweep_rate.max(1));
    let Some(first) = bundle.positions.first() else {
        return Geometry {
            analyzable: false,
            channels: 0,
            peak_time_ms: 0.0,
            post_peak_ms: 0.0,
            sample_rate: fallback_rate,
        };
    };
    let channels = first.ir.samples.len();
    let sample_rate = if first.ir.sample_rate > 0 {
        f64::from(first.ir.sample_rate)
    } else {
        fallback_rate
    };

    let mut analyzable = channels > 0;
    let mut peak_time_ms = f64::INFINITY;
    let mut post_peak_ms = f64::INFINITY;
    for position in &bundle.positions {
        let ir = &position.ir;
        if ir.samples.len() != channels || ir.sample_rate == 0 {
            analyzable = false;
            continue;
        }
        for channel in &ir.samples {
            // A peak at or past the end has no post-peak data and `apply_gate`
            // refuses it; a non-finite sample poisons every downstream fold.
            if ir.peak + 1 >= channel.len() || channel.iter().any(|s| !s.is_finite()) {
                analyzable = false;
                continue;
            }
            let rate = f64::from(ir.sample_rate);
            peak_time_ms = peak_time_ms.min(ir.peak as f64 * 1000.0 / rate);
            post_peak_ms = post_peak_ms.min((channel.len() - 1 - ir.peak) as f64 * 1000.0 / rate);
        }
    }
    Geometry {
        analyzable,
        channels,
        peak_time_ms: if peak_time_ms.is_finite() {
            peak_time_ms
        } else {
            0.0
        },
        post_peak_ms: if post_peak_ms.is_finite() {
            post_peak_ms
        } else {
            0.0
        },
        sample_rate,
    }
}

/// The `window_type` decision as the window module's own type.
pub(crate) fn window_kind(window_type: WindowType) -> WindowKind {
    match window_type {
        WindowType::BlackmanHarris => WindowKind::BlackmanHarris,
        WindowType::Hann => WindowKind::Hann,
        WindowType::Rect => WindowKind::Rect,
        WindowType::Tukey(alpha) => WindowKind::Tukey { alpha },
    }
}

/// The authority policy for a path: the two named constructors, never
/// re-specified numbers.
///
/// "`decide()` consumes the `AuthorityCurve` that module produces and must not
/// re-specify different numbers" (§ Authority). The only fields this sets are
/// the two the profile owns as POLICY rather than mechanism — the excursion
/// breakpoints (which the profile already reads from `paraeq_dsp::authority`)
/// and `boost_ratio`, § D-E's cut-only room default.
pub(crate) fn authority_policy(profile: &PathProfile) -> AuthorityPolicy {
    let base = match profile.coupling {
        CouplingPath::Coupler => AuthorityPolicy::coupler(),
        CouplingPath::Room => AuthorityPolicy::room(),
    };
    AuthorityPolicy {
        boost_ratio: profile.boost_ratio,
        excursion: profile.excursion_breakpoints_db.to_vec(),
        q_ceiling: profile.q_cap,
        ..base
    }
}

/// Resolve the `authority` decision's PRESET into the curve `Analysis`
/// publishes.
///
/// `Conservative` is the spec's "×0.5" column, applied to the excursion
/// envelope BEFORE composition rather than to the finished curve: `max_cut`,
/// `max_boost` and `max_q` are all functions of `e(f)`, and halving the
/// envelope is the one place that keeps the three consistent with each other.
/// `AuthorityCurve` is sealed anyway — `build_authority` is its only
/// constructor — so scaling the finished curve is not reachable, which is the
/// type doing its job.
///
/// **The EGD gate is OFF and the mask is `None` (R7, v1).** The trace ships as
/// `Evidence`; the gate ships in v1.1 behind `EgdGate::On`. Passing `None`
/// here is not "no evidence" defaulting closed — `build_authority_gated`
/// documents `None` as an all-true mask for exactly that reason.
pub(crate) fn resolve_authority(
    preset: &AuthorityPreset,
    grid: &LogGrid,
    sigma: &[f64],
    profile: &PathProfile,
) -> AuthorityCurve {
    if let AuthorityPreset::Custom(curve) = preset {
        return curve.clone();
    }
    let mut policy = authority_policy(profile);
    if matches!(preset, AuthorityPreset::Conservative) {
        for breakpoint in &mut policy.excursion {
            breakpoint.1 *= 0.5;
        }
    }
    policy.egd_gate = EgdGate::Off;
    build_authority_gated(grid, sigma, None, &policy).unwrap_or_else(|_| no_authority(grid))
}

/// A ceiling of zero everywhere, for the case where there is no σ(f) to compose
/// one from.
///
/// σ = `SIGMA_NONE_DB` is the value at which the confidence weight `w(f)`
/// reaches zero, so this is "we could not measure the disagreement between
/// positions, therefore we claim no authority" — the conservative direction,
/// stated in the composition's own vocabulary rather than by fabricating a
/// curve.
///
/// The `expect` is on constants: `LogGrid::standard()` and
/// `AuthorityPolicy::default()` are both statically valid, and `sigma` here is
/// a finite non-negative constant vector of exactly the grid's length, which is
/// the whole of `build_authority`'s validation. `LogGrid::standard()` carries
/// the same `expect` for the same reason.
fn no_authority(grid: &LogGrid) -> AuthorityCurve {
    build_authority_gated(
        grid,
        &vec![SIGMA_NONE_DB; grid.len()],
        None,
        &AuthorityPolicy::default(),
    )
    .expect("a constant sigma of the grid's own length is valid by construction")
}

/// `H(f)`: the REALIZED cascade of `bands` at `rate_hz`, dB, on `freqs`.
///
/// The realized cascade, not the requested one: rows failing the Jury stability
/// test are evaluated as identity, mirroring what the engine's own stability
/// funnel installs. Preamp EXCLUDED — the caller adds it as a constant offset,
/// because a safety interlock may not be conditioned on the hypothesis it is
/// testing.
///
/// `rate_hz` is an argument rather than the plan's `design_rate`, which is
/// "Provenance only — never the rate the engine designs at": predicting at the
/// design rate while the engine runs at another manufactures a bilinear-warp
/// residual at HF and blames the chain for it.
///
/// Unused in B7a because the skeleton fits no bands. It ships here rather than
/// with its first caller because it and [`realized_preamp_db`] are two halves of
/// one contract — the peak of THIS curve is THAT number — and the two consumers
/// are in different items: B7b's residual, and B8's `D(f) = H(f) + preamp_db`
/// prediction, which must use the same `H` the preamp was taken from or it
/// reports a prediction fault as an engine fault.
#[allow(dead_code)]
pub(crate) fn realized_response_db(bands: &[EQBand], freqs: &[f64], rate_hz: f64) -> Vec<f64> {
    ParametricEQ {
        bands: bands.to_vec(),
        sample_rate: rate_hz,
    }
    .realized_response(freqs, rate_hz)
}

/// `-max(0, peak of the REALIZED cascade)`, no headroom constant.
///
/// `ParametricEQ::preamp_db()` VERBATIM — the same call the engine makes at the
/// live rate and the same call the AutoEQ export makes, evaluated on
/// `preamp_grid()`. Calling it rather than re-deriving it is the point: two
/// implementations of one number is how the export, the engine and the
/// verification residual come to disagree.
pub(crate) fn realized_preamp_db(bands: &[EQBand], rate_hz: f64) -> f64 {
    ParametricEQ {
        bands: bands.to_vec(),
        sample_rate: rate_hz,
    }
    .preamp_db()
}

/// Run the pipeline. See the module header for the stage order and for what
/// "unanalysable" means.
pub(crate) fn analyze(
    bundle: &MeasurementBundle,
    grid: &LogGrid,
    settings: &AnalysisSettings,
) -> AnalysisProducts {
    let geometry = geometry(bundle);
    if !geometry.analyzable {
        return empty_products(grid, geometry.channels);
    }

    // One window for every position, so the same measurement is made at each of
    // them; see `AnalysisSettings::right_window_ms` for why it is bounded here
    // rather than left to `apply_gate`.
    let right_ms = settings.right_window_ms.min(geometry.post_peak_ms);

    // Per channel, per position: one smoothed, compensated dB curve on the
    // grid. Indexed [channel][position], because `align_spl` aligns a cohort of
    // positions and the cohort is per channel.
    let mut curves: Vec<Vec<Vec<f64>>> = vec![Vec::new(); geometry.channels];
    let mut egd_source: Option<(Vec<f64>, u32)> = None;
    for position in &bundle.positions {
        for (channel, samples) in position.ir.samples.iter().enumerate() {
            let Some((gated, applied_left_ms)) = gate(
                samples,
                position.ir.peak,
                position.ir.sample_rate,
                right_ms,
                bundle,
                settings,
            ) else {
                return empty_products(grid, geometry.channels);
            };
            if egd_source.is_none() {
                egd_source = Some((gated.clone(), position.ir.sample_rate));
            }
            let Some(curve) = curve_on_grid(
                &gated,
                position.ir.sample_rate,
                applied_left_ms,
                grid,
                bundle,
                settings,
            ) else {
                return empty_products(grid, geometry.channels);
            };
            curves[channel].push(curve);
        }
    }

    // `Analysis::per_position_db` is index-parallel to `positions`, not to
    // channels, so the per-channel curves collapse by dB mean. On a mono
    // capture that is the identity; on a stereo one it is the curve σ(f) is
    // read off, which is what the field's own doc requires ("σ(f) is one pass
    // over these").
    let collapsed: Vec<Vec<f64>> = (0..bundle.positions.len())
        .map(|p| {
            let mut mean = vec![0.0; grid.len()];
            for channel in &curves {
                for (m, v) in mean.iter_mut().zip(&channel[p]) {
                    *m += v;
                }
            }
            for m in &mut mean {
                *m /= geometry.channels as f64;
            }
            mean
        })
        .collect();

    let Ok(collapsed_set) = align_spl(&collapsed, grid.freqs(), settings.align_spl_band) else {
        return empty_products(grid, geometry.channels);
    };
    let mut averaged_db = Vec::with_capacity(geometry.channels);
    for channel in &curves {
        let Ok(set) = align_spl(channel, grid.freqs(), settings.align_spl_band) else {
            return empty_products(grid, geometry.channels);
        };
        let Some(average) = average(&set, settings.averaging) else {
            return empty_products(grid, geometry.channels);
        };
        averaged_db.push(average);
    }

    AnalysisProducts {
        averaged_db,
        channels: geometry.channels,
        excess_group_delay_s: egd_source
            .map(|(gated, rate)| egd_on_grid(&gated, rate, grid))
            .unwrap_or_else(|| vec![0.0; grid.len()]),
        per_position_db: collapsed_set.measurements_db().to_vec(),
        sigma_db: sigma_db(&collapsed_set),
    }
}

/// The products for a bundle the pipeline could not analyse: no curves, no
/// authority to speak of (σ pinned at the value where the confidence weight
/// reaches zero), and a flat EGD trace. See the module header for why this is
/// empty rather than fabricated.
fn empty_products(grid: &LogGrid, channels: usize) -> AnalysisProducts {
    AnalysisProducts {
        averaged_db: Vec::new(),
        channels,
        excess_group_delay_s: vec![0.0; grid.len()],
        per_position_db: Vec::new(),
        sigma_db: vec![SIGMA_NONE_DB; grid.len()],
    }
}

/// Stage 1 — the L/R gate around the peak. Returns the gated slice and the
/// left window `apply_gate` actually applied, which is where the peak sits in
/// that slice and therefore the bulk delay `derotate` removes.
fn gate(
    samples: &[f64],
    peak: usize,
    sample_rate: u32,
    right_ms: f64,
    bundle: &MeasurementBundle,
    settings: &AnalysisSettings,
) -> Option<(Vec<f64>, f64)> {
    let sweep = &bundle.capture.sweep;
    let ir = GatedIr {
        peak: peak as f64,
        sample_rate,
        samples: samples.to_vec(),
    };
    let spec = GateSpec {
        left_ms: settings.left_window_ms,
        right_ms,
        // Given, not omitted: with the sweep in hand `apply_gate` bounds the
        // left window by the Farina H2 arrival as well as by the peak, and the
        // `left_window_ms` rule has already taken the stricter half of that
        // bound, so this check never binds and is free insurance.
        sweep: Some(SweepParams {
            duration_s: sweep.duration_s,
            f1: sweep.f_start_hz,
            f2: sweep.f_end_hz,
        }),
        window: settings.window,
    };
    let (gated, report) = apply_gate(&ir, &spec).ok()?;
    Some((gated, report.applied_left_ms))
}

/// Stages 2–7 — spectrum, derotate, log grid, FDW, magnitude dB,
/// compensation, smoothing.
fn curve_on_grid(
    gated: &[f64],
    sample_rate: u32,
    applied_left_ms: f64,
    grid: &LogGrid,
    bundle: &MeasurementBundle,
    settings: &AnalysisSettings,
) -> Option<Vec<f64>> {
    // Zero-padded to the next power of two. `complex_spectrum` documents
    // `n_fft` as "a zero-PAD length, never a truncation", so this changes the
    // axis density and nothing else — and it keeps the transform off the
    // large-prime lengths a gate in milliseconds lands on.
    let n_fft = gated.len().next_power_of_two();
    let (freqs_linear, spectrum) = complex_spectrum(gated, sample_rate, Some(n_fft)).ok()?;
    // MANDATORY before the log resample: "at 20 kHz a 50 ms delay winds ~1000
    // full turns and no log grid could sample it". The bulk delay is where
    // `apply_gate` put the peak in its own output — at `applied_left_ms`.
    let derotated = derotate(&spectrum, &freqs_linear, applied_left_ms / 1000.0);
    let resampled =
        resample_complex_to_log_grid(&freqs_linear, &derotated, grid, RESAMPLE_PREFILTER).ok()?;
    // `apply_fdw` reads `post_cycles` ONLY, and that is by design: "`spec.pre_cycles`
    // does not enter: it is a pre-peak noise gate, not a resolution control",
    // realized in the TIME domain by `gating.rs` as a fixed left gate of
    // `FdwSpec::left_gate_s(f_min) = pre_cycles / f_min`.
    //
    // On this architecture that gate never binds, and the arithmetic says why:
    // the default 3 cycles at the grid's 20 Hz is 150 ms of pre-peak window
    // against the ~46-64 ms the Mac's audio path leaves before the impulse
    // arrives. `left_window_ms` is already clamped to `t_peak`, so the left gate
    // it produces is the SHORTER of the two and the pre-lobe is satisfied by
    // construction. Recorded here because "the `fdw_pre_cycles` decision has no
    // effect" is otherwise a thing a reader discovers by experiment.
    let windowed = match settings.fdw {
        Some(spec) => apply_fdw(&resampled, grid, &spec).ok()?,
        None => resampled,
    };

    let magnitude_db: Vec<f64> = windowed
        .iter()
        .map(|c| 20.0 * (c.norm() + MAGNITUDE_FLOOR).log10())
        .collect();
    let compensated = match &bundle.cal {
        // Applied AS-IS. Never normalized to 0 dB at a reference frequency: the
        // EARS jig encodes a real 2.1 dB L/R capsule offset IN the curve, and
        // erasing it would bake that imbalance into every correction.
        Some(cal) => apply_compensation(&magnitude_db, grid.freqs(), &cal.curve.0, &cal.curve.1),
        None => magnitude_db,
    };
    smooth(&compensated, grid, settings.smoothing.into()).ok()
}

/// Stage 8 — the spatial average. Room: power/RMS, after Align SPL. Coupler:
/// the dB-domain mean, which is correct there and fixture-locked.
///
/// Coherent (vector) averaging is unreachable from here by construction: it is
/// not a variant of `AveragingMode`, and `fr::average_measurements_vector`
/// refuses `n > 1` anyway. `CoherentAveragingRejected` therefore guards a door
/// the type already locked, which is the belt-and-braces the spec intends.
fn average(set: &AlignedSet, mode: AveragingMode) -> Option<Vec<f64>> {
    match mode {
        AveragingMode::DbMean => average_measurements(set.measurements_db()).ok(),
        AveragingMode::Power => Some(average_measurements_rms(set)),
    }
}

/// Stage 9 — the excess-group-delay trace, resampled onto the analysis grid.
///
/// **Evidence only in v1 (R7).** It is an input to no decision: the gate that
/// would consume it ships in v1.1 behind `EgdGate::On`, and `resolve_authority`
/// passes no mask.
///
/// One trace per bundle, from the first position's first channel, because
/// `Analysis::excess_group_delay_s` is one curve and not a per-position or
/// per-channel family. `Prefilter::None` on the resample: the anti-comb
/// prefilter is a lowpass justified for a magnitude spectrum, and averaging a
/// group delay over a neighbourhood is a different claim than the one the
/// prefilter's doc makes.
fn egd_on_grid(gated: &[f64], sample_rate: u32, grid: &LogGrid) -> Vec<f64> {
    let n_fft = gated.len().next_power_of_two();
    let Ok(egd) = excess_group_delay_s(gated, sample_rate, n_fft) else {
        return vec![0.0; grid.len()];
    };
    let freqs: Vec<f64> = (0..egd.len())
        .map(|i| i as f64 * f64::from(sample_rate) / n_fft as f64)
        .collect();
    resample_db_to_log_grid(&freqs, &egd, grid, Prefilter::None)
        .unwrap_or_else(|_| vec![0.0; grid.len()])
}
