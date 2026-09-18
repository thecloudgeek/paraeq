//! R1-10's deferred measured gate: the convolver at the room geometry.
//!
//! Spec: `docs/specs/2026-07-15-engine-hardening-design.md`, `R1-10 § Gate`,
//! verbatim: *"add an `#[ignore]`d bench running the convolver at 16384 taps /
//! 512 block, reporting µs/block. The threshold that justifies the work is
//! **>25% of the block period** -- 512/48000 = 10.67 ms, so **2.67 ms/block**.
//! Under that, leave it alone."*
//!
//! # Why this bench exists and why it is the whole deliverable
//!
//! `convolver.rs` is single-partition overlap-add: `n_fft = (block_size +
//! fir_len - 1).next_power_of_two()`, so a 16384-tap room FIR at a 512-frame
//! block runs a 32768-point transform over a buffer that is 1/64 populated.
//! R1-10 ranks the partitioned rewrite **deferred**, not wrong, and the
//! reasons are recorded in the spec rather than re-derived here: measured CPU
//! today is 0.2-0.4%, and (`R1-10 § Honest ranking` 2) *"The problem only
//! exists for the room FIR, which does not exist yet. And it may never: the
//! room-DSP argument favors **matched parametric filters for modal peaks** ...
//! which is an **IIR** path, not a FIR one."*
//!
//! The rest of the tree agrees. `docs/specs/2026-07-15-decision-engine-design.md`,
//! the decision table's `correction_kind` row, ships **`Peq` on all four
//! paths** and puts the reason in the user's words: *"We used {n} tone controls
//! rather than a convolution filter -- same result, no extra delay."*
//! `docs/specs/2026-07-15-measurement-suite-design.md`'s spec map calls R1-10
//! exactly what it is -- one of *"the deferred denormals + convolver-partitioning
//! items"* inside the R1 safety floor.
//!
//! So this file adds the number, and nothing else. It does **not** implement
//! partitioning and does not adopt `fft-convolver`. If a future room-FIR path
//! ever pushes the printed median over 2.67 ms/block, that is the trigger; the
//! shape of the fix (uniform partitioning at `B = block_size`) is in the spec.
//!
//! # What it asserts, and what it deliberately does not
//!
//! It does **not** assert a time threshold. A wall-clock threshold makes the
//! result machine-dependent, which is why its sibling deferred item is behind
//! `#[ignore]` too (`R1-9`: *"Note such a test is inherently flaky on CI and
//! would live behind `#[ignore]`."*). The timing here is reported, read by a
//! human, and asserted only to the extent that it ran.
//!
//! It **does** assert the realtime-lane contract, which is not machine-dependent
//! at all. `shared.rs`: *"Realtime lane rules: [`RtLink::poll`] and
//! [`RtProcessor::process_block`] never lock, never allocate, never deallocate,
//! never log."* The convolver is reachable from that lane
//! (`chain.rs`, `CorrectionKind::Fir`), and its own allocation contract
//! (`convolver.rs`) is what makes that possible: *"`process` is allocation-free
//! from its second call onward; the *first* call may allocate per-channel
//! overlap state sized to the channel count passed in that call."* So the bench
//! spends **one discarded warm-up `process()` call** -- exactly the call
//! `chain::warm_up` spends on the control plane -- and then counts every
//! allocation and deallocation over the timed steady-state blocks and requires
//! zero of each. Timing the first call would measure an allocation instead of
//! the steady state, and would silently hide the contract breaking.
//!
//! Tier 3 (CLAUDE.md: "Engine code gets synthetic-block unit tests") -- no
//! fixture is involved and none may be. No audio hardware is touched.
//!
//! Run it with:
//!
//! ```text
//! cargo test -p paraeq-engine --release --test test_convolver_bench -- --ignored --nocapture
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::time::Instant;

use paraeq_engine::convolver::OverlapAddConvolver;

/// R1-10's room geometry, verbatim from `R1-10 § Gate`.
const TAPS: usize = 16384;
const BLOCK: usize = 512;
/// The rate the gate's arithmetic is stated at ("512/48000 = 10.67 ms").
const RATE_HZ: f64 = 48_000.0;
/// Stereo, because that is what the realtime chain actually hands `process`:
/// one FIR broadcast to both channels, one `process` call per block. The
/// budget is per *block*, not per channel, so both channels must fit in it.
const CHANNELS: usize = 2;
/// Enough blocks for a stable median without making the run tedious: ~1 s of
/// wall time at a few hundred µs/block.
const TIMED_BLOCKS: usize = 2000;
/// A small pool of distinct input blocks, cycled. Distinct so the measurement
/// is not of one cache-resident block, small so it stays in cache and we
/// measure the transform rather than memory bandwidth.
const INPUT_BLOCKS: usize = 16;

