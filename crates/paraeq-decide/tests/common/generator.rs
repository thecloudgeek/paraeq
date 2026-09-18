//! The deterministic input generator for `fixtures/decide/`.
//!
//! **Why a Rust generator and not `prototype/tools/generate_fixtures.py`.**
//! `fixtures/decide/` is the one sanctioned exception to CLAUDE.md's "generated
//! ONLY by `generate_fixtures.py`" rule: the Python oracle generates DSP
//! parity fixtures, and there is no prototype decision engine to port — writing
//! one for the purpose "would not be an independent oracle, it would launder a
//! design bug into a golden fixture". The provenance rule the fixture policy is
//! really about is still honoured: **no byte under `fixtures/decide/` is
//! hand-authored.** Every `bundle.json` and every `ir/*.f64` sidecar comes out
//! of this file, seeded, re-runnable and byte-identical. Only `notes.md`
//! (prose the owner signs) and `README.md` are written by hand, and the freeze
//! manifest digests them so a later edit to either is visible.
//!
//! **Determinism.** The only randomness is an xorshift64\* seeded per case from
//! the `SEED_*` constants below, so a re-run reproduces every byte. No clock,
//! no filesystem reads, no platform floats: every number here is produced by
//! `paraeq-dsp` or by arithmetic on literals.
//!
//! **The inputs do not depend on any rule.** `overrides` is
//! `Overrides::default()` everywhere and nothing here reads `decide()`. That is
//! what lets B7b/B7c move policy without moving a fixture input, and it is why
//! only `expected.json` is re-blessed when policy changes.
//!
//! Driver: `cargo test -p paraeq-decide --test test_golden_bundles --
//! --ignored regenerating_the_inputs_rewrites_every_bundle_and_sidecar`.

#![allow(dead_code)]

use super::golden;
use paraeq_decide::{
    CalFile, CalVariant, CapturePlan, CaptureRouting, CaptureStats, CorrectionPlan,
    ImpulseResponse, MeasurementBundle, NoiseFloor, Overrides, Position, SweepPlan,
    TransducerClass, TwoClockFit, Verification,
};
use paraeq_dsp::authority::Clamp;
use paraeq_dsp::peq::{EQBand, FilterType, ParametricEQ};
use paraeq_dsp::targets::TargetCurve;
use paraeq_dsp::PerChannel;

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Every case captures at 48 kHz — the Mac default, and the rate the shipped
/// store-window arithmetic is quoted at. One rate across the set keeps the
/// sidecars comparable; rate variation is `test_props.rs`'s job, over generated
/// bundles, where it costs nothing to commit.
const RATE: u32 = 48_000;

/// Pre-peak extent, ms. `crates/paraeq-measure/src/store.rs`'s `STORE_PRE_MS`,
/// on both paths: it is what a saved profile actually holds before the direct
/// arrival, and it covers the whole `left_window_ms` domain.
const PRE_MS: f64 = 100.0;

/// Post-peak extent for the ROOM cases, ms — the shipped `STORE_POST_MS`.
///
/// Not trimmed: the `fdw_post_cycles` domain ceiling is DERIVED from this
/// number (`analysis::fdw_post_ceiling` = `STORE_POST_MS/1000 · grid.f_min()`
/// = 30 cycles at 20 Hz), so a room fixture shorter than the store window would
/// make the top of that domain unreachable in the one place it is supposed to
/// be exercised.
const ROOM_POST_MS: f64 = 1500.0;

/// Post-peak extent for the COUPLER cases, ms.
///
/// Deliberately shorter, and honestly so: a coupler has no reflection problem
/// to gate away (`GatingMode::None`), its energy is gone inside ~30 ms, and
/// 250 ms still resolves to ~4 Hz — far finer than the 1/6-octave smoothing the
/// path uses. `analysis.rs` bounds the decided `right_window_ms` by the
/// post-peak data the recording holds, so a shorter recording windows what is
/// there rather than refusing. Storing the full 1.6 s here would triple the
/// coupler sidecars to hold silence.
const COUPLER_POST_MS: f64 = 250.0;

/// Direct-arrival amplitude every position is normalised to, EXCEPT the one
/// position `room_clipped_position` rails.
///
/// 0.5 rather than 1.0 so that "a sample at or above −0.3 dBFS (0.9661)" is a
/// property of one position in one case instead of being true of every IR in
/// the set by construction.
const NOMINAL_IR_PEAK: f64 = 0.5;

/// The railed position's peak: −0.006 dBFS, comfortably past the −0.3 dBFS the
/// decision table refuses a position at.
const CLIPPED_IR_PEAK: f64 = 0.9993;

// Per-case seeds. Distinct so no two cases share a reflection pattern, fixed so
// a re-run is byte-identical.
const SEED_BOOKSHELF_CLEAN_ROOM: u64 = 0x0B00_C1EA_0000_0001;
const SEED_FLOORSTANDER_NULL: u64 = 0x0F10_0400_0000_0002;
const SEED_IN_EAR_CLEAN: u64 = 0x1EA2_C1EA_0000_0003;
const SEED_OVER_EAR_HEQ: u64 = 0x0EA2_4E90_0000_0004;
const SEED_OVER_EAR_SEAL: u64 = 0x0EA2_5EA1_0000_0005;
const SEED_CAL_OUTLIER: u64 = 0x0CA1_0074_0000_0006;
const SEED_CLIPPED: u64 = 0x0C11_9BED_0000_0007;
const SEED_NOISY: u64 = 0x0405_E000_0000_0008;

// ---------------------------------------------------------------------------
// Deterministic noise
// ---------------------------------------------------------------------------

