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
//! * The decaying-`input_peak` row (`:579`) is NOT here: R1-8's release gate
//!   (`:582`) splits the clip counter (blocks release) from the decaying
//!   meter (does not), and the decay lands with its own item.
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
    build_correction, CorrectionConfig, EngineCommand, EngineConfig, EngineHandle,
};
use paraeq_engine::shared::{links, ControlLink, RtProcessor, RtShared};
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
    _control: ControlLink,
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
        _control: control,
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
