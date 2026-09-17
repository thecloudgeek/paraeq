//! R1-8 meters (clip counter + output peak) and R1-1's engine half (the
//! `Correction.preamp_lin` carrier), including **the falsifier**.
//!
//! Spec: `docs/specs/2026-07-15-engine-hardening-design.md` R1-1 (`:46-119`)
//! and R1-8 (`:517-582`). The two ship together because `:38` says so
//! verbatim: *"R1-8's clip counter is R1-1's falsifier. Ship them together or
//! R1-1's central claim (\"the clamp never engages\") is unfalsifiable."*
//!
//! Tier 3 (CLAUDE.md: "Engine code gets synthetic-block unit tests") -- no
//! fixture is involved and none may be.
//!
//! Two reconciliations of the spec's own test table are pinned here by name:
//!
//! * `output_peak` is **pre-clamp**. The fix snippet at `:558` takes
//!   `let a = v.abs();` BEFORE `*o = v.clamp(-1.0, 1.0)`, so the row at `:577`
//!   asserting `output_peak == 1.0` for a full-scale sine at +6 dB is a
//!   spec-text defect: pre-clamp it is ~2.0. A post-clamp peak carries no
//!   information the clip counter does not (it saturates at exactly 1.0
//!   precisely when `clipped_samples > 0`), and pre-clamp is the number that
//!   proves the preamp is right -- which `:571` says is the drawer's job.
//! * The decaying-`input_peak` row (`:579`) asks for a stimulus that cannot
//!   reach its own assertion: "10 full-scale blocks then 100 silent" only
//!   decays to `0.98565^100 = 0.236`, while the -20 dB crossing it asserts is
//!   at 159.4 blocks. The assertion is kept (it pins the broadcast release
//!   rate) and the silent-block count is written as the formula
//!   `ceil(1.7 * rate / block) + 2`. Section 7 below; it landed after the
//!   clip counter because R1-8's release gate (`:582`) splits the two -- the
//!   counter blocks release as R1-1's falsifier, the meter does not.
//!
//! One measured finding the falsifier surfaced, recorded here because the
//! next reader will otherwise rediscover it: with `preamp_db() == -12` the
//! steady state is exactly 1.0, and the IIR filter's SWITCH-ON RING-UP
//! overshoots it by one f32 ULP, clipping 6 samples out of the prologue's
//! 16384. Inaudible, but it means `clipped_samples > 0` while
//! `auto_preamp_db` is active -- which spec `:571` calls a bug signal -- can
//! fire on an entirely healthy transient. The honest fix is a headroom
//! constant, which wizard `:532` records as "recorded but not taken", so it
//! stays an owner call and is NOT introduced here.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::mock_backend::MockBackend;
use paraeq_dsp::peq::{EQBand, FilterType};
use paraeq_engine::chain::{build_fir, build_iir, Correction, RealtimeChain};
use paraeq_engine::controller::{
    build_correction, decay_per_block, CorrectionConfig, EngineCommand, EngineConfig, EngineHandle,
};
use paraeq_engine::shared::{links, ControlLink, RtMsg, RtProcessor, RtShared};
use paraeq_engine::status::WatchdogConfig;

const BLOCK: usize = 512;
const CHANNELS: usize = 2;
const RATE: f64 = 48_000.0;
/// The identity section: a correction that changes nothing, so tests 1-3 are
/// about the gain/clamp loop and nothing else.
const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
/// Generous per-wait budget; each wait normally completes in tens of ms.
const WAIT: Duration = Duration::from_secs(5);