/// xorshift64\*, the same three-line generator `tests/common/mod.rs` already
/// uses for its synthetic bundles. No RNG crate, and the workspace must not
/// gain one for a fixture generator.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        // xorshift64 has a fixed point at zero.
        Rng(seed | 1)
    }

    /// Uniform in `[-1.0, 1.0)`. 53 bits is f64's mantissa, so the division is
    /// exact and the sequence is identical on every target.
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

// ---------------------------------------------------------------------------
// Impulse responses
// ---------------------------------------------------------------------------

fn samples_for(ms: f64) -> usize {
    (ms / 1000.0 * f64::from(RATE)).round() as usize
}

/// Scale so the largest absolute sample is exactly `peak`. Applied LAST, so a
/// case that wants a railed capture gets one at a known amplitude.
fn normalise(mut h: Vec<f64>, peak: f64) -> Vec<f64> {
    let max = h.iter().fold(0.0_f64, |acc, v| acc.max(v.abs()));
    assert!(
        max > 0.0,
        "an impulse response with no energy is not an input"
    );
    let scale = peak / max;
    for sample in h.iter_mut() {
        *sample *= scale;
    }
    h
}

/// The room, drawn once per case.
///
/// Positions 30 cm apart are five microphone placements in ONE room, not five
/// different rooms: they share the same boundaries, so they share the same
/// specular pattern AND the same decay, and what differs between them is a
/// modest amplitude and arrival jitter. Drawing either independently per
/// position — the obvious shortcut — produces seat-to-seat scatter of many dB,
/// which is not a room, and which the variance rule would be right to refuse.
struct RoomAcoustics {
    /// `(delay_ms, amplitude)`, twenty-four early reflections 2-80 ms after the
    /// direct arrival, each weaker than the last: the specular part.
    reflections: Vec<(f64, f64)>,
    /// The diffuse decay, one sample per post-peak sample, RT60 ≈ 0.45 s.
    tail: Vec<f64>,
}

fn room_acoustics(post_ms: f64, rng: &mut Rng) -> RoomAcoustics {
    let reflections = (1..=24)
        .map(|k| {
            let delay_ms = 2.0 + 78.0 * (f64::from(k) / 24.0);
            (delay_ms, 0.45 * (-(delay_ms / 26.0)).exp() * rng.next())
        })
        .collect();
    // tau = RT60 / 6.91 for RT60 = 0.45 s.
    let tau = 0.45 / 6.91;
    let tail = (0..samples_for(post_ms))
        .map(|offset| {
            let t = (offset + 1) as f64 / f64::from(RATE);
            0.05 * (-t / tau).exp() * rng.next()
        })
        .collect();
    RoomAcoustics { reflections, tail }
}

/// One position's impulse response: the room's direct arrival, reflection
/// pattern and decay, jittered by where the microphone stood.
///
/// The pre-peak region is not silent — a deconvolved capture carries the
/// sweep's own pre-ring there — but it is 60 dB down, which is what makes the
/// direct arrival findable at `peak`.
fn room_impulse(room: &RoomAcoustics, rng: &mut Rng) -> Vec<f64> {
    let peak = samples_for(PRE_MS);
    let len = peak + room.tail.len();
    let mut h = vec![0.0; len];

    for sample in h[..peak].iter_mut() {
        *sample = 1.0e-3 * rng.next();
    }
    h[peak] = 1.0;

    for (delay_ms, amplitude) in &room.reflections {
        // +/- 0.35 ms of path difference and +/- 15 % of level: one seat over.
        let index = peak + samples_for(delay_ms + 0.35 * rng.next());
        if index >= len {
            continue;
        }
        h[index] += amplitude * (1.0 + 0.15 * rng.next());
    }

    // The decay is the ROOM's, so it is shared and only its level moves with
    // the seat. A small independent part rides on top: that is the part of the
    // late field that really has lost its memory of the boundaries.
    let tail_gain = 1.0 + 0.10 * rng.next();
    let tau = 0.45 / 6.91;
    for (offset, sample) in h[peak + 1..].iter_mut().enumerate() {
        let t = (offset + 1) as f64 / f64::from(RATE);
        *sample += room.tail[offset] * tail_gain + 0.006 * (-t / tau).exp() * rng.next();
    }
    h
}

/// A plausible coupler impulse response: direct arrival, the canal reflection
/// ~0.25 ms later, and a tail gone inside ~25 ms. No room, so no reflections to
/// gate — which is why the coupler path runs `GatingMode::None`.
fn coupler_impulse(rng: &mut Rng) -> Vec<f64> {
    let peak = samples_for(PRE_MS);
    let len = peak + samples_for(COUPLER_POST_MS);
    let mut h = vec![0.0; len];

    for sample in h[..peak].iter_mut() {
        *sample = 5.0e-4 * rng.next();
    }
    h[peak] = 1.0;
    let canal = peak + samples_for(0.25);
    h[canal] += 0.28;

    let tau = 0.025 / 6.91;
    for (offset, sample) in h[peak + 1..].iter_mut().enumerate() {
        let t = (offset + 1) as f64 / f64::from(RATE);
        *sample += 0.05 * (-t / tau).exp() * rng.next();
    }
    h
}

/// Filter one channel through a band set at `rate_hz` — how every spectral
/// perturbation in this file is applied, so "a −30 dB null at 80 Hz" is a
/// biquad the DSP crate owns rather than a shape hand-drawn into samples.
fn filtered(samples: &[f64], bands: Vec<EQBand>, rate_hz: f64) -> Vec<f64> {
    ParametricEQ {
        bands,
        sample_rate: rate_hz,
    }
    .apply_offline(samples, rate_hz)
}

