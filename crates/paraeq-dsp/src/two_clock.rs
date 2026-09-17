//! Pure analysis and assembly for the two-clock problem: the timing markers a
//! stimulus is bracketed with, matched-filter marker location, least-squares
//! clock-skew estimation, and the reject bound on the adjustment that comes out
//! of it.
//!
//! **Moved here from `paraeq-coreaudio` in Stage 6 (build plan B12).** This
//! module's own header used to say it lived there "only because the `#[ignore]`d
//! hardware harness (tests/test_measure_hardware.rs) that feeds it real captures
//! lives here; it can migrate to a policy crate later" — this is that migration,
//! taken because `paraeq-measure` needs marker recovery for the verification
//! pass while CI forbids that crate from depending on `paraeq-coreaudio` (the
//! MS-1 dependency gate in `.github/workflows/ci.yml`). There is no FFI and no
//! `unsafe` here and there never was. `paraeq_coreaudio::two_clock` is now a
//! re-export of this module, so the hardware harness compiles unchanged.
//!
//! Method: the adopted two-clock resolution is REW's bracketed-timing-marker
//! skew estimate + resample (decision doc 2026-07-21 §Q6, ~12 ppm typical).
//! The marker is a Hann-windowed linear chirp: a chirp's matched filter
//! compresses to a narrow pulse, so cross-correlation gives sample-accurate
//! (sub-sample, after parabolic refinement) arrival times even through a
//! loudspeaker + room + mic chain. Playing markers at known positions on the
//! OUTPUT clock and locating them on the CAPTURE clock gives, via a linear
//! fit, the clock-rate ratio (skew, in ppm) and the per-marker residuals.
//!
//! Sign convention: positive [`SkewEstimate::skew_ppm`] means the capture
//! clock runs FAST relative to the playback clock (more capture samples
//! elapse between markers than playback samples).
//!
//! # Four markers, not two — and why the residual figure depends on it
//!
//! [`estimate_skew`] fits **two** parameters (an intercept and a slope). A
//! two-marker bracket therefore has **zero residual degrees of freedom**: the
//! fitted line passes exactly through both points and every entry of
//! [`SkewEstimate::residuals_samples`] is identically `0.0`. That figure is
//! surfaced to the user as evidence, so it must not be fiction — hence
//! [`MarkerLayout::markers_per_end`] defaults to **2**, giving four markers, two
//! degrees of freedom and a real `residual_peak_samples`.
//!
//! Two clusters of two is admittedly a weaker figure than four markers spread
//! evenly across the file: it mostly measures **intra-cluster** jitter, because
//! the two members of a pair sit 0.30 s apart while the pairs themselves sit a
//! whole sweep apart. It is nonetheless a **real** number rather than an
//! algebraic zero, and it is what the tight-bracket geometry allows — the
//! bracket has to hug the sweep or the file grows without buying anything.
//! Pinned by `four_markers_give_a_nonzero_residual_figure_and_two_do_not`.
//!
//! # Marker LEVEL is the caller's, and it is never 1.0
//!
//! [`default_marker`] is documented at peak **1.0**, and splicing it verbatim
//! into a file whose sweep sits at `L_verify ≈ −18 dBFS RMS` would put a
//! full-scale 50 ms burst into the transducer — louder than anything the
//! measurement caps table validated, and on the Direct path that burst is
//! tap-**excluded**, i.e. the one signal the engine's safety clamps never see
//! (`docs/specs/2026-07-15-measurement-safety-design.md`, the self-exclusion
//! invariant). So [`bracket`] takes `marker_scale` as an argument and the
//! caller passes the **realized peak of the levelled sweep**, `max|sweep|`. One
//! rule on both paths. Pinned by `markers_never_exceed_the_sweep_peak`.
//!
//! # The matched-filter template is an ARGUMENT, never built inside
//!
//! [`locate`] takes the template it correlates against. That asymmetry is the
//! only difference between the two capture paths and it must be visible at the
//! call site:
//!
//! - **Direct (baseline)** passes the raw marker, [`layout_marker`].
//! - **Helper (verification)** passes the marker **filtered through the
//!   installed correction**, `peq.apply_offline(layout_marker(..), rate) ×
//!   preamp_lin`.
//!
//! The reason is that the verification marker traverses the correction cascade
//! on its way to the mic, so correlating a *raw* template against a
//! *chain-shaped* marker biases the peak by the cascade's group delay in the
//! marker's 2–8 kHz band. That bias is **common-mode across both ends of the
//! bracket**, so it lands entirely in [`SkewEstimate::intercept_samples`] —
//! which is exactly the number used as `t = 0` — and it is invisible to a
//! residual-scatter gate. Filtering the template is unbiased by construction
//! (the autocorrelation of `h*m` peaks at zero lag) and it corrects magnitude
//! shaping as well as phase. Pinned by
//! `the_correction_biases_the_intercept_and_the_filtered_template_removes_it`
//! and by `skew_is_invariant_to_the_correction`.
//!
//! # Test tiers
//!
//! The three functions that moved keep their original declaration verbatim:
//!
//! > Test tier: 3 — analytic invariants (a synthetic capture with a known
//! > inserted skew must return that skew; a marker train embedded at known
//! > offsets must be found exactly). The hardware numbers themselves come only
//! > from the `#[ignore]`d harness on the owner's rig.
//!
//! — and that covers [`default_marker`], [`marker_chirp`], [`find_marker_train`]
//! and [`estimate_skew`]. The items added by the move declare their own:
//!
//! - [`MarkerLayout`] and [`SweepSpan`] — plain data. **No tier**, stated rather
//!   than omitted, because a silent omission reads as an oversight. They carry
//!   no arithmetic of their own; the arithmetic is in the three functions below
//!   and is graded there.
//! - [`MAX_CLOCK_ADJUST_PPM`] — a `[NEEDS DATA]` constant, not a computation.
//!   **No tier.**
//! - [`bracket`] — **Tier 3.** Splicing at known offsets and scaling by a
//!   supplied factor has no library delegate, and a numpy transcription of our
//!   own layout would launder the layout into a fixture, which is the one thing
//!   `prototype/tools/generate_fixtures.py`'s Tier-2 rule forbids ("the
//!   fixture's provenance is the library, not our own ported code").
//! - [`expected_marker_positions`] and [`layout_marker`] — **Tier 3**, same
//!   reason: they are the layout, and the layout is ours. The invariant that
//!   matters is that assembly and analysis agree, which is an in-crate
//!   round trip (`expected_marker_positions_matches_the_assembled_offsets`,
//!   `the_default_layouts_marker_is_exactly_default_marker`).
//! - [`locate`] — **Tier 3.** It is a thin adapter over [`find_marker_train`],
//!   which the declaration above already covers for the same reason.