fn db_to_lin(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// A synthetic realtime rig: the processor plus the shared atomics the
/// controller would read. The `ControlLink` is held (never used) so the swap
/// ring keeps both ends alive for `RtLink::poll`.
struct Rig {
    /// Held so the swap ring keeps both ends alive for `RtLink::poll`, and
    /// used by the swap tests to push a correction through the REAL ring --
    /// the same path `Controller::send_correction` uses.
    control: ControlLink,
    proc_: RtProcessor,
    shared: Arc<RtShared>,
}

/// Build a rig with `correction` already installed and `gain_db` on the trim.
/// The correction is installed directly on the chain rather than through the
/// ring: this file is about the meters, not the swap path (covered in
/// `test_shared.rs`).
fn rig(correction: Option<Correction>, gain_db: f64, bypass: bool) -> Rig {
    let shared = Arc::new(RtShared::default());
    shared.set_gain(db_to_lin(gain_db));
    shared.bypass.store(bypass, Ordering::Relaxed);
    let (control, rt) = links(1);
    let mut chain = RealtimeChain::new(CHANNELS, BLOCK);
    chain.set_correction(correction);
    Rig {
        control,
        proc_: RtProcessor::new(Arc::clone(&shared), rt, chain),
        shared,
    }
}

/// Pump one explicit block and return the per-channel output.
fn pump_with(rig: &mut Rig, input: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let views: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
    let mut out: Vec<Vec<f32>> = input.iter().map(|c| vec![0.0f32; c.len()]).collect();
    {
        let mut out_views: Vec<&mut [f32]> = out.iter_mut().map(Vec::as_mut_slice).collect();
        rig.proc_.process_block(&views, &mut out_views, 512.0);
    }
    out
}

/// One block of CONSTANT `amplitude` on every channel. The decay tests need a
/// flat block, not a sine: a sine's own zero crossings make the per-block peak
/// depend on where the block boundary falls, and the meter under test is
/// exactly "the largest sample in this block versus the decayed hold".
fn flat_block(amplitude: f32) -> Vec<Vec<f32>> {
    vec![vec![amplitude; BLOCK]; CHANNELS]
}

/// `frames` samples of a full-scale sine at `fc`, phase-continuous from
/// `start` (so consecutive blocks splice without a step).
fn sine_block(fc: f64, amplitude: f32, start: usize, frames: usize) -> Vec<Vec<f32>> {
    let channel: Vec<f32> = (0..frames)
        .map(|i| {
            let phase = 2.0 * std::f64::consts::PI * fc * ((start + i) as f64) / RATE;
            amplitude * (phase.sin() as f32)
        })
        .collect();
    vec![channel; CHANNELS]
}

/// Drive `blocks` phase-continuous blocks of a full-scale sine at `fc`,
/// starting at sample `start`; returns the next start sample.
fn pump_sine(rig: &mut Rig, fc: f64, blocks: usize, start: usize) -> usize {
    let mut n = start;
    for _ in 0..blocks {
        pump_with(rig, &sine_block(fc, 1.0, n, BLOCK));
        n += BLOCK;
    }
    n
}

/// The largest `|sample|` in a pumped block set.
fn max_abs(out: &[Vec<f32>]) -> f32 {
    out.iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |m, v| m.max(v.abs()))
}

fn peaking(fc: f64, gain_db: f64, q: f64) -> EQBand {
    EQBand {
        filter_type: FilterType::Peaking,
        fc,
        gain_db,
        q,
    }
}

/// Poll `pred` every couple of ms until it holds or `timeout` elapses.
fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn fast_config() -> EngineConfig {
    EngineConfig {
        enabled: true,
        fail_open_after_ms: None,
        requested_buffer_frames: None,
        ring_capacity: 4,
        tick_ms: 10,
        watchdog: WatchdogConfig {
            engage_tolerance_ms: 200,
            idle_window_ms: 150,
            silence_window_ms: 150,
        },
    }
}

// ---------------------------------------------------------------------------
// 1-3: R1-8's gain/clamp scan, on both chain paths
// ---------------------------------------------------------------------------

/// Test 1 (spec `:578`). Full-scale 1 kHz sine at -6 dB: the clamp never
/// engages and `output_peak` reports the signal. Consistent under BOTH the
/// pre- and post-clamp readings of `output_peak`, which is why it is the safe
/// one to write first.
#[test]
fn no_clip_below_unity_and_output_peak_tracks_the_signal() {
    let mut r = rig(
        Some(build_iir(vec![vec![IDENTITY]; CHANNELS], CHANNELS, BLOCK, 1.0).0),
        -6.0,
        false,
    );
    pump_sine(&mut r, 1_000.0, 4, 0);

    assert_eq!(
        r.shared.clipped_samples.load(Ordering::Relaxed),
        0,
        "nothing above unity can clip"
    );
    let peak = r.shared.peak_out();
    assert!(
        (peak - db_to_lin(-6.0)).abs() < 1e-4,
        "output_peak {peak} should track the -6 dB signal (~0.5012)"
    );
    assert!(
        (r.shared.peak_in() - 1.0).abs() < 1e-6,
        "the INPUT peak is full scale; only the output is attenuated"
    );
}

/// Test 2 (spec `:577`, restated). Same sine at +6 dB: the clamp engages and
/// `output_peak` reports the overshoot PRE-clamp (~1.995), not the clamped
/// 1.0 the spec row prints. The name records the reconciliation so nobody
/// re-derives the spec defect -- see this file's header.
#[test]
fn clip_counter_rises_and_output_peak_reports_the_overshoot_pre_clamp() {
    let mut r = rig(
        Some(build_iir(vec![vec![IDENTITY]; CHANNELS], CHANNELS, BLOCK, 1.0).0),
        6.0,
        false,
    );
    let out = pump_with(&mut r, &sine_block(1_000.0, 1.0, 0, BLOCK));

    assert!(
        r.shared.clipped_samples.load(Ordering::Relaxed) > 0,
        "a full-scale sine at +6 dB must clip"
    );
    let peak = r.shared.peak_out();
    assert!(
        (peak - db_to_lin(6.0)).abs() < 1e-3,
        "output_peak {peak} must be the PRE-clamp overshoot (~1.995), not 1.0"
    );
    assert!(
        max_abs(&out) <= 1.0,
        "the delivered samples are still clamped"
    );
}