fn scaled(samples: &[f64], gain_db: f64) -> Vec<f64> {
    let gain = 10.0_f64.powf(gain_db / 20.0);
    samples.iter().map(|s| s * gain).collect()
}

// ---------------------------------------------------------------------------
// Shared bundle parts
// ---------------------------------------------------------------------------

/// The 1/3-octave grid the noise floor is reported on: 20 Hz · 2^(k/3), the top
/// point clamped to 20 kHz so the grid never claims data past the sweep.
fn third_octave_grid() -> Vec<f64> {
    let mut freqs: Vec<f64> = (0..=30)
        .map(|k| (20.0 * 2.0_f64.powf(f64::from(k) / 3.0) * 1000.0).round() / 1000.0)
        .collect();
    let last = freqs.len() - 1;
    freqs[last] = 20_000.0;
    freqs
}

/// A noise floor `offset_db` below the reference shape: −12 dB/decade of pink
/// tilt with a mains-ish bump at 50 Hz, which is what a real room floor looks
/// like and what makes a per-band SNR gate non-trivial.
fn noise_floor(channels: usize, offset_db: f64) -> NoiseFloor {
    let freqs_hz = third_octave_grid();
    let shape: Vec<f64> = freqs_hz
        .iter()
        .map(|f| {
            let tilt = -12.0 * (f / 20.0).log10();
            let mains = 6.0 * (-((f - 50.0) / 8.0).powi(2)).exp();
            // Rounded so the committed JSON stays readable; the value is the
            // input, and 1e-3 dB is far below anything that decides.
            ((-70.0 + tilt + mains + offset_db) * 1000.0).round() / 1000.0
        })
        .collect();
    // Broadband RMS of that spectrum, to the same 1e-3 dB.
    let power: f64 = shape.iter().map(|db| 10.0_f64.powf(db / 10.0)).sum();
    let rms = (10.0 * power.log10() * 1000.0).round() / 1000.0;
    NoiseFloor {
        freqs_hz,
        rms_dbfs: vec![rms; channels],
        spectrum_db: vec![shape; channels],
    }
}

/// A clean capture readout: nothing clipped, comfortable headroom.
fn clean_stats() -> CaptureStats {
    CaptureStats {
        clipped_samples: 0,
        peak_dbfs: -8.4,
        rms_dbfs: -26.1,
    }
}

/// The railed readout `room_clipped_position` carries on its bad position. The
/// ADC's own count, on the far side of the transducer — `MS-21`'s number, not
/// the engine's output clamp.
fn clipped_stats() -> CaptureStats {
    CaptureStats {
        clipped_samples: 4_096,
        peak_dbfs: -0.006,
        rms_dbfs: -11.2,
    }
}

fn capture_plan(
    class: TransducerClass,
    clock_skew_ppm: Option<f64>,
    sensitivity: f64,
    sweep_f_start_hz: f64,
) -> CapturePlan {
    CapturePlan {
        chain_sensitivity_spl_per_dbfs: Some(sensitivity),
        // The drawer's clock-adjust toggle is defaulted on, and the capture
        // layer records whether the resample actually ran.
        clock_adjusted: clock_skew_ppm.is_some(),
        clock_skew_ppm,
        input_present: true,
        input_rate: RATE,
        input_uid: match class {
            TransducerClass::Bookshelf | TransducerClass::Floorstander => {
                "UMIK-1:7005770".to_string()
            }
            TransducerClass::InEar | TransducerClass::OverEar => "miniDSP EARS:0A2F19".to_string(),
        },
        output_rate: RATE,
        output_uid: match class {
            TransducerClass::Bookshelf | TransducerClass::Floorstander => {
                "AppleUSBAudioEngine:Topping:D10s".to_string()
            }
            TransducerClass::InEar | TransducerClass::OverEar => {
                "AppleUSBAudioEngine:Schiit:Modi3".to_string()
            }
        },
        self_excluded: true,
        sweep: SweepPlan {
            duration_s: 5.5,
            f_end_hz: 20_000.0,
            f_start_hz: sweep_f_start_hz,
            level_dbfs: -18.0,
        },
        sweep_rate: RATE,
    }
}

/// Build a cal file whose `content` and `curve` cannot disagree: the text is
/// rendered FROM the pairs, so the verbatim provenance record and the parsed
/// curve are the same data.
fn cal_from_pairs(
    header: &str,
    pairs: &[(f64, f64)],
    sensitivity_db: Option<f64>,
    serial: &str,
    variant: CalVariant,
) -> CalFile {
    let mut content = String::new();
    content.push_str(header);
    for (freq, gain) in pairs {
        content.push_str(&format!("{freq:.3},{gain:.4}\n"));
    }
    CalFile {
        content,
        curve: (
            pairs.iter().map(|(f, _)| *f).collect(),
            pairs.iter().map(|(_, g)| *g).collect(),
        ),
        gain_db: Some(18.0),
        sensitivity_db,
        serial: Some(serial.to_string()),
        variant,
    }
}

/// A UMIK-1 sensitivity file: the vendor's own header line, then a coarse but
/// real transfer function. NEVER normalised to 0 dB anywhere.
fn umik_cal() -> CalFile {
    cal_from_pairs(
        "\"Sens Factor =-1.4dB\", SERNO: 7005770\n",
        &UMIK_PAIRS,
        Some(-1.4),
        "7005770",
        CalVariant::Plain,
    )
}

