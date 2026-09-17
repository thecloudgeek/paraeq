//! R1-6 — rate independence, per the resolved Open Question 1
//! (`docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q1 and the
//! `DECIDED (2026-07-21)` block at
//! `docs/specs/2026-07-15-engine-hardening-design.md:403`).
//!
//! Two halves, both here so the whole story reads in one file:
//!
//! 1. **Pure `build_correction`.** `CorrectionConfig::Peq` carries design
//!    *intent* and re-derives coefficients at the LIVE stream rate, so a
//!    correction survives a rate switch (a 47 Hz mode filter stays at 47 Hz).
//!    Its `design_rate` is provenance only. The baked variants
//!    (`Fir`/`Iir`) cannot be re-derived, so they keep R1-6's original net:
//!    refuse on an EXACT rate mismatch and fail open to flat.
//! 2. **Controller integration** against `MockBackend`, whose per-start rate
//!    queue scripts an AirPods-style handoff. These assert the spec's own
//!    test table (`:430-435`) plus gap 3 (`:407`): the refusal is published in
//!    the same start that used to install stale coefficients, with no
//!    `tick_ms` window of audible wrong-EQ.
//!
//! Oracle tier: in-crate analytic (`EQBand::to_sos` is already Tier-1
//! fixture-pinned by `paraeq-dsp`'s `test_biquad.rs`), so no new fixture.

mod common;

use std::time::{Duration, Instant};

use common::mock_backend::MockBackend;
use paraeq_dsp::biquad;
use paraeq_dsp::peq::{EQBand, FilterType};
use paraeq_engine::backend::BackendEvent;
use paraeq_engine::chain::{Correction, CorrectionKind};
use paraeq_engine::controller::{
    build_correction, CorrectionConfig, EngineCommand, EngineConfig, EngineHandle, EngineState,
};
use paraeq_engine::status::WatchdogConfig;
use proptest::prelude::*;

const BLOCK: usize = 512;
const CHANNELS: usize = 2;
const TICK_MS: u64 = 10;
/// Generous per-wait budget; each wait normally completes in tens of ms.
const WAIT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn peaking(fc: f64, gain_db: f64, q: f64) -> EQBand {
    EQBand {
        filter_type: FilterType::Peaking,
        fc,
        gain_db,
        q,
    }
}

/// A one-channel `Peq` config (the desktop's shape: one set, broadcast).
fn peq(bands: Vec<EQBand>, design_rate: f64) -> CorrectionConfig {
    CorrectionConfig::Peq {
        bands: vec![bands],
        design_rate,
    }
}

/// A memoryless gain-`g` baked IIR correction (`y = g * x`).
///
/// Callers that assert on OUTPUT AMPLITUDES pass `g <= 1`: R1-1 computes an
/// auto-preamp for the baked arm too (from the realized cascade's magnitude),
/// so a boosting `g` would be pulled straight back to unity and the test
/// could no longer tell a correction from pass-through. A pure cut gets a
/// preamp of exactly 0 dB, which keeps the arithmetic about the swap.
fn baked_iir(g: f64, design_rate: f64) -> CorrectionConfig {
    CorrectionConfig::Iir {
        design_rate,
        sos_per_channel: vec![vec![[g, 0.0, 0.0, 1.0, 0.0, 0.0]]; CHANNELS],
    }
}

/// The SOS rows a built correction actually installed on `channel`.
fn installed_sos(correction: &Correction, channel: usize) -> Vec<[f64; 6]> {
    match &correction.kind {
        CorrectionKind::Iir(p) => p.sos(channel).expect("channel installed").to_vec(),
        CorrectionKind::Fir(_) => panic!("expected an Iir correction"),
    }
}

/// Build at `rate` and return the installed rows for channel 0.
fn build_rows(config: &CorrectionConfig, rate: f64) -> Vec<[f64; 6]> {
    let (correction, _) = build_correction(config, CHANNELS, BLOCK, rate).expect("builds");
    installed_sos(&correction, 0)
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
        tick_ms: TICK_MS,
        watchdog: WatchdogConfig {
            engage_tolerance_ms: 200,
            idle_window_ms: 150,
            silence_window_ms: 150,
        },
    }
}