/// Test 3 (spec `:554`, "fuse into the gain/clamp loops -- **both** of
/// them"). The pass-through loop is the one most likely to be forgotten, and
/// it is reached two ways: user bypass, and a frame count the active
/// correction cannot take.
#[test]
fn pass_through_path_also_counts_clips() {
    // (a) user bypass.
    let mut bypassed = rig(
        Some(build_iir(vec![vec![IDENTITY]; CHANNELS], CHANNELS, BLOCK, 1.0).0),
        6.0,
        true,
    );
    pump_with(&mut bypassed, &sine_block(1_000.0, 1.0, 0, BLOCK));
    assert!(
        bypassed.shared.clipped_samples.load(Ordering::Relaxed) > 0,
        "the bypassed path clamps too, so it must count"
    );
    assert!(
        (bypassed.shared.peak_out() - db_to_lin(6.0)).abs() < 1e-3,
        "the bypassed path reports its own output peak"
    );

    // (b) frame mismatch: a FIR correction requires exactly `BLOCK` frames,
    // so a short block passes through.
    let mut mismatched = rig(
        Some(build_fir(vec![vec![1.0]; CHANNELS], CHANNELS, BLOCK, 1.0)),
        6.0,
        false,
    );
    pump_with(&mut mismatched, &sine_block(1_000.0, 1.0, 0, BLOCK / 2));
    assert_eq!(
        mismatched
            .shared
            .frame_mismatch_blocks
            .load(Ordering::Relaxed),
        1,
        "the short block should have taken the pass-through path"
    );
    assert!(
        mismatched.shared.clipped_samples.load(Ordering::Relaxed) > 0,
        "a degraded (frame-mismatched) block still clamps, so it must count"
    );
}

// ---------------------------------------------------------------------------
// 4-5: R1-1's carrier
// ---------------------------------------------------------------------------

/// Test 4 (spec `:99`, verbatim: "Critically, **the preamp applies only on
/// the corrected path** -- `chain.rs:181-185`'s pass-through ... must not
/// attenuate, because there is no boost to compensate").
///
/// This is the test that FAILS under the rejected `SetGainDb` carrier
/// (engine-hardening Decisions Log `:20`), which would put the preamp on
/// `gain_bits` -- read once and applied on both paths -- so pressing Bypass
/// would leave the whole auto-attenuation in place and bias the product's
/// headline A/B control. It is therefore the mechanical decider for the
/// carrier, not a preference.
#[test]
fn preamp_does_not_attenuate_the_bypassed_path() {
    const PREAMP: f32 = 0.25;
    let mut r = rig(
        Some(build_iir(vec![vec![IDENTITY]; CHANNELS], CHANNELS, BLOCK, PREAMP).0),
        0.0,
        true,
    );
    let input = vec![vec![0.8f32; BLOCK]; CHANNELS];
    let out = pump_with(&mut r, &input);

    for (ch, samples) in out.iter().enumerate() {
        for (i, &v) in samples.iter().enumerate() {
            assert_eq!(
                v, 0.8,
                "bypassed sample [{ch}][{i}] was attenuated by the preamp"
            );
        }
    }
    assert!(
        (r.shared.peak_out() - 0.8).abs() < 1e-6,
        "and the meter agrees: no attenuation on the bypassed path"
    );
}