/// **The 10 kHz row is 0.75 dB, and it was 1.42.** On this coarse
/// twelve-point grid the neighbour-outlier rule reads `|g[i] − (g[i−1]+g[i+1])/2|`
/// against its 1.5 dB threshold, and 10 kHz is the LOG MIDPOINT of 5 kHz and
/// 20 kHz — so a 1.42 dB point between +0.63 and −2.08 deviates by 2.145 dB and
/// was a cal DEFECT in five of the eight cases (ruling R-A10). That is not what
/// a UMIK-1 file looks like and it is not what those cases are for: the vendor
/// defect belongs to `room_cal_neighbour_outlier` alone, which carries the real
/// `7005770_90deg.txt` zero at 19.611 Hz. 0.75 deviates by 1.475 dB, inside the
/// threshold with 0.025 dB to spare, and leaves the curve's shape — a gentle
/// HF rise then a top-octave roll-off — intact.
const UMIK_PAIRS: [(f64, f64); 12] = [
    (20.0, -3.13),
    (31.5, -1.86),
    (50.0, -0.94),
    (80.0, -0.41),
    (125.0, -0.12),
    (250.0, 0.04),
    (500.0, 0.00),
    (1000.0, 0.00),
    (2000.0, 0.21),
    (5000.0, 0.63),
    (10000.0, 0.75),
    (20000.0, -2.08),
];

/// A 711-class coupler cal — a plain transfer function, no target baked in.
fn coupler_cal() -> CalFile {
    cal_from_pairs(
        "\"Sens Factor =-0.9dB\", SERNO: 0A2F19\n",
        &COUPLER_PAIRS,
        Some(-0.9),
        "0A2F19",
        CalVariant::Plain,
    )
}

const COUPLER_PAIRS: [(f64, f64); 12] = [
    (20.0, 1.10),
    (31.5, 0.74),
    (50.0, 0.41),
    (80.0, 0.18),
    (125.0, 0.05),
    (250.0, -0.06),
    (500.0, 0.00),
    (1000.0, 0.00),
    (2000.0, -0.84),
    (5000.0, -3.92),
    (10000.0, -6.31),
    (20000.0, -9.04),
];

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

/// The nine-point log grid every target curve in this set is sampled on. Coarse
/// on purpose: `bundle.json` has to stay readable, and the target decision is a
/// choice among curves rather than a fit to one.
const TARGET_FREQS: [f64; 9] = [
    20.0, 50.0, 125.0, 315.0, 800.0, 2000.0, 5000.0, 12500.0, 20000.0,
];

fn target(name: &str, gains_db: [f64; 9], classes: Vec<TransducerClass>) -> TargetCurve {
    TargetCurve {
        name: name.to_string(),
        frequencies: TARGET_FREQS.to_vec(),
        gains_db: gains_db.to_vec(),
        category: None,
        classes,
        description: None,
        source: None,
    }
}

fn flat_target() -> TargetCurve {
    target(
        "flat",
        [0.0; 9],
        vec![
            TransducerClass::Bookshelf,
            TransducerClass::Floorstander,
            TransducerClass::InEar,
            TransducerClass::OverEar,
        ],
    )
}

fn room_targets() -> Vec<TargetCurve> {
    vec![
        target(
            "bk_1974",
            [6.0, 5.2, 3.4, 1.4, 0.0, -1.3, -3.0, -5.4, -6.6],
            vec![TransducerClass::Bookshelf, TransducerClass::Floorstander],
        ),
        flat_target(),
    ]
}

fn coupler_targets(class: TransducerClass) -> Vec<TargetCurve> {
    let mut curves = vec![
        target(
            "diffuse_field",
            [0.0, 0.0, 0.0, 0.4, 1.6, 6.2, 9.4, 1.1, -4.0],
            vec![TransducerClass::InEar, TransducerClass::OverEar],
        ),
        flat_target(),
    ];
    match class {
        TransducerClass::InEar => {
            curves.push(target(
                "harman_ie_2019",
                [7.6, 6.1, 2.4, 0.0, 0.2, 5.4, 8.8, -0.6, -5.2],
                vec![TransducerClass::InEar],
            ));
            curves.push(target(
                "harman_ie_2019_without_bass",
                [0.0, 0.0, 0.1, 0.0, 0.2, 5.4, 8.8, -0.6, -5.2],
                vec![TransducerClass::InEar],
            ));
        }
        _ => curves.push(target(
            "harman_oe_2018",
            [5.4, 4.3, 1.8, 0.0, 0.3, 4.9, 7.1, -1.8, -6.0],
            vec![TransducerClass::OverEar],
        )),
    }
    // `TargetCurve::classes` is documented as always sorted; the candidate list
    // itself is sorted by name so the committed JSON has one stable order.
    curves.sort_by(|a, b| a.name.cmp(&b.name));
    curves
}

// ---------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------

/// How one position's channels are built, after the base impulse response.
///
/// `perturb(position, channel, samples) -> samples` is where every case-specific
/// defect lives: the null, the lost seal, the railed capture. It runs BEFORE
/// normalisation for spectral perturbations and is handed the peak to use, so a
/// case can rail exactly one position.
struct PositionPlan {
    channels: usize,
    coupler: bool,
    count: usize,
    labels: &'static [&'static str],
    routing: CaptureRouting,
    seed: u64,
}

