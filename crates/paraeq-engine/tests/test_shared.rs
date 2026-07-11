//! Lock-free control-plane <-> realtime tests: swap/retire rings under
//! churn, retire-gated deferral, and telemetry counters.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use paraeq_engine::chain::{build_iir, RealtimeChain};
use paraeq_engine::shared::{links, RtMsg, RtProcessor, RtShared};

const BLOCK: usize = 64;

/// A memoryless "multiply by b0" section: output = b0 * input, exactly.
fn scale_sos(b0: f64) -> Vec<Vec<[f64; 6]>> {
    vec![vec![[b0, 0.0, 0.0, 1.0, 0.0, 0.0]]]
}

/// Pump one constant-amplitude block through the processor and return the
/// output channels.
fn pump(proc_: &mut RtProcessor, channels: usize, frames: usize, amplitude: f32) -> Vec<Vec<f32>> {
    let input = vec![vec![amplitude; frames]; channels];
    let views: Vec<&[f32]> = input.iter().map(|v| v.as_slice()).collect();
    let mut out = vec![vec![0.0f32; frames]; channels];
    {
        let mut out_views: Vec<&mut [f32]> = out.iter_mut().map(|v| v.as_mut_slice()).collect();
        proc_.process_block(&views, &mut out_views, 512.0);
    }
    out
}

#[test]
fn swap_under_churn_is_clean() {
    let (mut ctl, rt) = links(4);
    let shared = Arc::new(RtShared::default());
    let chain = RealtimeChain::new(2, BLOCK);
    let mut proc_ = RtProcessor::new(shared.clone(), rt, chain);

    let done = Arc::new(AtomicBool::new(false));
    let done_ctl = done.clone();

    // Control thread: 200 alternating Iir/None swaps, retrying on a full
    // ring and draining retired configs continuously (they are freed HERE,
    // never on the rt side).
    let control = thread::spawn(move || {
        let mut accepted = 0u64;
        let mut retired = 0u64;
        for i in 0..200 {
            loop {
                let msg = if i % 2 == 0 {
                    RtMsg::Correction(Some(build_iir(
                        vec![scale_sos(0.5).remove(0), scale_sos(0.5).remove(0)],
                        2,
                        BLOCK,
                    )))
                } else {
                    RtMsg::Correction(None)
                };
                match ctl.send(msg) {
                    Ok(()) => {
                        accepted += 1;
                        break;
                    }
                    Err(_) => {
                        retired += ctl.drain_retired() as u64;
                        thread::yield_now();
                    }
                }
            }
            retired += ctl.drain_retired() as u64;
        }
        // Settle: every accepted swap must eventually come back retired.
        let mut spins = 0u64;
        while retired < accepted {
            retired += ctl.drain_retired() as u64;
            thread::yield_now();
            spins += 1;
            assert!(spins < 100_000_000, "retired configs never came back");
        }
        done_ctl.store(true, Ordering::Relaxed);
        (accepted, retired)
    });

    // Realtime side: pump synthetic blocks, polling each block (poll runs
    // inside process_block), until the control thread is finished and at
    // least 10_000 blocks have gone through.
    let mut pumps = 0u64;
    while !done.load(Ordering::Relaxed) || pumps < 10_000 {
        let out = pump(&mut proc_, 2, BLOCK, 0.5);
        for ch in &out {
            for &s in ch {
                assert!(s.is_finite(), "non-finite output during swap churn");
            }
        }
        pumps += 1;
        assert!(pumps < 50_000_000, "control thread never finished");
    }

    let (accepted, retired) = control.join().expect("control thread panicked");
    assert_eq!(accepted, 200, "every send must eventually be accepted");
    assert_eq!(
        retired, accepted,
        "every accepted swap must produce exactly one retired config"
    );
    assert_eq!(shared.callbacks.load(Ordering::Relaxed), pumps);
}