/// Test 5 -- **THE FALSIFIER** (spec `:116` and `:580`, the same row twice).
/// A +12 dB band, a full-scale sine at its `fc`, the computed auto-preamp
/// live: `clipped_samples == 0`. Without R1-8's counter this claim is an
/// assertion; with it, it is a measurement.
///
/// **Zero design margin by construction.** With `preamp_db() == -12` the
/// steady-state output at `fc` is exactly 1.0 against a strict `a > 1.0`
/// clip test, so f32 rounding and the DF2T switch-on transient can each push
/// it over. Two mitigations, neither of which is a headroom constant
/// (wizard `:532` records the inter-sample-peak alternative as "recorded but
/// not taken", so introducing one quietly here would be a silent spec
/// change):
///
/// 1. A settling prologue, after which the two meters are re-zeroed directly
///    (the test owns both ends of the atomics here -- this adds no reset API
///    to `RtShared`, which measurement-safety MS-21 deliberately keeps
///    separate), so the assertion is about the steady state the preamp is
///    computed for.
/// 2. An explicit margin assertion, so a regression reads as a margin loss
///    rather than as a flake.
#[test]
fn auto_preamp_prevents_the_clamp_from_engaging_the_falsifier() {
    let config = CorrectionConfig::Peq {
        bands: vec![vec![peaking(1_000.0, 12.0, 1.0)]; CHANNELS],
        design_rate: RATE,
    };
    let (correction, report) =
        build_correction(&config, CHANNELS, BLOCK, RATE).expect("a +12 dB band builds");
    assert!(
        (report.preamp_db + 12.0).abs() < 0.02,
        "a single +12 dB Q=1 peaking band realizes +12 dB at fc; \
         the preamp was {} dB",
        report.preamp_db
    );

    let mut r = rig(Some(correction), 0.0, false);
    // Settling prologue: the filter rings up from zero state.
    let start = pump_sine(&mut r, 1_000.0, 16, 0);
    // MEASURED, and the reason the prologue is not decorative: the ring-up
    // clips 6 of the prologue's 16384 samples, at a peak of 1.0000001 --
    // ONE f32 ULP over unity. The steady state below is clean. See the
    // module header's note; this is R1-1's zero-headroom convention showing
    // its edge, not a chain defect, and it is reported rather than papered
    // over with a headroom constant.
    r.shared.clipped_samples.store(0, Ordering::Relaxed);
    r.shared.peak_out_bits.store(0, Ordering::Relaxed);

    pump_sine(&mut r, 1_000.0, 16, start);

    assert_eq!(
        r.shared.clipped_samples.load(Ordering::Relaxed),
        0,
        "THE FALSIFIER: the clamp engaged with the auto-preamp live"
    );
    let peak = r.shared.peak_out();
    assert!(
        peak <= 1.0 + 4.0 * f32::EPSILON,
        "no margin left: output_peak was {peak}"
    );
    assert!(
        peak > 0.99,
        "the stimulus never reached full scale ({peak}); the test would pass vacuously"
    );
}

/// Test 5b -- the FIR arm's falsifier. `build_fir` is dispatched live from
/// `build_correction`, so its preamp is NOT dead code: spec `:101`, verbatim,
/// *"For the FIR arm, 'the realized cascade' is the FIR's own magnitude
/// response -- `preamp_lin = 1.0 / max(1.0, max|H(f)|)` over the same grid,
/// computed with one FFT of the tap vector on the control plane."*
///
/// `h = [c, 0, -c]` has `|H(w)| = 2c|sin w|`, i.e. a +12 dB peak at exactly
/// `fs/4` when `2c = 10^(12/20)` -- a peak the FFT grid resolves exactly
/// (bin `n_fft/4`) and a frequency a 48 kHz sine hits at full scale every
/// fourth sample. Same zero-margin construction as test 5, same mitigations.
#[test]
fn fir_auto_preamp_prevents_the_clamp_from_engaging() {
    let c = db_to_lin(12.0) as f64 / 2.0;
    let config = CorrectionConfig::Fir {
        design_rate: RATE,
        firs: vec![vec![c, 0.0, -c]; CHANNELS],
    };
    let (correction, report) =
        build_correction(&config, CHANNELS, BLOCK, RATE).expect("a +12 dB FIR builds");
    assert!(
        (report.preamp_db + 12.0).abs() < 0.02,
        "|H| peaks at 2c = +12 dB; the preamp was {} dB",
        report.preamp_db
    );

    let mut r = rig(Some(correction), 0.0, false);
    let start = pump_sine(&mut r, RATE / 4.0, 4, 0);
    // The FIR arm has no ring-up to speak of (3 taps): measured 0 clips at a
    // peak of exactly 1.0 across the prologue too. The reset is kept so both
    // falsifiers read the same way.
    r.shared.clipped_samples.store(0, Ordering::Relaxed);
    r.shared.peak_out_bits.store(0, Ordering::Relaxed);

    pump_sine(&mut r, RATE / 4.0, 4, start);

    assert_eq!(
        r.shared.clipped_samples.load(Ordering::Relaxed),
        0,
        "the FIR arm's auto-preamp let the clamp engage"
    );
    let peak = r.shared.peak_out();
    assert!(
        peak <= 1.0 + 4.0 * f32::EPSILON,
        "no margin left: output_peak was {peak}"
    );
    assert!(
        peak > 0.99,
        "the stimulus never reached full scale ({peak}); the test would pass vacuously"
    );
}

/// A pure-cut correction gets no preamp at all (DIVERGENCES.md #14: ParaEQ's
/// `preamp_db` is clamped at <= 0, AutoEQ's is signed), so a cut-only band
/// set must not be quietly boosted back to unity.
#[test]
fn pure_cut_correction_gets_no_preamp() {
    let config = CorrectionConfig::Peq {
        bands: vec![vec![peaking(1_000.0, -6.0, 1.0)]; CHANNELS],
        design_rate: RATE,
    };
    let (correction, report) = build_correction(&config, CHANNELS, BLOCK, RATE).expect("builds");
    assert_eq!(report.preamp_db, 0.0);
    assert_eq!(correction.preamp_lin, 1.0);
}