fn build_positions(
    plan: &PositionPlan,
    mut perturb: impl FnMut(usize, usize, Vec<f64>) -> Vec<f64>,
    peak_for: impl Fn(usize) -> f64,
    stats_for: impl Fn(usize) -> CaptureStats,
) -> Vec<Position> {
    let mut rng = Rng::new(plan.seed);
    // Drawn FIRST, so every position in this case shares one room. Drawn even
    // on the coupler path, so the two paths consume the stream identically and
    // a case that changes path does not silently re-roll its neighbours.
    let room = room_acoustics(ROOM_POST_MS, &mut rng);
    let peak_index = samples_for(PRE_MS);
    (0..plan.count)
        .map(|index| Position {
            capture: stats_for(index),
            // Provenance only, and never a decision input: a fixed epoch plus
            // 45 s per position, so a re-run cannot move a byte.
            captured_at_ms: 1_752_537_600_000 + index as u64 * 45_000,
            index,
            ir: ImpulseResponse {
                peak: peak_index,
                sample_rate: RATE,
                samples: (0..plan.channels)
                    .map(|channel| {
                        let base = if plan.coupler {
                            coupler_impulse(&mut rng)
                        } else {
                            room_impulse(&room, &mut rng)
                        };
                        normalise(perturb(index, channel, base), peak_for(index))
                    })
                    .collect(),
            },
            label: plan.labels[index].to_string(),
            routing: plan.routing,
        })
        .collect()
}

const ROOM_LABELS: [&str; 9] = [
    "Position 1 (primary seat)",
    "Position 2 (primary seat, +30 cm left)",
    "Position 3 (primary seat, +30 cm right)",
    "Position 4 (primary seat, +30 cm forward)",
    "Position 5 (primary seat, +30 cm back)",
    "Position 6 (primary seat, +30 cm up)",
    "Position 7 (second row, left)",
    "Position 8 (second row, centre)",
    "Position 9 (second row, right)",
];

const RESEAT_LABELS: [&str; 5] = ["Reseat 1", "Reseat 2", "Reseat 3", "Reseat 4", "Reseat 5"];

// ---------------------------------------------------------------------------
// The eight cases
// ---------------------------------------------------------------------------

pub fn bundle_for(case: &str) -> MeasurementBundle {
    match case {
        "bookshelf_clean_room" => bookshelf_clean_room(),
        "floorstander_null_one_of_five" => floorstander_null_one_of_five(),
        "in_ear_clean_coupler" => in_ear_clean_coupler(),
        "over_ear_ears_heq_cal" => over_ear_ears_heq_cal(),
        "over_ear_lost_seal_reseat" => over_ear_lost_seal_reseat(),
        "room_cal_neighbour_outlier" => room_cal_neighbour_outlier(),
        "room_clipped_position" => room_clipped_position(),
        "room_noisy_snr_boundary" => room_noisy_snr_boundary(),
        other => panic!("{other} is not one of the eight owner-reviewed cases"),
    }
}

/// A room measurement captures through ONE microphone, so one capture channel,
/// and the stimulus goes to both output channels because the system is being
/// corrected as one. That is exactly what `CaptureRouting::Both` means on a
/// room run, and it is also the branch the verification prediction is defined
/// over.
const ROOM_CHANNELS: usize = 1;

/// An EARS jig captures BOTH ear capsules at once, so two capture channels, and
/// both drivers play. `installed.bands` carries the matching two channels.
const COUPLER_CHANNELS: usize = 2;

fn room_bundle(
    cal: CalFile,
    class: TransducerClass,
    clock_skew_ppm: Option<f64>,
    noise_offset_db: f64,
    positions: Vec<Position>,
) -> MeasurementBundle {
    let sweep_f_start_hz = match class {
        TransducerClass::Floorstander => 20.0,
        _ => 30.0,
    };
    MeasurementBundle {
        cal: Some(cal),
        capture: capture_plan(class, clock_skew_ppm, 104.0, sweep_f_start_hz),
        class,
        noise_floor: noise_floor(ROOM_CHANNELS, noise_offset_db),
        overrides: Overrides::default(),
        positions,
        targets: room_targets(),
        verification: None,
    }
}

fn coupler_bundle(
    cal: CalFile,
    class: TransducerClass,
    positions: Vec<Position>,
    verification: Option<Verification>,
) -> MeasurementBundle {
    MeasurementBundle {
        cal: Some(cal),
        capture: capture_plan(class, Some(6.2), 100.0, 20.0),
        class,
        noise_floor: noise_floor(COUPLER_CHANNELS, -14.0),
        overrides: Overrides::default(),
        positions,
        targets: coupler_targets(class),
        verification,
    }
}

/// The room happy path at the profile's own default position count.
fn bookshelf_clean_room() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: ROOM_CHANNELS,
            coupler: false,
            count: 9,
            labels: &ROOM_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_BOOKSHELF_CLEAN_ROOM,
        },
        |_, _, samples| samples,
        |_| NOMINAL_IR_PEAK,
        |_| clean_stats(),
    );
    room_bundle(
        umik_cal(),
        TransducerClass::Bookshelf,
        Some(11.4),
        -14.0,
        positions,
    )
}

/// The averaging-divergence case: one of five positions carries a −30 dB null.
///
/// The null is a `Peaking` biquad at 80 Hz, Q 2.0, gain −30 dB, applied to the
/// fifth position only — so the depth is the DSP crate's own arithmetic rather
/// than a shape drawn into samples, and it is narrow enough to be a null and
/// wide enough to survive the variable smoothing the room path applies in the
/// bass.
fn floorstander_null_one_of_five() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: ROOM_CHANNELS,
            coupler: false,
            count: 5,
            labels: &ROOM_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_FLOORSTANDER_NULL,
        },
        |position, _, samples| {
            if position == NULL_POSITION {
                filtered(&samples, vec![null_band()], f64::from(RATE))
            } else {
                samples
            }
        },
        |_| NOMINAL_IR_PEAK,
        |_| clean_stats(),
    );
    room_bundle(
        umik_cal(),
        TransducerClass::Floorstander,
        Some(9.8),
        -14.0,
        positions,
    )
}