#[test]
fn swap_defers_when_retire_ring_full() {
    // Capacity-1 rings: the second consecutive send errors, so sends MUST
    // interleave with pumps.
    let (mut ctl, rt) = links(1);
    let shared = Arc::new(RtShared::default());
    let chain = RealtimeChain::new(1, 8);
    let mut proc_ = RtProcessor::new(shared, rt, chain);

    // Send #1 (0.5x) -> pump: swap #1 lands, retire ring now 1/1 full.
    ctl.send(RtMsg::Correction(Some(build_iir(scale_sos(0.5), 1, 8))))
        .expect("send #1 fits an empty ring");
    let out = pump(&mut proc_, 1, 8, 0.8);
    assert_eq!(out[0][0], 0.4, "correction #1 (0.5x) must be active");

    // Send #2 (0.25x) fits -- the control->rt ring was drained by the pump.
    ctl.send(RtMsg::Correction(Some(build_iir(scale_sos(0.25), 1, 8))))
        .expect("send #2 fits: control ring was drained by the pump");

    // Pump: swap #2 is DEFERRED -- the retire ring is full, and the rt side
    // must never be forced to drop (deallocate) a processor.
    let out = pump(&mut proc_, 1, 8, 0.8);
    assert_eq!(
        out[0][0], 0.4,
        "swap must defer while the retire ring is full"
    );

    // Free the retire slot, pump again: swap #2 lands.
    assert_eq!(ctl.drain_retired(), 1);
    let out = pump(&mut proc_, 1, 8, 0.8);
    assert_eq!(out[0][0], 0.2, "correction #2 (0.25x) lands after drain");
}

#[test]
fn zero_and_nonzero_blocks_counted() {
    let (_ctl, rt) = links(1);
    let shared = Arc::new(RtShared::default());
    let chain = RealtimeChain::new(2, BLOCK);
    let mut proc_ = RtProcessor::new(shared.clone(), rt, chain);

    pump(&mut proc_, 2, BLOCK, 0.0);
    assert_eq!(shared.zero_blocks.load(Ordering::Relaxed), 1);
    assert_eq!(shared.nonzero_blocks.load(Ordering::Relaxed), 0);

    pump(&mut proc_, 2, BLOCK, 0.75);
    assert_eq!(shared.zero_blocks.load(Ordering::Relaxed), 1);
    assert_eq!(shared.nonzero_blocks.load(Ordering::Relaxed), 1);
    assert_eq!(shared.peak_in(), 0.75);

    // A quieter block bumps the counter but not the peak.
    pump(&mut proc_, 2, BLOCK, 0.5);
    assert_eq!(shared.nonzero_blocks.load(Ordering::Relaxed), 2);
    assert_eq!(shared.peak_in(), 0.75, "peak tracks the maximum");

    assert_eq!(shared.callbacks.load(Ordering::Relaxed), 3);
    assert_eq!(shared.sample_time_delta(), 512.0);
}

#[test]
fn note_skipped_block_counts_the_callback_too() {
    let (_ctl, rt) = links(1);
    let shared = Arc::new(RtShared::default());
    let chain = RealtimeChain::new(2, BLOCK);
    let mut proc_ = RtProcessor::new(shared.clone(), rt, chain);

    // A backend-level skip is still an IOProc invocation: `callbacks` must
    // advance (or the watchdog would misreport a persistently-skipping
    // session as benign Idle) alongside `skipped_blocks`.
    proc_.note_skipped_block();
    assert_eq!(shared.callbacks.load(Ordering::Relaxed), 1);
    assert_eq!(shared.skipped_blocks.load(Ordering::Relaxed), 1);

    // A processed block advances callbacks only.
    pump(&mut proc_, 2, BLOCK, 0.5);
    assert_eq!(shared.callbacks.load(Ordering::Relaxed), 2);
    assert_eq!(shared.skipped_blocks.load(Ordering::Relaxed), 1);
}