/// The two preamp implementations must not drift: a `Peq` config and the
/// same cascade handed over as BAKED SOS rows are the same realized
/// response, so they must agree. They are computed differently on purpose --
/// `Peq` unions every band's `fc` into the grid (exact peaks at any Q), a
/// baked row has forgotten the band it came from -- so a moderate-Q band is
/// the honest comparison.
#[test]
fn baked_iir_preamp_agrees_with_the_peq_preamp() {
    let band = peaking(1_000.0, 9.0, 1.0);
    let peq = CorrectionConfig::Peq {
        bands: vec![vec![band.clone()]; CHANNELS],
        design_rate: RATE,
    };
    let baked = CorrectionConfig::Iir {
        design_rate: RATE,
        sos_per_channel: vec![vec![band.to_sos(RATE)]; CHANNELS],
    };
    let (_, peq_report) = build_correction(&peq, CHANNELS, BLOCK, RATE).expect("builds");
    let (_, baked_report) = build_correction(&baked, CHANNELS, BLOCK, RATE).expect("builds");
    assert!(
        (peq_report.preamp_db - baked_report.preamp_db).abs() < 0.01,
        "the two preamp paths disagree: {} vs {} dB",
        peq_report.preamp_db,
        baked_report.preamp_db
    );
}

/// The baked arm's preamp must mirror `build_iir`'s R1-3 identity
/// substitution, and nothing reached it before this test: every other baked
/// case in the suite carries only stable rows.
///
/// `preamp::sos_preamp_db` evaluates a row failing `biquad::is_stable` AS
/// THE IDENTITY SECTION, because that is what the funnel installs. Drop the
/// mirror and one poisoned row turns the whole cascade's response NaN; the
/// `if db > peak` fold discards NaN, `peak` stays `NEG_INFINITY`, and the
/// function hands back 0.0 -- so the surviving +12 dB boost is installed
/// live with `preamp_lin = 1.0`. Fail-unsafe in exactly the direction the
/// preamp exists to prevent, and the module's own doc says so.
#[test]
fn the_baked_preamp_mirrors_the_identity_substitution_for_an_unstable_row() {
    let boost = peaking(1_000.0, 12.0, 1.0).to_sos(RATE);
    // `q = 0` designs NaN; spelling the NaN row out keeps the test about
    // the funnel rather than about `EQBand`'s validation.
    let poisoned = [f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0];
    let config = CorrectionConfig::Iir {
        design_rate: RATE,
        sos_per_channel: vec![vec![boost, poisoned]; CHANNELS],
    };
    let (correction, report) =
        build_correction(&config, CHANNELS, BLOCK, RATE).expect("one bad row must not refuse");
    assert_eq!(
        report.sections_substituted, CHANNELS,
        "the unstable row must be funnelled to the identity, once per channel"
    );
    assert!(
        (report.preamp_db + 12.0).abs() < 0.05,
        "the preamp must still see the surviving +12 dB boost; it was {} dB \
         (0.0 means the NaN response was folded away -- the mirror is gone)",
        report.preamp_db
    );

    // ...and the number reaches the chain, so the boost is actually protected.
    // -1 dBFS, not full scale: the baked grid carries no per-band `fc`, so it
    // reads the peak a whisker low (-11.998 dB here) and full scale would sit
    // a fraction of a dB over unity by construction. Without the mirror the
    // same stimulus runs at +12 dB and clips continuously.
    let mut r = rig(Some(correction), 0.0, false);
    let level = db_to_lin(-1.0);
    let mut n = 0;
    for _ in 0..16 {
        pump_with(&mut r, &sine_block(1_000.0, level, n, BLOCK));
        n += BLOCK;
    }
    r.shared.clipped_samples.store(0, Ordering::Relaxed);
    r.shared.peak_out_bits.store(0, Ordering::Relaxed);
    for _ in 0..16 {
        pump_with(&mut r, &sine_block(1_000.0, level, n, BLOCK));
        n += BLOCK;
    }
    assert_eq!(
        r.shared.clipped_samples.load(Ordering::Relaxed),
        0,
        "a cascade with one substituted row got no headroom for its surviving boost"
    );
    assert!(
        r.shared.peak_out() > 0.8,
        "the stimulus never reached the boost; the assertion would pass vacuously"
    );
}

// ---------------------------------------------------------------------------
// 5c: R1-1 x R1-7a -- a correction swap must not spend the preamp's headroom
// ---------------------------------------------------------------------------