/// Which position carries the null, and the band that makes it. Public so
/// `test_arithmetic_pins.rs` asserts against the same two numbers the generator
/// used instead of restating them.
pub const NULL_POSITION: usize = 4;
pub const NULL_FC_HZ: f64 = 80.0;
pub const NULL_DEPTH_DB: f64 = -30.0;

pub fn null_band() -> EQBand {
    EQBand {
        filter_type: FilterType::Peaking,
        fc: NULL_FC_HZ,
        gain_db: NULL_DEPTH_DB,
        q: 2.0,
    }
}

/// The in-ear happy path: five reseats, both ear capsules, no gating.
fn in_ear_clean_coupler() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: COUPLER_CHANNELS,
            coupler: true,
            count: 5,
            labels: &RESEAT_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_IN_EAR_CLEAN,
        },
        |_, _, samples| samples,
        |_| NOMINAL_IR_PEAK,
        |_| clean_stats(),
    );
    coupler_bundle(coupler_cal(), TransducerClass::InEar, positions, None)
}

/// An EARS HEQ cal: a target is already baked into the curve, so the target
/// decision is forced to `flat` and the set carries a warning rather than
/// applying a second target on top of the first.
fn over_ear_ears_heq_cal() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: COUPLER_CHANNELS,
            coupler: true,
            count: 5,
            labels: &RESEAT_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_OVER_EAR_HEQ,
        },
        |_, _, samples| samples,
        |_| NOMINAL_IR_PEAK,
        |_| clean_stats(),
    );
    let cal = cal_from_pairs(
        "\"Sens Factor =-0.9dB\", SERNO: 0A2F19, HEQ\n",
        &EARS_HEQ_PAIRS,
        Some(-0.9),
        "0A2F19",
        CalVariant::EarsHeq,
    );
    coupler_bundle(cal, TransducerClass::OverEar, positions, None)
}

/// The EARS HEQ curve: the plain coupler transfer function with the Harman
/// over-ear target already subtracted into it — which is precisely why applying
/// another target would apply it twice.
const EARS_HEQ_PAIRS: [(f64, f64); 12] = [
    (20.0, -4.30),
    (31.5, -3.71),
    (50.0, -2.94),
    (80.0, -2.02),
    (125.0, -1.35),
    (250.0, -0.36),
    (500.0, 0.00),
    (1000.0, 0.00),
    (2000.0, -1.94),
    (5000.0, -8.42),
    (10000.0, -5.11),
    (20000.0, -3.04),
];

/// One reseat lost its seal: everything below ~200 Hz leaked away.
///
/// A `LowShelf` at 200 Hz, −12 dB, Q 0.7, on the third reseat only. The mean
/// absolute deviation over 20-200 Hz is then far past the 6 dB the coupler
/// outlier rule refuses a position at, while the survivors still outnumber the
/// hard minimum.
///
/// **This is also the case that carries the verification pass** — see
/// [`seal_verification`].
fn over_ear_lost_seal_reseat() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: COUPLER_CHANNELS,
            coupler: true,
            count: 5,
            labels: &RESEAT_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_OVER_EAR_SEAL,
        },
        |position, _, samples| {
            if position == LOST_SEAL_POSITION {
                filtered(&samples, vec![lost_seal_band()], f64::from(RATE))
            } else {
                samples
            }
        },
        |_| NOMINAL_IR_PEAK,
        |_| clean_stats(),
    );
    let verification = seal_verification(&positions);
    coupler_bundle(
        coupler_cal(),
        TransducerClass::OverEar,
        positions,
        Some(verification),
    )
}

pub const LOST_SEAL_POSITION: usize = 2;

pub fn lost_seal_band() -> EQBand {
    EQBand {
        filter_type: FilterType::LowShelf,
        fc: 200.0,
        gain_db: -12.0,
        q: 0.7,
    }
}

/// The installed correction the verification pass was measured through: one
/// band set, applied identically to both engine channels.
///
/// Identical rows on purpose. `CaptureRouting::Both` means the capture heard
/// every output channel, and a sum of DIFFERENTLY EQ'd channels is not the
/// response of any one band set — the residual is only defined when the rows
/// agree. This case takes the branch where it is defined; the branch where it
/// is not is a refusal with no golden case, by design (README).
pub fn installed_bands() -> Vec<EQBand> {
    vec![
        EQBand {
            filter_type: FilterType::Peaking,
            fc: 62.0,
            gain_db: -4.5,
            q: 1.1,
        },
        EQBand {
            filter_type: FilterType::Peaking,
            fc: 3150.0,
            gain_db: 2.5,
            q: 2.2,
        },
        EQBand {
            filter_type: FilterType::Peaking,
            fc: 8000.0,
            gain_db: -6.0,
            q: 3.0,
        },
        // The band that makes the rate visible. A +3.5 dB peak at 12.5 kHz is
        // 0.26 of Nyquist at 48 kHz and 0.28 at 44.1 kHz, and the bilinear
        // warp moves it against the high shelf beside it by enough that the
        // realized cascade peak — and therefore the preamp — is a DIFFERENT
        // number at the two rates. Without something up here the two preamps
        // agree to three decimals and an implementation that simply copied
        // `installed.preamp_db` would look correct.
        EQBand {
            filter_type: FilterType::Peaking,
            fc: 12500.0,
            gain_db: 3.5,
            q: 1.8,
        },
        EQBand {
            filter_type: FilterType::HighShelf,
            fc: 10000.0,
            gain_db: 1.5,
            q: 0.7,
        },
    ]
}