/// One block of impulse (`amplitude` at sample 0, silence after) on every
/// channel -- the only excitation that discriminates a peaking band, since a
/// constant block sits at DC where every peaking design is 0 dB.
fn impulse_block(amplitude: f32) -> Vec<Vec<f32>> {
    let mut channel = vec![0.0f32; BLOCK];
    channel[0] = amplitude;
    vec![channel; CHANNELS]
}

/// The impulse response a correction built at `rate` produces, computed
/// OUTSIDE the engine: the reference the live chain is compared against.
///
/// R1-1's auto-preamp is part of the corrected path, so it is part of the
/// reference: the chain applies `y * preamp_lin * gain`, and a reference that
/// skipped the preamp would fail every boosting band set.
fn reference_response(config: &CorrectionConfig, rate: f64, amplitude: f32) -> Vec<f32> {
    let (mut correction, _) =
        build_correction(config, CHANNELS, BLOCK, rate).expect("reference builds");
    let mut input = vec![0.0f64; BLOCK];
    input[0] = f64::from(amplitude);
    let inputs: Vec<&[f64]> = (0..CHANNELS).map(|_| input.as_slice()).collect();
    let mut outputs = vec![vec![0.0f64; BLOCK]; CHANNELS];
    let preamp_lin = correction.preamp_lin;
    match &mut correction.kind {
        CorrectionKind::Fir(c) => c.process(&inputs, &mut outputs),
        CorrectionKind::Iir(p) => p.process(&inputs, &mut outputs),
    }
    outputs[0]
        .iter()
        .map(|&v| (v as f32) * preamp_lin)
        .collect()
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

// ---------------------------------------------------------------------------
// 1-9, 17: pure `build_correction`
// ---------------------------------------------------------------------------

/// Test 1. The whole point of Q1: a `Peq` config installs exactly the
/// coefficients `EQBand::to_sos` designs at the LIVE rate -- bit for bit, at
/// every rate, with no reference to `design_rate`.
#[test]
fn peq_sos_equal_to_sos_at_the_live_rate() {
    let bands = vec![peaking(47.0, 6.0, 2.0), peaking(3_200.0, -4.5, 1.1)];
    let config = peq(bands.clone(), 48_000.0);
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let rows = build_rows(&config, rate);
        let expected: Vec<[f64; 6]> = bands.iter().map(|b| b.to_sos(rate)).collect();
        assert_eq!(rows, expected, "re-derivation drifted at {rate} Hz");
    }
}

/// Test 2. The falsifier for test 1: if re-derivation regressed to "design
/// once, reuse", the two rates would produce identical rows and test 1 would
/// pass vacuously.
#[test]
fn peq_at_two_rates_produces_different_coefficients() {
    let config = peq(vec![peaking(1_000.0, 6.0, 1.0)], 48_000.0);
    let at_44100 = build_rows(&config, 44_100.0);
    let at_48000 = build_rows(&config, 48_000.0);
    assert_eq!(at_44100.len(), 1);
    let worst = at_44100[0]
        .iter()
        .zip(&at_48000[0])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f64, f64::max);
    assert!(
        worst > 1e-9,
        "44.1 and 48 kHz designs must differ; worst coefficient delta was {worst:e}"
    );
}

/// Test 3. `Peq { design_rate }` is PROVENANCE ONLY. This pins §Q1 against
/// R1-6's literal "refuse on `design_rate != stream_rate`" text, which the
/// DECIDED block supersedes for re-derivable configs.
#[test]
fn peq_design_rate_is_provenance_only() {
    let bands = vec![peaking(1_000.0, 3.0, 1.0)];
    let config = peq(bands.clone(), 96_000.0);
    let (correction, report) =
        build_correction(&config, CHANNELS, BLOCK, 48_000.0).expect("a stale design_rate is fine");
    assert_eq!(report.bands_dropped, 0);
    assert_eq!(
        installed_sos(&correction, 0),
        vec![bands[0].to_sos(48_000.0)],
        "coefficients must come from the LIVE rate, not design_rate"
    );
}