/// `RealtimeChain::set_correction` transplants the OUTGOING correction's
/// DF2T delay lines into the incoming one so a band edit is click-free
/// (R1-7a), but that tail carries the outgoing cascade's **un-preamped**
/// energy and the INCOMING `preamp_lin` is what multiplies it on the very
/// first block. A swap that WEAKENS the preamp -- a boost dragged down, a
/// band flattened, a preset loaded -- therefore hands boosted energy to a
/// correction with no headroom left for it, and the +-1.0 clamp engages.
///
/// That is precisely the state R1-1 exists to prevent (spec `:571`: "a
/// nonzero `clipped_samples` while `auto_preamp_db` is active is a bug
/// signal ... it is R1-1's falsifier"), and the product reaches it on every
/// band drag: `desktop/src-tauri/src/commands.rs::apply_bands` sends
/// `SetCorrection` on each change and `desktop/ui/src/tabs/EqTab.tsx`
/// applies at ~10 Hz DURING a pointer drag.
///
/// MEASURED on this exact stimulus before the fix: 68 clipped samples at a
/// pre-clamp peak of 2.64 on the block after the swap.
#[test]
fn a_swap_that_weakens_the_preamp_does_not_clip() {
    let boost = CorrectionConfig::Peq {
        bands: vec![vec![peaking(1_000.0, 12.0, 1.0)]; CHANNELS],
        design_rate: RATE,
    };
    // The band dragged flat: an identity cascade that needs no headroom, so
    // its own steady-state output is exactly the input. Anything above unity
    // after the swap came from the transplanted tail and nowhere else.
    let flat = CorrectionConfig::Peq {
        bands: vec![vec![peaking(1_000.0, 0.0, 1.0)]; CHANNELS],
        design_rate: RATE,
    };
    let (boosted, boost_report) =
        build_correction(&boost, CHANNELS, BLOCK, RATE).expect("a +12 dB band builds");
    let (flattened, flat_report) =
        build_correction(&flat, CHANNELS, BLOCK, RATE).expect("a 0 dB band builds");
    assert!(
        (boost_report.preamp_db + 12.0).abs() < 0.02,
        "the outgoing correction must really be preamped; it was {} dB",
        boost_report.preamp_db
    );
    assert_eq!(
        flat_report.preamp_db, 0.0,
        "the incoming correction must really have no headroom to spare"
    );

    let mut r = rig(Some(boosted), 0.0, false);
    // Ring up to the steady state the preamp is computed for.
    let start = pump_sine(&mut r, 1_000.0, 16, 0);
    assert!(
        r.shared.peak_out() > 0.99,
        "the prologue never reached full scale; the swap would have nothing to overshoot"
    );
    r.shared.clipped_samples.store(0, Ordering::Relaxed);
    r.shared.peak_out_bits.store(0, Ordering::Relaxed);

    // Through the REAL swap ring, exactly as `send_correction` does it.
    r.control
        .send(RtMsg::Correction(Some(flattened)))
        .expect("a fresh ring has room");
    pump_sine(&mut r, 1_000.0, 4, start);

    assert_eq!(
        r.shared.clipped_samples.load(Ordering::Relaxed),
        0,
        "the clamp engaged on a correction swap while the auto-preamp was live"
    );
    let peak = r.shared.peak_out();
    assert!(
        peak <= 1.0 + 4.0 * f32::EPSILON,
        "the swap overshot unity: output_peak was {peak}"
    );
    assert!(
        peak > 0.99,
        "the stimulus stopped mid-test ({peak}); the assertion would pass vacuously"
    );
}

// ---------------------------------------------------------------------------
// 6: publication
// ---------------------------------------------------------------------------

/// Test 6 (spec `:566`, "the **counters compare exactly** -- a clip must
/// publish"). The meters reach `EngineState`, and a tick in which ONLY
/// `clipped_samples` moved still emits a fresh snapshot: under SHELL's
/// `frame_mismatch_blocks`-style `> 0` compare the second clip would be
/// invisible, and a clip count that stops moving is exactly the signal the
/// Advanced drawer exists to show.
#[test]
fn meter_fields_reach_engine_state_and_a_clip_publishes() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fast_config());
    let snapshots = handle.subscribe();

    handle.send(EngineCommand::SetGainDb(6.0));
    assert!(wait_until(WAIT, || {
        backend.pump(BLOCK, 1.0);
        let s = handle.state();
        s.clipped_samples > 0 && s.output_peak > 1.5
    }));

    let state = handle.state();
    assert!(
        (state.input_peak_session - 1.0).abs() < 1e-6,
        "the session input max is full scale"
    );
    assert_eq!(state.invalid_samples, 0, "nothing non-finite was fed in");
    assert_eq!(
        state.auto_preamp_db, None,
        "no correction is installed, so there is no auto-preamp to report"
    );

    // Only `clipped_samples` (and `invalid_samples`, pinned at 0) can move
    // from here: the peaks are saturated and the geometry is fixed.
    let before = state.clipped_samples;
    while snapshots.try_recv().is_ok() {}
    assert!(
        wait_until(WAIT, || {
            backend.pump(BLOCK, 1.0);
            std::iter::from_fn(|| snapshots.try_recv().ok())
                .any(|s: Arc<paraeq_engine::controller::EngineState>| s.clipped_samples > before)
        }),
        "a rising clip count must publish a fresh snapshot"
    );
}