/// The rate the plan was FITTED at. Deliberately not the rate the engine is
/// running at: a plan designed in a 44.1 kHz session and re-armed at 48 kHz is
/// the ordinary case, and it is the one that proves `installed_preamp_db` is
/// the engine's own live-rate number rather than a copy of `installed.preamp_db`.
pub const INSTALLED_DESIGN_RATE: f64 = 44_100.0;

/// The LIVE stream rate during the verification capture — the rate `H(f)` is
/// evaluated at, and the rate the sidecar samples are at.
pub const VERIFY_RUNNING_RATE: f64 = 48_000.0;

/// `preamp_db` as the PLAN carries it: `decide()`'s number at `design_rate`.
pub fn plan_preamp_db() -> f64 {
    ParametricEQ {
        bands: installed_bands(),
        sample_rate: INSTALLED_DESIGN_RATE,
    }
    .preamp_db()
}

/// `installed_preamp_db` as the ENGINE carries it: recomputed at the live rate.
/// Folded `min` across channels ("the worst channel wins") — a fold over two
/// identical rows, which is the point: the number differs from
/// [`plan_preamp_db`] because of the RATE, not because of the channels.
pub fn engine_preamp_db() -> f64 {
    let per_channel: Vec<f64> = vec![installed_bands(), installed_bands()]
        .into_iter()
        .map(|bands| {
            ParametricEQ {
                bands,
                sample_rate: VERIFY_RUNNING_RATE,
            }
            .preamp_db()
        })
        .collect();
    per_channel.into_iter().fold(f64::INFINITY, f64::min)
}

/// The verification pass on `over_ear_lost_seal_reseat`.
///
/// **How the capture is constructed, stated so the owner can check it at the
/// bless rather than take it on faith.** A verification capture is the baseline
/// capture heard through the installed correction at a quieter level:
///
/// - the engine applies the cascade `H(f)` plus the preamp, so the capture
///   carries `H + preamp_db`;
/// - MS-19 levels the pass at `L_verify = L_measure − max(0, peak boost)`,
///   which is `L_measure + preamp_db` because `preamp_db = −max(0, peak)`, so
///   the capture carries a further `preamp_db`.
///
/// The sidecar is therefore `baseline ⊛ cascade` scaled by
/// `2 · installed_preamp_db` dB, evaluated at the LIVE rate. Under the spec's
/// own level compensation (`K = L_measure − L_verify = −preamp_db`) the residual
/// of this pass is zero by construction, which is what a fixture for a PASSING
/// verification has to be.
fn seal_verification(positions: &[Position]) -> Verification {
    let preamp_db = engine_preamp_db();
    let baseline = &positions[VERIFY_POSITION].ir;
    let samples = baseline
        .samples
        .iter()
        .map(|channel| {
            scaled(
                &filtered(channel, installed_bands(), VERIFY_RUNNING_RATE),
                2.0 * preamp_db,
            )
        })
        .collect();
    Verification {
        capture: CaptureStats {
            clipped_samples: 0,
            peak_dbfs: -11.9,
            rms_dbfs: -29.4,
        },
        // MUST be 0.0: the preamp rides inside the correction, never on the
        // engine's gain stage, and the verify gate pins and restores it.
        gain_db: 0.0,
        installed: CorrectionPlan {
            bands: PerChannel::new(vec![installed_bands(), installed_bands()])
                .expect("two channels is not empty"),
            clamps: vec![
                vec![Clamp::QToBoostCap { from: 3.4, to: 2.2 }],
                vec![Clamp::QToBoostCap { from: 3.4, to: 2.2 }],
            ],
            design_rate: INSTALLED_DESIGN_RATE,
            dropped: vec![4],
            preamp_db: plan_preamp_db(),
        },
        installed_preamp_db: preamp_db,
        ir: ImpulseResponse {
            peak: baseline.peak,
            sample_rate: RATE,
            samples,
        },
        level_dbfs: -18.0 + preamp_db,
        position_index: VERIFY_POSITION,
        routing: CaptureRouting::Both,
        running_rate_hz: VERIFY_RUNNING_RATE,
        two_clock: Some(TwoClockFit {
            intercept_samples: 4_803.25,
            residual_peak_samples: 0.74,
            residual_rms_samples: 0.29,
            skew_ppm: 6.2,
        }),
    }
}

/// Auto mode verifies ONE position, and it verifies the first — the seat the
/// user was sitting in, not the reseat that failed.
pub const VERIFY_POSITION: usize = 0;

/// A cal file carrying the shipping `7005770_90deg.txt` defect: an exact
/// `0.0000` at 19.611 Hz between −3.13 and −3.11.
///
/// The vendor file itself is not in the repo; the pattern is synthesised, which
/// is the same thing `crates/paraeq-dsp/tests/test_compensation.rs` does with
/// the identical three rows.
fn room_cal_neighbour_outlier() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: ROOM_CHANNELS,
            coupler: false,
            count: 5,
            labels: &ROOM_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_CAL_OUTLIER,
        },
        |_, _, samples| samples,
        |_| NOMINAL_IR_PEAK,
        |_| clean_stats(),
    );
    let mut pairs = vec![
        (19.111, -3.15),
        (19.361, -3.13),
        (19.611, 0.0000),
        (19.861, -3.11),
        (20.111, -3.09),
    ];
    pairs.extend_from_slice(&UMIK_PAIRS[1..]);
    let cal = cal_from_pairs(
        "\"Sens Factor =-1.4dB\", SERNO: 7005770, 90deg\n",
        &pairs,
        Some(-1.4),
        "7005770",
        CalVariant::Plain,
    );
    room_bundle(
        cal,
        TransducerClass::Bookshelf,
        Some(10.7),
        -14.0,
        positions,
    )
}