// ---------------------------------------------------------------------------
// Allocation counting (test-only, never in `src/`)
// ---------------------------------------------------------------------------

thread_local! {
    /// Armed only around the timed loop, and only on the thread running it.
    ///
    /// Thread-local rather than a global atomic on purpose: this counter has
    /// to be exact, and the libtest harness thread is alive and allocating
    /// (channel wakeups, timeout bookkeeping, result formatting) while our
    /// test thread runs. A global counter would fold that noise into the
    /// assertion and fail for a reason that has nothing to do with the
    /// convolver.
    ///
    /// `const`-initialized and `Copy` with no destructor, so touching it from
    /// inside the allocator cannot re-enter the allocator: there is no lazy
    /// `Box` and no destructor registration behind the first access.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static DEALLOCS: Cell<u64> = const { Cell::new(0) };
}

struct CountingAllocator;

// Only `alloc` and `dealloc` are overridden: `GlobalAlloc`'s default
// `alloc_zeroed` and `realloc` are defined in terms of those two, so every
// allocation event still passes through the counters.
//
// SAFETY: `GlobalAlloc`'s contract is discharged by delegating it wholesale.
// - Every call FORWARDS to `System`, unchanged: the same `Layout` goes in, the
//   pointer that comes back is returned verbatim with `System`'s provenance,
//   and `dealloc` hands `System` back exactly the pointer and layout pair it
//   issued. Nothing here allocates, adjusts, aligns or reinterprets anything,
//   so this impl is safe exactly to the degree `System` is.
// - The counting is NON-RE-ENTRANT by construction: the three cells are
//   `thread_local!` `Cell<_>`s, `const`-initialized and `Copy` with no
//   destructor, so touching one from inside the allocator cannot allocate.
//   A lazily-`Box`ed or destructor-registering thread-local would re-enter
//   `alloc` on first access and recurse forever.
// - Being the `#[global_allocator]` makes this process-wide, and the counters
//   are per-thread, which is what the benchmark wants: it asserts about
//   allocations on the realtime thread and must not see the harness's.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.with(Cell::get) {
            DEALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

// ---------------------------------------------------------------------------
// Deterministic test signal
// ---------------------------------------------------------------------------

/// xorshift64 in [-0.5, 0.5). Seeded and deterministic so two runs on the
/// same machine are comparable; the same generator the other convolver tests
/// use, so there is one pseudo-random source in this crate's tests.
fn next_sample(state: &mut u64) -> f64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
}

/// A plausible 16384-tap room impulse response: exponentially decaying noise.
///
/// The FFT's cost does not depend on the values, but an all-zero FIR would
/// make the product spectrum identically zero, and "the bench measured
/// nothing but zeros" is a question nobody should have to ask of the number
/// this file prints.
fn decaying_noise_fir(state: &mut u64) -> Vec<f64> {
    // ~60 dB of decay across the FIR, i.e. an RT60 that fills the window.
    let decay = (-6.9 / TAPS as f64).exp();
    let mut envelope = 1.0;
    let mut fir = Vec::with_capacity(TAPS);
    for _ in 0..TAPS {
        fir.push(next_sample(state) * envelope);
        envelope *= decay;
    }
    fir
}

/// Nearest-rank percentile over an ascending-sorted slice.
fn percentile(sorted_us: &[f64], q: f64) -> f64 {
    let last = sorted_us.len() - 1;
    sorted_us[(q * last as f64).round() as usize]
}

// ---------------------------------------------------------------------------
// The bench
// ---------------------------------------------------------------------------