/// The counters survive a torn-down session (SHELL's `frame_mismatch_blocks`
/// retention pattern): a `Disable` must not blank the clip count the user is
/// looking at, because the count is the whole disclosure.
#[test]
fn clip_count_is_retained_across_a_teardown() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    handle.send(EngineCommand::SetGainDb(6.0));
    assert!(wait_until(WAIT, || {
        backend.pump(BLOCK, 1.0);
        handle.state().clipped_samples > 0
    }));
    let clipped = handle.state().clipped_samples;

    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || !backend.is_running()));
    assert_eq!(
        handle.state().clipped_samples,
        clipped,
        "a Disable blanked the clip count"
    );
}

/// `auto_preamp_db` is the number the Advanced drawer explains (spec `:106`:
/// "The auto front-end must never apply a number it cannot explain"), so it
/// has to ride the published snapshot, not just live inside `build_correction`.
#[test]
fn auto_preamp_db_rides_the_published_snapshot() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    handle.send(EngineCommand::SetCorrection(CorrectionConfig::Peq {
        bands: vec![vec![peaking(1_000.0, 12.0, 1.0)]; CHANNELS],
        design_rate: RATE,
    }));
    assert!(wait_until(WAIT, || handle
        .state()
        .auto_preamp_db
        .is_some_and(|p| (p + 12.0).abs() < 0.02)));

    handle.send(EngineCommand::ClearCorrection);
    assert!(wait_until(WAIT, || handle.state().auto_preamp_db.is_none()));
}

/// ...and a change to `auto_preamp_db` ALONE must publish. `effectively_equal`
/// compares it exactly for this reason, and nothing else in the snapshot can
/// carry the publish for it: `CorrectionConfig::descriptor()` is the band
/// COUNT (`"peq:1-band"`), so editing one band's gain from +3 dB to +12 dB
/// moves the preamp and leaves every other published field where it was.
///
/// `auto_preamp_db_rides_the_published_snapshot` above cannot see this --
/// both of its transitions also flip the descriptor between `Some` and
/// `None`, so they publish either way.
#[test]
fn a_preamp_only_change_publishes() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    let set = |gain_db: f64| CorrectionConfig::Peq {
        bands: vec![vec![peaking(1_000.0, gain_db, 1.0)]; CHANNELS],
        design_rate: RATE,
    };
    handle.send(EngineCommand::SetCorrection(set(3.0)));
    assert!(wait_until(WAIT, || handle
        .state()
        .auto_preamp_db
        .is_some_and(|p| (p + 3.0).abs() < 0.02)));
    let descriptor = handle.state().correction.clone();

    handle.send(EngineCommand::SetCorrection(set(12.0)));
    assert!(
        wait_until(WAIT, || handle
            .state()
            .auto_preamp_db
            .is_some_and(|p| (p + 12.0).abs() < 0.02)),
        "a band edit that moved ONLY the preamp never reached the published snapshot"
    );
    assert_eq!(
        handle.state().correction,
        descriptor,
        "the descriptor is a band COUNT, so it cannot have carried that publish"
    );
}

// ---------------------------------------------------------------------------
// 7: R1-8's decaying `input_peak` -- the meter half (spec `:515-528`, `:579`)
// ---------------------------------------------------------------------------

/// The coefficient itself. It is the one piece of arithmetic in R1-8 that can
/// be silently wrong: the control plane computes it once per start and the
/// realtime lane then applies it blindly, one multiply per block, for the
/// life of the session.
///
/// Spec `:525`: `decay = 10^( -(20/1.7) * (block_size/sample_rate) / 20 )` --
/// the broadcast-standard 20 dB / 1.7 s release, computed from the REPORTED
/// geometry so the release is the same WALL-CLOCK rate at every buffer size
/// and sample rate, which a hardcoded constant would not be (spec `:528`).
#[test]
fn the_decay_coefficient_is_the_broadcast_release_rate() {
    // Spec `:528`: "At 512 frames / 48 kHz (10.67 ms/block) that is 0.1255
    // dB/block -> decay = 0.98565." Exactly: (20/1.7) * (512/48000) =
    // 0.1254902 dB/block, and 10^(-0.1254902/20) = 0.9856563.
    let d = decay_per_block(BLOCK, RATE);
    assert!(
        (d - 0.985_656_3).abs() < 1e-6,
        "512 frames / 48 kHz must give 0.9856563, got {d}"
    );

    // The property the number encodes, at three geometries: 1.7 s worth of
    // blocks is 20 dB down, whatever the block size and rate.
    for (block, rate) in [(512usize, 48_000.0f64), (64, 44_100.0), (2048, 96_000.0)] {
        let blocks = 1.7 * rate / block as f64;
        let db = 20.0 * f64::from(decay_per_block(block, rate)).powf(blocks).log10();
        assert!(
            (db + 20.0).abs() < 1e-3,
            "{block} frames / {rate} Hz releases {db} dB in 1.7 s, not -20"
        );
    }

    // Degenerate geometry must leave the meter a plain peak hold rather than
    // emptying it instantly or producing a NaN the RT lane would then store.
    assert_eq!(decay_per_block(0, RATE), 1.0, "0 frames must not decay");
    assert_eq!(decay_per_block(BLOCK, 0.0), 1.0, "0 Hz must not decay");
}