/// One position railed the microphone's ADC.
///
/// Expressed BOTH ways, because the two are different facts measured on
/// different sides: `capture.clipped_samples` / `peak_dbfs` is the capture
/// meter's own readout (MS-21), and the position's impulse response is
/// normalised to −0.006 dBFS so a sample-domain read of the same event agrees
/// with it. Every other position sits at −6 dBFS.
fn room_clipped_position() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: ROOM_CHANNELS,
            coupler: false,
            count: 5,
            labels: &ROOM_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_CLIPPED,
        },
        |_, _, samples| samples,
        |position| {
            if position == CLIPPED_POSITION {
                CLIPPED_IR_PEAK
            } else {
                NOMINAL_IR_PEAK
            }
        },
        |position| {
            if position == CLIPPED_POSITION {
                clipped_stats()
            } else {
                clean_stats()
            }
        },
    );
    room_bundle(
        umik_cal(),
        TransducerClass::Bookshelf,
        Some(12.9),
        -14.0,
        positions,
    )
}

pub const CLIPPED_POSITION: usize = 3;

/// A noisy room, in the middle of the soft SNR band.
///
/// **`noise_offset_db = 52.0`, and the number is measured rather than chosen.**
/// The SNR rows compare the analysed per-position curve against the silence
/// capture's own spectrum, both reduced to a band RMS (ruling R-A8), so the
/// offset that places this case inside the soft window is a property of this
/// fixture's analysed level and not a round number. At 52.0 the in-band SNR is
/// **22.24 dB**: 2.76 dB under the 25 dB soft gate, 7.24 dB over the 15 dB hard
/// refusal, and the floor's own band RMS is −26.11 dBFS, 2.11 dB under Dirac's
/// −24 dBFS gate. All three margins are stated in `notes.md` because all three
/// are what the case is for — `LowSnrHard` and `NoiseFloorTooHigh` must not
/// fire, and the window between them is only about 5 dB wide on this capture
/// level.
///
/// The old value, 22.0, was ABSOLUTE on the −70 dB reference like every other
/// case's, not a 22 dB raise as the comment beside it used to claim; under the
/// broadband SNR rule it put this case 0.33 dB past the HARD refusal.
///
/// The capture carries its own broadband bed as well. It is deliberately NOT a
/// calibrated match for the reported floor — the reported spectrum is what the
/// spec's Detection column grades against ("capture RMS − floor RMS"), and the
/// bed is there so the captures are not artificially silent.
///
/// The same case carries `clock_skew_ppm: None`, and the pairing is physical
/// rather than convenient: the marker fit that produces the ppm estimate is a
/// matched filter against the same noise, and a floor this high is exactly
/// where it fails to converge.
fn room_noisy_snr_boundary() -> MeasurementBundle {
    let positions = build_positions(
        &PositionPlan {
            channels: ROOM_CHANNELS,
            coupler: false,
            count: 5,
            labels: &ROOM_LABELS,
            routing: CaptureRouting::Both,
            seed: SEED_NOISY,
        },
        |_, _, mut samples| {
            // A broadband bed in the capture itself, so the captures of a noisy
            // room are not artificially silent. Not a calibrated match for the
            // reported floor spectrum — see this function's doc comment.
            let mut rng = Rng::new(SEED_NOISY ^ 0x9E37_79B9_7F4A_7C15);
            for sample in samples.iter_mut() {
                *sample += 2.0e-3 * rng.next();
            }
            samples
        },
        |_| NOMINAL_IR_PEAK,
        |_| clean_stats(),
    );
    room_bundle(
        umik_cal(),
        TransducerClass::Bookshelf,
        None,
        NOISY_FLOOR_OFFSET_DB,
        positions,
    )
}

/// The noisy case's `noise_offset_db`. See [`room_noisy_snr_boundary`] for how
/// it was measured and what the three margins are.
pub const NOISY_FLOOR_OFFSET_DB: f64 = 52.0;

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Regenerate every input and the freeze manifest. Idempotent by construction:
/// a second run writes the same bytes, which `git status` is the test for.
pub fn write_all() {
    for case in golden::CASES {
        write_case(case);
    }
    let manifest = golden::build_manifest(None, None);
    std::fs::write(golden::manifest_path(), golden::canonical_json(&manifest))
        .expect("fixtures/decide/manifest.json is writable");
}

fn write_case(case: &str) {
    let dir = golden::case_dir(case);
    let notes = dir.join("notes.md");
    assert!(
        notes.is_file(),
        "{case}/notes.md must exist before the inputs are written: it is the \
         paragraph the owner signs at the bless, and the generator digests it \
         rather than authoring it"
    );
    // Clear the sidecars rather than overwriting them, so a renamed or dropped
    // channel cannot leave an orphan the manifest would then freeze.
    let ir = dir.join("ir");
    if ir.exists() {
        std::fs::remove_dir_all(&ir).expect("the ir/ directory is removable");
    }
    let mut envelope =
        serde_json::to_value(bundle_for(case)).expect("MeasurementBundle is plain derived data");
    golden::dehydrate(&mut envelope, &dir);
    std::fs::write(dir.join("bundle.json"), golden::canonical_json(&envelope))
        .unwrap_or_else(|e| panic!("{case}/bundle.json: {e}"));
}