/// One located marker in a capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerHit {
    /// Matched-filter value at the peak (sign preserved: a polarity-inverted
    /// chain correlates negative, which is legal and detected via |corr|).
    pub peak: f64,
    /// Fractional sample index (in the capture) of the marker's START, after
    /// parabolic sub-sample refinement.
    pub position: f64,
    /// Peak-to-floor ratio of the matched filter in dB, floor = RMS of the
    /// correlation outside every marker's exclusion zone. `f64::INFINITY` on
    /// a noiseless synthetic capture.
    pub snr_db: f64,
}

/// Least-squares fit of measured marker positions against expected ones:
/// `measured ≈ intercept + (1 + skew_ppm·1e-6) · expected`.
#[derive(Clone, Debug, PartialEq)]
pub struct SkewEstimate {
    /// Fit intercept in capture samples — the constant transport offset
    /// (playback start latency + acoustic propagation).
    pub intercept_samples: f64,
    /// Per-marker residuals from the fit, in capture samples — the t=0
    /// jitter figure the two-clock risk row asks for, **and only with three or
    /// more markers**. The fit has two parameters, so a two-marker bracket has
    /// zero residual degrees of freedom and every entry here is identically
    /// `0.0`: the line passes exactly through both points. That is why
    /// [`MarkerLayout::markers_per_end`] is 2 and the product's bracket carries
    /// four. With four markers the figure is real but weighted toward
    /// intra-pair jitter — see the module header.
    pub residuals_samples: Vec<f64>,
    pub residual_peak_samples: f64,
    pub residual_rms_samples: f64,
    /// Clock-rate difference in parts per million; see the module docs for
    /// the sign convention.
    pub skew_ppm: f64,
}

/// The default timing marker: 50 ms Hann-windowed linear chirp, 2→8 kHz,
/// peak 1.0. Short enough to bracket tightly, wide-band enough (6 kHz) for a
/// matched-filter main lobe of a fraction of a millisecond.
pub fn default_marker(sample_rate_hz: f64) -> Vec<f64> {
    marker_chirp(sample_rate_hz, 0.05, 2_000.0, 8_000.0)
}

/// Hann-windowed linear chirp `f_start → f_end` over `duration_s`, peak ≤ 1.
/// Endpoints are exactly 0.0 (the window), so splicing it into silence never
/// clicks.
pub fn marker_chirp(
    sample_rate_hz: f64,
    duration_s: f64,
    f_start_hz: f64,
    f_end_hz: f64,
) -> Vec<f64> {
    let n = (duration_s * sample_rate_hz).round() as usize;
    if n < 2 {
        return vec![0.0; n];
    }
    let sweep_rate = (f_end_hz - f_start_hz) / duration_s;
    (0..n)
        .map(|i| {
            let t = i as f64 / sample_rate_hz;
            // Linear chirp phase: 2π (f0 t + k t² / 2).
            let phase = 2.0 * std::f64::consts::PI * (f_start_hz * t + 0.5 * sweep_rate * t * t);
            // Symmetric Hann window (0.0 at both endpoints).
            let window =
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos());
            window * phase.sin()
        })
        .collect()
}