/// Spec `:579`, with this plan's reconciliation of its stimulus. The row asks
/// for "10 full-scale blocks then 100 silent" and a crossing of 0.1 (-20 dB)
/// within `1.7 s / block_duration` blocks +-1 -- but at 512/48 kHz the
/// crossing is at 159.4 blocks and 100 blocks only reach `0.98565^100 =
/// 0.236`, so the stated stimulus cannot reach its own assertion. The
/// ASSERTION is the meaningful half (it pins the broadcast release rate), so
/// it is kept verbatim and the silent-block count is written as the formula
/// `ceil(1.7 * rate / block) + 2`, which also survives a geometry change.
#[test]
fn input_peak_decays_at_the_broadcast_release_rate() {
    let mut r = rig(None, 0.0, false);
    r.shared.set_decay_per_block(decay_per_block(BLOCK, RATE));

    for _ in 0..10 {
        pump_with(&mut r, &flat_block(1.0));
    }
    assert_eq!(
        r.shared.peak_in(),
        1.0,
        "ten full-scale blocks pin the meter at full scale"
    );

    let block_duration = BLOCK as f64 / RATE;
    let expected_crossing = 1.7 / block_duration;
    let silent_blocks = expected_crossing.ceil() as usize + 2;

    let mut previous = r.shared.peak_in();
    let mut crossing: Option<usize> = None;
    for n in 1..=silent_blocks {
        pump_with(&mut r, &flat_block(0.0));
        let now = r.shared.peak_in();
        assert!(
            now < previous,
            "silent block {n}: the meter must fall monotonically ({previous} -> {now})"
        );
        if crossing.is_none() && now < 0.1 {
            crossing = Some(n);
        }
        previous = now;
    }

    let crossing = crossing.expect("the meter must cross -20 dB within the release window");
    assert!(
        ((crossing as f64) - expected_crossing).abs() <= 1.0,
        "crossed 0.1 at silent block {crossing}, expected {expected_crossing} +-1"
    );
    assert_eq!(
        r.shared.peak_in_session(),
        1.0,
        "the session statistic never decays (spec `:552`)"
    );
}

/// The concrete regression the hoist exists for. A literal transcription of
/// spec `:518`'s snippet leaves the store inside `process_block`'s
/// `if peak == 0.0 { .. } else { .. }` else-branch, so SILENCE -- the exact
/// case a decaying meter exists for -- never decays at all: the meter would
/// hold full scale forever after the music stopped. The spec does not mention
/// this; it is an implementation consequence, so it gets its own test rather
/// than riding on the release-rate one above.
#[test]
fn decay_runs_on_all_zero_blocks_too() {
    let decay = decay_per_block(BLOCK, RATE);
    let mut r = rig(None, 0.0, false);
    r.shared.set_decay_per_block(decay);

    pump_with(&mut r, &flat_block(1.0));
    assert_eq!(r.shared.peak_in(), 1.0);

    for n in 1..=5i32 {
        pump_with(&mut r, &flat_block(0.0));
        let expected = decay.powi(n);
        let now = r.shared.peak_in();
        assert!(
            (now - expected).abs() < 1e-6,
            "after {n} all-zero blocks the meter reads {now}, expected {expected} \
             (1.0 means the decay is still trapped in the nonzero branch)"
        );
    }
    assert_eq!(
        r.shared.zero_blocks.load(Ordering::Relaxed),
        5,
        "the five decaying blocks are still counted as silence"
    );
    assert_eq!(r.shared.peak_in_session(), 1.0);
}

/// The control-plane half: `start_with` must install the coefficient from the
/// NEGOTIATED `StreamInfo` before the meter is read, or every field above is
/// correct in isolation and dead in production. `RtShared::default()` is
/// deliberately 1.0 (no decay), so a missing store does not fail loudly --
/// the meter just silently goes back to being a session statistic. This is
/// the test that notices.
#[test]
fn start_installs_the_decay_coefficient_from_the_negotiated_geometry() {
    let backend = MockBackend::new();
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    assert!(
        wait_until(WAIT, || {
            backend.pump(BLOCK, 1.0);
            handle.state().input_peak > 0.99
        }),
        "full-scale blocks must drive the published meter to full scale"
    );
    assert!(
        wait_until(WAIT, || {
            backend.pump(BLOCK, 0.0);
            handle.state().input_peak < 0.1
        }),
        "silence must release the published meter past -20 dB \
         (it never does if the coefficient is left at the 1.0 default)"
    );

    let state = handle.state();
    assert!(
        (state.input_peak_session - 1.0).abs() < 1e-6,
        "the session statistic stays at full scale while the meter falls"
    );
}