#[test]
#[ignore = "bench: run manually"]
fn steady_state_blocks_report_cost_and_never_allocate() {
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let fir = decaying_noise_fir(&mut rng);
    // One FIR broadcast to every channel -- `OverlapAddConvolver`'s documented
    // mono-FIR broadcast, and what `build_fir` does for a single-FIR config.
    let mut conv = OverlapAddConvolver::new(vec![fir], BLOCK);

    let input: Vec<Vec<Vec<f64>>> = (0..INPUT_BLOCKS)
        .map(|_| {
            (0..CHANNELS)
                .map(|_| (0..BLOCK).map(|_| next_sample(&mut rng)).collect())
                .collect()
        })
        .collect();
    // Borrow-only views, built up front so the timed loop does no work beyond
    // `process` itself.
    let views: Vec<Vec<&[f64]>> = input
        .iter()
        .map(|blk| blk.iter().map(Vec::as_slice).collect())
        .collect();
    let mut out = vec![vec![0.0f64; BLOCK]; CHANNELS];

    // The one discarded warm-up call. Per `convolver.rs`'s allocation
    // contract this is the call that may allocate the per-channel overlap
    // state, so it is spent before the counters are armed and its time is
    // thrown away.
    conv.process(&views[0], &mut out);
    conv.reset();

    // Preallocated: pushing into this inside the armed region must not
    // allocate, or the harness would be measuring itself.
    let mut per_block_us: Vec<f64> = Vec::with_capacity(TIMED_BLOCKS);

    // Touch every thread-local once before arming, so first-access TLS setup
    // (which happens outside our control) cannot land inside the counted
    // region.
    COUNTING.with(|c| c.set(false));
    ALLOCS.with(|c| c.set(0));
    DEALLOCS.with(|c| c.set(0));

    COUNTING.with(|c| c.set(true));
    for b in 0..TIMED_BLOCKS {
        let block = &views[b % INPUT_BLOCKS];
        let t0 = Instant::now();
        conv.process(block, &mut out);
        let elapsed = t0.elapsed();
        // Keep the optimizer from deciding the output is dead.
        black_box(&out);
        per_block_us.push(elapsed.as_secs_f64() * 1e6);
    }
    COUNTING.with(|c| c.set(false));

    let allocs = ALLOCS.with(Cell::get);
    let deallocs = DEALLOCS.with(Cell::get);

    // "Assert only that it ran" (the timing half).
    assert_eq!(per_block_us.len(), TIMED_BLOCKS);
    assert!(
        per_block_us.iter().all(|us| us.is_finite() && *us > 0.0),
        "the clock produced a non-positive or non-finite block time"
    );

    let mean_us = per_block_us.iter().sum::<f64>() / TIMED_BLOCKS as f64;
    let mut sorted_us = per_block_us;
    sorted_us.sort_by(f64::total_cmp);
    let median_us = percentile(&sorted_us, 0.50);
    let p95_us = percentile(&sorted_us, 0.95);

    let period_us = BLOCK as f64 / RATE_HZ * 1e6;
    let gate_us = 0.25 * period_us;
    let n_fft = (BLOCK + TAPS - 1).next_power_of_two();

    println!();
    println!("convolver bench -- R1-10's deferred gate");
    println!("  geometry ...................... {TAPS} taps / {BLOCK} frames / {CHANNELS} ch @ {RATE_HZ} Hz");
    println!("  n_fft (single-partition) ...... {n_fft}");
    println!("  timed blocks .................. {TIMED_BLOCKS} (after 1 discarded warm-up call)");
    println!("  median ........................ {median_us:8.1} us/block");
    println!("  p95 ........................... {p95_us:8.1} us/block");
    println!(
        "  min / max ..................... {:8.1} / {:.1} us/block",
        sorted_us[0],
        sorted_us[TIMED_BLOCKS - 1]
    );
    println!("  mean .......................... {mean_us:8.1} us/block");
    println!(
        "  median per channel ............ {:8.1} us",
        median_us / CHANNELS as f64
    );
    println!("  block period (512/48000) ...... {period_us:8.1} us");
    println!("  R1-10 gate (25% of period) .... {gate_us:8.1} us");
    println!(
        "  median is {:.2}% of the block period -- {}",
        100.0 * median_us / period_us,
        if median_us > gate_us {
            "OVER the gate: R1-10's uniform partitioning is now justified (see the spec, R1-10 'If it lands')"
        } else {
            "UNDER the gate: leave it alone, R1-10 stays deferred"
        }
    );
    println!("  allocation events in the timed loop: {allocs} alloc / {deallocs} dealloc");
    println!();

    // The half that is NOT machine-dependent, and the reason this bench is a
    // test and not a script: the realtime lane "never allocate[s], never
    // deallocate[s]" (`shared.rs`), and the convolver's own contract promises
    // that from the second `process` call onward.
    assert_eq!(
        (allocs, deallocs),
        (0, 0),
        "OverlapAddConvolver::process allocated after its warm-up call: \
         {allocs} alloc / {deallocs} dealloc over {TIMED_BLOCKS} blocks. \
         The realtime lane must never allocate or deallocate (shared.rs)."
    );
}