/// Locate exactly `expected` markers in `capture` by matched filter.
///
/// Greedy peak-picking on |correlation| with an exclusion zone of
/// ±`min_separation` samples around each pick, then a threshold: every picked
/// peak must clear 6× the off-peak correlation RMS (≈15.6 dB), else the train
/// is not credibly present and the answer is `None` — the caller's
/// `Warn(TwoClock)` path, never a fabricated estimate. `None` also when the
/// exclusion zones cover the whole correlation (no off-peak samples ⇒ no floor
/// ⇒ no credibility judgment). Hits are returned sorted by position.
pub fn find_marker_train(
    capture: &[f64],
    marker: &[f64],
    expected: usize,
    min_separation: usize,
) -> Option<Vec<MarkerHit>> {
    if expected == 0 || marker.is_empty() || capture.len() < marker.len() {
        return None;
    }
    let corr = cross_correlate(capture, marker);
    let separation = min_separation.max(1);

    // Greedy peak-picking on |corr| with exclusion zones.
    let mut excluded = vec![false; corr.len()];
    let mut picks: Vec<usize> = Vec::with_capacity(expected);
    for _ in 0..expected {
        let best = corr
            .iter()
            .enumerate()
            .filter(|&(i, _)| !excluded[i])
            .max_by(|a, b| {
                a.1.abs()
                    .partial_cmp(&b.1.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?;
        let idx = best.0;
        picks.push(idx);
        let lo = idx.saturating_sub(separation);
        let hi = (idx + separation + 1).min(corr.len());
        excluded[lo..hi].fill(true);
    }

    // Off-peak floor: RMS of the correlation outside every exclusion zone.
    let mut noise_energy = 0.0f64;
    let mut noise_count = 0usize;
    for (i, &c) in corr.iter().enumerate() {
        if !excluded[i] {
            noise_energy += c * c;
            noise_count += 1;
        }
    }
    // No off-peak samples at all (the exclusion zones cover the ENTIRE
    // correlation — a capture short enough that one pick's ±separation zone
    // blankets it): no floor can be estimated, so no credibility judgment can
    // be made. Refuse rather than fabricate. A `floor = 0.0` here would
    // silently disable BOTH the 6x gate below (`floor > 0.0` is false) and
    // make every `snr_db` INFINITY, passing any pick as a credible marker.
    // This is the `Warn(TwoClock)`/no-estimate path. NOTE the deliberate
    // asymmetry with `noise_energy == 0`: real silence outside the exclusion
    // zones still has `noise_count > 0` and legitimately yields `floor = 0.0`
    // with an INFINITY SNR (the "embed into silence" case) — only the
    // all-excluded (`noise_count == 0`) case is refused here.
    if noise_count == 0 {
        return None;
    }
    let floor = (noise_energy / noise_count as f64).sqrt();

    picks.sort_unstable();
    let mut hits = Vec::with_capacity(expected);
    for &idx in &picks {
        let peak = corr[idx];
        if peak == 0.0 {
            return None; // no signal at all
        }
        // 6x the floor (~15.6 dB): below this the "marker" is not credibly
        // present and no estimate may be formed.
        if floor > 0.0 && peak.abs() < 6.0 * floor {
            return None;
        }
        let snr_db = if floor > 0.0 {
            20.0 * (peak.abs() / floor).log10()
        } else {
            f64::INFINITY
        };
        hits.push(MarkerHit {
            peak,
            position: idx as f64 + parabolic_offset(&corr, idx),
            snr_db,
        });
    }
    Some(hits)
}

/// Naive full cross-correlation, `corr[i] = Σ_j capture[i+j]·marker[j]`.
/// O(N·M); fine for a harness (run the hardware tests with `--release`).
fn cross_correlate(capture: &[f64], marker: &[f64]) -> Vec<f64> {
    let n = capture.len() - marker.len() + 1;
    (0..n)
        .map(|i| {
            marker
                .iter()
                .zip(&capture[i..i + marker.len()])
                .map(|(&m, &c)| m * c)
                .sum()
        })
        .collect()
}

/// Sub-sample peak refinement: fit a parabola through corr² at
/// `idx-1, idx, idx+1`; the vertex offset is within ±0.5 for a true local
/// maximum (clamped defensively).
fn parabolic_offset(corr: &[f64], idx: usize) -> f64 {
    if idx == 0 || idx + 1 >= corr.len() {
        return 0.0;
    }
    let y0 = corr[idx - 1] * corr[idx - 1];
    let y1 = corr[idx] * corr[idx];
    let y2 = corr[idx + 1] * corr[idx + 1];
    let denom = y0 - 2.0 * y1 + y2;
    if denom == 0.0 {
        0.0
    } else {
        (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5)
    }
}

/// Least-squares linear fit of `measured` (capture-clock positions) against
/// `expected` (playback-clock positions). `None` when fewer than two markers,
/// mismatched lengths, or degenerate (all expected positions equal) — the
/// cases where no skew estimate can be formed.
pub fn estimate_skew(expected: &[f64], measured: &[f64]) -> Option<SkewEstimate> {
    if expected.len() != measured.len() || expected.len() < 2 {
        return None;
    }
    let n = expected.len() as f64;
    let mean_e = expected.iter().sum::<f64>() / n;
    let mean_m = measured.iter().sum::<f64>() / n;
    let sxx: f64 = expected.iter().map(|&x| (x - mean_e) * (x - mean_e)).sum();
    if sxx == 0.0 {
        return None; // all expected positions identical: no slope exists
    }
    let sxy: f64 = expected
        .iter()
        .zip(measured)
        .map(|(&x, &y)| (x - mean_e) * (y - mean_m))
        .sum();
    let slope = sxy / sxx;
    let intercept = mean_m - slope * mean_e;
    let residuals: Vec<f64> = expected
        .iter()
        .zip(measured)
        .map(|(&x, &y)| y - (intercept + slope * x))
        .collect();
    let residual_rms =
        (residuals.iter().map(|&r| r * r).sum::<f64>() / residuals.len() as f64).sqrt();
    let residual_peak = residuals.iter().fold(0.0f64, |a, &r| a.max(r.abs()));
    Some(SkewEstimate {
        intercept_samples: intercept,
        residuals_samples: residuals,
        residual_peak_samples: residual_peak,
        residual_rms_samples: residual_rms,
        skew_ppm: (slope - 1.0) * 1e6,
    })
}

// ---------------------------------------------------------------------------
// Added with the Stage 6 move (build plan B12): the bracket layout, its
// assembly and recovery halves, and §Q6's reject bound on the adjustment.
// ---------------------------------------------------------------------------

/// §Q6's "clock adjustment too large" reject bound, in parts per million.
///
/// `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q6, verbatim:
/// "set a **'clock adjustment too large' reject bound** from that data (REW
/// warns above its own bound)". A credibility ladder that gates
/// [`SkewEstimate::residual_peak_samples`] **only** would bless an absurd
/// `skew_ppm` that happens to fit tightly — four markers on a straight line fit
/// a straight line perfectly no matter how steep it is — and then stretch the
/// capture by half a percent on the strength of it. This constant is the only
/// thing between that fit and that resample; the consumer refuses above it.
///
/// **`[NEEDS DATA]`.** Cut against §Q6's own measured figure — "Only 12 ppm …
/// when output and input are on different devices" — with an order of magnitude
/// of headroom, so a real rig has room to be worse than REW's without being
/// refused, while a mis-scaled fit (a factor of two is 500 000 ppm) cannot pass.
/// Retune it from the first hardware runs; it is a starting value, not a
/// measurement. Pinned by `an_absurd_ppm_with_a_tight_fit_still_refuses`.
pub const MAX_CLOCK_ADJUST_PPM: f64 = 200.0;

/// The shipped bracket, and the only one the product assembles.
///
/// ```text
///  0.50 lead-in │ M1a 0.05 │ 0.25 │ M1b 0.05 │ 0.25 │ SWEEP T │ 0.25 │ M2a 0.05 │ 0.25 │ M2b 0.05 │ 0.50 tail
/// ```
///
/// 2.20 s of overhead, so a sweep capped at 5.5 s per class produces a 7.70 s
/// file. `markers_per_end = 2` is the four-marker bracket the module header
/// argues for. Pinned by `the_default_layout_costs_two_point_two_seconds`.
pub const DEFAULT_MARKER_LAYOUT: MarkerLayout = MarkerLayout {
    guard_gap_s: 0.25,
    lead_in_s: 0.50,
    marker_s: 0.05,
    markers_per_end: 2,
    pair_gap_s: 0.25,
    tail_s: 0.50,
};

/// Where the timing markers sit, in seconds on the PLAYBACK clock.
///
/// Recorded with every capture, because two captures that are aligned by
/// different means cannot be subtracted: the verification pass compares a
/// corrected measurement against an uncorrected baseline, and that subtraction
/// is only apples-to-apples if both were bracketed the same way.
///
/// See [`DEFAULT_MARKER_LAYOUT`] for the timeline this describes. Fields are in
/// alphabetical order, not timeline order — the diagram is the timeline.
///
/// Durations that are negative, `NaN` or absurd collapse to zero frames rather
/// than panicking; the resulting file is short and unbracketed, and [`locate`]
/// then returns `None`, which is the caller's refusal path.
///
/// **No test tier** — plain data, no arithmetic of its own. See the module
/// header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerLayout {
    /// Silence between the lead-in marker pair and the sweep, and between the
    /// sweep and the trailing pair.
    pub guard_gap_s: f64,
    /// Silence before the first marker. It exists so the first marker cannot
    /// fall before the capture's first sample when the capture starts late.
    pub lead_in_s: f64,
    /// Length of one marker chirp. [`DEFAULT_MARKER_LAYOUT`] uses 0.05, which
    /// makes [`layout_marker`] exactly [`default_marker`].
    pub marker_s: f64,
    /// Markers at EACH end, so the train has `2 × markers_per_end` members.
    /// 2 is the shipped value; 1 is degenerate (the fit then has zero residual
    /// degrees of freedom — see the module header) and 3 is what the hardware
    /// harness uses.
    pub markers_per_end: usize,
    /// Silence between the two markers of one pair.
    pub pair_gap_s: f64,
    /// Silence after the last marker.
    pub tail_s: f64,
}

impl MarkerLayout {
    /// Frames from the sweep's last sample to the first trailing marker's start
    /// is [`MarkerLayout::guard_gap_s`]; this is that gap in frames.
    pub fn guard_gap_frames(&self, sample_rate_hz: f64) -> usize {
        frames(self.guard_gap_s, sample_rate_hz)
    }

    /// Frames of silence before the first marker.
    pub fn lead_in_frames(&self, sample_rate_hz: f64) -> usize {
        frames(self.lead_in_s, sample_rate_hz)
    }

    /// Length of one marker in frames. Equals `layout_marker(..).len()` by
    /// construction: both round `marker_s × rate`.
    pub fn marker_frames(&self, sample_rate_hz: f64) -> usize {
        frames(self.marker_s, sample_rate_hz)
    }

    /// Total markers in the train, `2 × markers_per_end`. This is the count
    /// [`locate`] asks [`find_marker_train`] for and the length
    /// [`expected_marker_positions`] returns.
    pub fn markers(&self) -> usize {
        2 * self.markers_per_end
    }

    /// Frames of silence between the two markers of one pair.
    pub fn pair_gap_frames(&self, sample_rate_hz: f64) -> usize {
        frames(self.pair_gap_s, sample_rate_hz)
    }

    /// Start-to-start spacing inside a pair, in frames: one marker plus one
    /// pair gap. The tightest spacing anywhere in the train, which is what sets
    /// [`locate`]'s matched-filter exclusion radius.
    pub fn pair_step_frames(&self, sample_rate_hz: f64) -> usize {
        self.marker_frames(sample_rate_hz) + self.pair_gap_frames(sample_rate_hz)
    }

    /// Frame index of the sweep's first sample in an assembled file.
    pub fn sweep_start(&self, sample_rate_hz: f64) -> usize {
        let lead = self.lead_in_frames(sample_rate_hz);
        if self.markers_per_end == 0 {
            return lead;
        }
        lead + (self.markers_per_end - 1) * self.pair_step_frames(sample_rate_hz)
            + self.marker_frames(sample_rate_hz)
            + self.guard_gap_frames(sample_rate_hz)
    }

    /// Frames of silence after the last marker.
    pub fn tail_frames(&self, sample_rate_hz: f64) -> usize {
        frames(self.tail_s, sample_rate_hz)
    }

    /// Total length of the assembled file, in frames, for a sweep of
    /// `sweep_len` samples.
    pub fn total_len(&self, sweep_len: usize, sample_rate_hz: f64) -> usize {
        let after_sweep = self.sweep_start(sample_rate_hz) + sweep_len;
        if self.markers_per_end == 0 {
            return after_sweep + self.tail_frames(sample_rate_hz);
        }
        after_sweep
            + self.guard_gap_frames(sample_rate_hz)
            + (self.markers_per_end - 1) * self.pair_step_frames(sample_rate_hz)
            + self.marker_frames(sample_rate_hz)
            + self.tail_frames(sample_rate_hz)
    }
}

/// Where the sweep sits inside an assembled, bracketed file.
///
/// The verification level book measures `level_dbfs` over the SWEEP SPAN and
/// not over the file, because the lead-in, the gaps and the tail are silence
/// and would drag the RMS down by an amount that depends on the layout rather
/// than on the level. This is the span it means.
///
/// **No test tier** — plain data. See the module header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweepSpan {
    /// Sweep length in samples. Identical to the input sweep's length:
    /// [`bracket`] copies, never resamples or re-levels.
    pub len: usize,
    /// Frame index of the sweep's first sample.
    pub start: usize,
}

/// Splice `sweep` into a bracketed file: lead-in, a marker pair, a guard, the
/// sweep, a guard, a marker pair, a tail.
///
/// `marker_scale` is the linear amplitude each marker is scaled to. Pass the
/// **realized peak of the levelled sweep**, `max|sweep|` — see the module
/// header's level section for why the raw peak-1.0 marker must not be spliced
/// verbatim. A scale that is not a finite number in `[0, 1]` is clamped into
/// range (`NaN` becomes 0), because a `NaN` or out-of-range sample cannot be
/// written to a WAV and a silently missing marker fails loudly one step later:
/// [`locate`] returns `None` and the caller refuses.
///
/// The returned [`SweepSpan`] locates the sweep, whose samples are copied
/// **bit-identically** — the gaps around it are silence, so nothing is added
/// into the span and the span's RMS is the sweep's RMS.
///
/// **Tier 3.** See the module header.
pub fn bracket(
    sweep: &[f64],
    layout: &MarkerLayout,
    marker_scale: f64,
    sample_rate_hz: f64,
) -> (Vec<f64>, SweepSpan) {
    let start = layout.sweep_start(sample_rate_hz);
    let total = layout.total_len(sweep.len(), sample_rate_hz);
    let mut out = vec![0.0; total];
    out[start..start + sweep.len()].copy_from_slice(sweep);

    let scale = if marker_scale.is_finite() {
        marker_scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if scale > 0.0 {
        let marker = layout_marker(layout, sample_rate_hz);
        for position in expected_marker_positions(layout, sweep.len(), sample_rate_hz) {
            let at = position as usize;
            // Added rather than assigned: the marker regions are silence by
            // construction, so this is the same value, and if a future layout
            // ever overlapped something the overlap would be audible rather
            // than silently overwritten.
            for (i, &m) in marker.iter().enumerate() {
                if at + i < total {
                    out[at + i] += scale * m;
                }
            }
        }
    }
    (
        out,
        SweepSpan {
            len: sweep.len(),
            start,
        },
    )
}

/// Marker START positions in an assembled file, in PLAYBACK-clock samples, in
/// order: the lead-in pair, then the trailing pair.
///
/// This is the `expected` half of [`estimate_skew`]'s fit. It is a single pure
/// function so that assembly ([`bracket`]) and analysis ([`locate`] plus the
/// fit) cannot disagree about where the markers are — the one bug class that
/// would produce a confident, wrong `t = 0`.
///
/// **Tier 3.** See the module header.
pub fn expected_marker_positions(
    layout: &MarkerLayout,
    sweep_len: usize,
    sample_rate_hz: f64,
) -> Vec<f64> {
    let count = layout.markers_per_end;
    let mut positions = Vec::with_capacity(2 * count);
    if count == 0 {
        return positions;
    }
    let step = layout.pair_step_frames(sample_rate_hz);
    let lead = layout.lead_in_frames(sample_rate_hz);
    for k in 0..count {
        positions.push((lead + k * step) as f64);
    }
    let trailing =
        layout.sweep_start(sample_rate_hz) + sweep_len + layout.guard_gap_frames(sample_rate_hz);
    for k in 0..count {
        positions.push((trailing + k * step) as f64);
    }
    positions
}

/// The marker chirp a given layout splices: a Hann-windowed 2→8 kHz linear
/// chirp of `layout.marker_s` seconds, peak 1.0 before [`bracket`] scales it.
///
/// [`default_marker`] is the `marker_s = 0.05` case and stays the definition of
/// the band; this function exists so the caller building [`locate`]'s template
/// reads the length off the same layout the file was assembled from instead of
/// guessing. Pinned by `the_default_layouts_marker_is_exactly_default_marker`.
///
/// **Tier 3.** See the module header.
pub fn layout_marker(layout: &MarkerLayout, sample_rate_hz: f64) -> Vec<f64> {
    marker_chirp(sample_rate_hz, layout.marker_s, 2_000.0, 8_000.0)
}

/// Locate a layout's whole marker train in `capture` and return the members'
/// CAPTURE-clock positions, in order.
///
/// `template` is the waveform to correlate against and is deliberately **not**
/// built here — see the module header. Pass [`layout_marker`] on the baseline
/// path and the correction-filtered marker on the verification path.
///
/// The exclusion radius handed to [`find_marker_train`] is **half the tightest
/// start-to-start spacing in the train**, which is the within-pair step
/// (0.15 s for [`DEFAULT_MARKER_LAYOUT`], against a 0.30 s spacing). Half, not
/// the whole step: the zones must not tile the correlation, or there are no
/// off-peak samples left, no noise floor can be estimated and
/// [`find_marker_train`] refuses for want of a credibility judgment.
///
/// Returns `None` exactly when [`find_marker_train`] does — most importantly
/// when a picked peak fails its own credibility floor ("every picked peak must
/// clear 6× the off-peak correlation RMS (≈15.6 dB), else the train is not
/// credibly present"), which is the first thing that fails when the
/// verification sweep is quiet. That branch is load-bearing: the caller maps it
/// to its own refusal rather than to a generic not-found.
///
/// **Tier 3.** See the module header.
pub fn locate(
    capture: &[f64],
    template: &[f64],
    layout: &MarkerLayout,
    sample_rate_hz: f64,
) -> Option<Vec<f64>> {
    let separation = layout.pair_step_frames(sample_rate_hz) / 2;
    let hits = find_marker_train(capture, template, layout.markers(), separation.max(1))?;
    Some(hits.iter().map(|hit| hit.position).collect())
}

/// Seconds to frames, rounding to nearest, with every unrepresentable answer
/// collapsing to zero rather than to a panic or a wrapped length.
fn frames(seconds: f64, sample_rate_hz: f64) -> usize {
    let n = (seconds * sample_rate_hz).round();
    if n.is_finite() && n > 0.0 && n < usize::MAX as f64 {
        n as usize
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 16_000.0;

    /// Deterministic LCG noise in [-amp, amp] (no rand dependency; seeded).
    fn lcg_noise(len: usize, amp: f64, seed: u64) -> Vec<f64> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let unit = (state >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
                amp * (2.0 * unit - 1.0)
            })
            .collect()
    }

    /// Embed `marker` (scaled by `gain`) into `signal` starting at `at`.
    fn embed(signal: &mut [f64], marker: &[f64], at: usize, gain: f64) {
        for (i, &m) in marker.iter().enumerate() {
            signal[at + i] += gain * m;
        }
    }

    /// Simulate capture on a clock running `1 + ppm·1e-6` times the playback
    /// clock: capture[k] = signal(k / ratio), linear interpolation.
    fn resample_by_ppm(signal: &[f64], ppm: f64) -> Vec<f64> {
        let ratio = 1.0 + ppm * 1e-6;
        let out_len = ((signal.len() as f64 - 1.0) * ratio).floor() as usize;
        (0..out_len)
            .map(|k| {
                let x = k as f64 / ratio;
                let i = x.floor() as usize;
                let frac = x - i as f64;
                if i + 1 < signal.len() {
                    signal[i] * (1.0 - frac) + signal[i + 1] * frac
                } else {
                    signal[i]
                }
            })
            .collect()
    }

    #[test]
    fn marker_chirp_has_the_requested_length_peak_and_zero_endpoints() {
        let m = marker_chirp(RATE, 0.03, 1_000.0, 4_000.0);
        assert_eq!(m.len(), 480);
        let peak = m.iter().fold(0.0f64, |a, &v| a.max(v.abs()));
        assert!(peak <= 1.0, "peak {peak} must not exceed 1.0");
        assert!(peak > 0.5, "a Hann-windowed chirp peaks well above 0.5");
        assert_eq!(m[0], 0.0, "windowed start is exactly zero");
        assert!(
            m[m.len() - 1].abs() < 1e-9,
            "windowed end is (numerically) zero, got {}",
            m[m.len() - 1]
        );
    }

    #[test]
    fn find_marker_train_locates_clean_markers_exactly() {
        let marker = marker_chirp(RATE, 0.03, 1_000.0, 4_000.0);
        let mut signal = vec![0.0; 16_000];
        let starts = [2_000usize, 7_000, 12_000];
        for &s in &starts {
            embed(&mut signal, &marker, s, 0.8);
        }
        let hits = find_marker_train(&signal, &marker, 3, 2_000).expect("markers present");
        assert_eq!(hits.len(), 3);
        for (hit, &s) in hits.iter().zip(&starts) {
            assert!(
                (hit.position - s as f64).abs() < 0.5,
                "marker at {s} located at {}",
                hit.position
            );
            assert!(hit.peak > 0.0, "no polarity inversion in this synthesis");
            assert!(
                hit.snr_db > 20.0,
                "clean embed has high SNR, got {}",
                hit.snr_db
            );
        }
    }

    #[test]
    fn find_marker_train_survives_noise_and_polarity_inversion() {
        let marker = marker_chirp(RATE, 0.03, 1_000.0, 4_000.0);
        let mut signal = lcg_noise(16_000, 0.05, 42);
        let starts = [3_000usize, 9_000];
        for &s in &starts {
            embed(&mut signal, &marker, s, -0.7); // inverted chain
        }
        let hits = find_marker_train(&signal, &marker, 2, 2_000).expect("markers present");
        for (hit, &s) in hits.iter().zip(&starts) {
            assert!(
                (hit.position - s as f64).abs() < 1.0,
                "marker at {s} located at {}",
                hit.position
            );
            assert!(hit.peak < 0.0, "inverted marker correlates negative");
        }
    }

    #[test]
    fn find_marker_train_refuses_a_missing_marker() {
        let marker = marker_chirp(RATE, 0.03, 1_000.0, 4_000.0);
        let mut signal = lcg_noise(16_000, 0.02, 7);
        embed(&mut signal, &marker, 3_000, 0.8);
        embed(&mut signal, &marker, 9_000, 0.8);
        // Asking for 3 markers when only 2 exist: the third "peak" is noise
        // below the 6x floor threshold, so the train is refused.
        assert!(
            find_marker_train(&signal, &marker, 3, marker.len() * 4).is_none(),
            "a train with a missing marker must be refused, not fabricated"
        );
    }

    #[test]
    fn find_marker_train_refuses_degenerate_inputs() {
        let marker = marker_chirp(RATE, 0.03, 1_000.0, 4_000.0);
        assert!(find_marker_train(&[], &marker, 1, 100).is_none());
        assert!(find_marker_train(&[0.0; 100], &[], 1, 100).is_none());
        assert!(find_marker_train(&[0.0; 100], &marker, 0, 100).is_none());
        // capture shorter than the marker
        assert!(find_marker_train(&[0.0; 10], &marker, 1, 100).is_none());
        // all-zero capture: no peak can clear any threshold
        assert!(find_marker_train(&[0.0; 16_000], &marker, 1, 100).is_none());
    }

    #[test]
    fn find_marker_train_refuses_when_every_sample_is_excluded() {
        // A capture so short that the single pick's exclusion zone
        // (±min_separation) blankets the ENTIRE correlation, leaving zero
        // off-peak samples. With no off-peak samples no floor can be
        // estimated, so no 6x credibility judgment can be made — the train
        // must be refused (the Warn(TwoClock)/no-estimate path), never blessed
        // with an INFINITY SNR by a silently-zero floor.
        let marker = [1.0, 0.5, 0.25];
        let capture = [1.0, 1.0, 1.0, 1.0, 1.0]; // corr has 3 taps, all == 1.75
        assert!(
            find_marker_train(&capture, &marker, 1, 10).is_none(),
            "an all-excluded correlation has no floor and must be refused"
        );
    }

    #[test]
    fn find_marker_train_with_real_noise_reports_finite_snr_and_gates() {
        // Real off-peak noise (not exact silence) gives a POSITIVE floor, so
        // the SNR is finite and the 6x gate is live. This is the legitimate
        // `floor > 0` path the all-excluded refusal must not disturb.
        let marker = marker_chirp(RATE, 0.03, 1_000.0, 4_000.0);
        let mut signal = lcg_noise(16_000, 0.03, 99);
        let starts = [3_000usize, 9_000];
        for &s in &starts {
            embed(&mut signal, &marker, s, 0.9);
        }
        let hits =
            find_marker_train(&signal, &marker, 2, 2_000).expect("two strong markers present");
        for hit in &hits {
            assert!(
                hit.snr_db.is_finite(),
                "a real noise floor yields a finite SNR, got {}",
                hit.snr_db
            );
            assert!(
                hit.snr_db > 15.6,
                "strong markers clear the 6x (~15.6 dB) gate, got {}",
                hit.snr_db
            );
        }
        // The gate still refuses a pick that does NOT clear 6x the floor:
        // asking for a third marker that is not there finds only sub-threshold
        // noise, so the whole train is refused rather than fabricated.
        assert!(
            find_marker_train(&signal, &marker, 3, 2_000).is_none(),
            "a below-6x-floor pick fails the credibility gate"
        );
    }

    #[test]
    fn estimate_skew_recovers_a_known_slope_and_offset() {
        // 200 ppm fast capture clock, 1234-sample transport offset.
        let expected = [8_000.0, 16_000.0, 24_000.0, 32_000.0, 40_000.0];
        let measured: Vec<f64> = expected
            .iter()
            .map(|&e| 1_234.0 + e * (1.0 + 200e-6))
            .collect();
        let est = estimate_skew(&expected, &measured).expect("well-formed fit");
        assert!(
            (est.skew_ppm - 200.0).abs() < 1e-6,
            "exact line fits exactly, got {} ppm",
            est.skew_ppm
        );
        assert!((est.intercept_samples - 1_234.0).abs() < 1e-6);
        assert!(est.residual_rms_samples < 1e-9);
        assert!(est.residual_peak_samples < 1e-9);
        assert_eq!(est.residuals_samples.len(), 5);
    }

    #[test]
    fn estimate_skew_reports_jitter_as_residuals() {
        let expected = [0.0, 10_000.0, 20_000.0, 30_000.0];
        // Perfect clock (0 ppm) with +-0.5-sample jitter on two markers.
        let measured = [100.0, 10_100.5, 20_099.5, 30_100.0];
        let est = estimate_skew(&expected, &measured).expect("well-formed fit");
        assert!(
            est.skew_ppm.abs() < 20.0,
            "jitter must not read as skew, got {} ppm",
            est.skew_ppm
        );
        assert!(est.residual_rms_samples > 0.1, "jitter visible in rms");
        assert!(est.residual_peak_samples >= est.residual_rms_samples);
    }

    #[test]
    fn estimate_skew_refuses_degenerate_inputs() {
        assert!(estimate_skew(&[], &[]).is_none(), "no markers");
        assert!(estimate_skew(&[1.0], &[2.0]).is_none(), "one marker");
        assert!(
            estimate_skew(&[1.0, 2.0], &[1.0]).is_none(),
            "mismatched lengths"
        );
        assert!(
            estimate_skew(&[5.0, 5.0], &[1.0, 2.0]).is_none(),
            "degenerate expected positions"
        );
    }

    /// End-to-end: a marker train resampled by a known ppm must come back as
    /// that ppm. This is the headless proof of the whole analysis chain the
    /// hardware harness runs on real captures.
    #[test]
    fn synthetic_two_clock_roundtrip_recovers_inserted_skew() {
        let marker = marker_chirp(RATE, 0.03, 1_000.0, 4_000.0);
        let interval = 6_400usize; // 0.4 s
        let starts: Vec<usize> = (0..5).map(|k| 3_200 + k * interval).collect();
        let mut signal = vec![0.0; 3_200 + 4 * interval + marker.len() + 3_200];
        for &s in &starts {
            embed(&mut signal, &marker, s, 0.8);
        }

        for inserted_ppm in [0.0, 200.0, -150.0] {
            let capture = resample_by_ppm(&signal, inserted_ppm);
            // Separation is interval/4, not interval/2: the chirp's matched
            // filter main lobe is a few samples, so interval/4 amply separates
            // markers 0.4 s apart while leaving off-peak samples between the
            // exclusion zones — the correlation must NOT be fully tiled, or
            // there is no floor and find_marker_train refuses (no off-peak
            // samples ⇒ no credibility judgment).
            let hits = find_marker_train(&capture, &marker, starts.len(), interval / 4)
                .unwrap_or_else(|| panic!("markers findable at {inserted_ppm} ppm"));
            let expected: Vec<f64> = starts.iter().map(|&s| s as f64).collect();
            let measured: Vec<f64> = hits.iter().map(|h| h.position).collect();
            let est = estimate_skew(&expected, &measured).expect("fit");
            assert!(
                (est.skew_ppm - inserted_ppm).abs() < 20.0,
                "inserted {inserted_ppm} ppm, recovered {} ppm",
                est.skew_ppm
            );
            assert!(
                est.residual_rms_samples < 0.5,
                "linear-interp resample keeps sub-sample residuals, got {}",
                est.residual_rms_samples
            );
        }
    }
}