/// Test 4. The retained R1-6 net: baked coefficients carry no intent, so a
/// rate mismatch is refused (and a match installs normally).
#[test]
fn baked_iir_and_fir_refuse_on_rate_mismatch() {
    let iir = baked_iir(2.0, 48_000.0);
    assert!(build_correction(&iir, CHANNELS, BLOCK, 44_100.0).is_err());
    assert!(build_correction(&iir, CHANNELS, BLOCK, 48_000.0).is_ok());

    let fir = CorrectionConfig::Fir {
        design_rate: 48_000.0,
        firs: vec![vec![2.0]; CHANNELS],
    };
    assert!(build_correction(&fir, CHANNELS, BLOCK, 44_100.0).is_err());
    assert!(build_correction(&fir, CHANNELS, BLOCK, 48_000.0).is_ok());
}

/// Test 5. Spec `:420` — "**Exact comparison**, not a tolerance": device
/// rates are f64s that round-trip exactly, and a fuzzy compare would silently
/// accept a genuinely different rate.
#[test]
fn rate_compare_is_exact_not_tolerant() {
    let config = baked_iir(2.0, 48_000.0 + 1e-9);
    assert!(
        build_correction(&config, CHANNELS, BLOCK, 48_000.0).is_err(),
        "48000.0 + 1e-9 is not 48000.0"
    );
}

/// Test 6. R1-3's precedent (spec `:216`, "never bricked by one bad band")
/// applied to the Nyquist half: a band that is legal at 48 kHz but at/above
/// Nyquist at 44.1 kHz is DROPPED and counted, and the rest still design.
/// 23 kHz is the pivot the desktop's own suite already uses -- below 48 kHz's
/// 24 kHz Nyquist, above 44.1 kHz's 22.05 kHz.
#[test]
fn peq_band_at_or_above_new_nyquist_is_dropped_and_counted() {
    let high = peaking(23_000.0, -3.0, 1.0);
    let low = peaking(1_000.0, 6.0, 1.0);
    let config = peq(vec![low.clone(), high], 48_000.0);

    let (at_48k, report_48k) = build_correction(&config, CHANNELS, BLOCK, 48_000.0).expect("ok");
    assert_eq!(report_48k.bands_dropped, 0);
    assert_eq!(installed_sos(&at_48k, 0).len(), 2);

    let (at_44k, report_44k) = build_correction(&config, CHANNELS, BLOCK, 44_100.0).expect("ok");
    assert_eq!(
        report_44k.bands_dropped, 1,
        "23 kHz is above Nyquist at 44.1 kHz"
    );
    assert_eq!(
        installed_sos(&at_44k, 0),
        vec![low.to_sos(44_100.0)],
        "the surviving band still designs at the live rate"
    );
}

/// Test 7. ...but when NOTHING survives there is no correction to install, so
/// the whole config is refused and the controller fails open to flat
/// (spec `:422`: "audibly un-EQ'd is always better than audibly wrong-EQ'd").
#[test]
fn peq_with_every_band_above_nyquist_refuses_and_fails_open() {
    let config = peq(
        vec![peaking(23_000.0, -3.0, 1.0), peaking(23_500.0, 2.0, 1.0)],
        48_000.0,
    );
    assert!(build_correction(&config, CHANNELS, BLOCK, 48_000.0).is_ok());
    assert!(
        build_correction(&config, CHANNELS, BLOCK, 44_100.0).is_err(),
        "no band survives 44.1 kHz -> refuse, not an empty correction"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Test 8. The hostile box, mirroring R1-3's own proptest
    /// (`test_chain.rs::hostile_box_never_installs_unstable_sections`,
    /// spec `:226`): whatever reaches `build_correction`, it never panics and
    /// never installs a section failing the strict Jury form. `design_rate`
    /// is deliberately hostile too -- for `Peq` it must never matter.
    #[test]
    fn build_correction_never_panics_and_never_installs_an_unstable_row(
        fc in prop_oneof![
            Just(f64::NAN),
            Just(f64::INFINITY),
            Just(0.0f64),
            Just(-1.0f64),
            1e-3f64..1e6,
        ],
        gain_db in prop_oneof![Just(f64::NAN), -200.0f64..200.0],
        q in prop_oneof![Just(0.0f64), Just(f64::NAN), Just(1e-310f64), 1e-3f64..1e4],
        filter in 0usize..4,
        design_rate in prop_oneof![Just(f64::NAN), Just(0.0f64), Just(44_100.0f64), Just(48_000.0f64)],
        stream_rate in prop_oneof![
            Just(44_100.0f64),
            Just(48_000.0f64),
            Just(88_200.0f64),
            Just(96_000.0f64),
        ],
    ) {
        let filter_type = [
            FilterType::HighShelf,
            FilterType::LowShelf,
            FilterType::Notch,
            FilterType::Peaking,
        ][filter];
        let config = CorrectionConfig::Peq {
            bands: vec![vec![
                EQBand { filter_type, fc, gain_db, q },
                peaking(1_000.0, 3.0, 1.0),
            ]],
            design_rate,
        };
        // Never panics: the degenerate cases are Err, never an assert.
        if let Ok((correction, _)) = build_correction(&config, CHANNELS, BLOCK, stream_rate) {
            for ch in 0..CHANNELS {
                for row in installed_sos(&correction, ch) {
                    prop_assert!(
                        row.iter().all(|c| c.is_finite()),
                        "installed non-finite row {row:?}"
                    );
                    prop_assert!(biquad::is_stable(&row), "installed unstable row {row:?}");
                }
            }
        }
    }
}

/// Test 17. `CorrectionConfig::Peq` goes on the wire with `EQBand` inside it,
/// so `paraeq-engine` must enable `paraeq-dsp`'s `serde` feature and the
/// `FilterType` spelling must stay snake_case (the same spelling
/// `desktop/ui/src/ipc/types.ts` pins by hand).
#[test]
fn peq_config_serde_round_trip_snake_case() {
    let config = peq(vec![peaking(1_000.0, 3.0, 1.0)], 44_100.0);
    let json = serde_json::to_value(&config).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "Peq": {
                "bands": [[{
                    "filter_type": "peaking",
                    "fc": 1000.0,
                    "gain_db": 3.0,
                    "q": 1.0
                }]],
                "design_rate": 44100.0
            }
        })
    );
    let back: CorrectionConfig = serde_json::from_value(json).expect("deserializes");
    match back {
        CorrectionConfig::Peq { bands, design_rate } => {
            assert_eq!(design_rate, 44_100.0);
            assert_eq!(bands, vec![vec![peaking(1_000.0, 3.0, 1.0)]]);
        }
        other => panic!("round trip changed the variant: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 10-14: controller integration (MockBackend)
// ---------------------------------------------------------------------------

/// Test 10 (and spec test-row 1's `Peq` half, which §Q1 splits off). One
/// session, three `FormatChanged` rebuilds at 44.1 -> 48 -> 96 kHz: at every
/// rate the live chain's impulse response matches a correction built at THAT
/// rate, and differs measurably from the previous rate's response.
#[test]
fn peq_survives_rate_flip_44100_48000_96000() {
    const AMPLITUDE: f32 = 0.1;
    let backend = MockBackend::new();
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        backend.queue_sample_rate(rate);
    }
    backend.set_reported_sample_rate(96_000.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    let config = peq(vec![peaking(1_000.0, 9.0, 1.0)], 48_000.0);
    handle.send(EngineCommand::SetCorrection(config.clone()));

    let input = impulse_block(AMPLITUDE);
    let mut previous: Option<Vec<f32>> = None;
    for (step, rate) in [44_100.0f64, 48_000.0, 96_000.0].into_iter().enumerate() {
        if step > 0 {
            backend.queue_event(BackendEvent::DefaultOutputChanged);
        }
        assert!(
            wait_until(WAIT, || handle
                .state()
                .stream
                .as_ref()
                .is_some_and(|s| s.sample_rate == rate)),
            "the session never reached {rate} Hz"
        );
        let expected = reference_response(&config, rate, AMPLITUDE);
        assert!(
            wait_until(WAIT, || {
                backend
                    .pump_samples(&input)
                    .is_some_and(|out| max_abs_diff(&out[0], &expected) < 1e-6)
            }),
            "the chain at {rate} Hz does not match a correction designed at {rate} Hz"
        );
        assert_eq!(
            handle.state().correction_rate_mismatch,
            None,
            "a re-derivable config must never raise the refusal flag"
        );
        if let Some(prev) = &previous {
            assert!(
                max_abs_diff(prev, &expected) > 1e-4,
                "the {rate} Hz response is indistinguishable from the previous rate's"
            );
        }
        previous = Some(expected);
    }
}

/// Test 11 = the spec's test-row 1 verbatim, for the baked arm: "synthetic
/// backend reporting 48000 then 44100 -> `build_correction` refuses; the
/// chain runs pass-through (feed an impulse, assert output == input x gain --
/// *not* stale-corrected); `correction_rate_mismatch` is published".
#[test]
fn baked_iir_fails_open_to_pass_through_on_rate_flip() {
    let backend = MockBackend::new();
    backend.queue_sample_rate(48_000.0);
    backend.set_reported_sample_rate(44_100.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    // -6.0206 dB is 0.5 linear; the correction halves. At the design rate:
    // 0.2 -> 0.1 -> 0.05.
    handle.send(EngineCommand::SetCorrection(baked_iir(0.5, 48_000.0)));
    handle.send(EngineCommand::SetGainDb(-6.020_6));
    let input = impulse_block(0.2);
    assert!(wait_until(WAIT, || {
        backend
            .pump_samples(&input)
            .is_some_and(|out| (out[0][0] - 0.05).abs() < 1e-5)
    }));
    assert_eq!(handle.state().correction_rate_mismatch, None);

    // Rate flip: the baked coefficients cannot be re-derived, so the chain
    // must run FLAT -- input x gain only (0.2 * 0.5 = 0.1), never the stale
    // 0.05 the old coefficients would have produced.
    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(wait_until(WAIT, || handle.state().correction_rate_mismatch
        == Some(44_100.0)));
    assert!(
        wait_until(WAIT, || {
            backend
                .pump_samples(&input)
                .is_some_and(|out| (out[0][0] - 0.1).abs() < 1e-5)
        }),
        "a refused correction must leave flat pass-through, not stale EQ"
    );
}

/// Test 12 = the spec's test-row 2: a rebuild at the SAME rate installs
/// normally and raises no flag.
#[test]
fn rate_unchanged_installs_with_no_flag() {
    let backend = MockBackend::new();
    backend.set_reported_sample_rate(48_000.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    handle.send(EngineCommand::SetCorrection(baked_iir(0.5, 48_000.0)));
    let input = impulse_block(0.2);
    assert!(wait_until(WAIT, || {
        backend
            .pump_samples(&input)
            .is_some_and(|out| (out[0][0] - 0.1).abs() < 1e-5)
    }));

    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(wait_until(WAIT, || backend.start_count() == 2));
    assert!(wait_until(WAIT, || {
        backend
            .pump_samples(&input)
            .is_some_and(|out| (out[0][0] - 0.1).abs() < 1e-5)
    }));
    assert_eq!(handle.state().correction_rate_mismatch, None);
}

/// Test 13 — gap 3 (spec `:407`). The refusal is visible on the FIRST
/// snapshot that carries the new rate: there is no `tick_ms` (250 ms) window
/// in which the stale coefficients are live and audible while the desktop
/// waits to be told.
#[test]
fn refusal_is_published_in_the_same_start_that_used_to_install_stale_coefficients() {
    let backend = MockBackend::new();
    backend.queue_sample_rate(48_000.0);
    backend.set_reported_sample_rate(44_100.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());
    let snapshots = handle.subscribe();

    handle.send(EngineCommand::SetCorrection(baked_iir(2.0, 48_000.0)));
    assert!(wait_until(WAIT, || handle
        .state()
        .correction
        .as_deref()
        .is_some_and(|c| c.starts_with("iir"))));

    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(wait_until(WAIT, || backend.start_count() == 2));

    // EVERY snapshot reporting 44.1 kHz already carries the flag -- including
    // the first one.
    let mut saw_new_rate = false;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        while let Ok(snapshot) = snapshots.try_recv() {
            let at_44100 = snapshot
                .stream
                .as_ref()
                .is_some_and(|s| s.sample_rate == 44_100.0);
            if at_44100 {
                saw_new_rate = true;
                assert_eq!(
                    snapshot.correction_rate_mismatch,
                    Some(44_100.0),
                    "a snapshot announced the new rate before announcing the refusal"
                );
            }
        }
        if saw_new_rate {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(saw_new_rate, "no snapshot ever reported the new rate");
}

/// Test 14. The flag is not a latch: a config the engine CAN re-derive
/// clears it (and installs).
#[test]
fn mismatch_flag_clears_when_a_re_derivable_config_arrives() {
    let backend = MockBackend::new();
    backend.set_reported_sample_rate(44_100.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    handle.send(EngineCommand::SetCorrection(baked_iir(2.0, 48_000.0)));
    assert!(wait_until(WAIT, || handle.state().correction_rate_mismatch
        == Some(44_100.0)));

    let band = peaking(1_000.0, 9.0, 1.0);
    let config = peq(vec![band], 48_000.0);
    handle.send(EngineCommand::SetCorrection(config.clone()));
    assert!(wait_until(WAIT, || handle
        .state()
        .correction_rate_mismatch
        .is_none()));

    let input = impulse_block(0.1);
    let expected = reference_response(&config, 44_100.0, 0.1);
    assert!(wait_until(WAIT, || {
        backend
            .pump_samples(&input)
            .is_some_and(|out| max_abs_diff(&out[0], &expected) < 1e-6)
    }));
}

/// R1-6 behaviour item 3, verbatim: "On refusal the controller sends **no**
/// correction -- flat pass-through." The two spec test rows above cannot
/// reach it. Both refuse across a REBUILD, and `start_with` has just built a
/// fresh `RealtimeChain` that is already flat, so "send nothing" and "send an
/// explicit `Correction(None)`" are indistinguishable there.
///
/// The state where the clear does work is a refusal arriving while a
/// correction is LIVE: `EngineCommand::SetCorrection` stores the config and
/// calls `send_correction()` with no stop and no rebuild. Delete the explicit
/// clear and this is the only test in the workspace that notices -- the
/// engine keeps the previous correction audible under a published "EQ paused
/// -- correction must be redesigned" banner.
#[test]
fn a_refused_correction_replaces_a_live_one_with_flat_pass_through() {
    const AMPLITUDE: f32 = 0.1;
    let backend = MockBackend::new();
    backend.set_reported_sample_rate(44_100.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    // A real, audible correction at the live rate.
    let config = peq(vec![peaking(1_000.0, 9.0, 1.0)], 44_100.0);
    handle.send(EngineCommand::SetCorrection(config.clone()));
    let input = impulse_block(AMPLITUDE);
    let corrected = reference_response(&config, 44_100.0, AMPLITUDE);
    let flat = input[0].clone();
    assert!(
        max_abs_diff(&corrected, &flat) > 1e-3,
        "the band must be audible, or a stale correction would look like pass-through"
    );
    assert!(
        wait_until(WAIT, || {
            backend
                .pump_samples(&input)
                .is_some_and(|out| max_abs_diff(&out[0], &corrected) < 1e-6)
        }),
        "the correction never went live"
    );

    // Mid-session refusal: baked coefficients from another rate, no rebuild
    // and no fresh chain. Gain is still 0 dB, so flat pass-through IS the
    // input, bit for bit.
    handle.send(EngineCommand::SetCorrection(baked_iir(0.5, 48_000.0)));
    assert!(wait_until(WAIT, || handle.state().correction_rate_mismatch
        == Some(44_100.0)));
    assert!(
        wait_until(WAIT, || {
            backend
                .pump_samples(&input)
                .is_some_and(|out| max_abs_diff(&out[0], &flat) < 1e-6)
        }),
        "a refused correction left the PREVIOUS correction audible instead of failing open to flat"
    );
}

/// `correction_rate_mismatch` and `auto_preamp_db` are SESSION-SCOPED, and
/// their own docs say so: the flag names the LIVE stream's rate and the
/// preamp names what the LIVE chain is applying, so with no session both
/// statements are stale. Nothing pinned the invariant before this test --
/// deleting `stop_session`'s two clearing lines left the whole workspace
/// green, and a published snapshot with `stream == None` still renders the
/// destructive "EQ paused -- correction must be redesigned for 44.1 kHz"
/// chip while the engine is stopped.
#[test]
fn a_teardown_clears_the_session_scoped_publications() {
    let backend = MockBackend::new();
    backend.set_reported_sample_rate(44_100.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    // Half 1: a refusal raises the flag, and a Disable must retract it.
    handle.send(EngineCommand::SetCorrection(baked_iir(2.0, 48_000.0)));
    assert!(wait_until(WAIT, || handle.state().correction_rate_mismatch
        == Some(44_100.0)));
    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || handle.state().stream.is_none()));
    let stopped = handle.state();
    assert_eq!(
        stopped.correction_rate_mismatch, None,
        "no stream, but the snapshot still names a rate the correction must be redesigned for"
    );
    assert_eq!(
        stopped.auto_preamp_db, None,
        "no stream, but the snapshot still reports an auto-preamp being applied"
    );

    // Half 2: the same for a correction that INSTALLED (the preamp half is
    // reachable on every ordinary session, not only on a refusal).
    handle.send(EngineCommand::Enable);
    handle.send(EngineCommand::SetCorrection(peq(
        vec![peaking(1_000.0, 9.0, 1.0)],
        44_100.0,
    )));
    assert!(wait_until(WAIT, || handle
        .state()
        .auto_preamp_db
        .is_some_and(|p| (p + 9.0).abs() < 0.05)));
    handle.send(EngineCommand::Disable);
    assert!(wait_until(WAIT, || handle.state().stream.is_none()));
    let stopped = handle.state();
    assert_eq!(stopped.auto_preamp_db, None);
    assert_eq!(stopped.correction_rate_mismatch, None);
}

/// The other way to end up with no session: `start_once`'s geometry
/// renegotiation. It does NOT route through `stop_session` -- it is a bare
/// `stop()` + `self.session = None` -- so if the retry `start` then fails,
/// `start_with` returns before assigning `self.session` and `publish()` emits
/// `stream: None` carrying the aborted attempt's session-scoped fields.
///
/// Asserted as an invariant over every published snapshot, which is what the
/// desktop actually consumes, rather than at one sampled instant: the stale
/// window is bounded by one tick, and a point read would race it.
#[test]
fn a_failed_renegotiation_retry_clears_the_session_scoped_publications() {
    let backend = MockBackend::new();
    backend.set_reported_sample_rate(48_000.0);
    // Start #1 converges (the chain's provisional geometry is 2 x 512).
    backend.queue_report(CHANNELS, BLOCK);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());

    // A retained correction the live rate refuses, so the flag is up before
    // the rebuild and `start_with` re-raises it on the aborted attempt.
    handle.send(EngineCommand::SetCorrection(baked_iir(0.5, 44_100.0)));
    assert!(wait_until(WAIT, || handle.state().correction_rate_mismatch
        == Some(48_000.0)));
    let snapshots = handle.subscribe();

    // Rebuild: start #2 reports a geometry the chain was not built for ->
    // renegotiate; the retry (start #3) fails.
    backend.queue_report(CHANNELS, BLOCK / 2);
    backend.fail_start_number(3);
    backend.queue_event(BackendEvent::DefaultOutputChanged);
    assert!(
        wait_until(WAIT, || backend.start_count() >= 4),
        "the renegotiation + failed retry never happened"
    );

    let mut saw_stopped_snapshot = false;
    for snapshot in std::iter::from_fn(|| snapshots.try_recv().ok()) {
        let snapshot: std::sync::Arc<EngineState> = snapshot;
        if snapshot.stream.is_some() {
            continue;
        }
        saw_stopped_snapshot = true;
        assert_eq!(
            snapshot.correction_rate_mismatch, None,
            "published a snapshot with NO stream but correction_rate_mismatch still set"
        );
        assert_eq!(
            snapshot.auto_preamp_db, None,
            "published a snapshot with NO stream but auto_preamp_db still set"
        );
    }
    assert!(
        saw_stopped_snapshot,
        "no session-less snapshot was published; the invariant was never exercised"
    );
}

/// The published `EngineState` is the only way the desktop learns any of
/// this, so pin that the flag actually rides a snapshot (and is not merely
/// controller-internal).
#[test]
fn mismatch_flag_rides_the_published_snapshot() {
    let backend = MockBackend::new();
    backend.set_reported_sample_rate(44_100.0);
    let handle = EngineHandle::spawn(backend.clone(), fast_config());
    let snapshots = handle.subscribe();

    handle.send(EngineCommand::SetCorrection(baked_iir(2.0, 48_000.0)));
    assert!(wait_until(WAIT, || {
        std::iter::from_fn(|| snapshots.try_recv().ok())
            .any(|s: std::sync::Arc<EngineState>| s.correction_rate_mismatch == Some(44_100.0))
    }));
}
