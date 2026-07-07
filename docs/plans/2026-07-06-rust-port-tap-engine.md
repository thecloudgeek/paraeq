# Production Tap Engine (Stage 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the production Core Audio tap engine: `paraeq-coreaudio` grows the full tap/aggregate/IOProc/listener FFI surface (RAII, sound lifetimes), `paraeq-engine` grows the realtime chain, lock-free control plane, silence watchdog, and a backend-agnostic controller with serializable state snapshots. Plus the five stage-2 review carry-forwards recorded in `docs/CONTEXT.md:109-114`.

**Architecture:** Per `docs/specs/2026-07-02-rust-port-design.md` (stage 3 = line 195: "production tap engine, IIR/FIR processors, controller, state snapshots, rebuild-on-change, fail-safe teardown") and the validated spike findings (`docs/spikes/2026-07-tap-spike.md`). Three deliberate design decisions this plan makes concrete:

1. **Single IOProc** (spike-validated, doc line 18): one IOProc on the private (real-output-sub-device + tap) aggregate receives tap input AND writes device output. No main-path ring buffer, no second IOProc, no resampler. The spec's SPSC ring (`rtrb`) is used for **processor swaps** (control → realtime) and retired-config return (realtime → control), not for audio samples.
2. **Dependency direction:** `paraeq-engine` defines the `AudioBackend` trait (the spec's "acquisition behind a trait" seam, spec line 76); `paraeq-coreaudio` depends on `paraeq-engine` and implements `TapBackend`. The engine still depends on `paraeq-dsp` only, never on coreaudio/Tauri — daemon extraction stays "move the crate behind a socket". All lifecycle logic (engage gating, watchdog, rebuild-on-change) lives in the engine controller where it is unit-testable against a `MockBackend`; the eqMac-class lifecycle scenarios become engine tests (spec line 209).
3. **Realtime state sharing** (the anti-spike pattern, spike doc line 30): the realtime side owns its mutable DSP state inside the callback closure; cross-thread communication is `Arc<RtShared>` (atomics only, shared `&`) plus two `rtrb` SPSC rings. **No `&mut` aliased across threads, no `&'static mut` fabricated from callback pointers.**

**Tech Stack:** Rust stable; existing workspace deps `arc-swap`, `rtrb`, `objc2-core-audio` 0.3, `objc2-core-audio-types` 0.3, `objc2(-core)-foundation` 0.3, `libc`, `thiserror`; engine adds `serde` (workspace, derive); new workspace dep `log` 0.4 (facade only — owner prefers structured logging; control plane only, NEVER the realtime lane); coreaudio dev-deps `ctrlc` 3.5 + `env_logger` 0.11 (example binary only).

## Global Constraints

- **Crate boundaries** (spec lines 43-76 + CLAUDE.md): `paraeq-dsp` zero platform deps; `paraeq-engine` depends on `paraeq-dsp` only (no Tauri, no coreaudio); `paraeq-coreaudio` is the ONLY crate with unsafe CoreAudio FFI. **New, deliberate:** `paraeq-coreaudio` depends on `paraeq-engine` (it implements the engine's `AudioBackend` trait). Forbidden deps: ndarray, scirs2-*, fundsp.
- **Realtime lane rules** (spec line 85): no locks, no allocation, no deallocation, no logging, no syscalls on the IOProc path. Processor construction/warm-up/destruction happens on the control plane; the realtime side only moves values through preallocated ring slots. `Vec::resize`/`clear`+`extend` within existing capacity are allowed; anything that can grow capacity is not.
- **Teardown order is the invariant** (spike main.rs:243-251): `AudioDeviceStop` → `AudioDeviceDestroyIOProcID` → `AudioHardwareDestroyAggregateDevice` → `AudioHardwareDestroyProcessTap`, every OSStatus surfaced (log + collect, never silently discarded). Every exit path — command, drop, control-plane panic — runs it (Drop guards). A dead ParaEQ must never leave the system muted; hard-kill recovery is the OS's job (spike-validated, doc line 13). **Wording note:** the spec (line 94) and CLAUDE.md say "destroys the tap *first*" — the *intent* (tap teardown completes on every exit path before anything can wedge, so the system never stays muted) is what this plan enforces; the literal call order within the sequence is the spike-validated one above (tap destruction is the final call). Task 14 updates the spec + CLAUDE.md wording so no future reader "fixes" the validated order.
- **TCC reality** (spike doc lines 9, 26-27): `AudioHardwareCreateProcessTap` succeeds and delivers silent zeros when permission is missing; there is no query API. The engine gates `Running` on **first nonzero input**, tolerates ~5 s of zeros before hinting at the permission ("engage tolerance"), and never hard-fails on silence alone (silence is also just "no music playing"). Dev loop: unsigned CLI binaries never get the TCC prompt — grant manually via System Settings → Privacy & Security → Screen & System Audio Recording → add the terminal, then fully relaunch the terminal.
- **Sample formats** (spec line 91): samples f32 (CoreAudio native); filter state/design math f64.
- **FFI discipline:** only symbols on the verified list below; any new symbol must first be verified against the local registry source (`~/.cargo/registry/src/*/objc2-core-audio-0.3.2/src/generated/`). Every `unsafe` block gets a `// SAFETY:` comment; `crates/paraeq-coreaudio/src/lib.rs` sets `#![warn(clippy::undocumented_unsafe_blocks)]`.
- **Testing split** (spec line 180): engine = synthetic-block unit tests (no hardware); coreaudio = pure unit tests where possible (buffer views, fourcc) + hardware integration tests behind `#[ignore]` (CI has no audio devices; run locally with `cargo test -p paraeq-coreaudio -- --ignored`).
- Alphabetical ordering for imports/module lists/dep lists where order doesn't matter functionally.
- Every commit message ends with the two trailers: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and the `Claude-Session:` link line the harness supplies.
- Gates before every commit: the task's tests green, `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --all` applied.
- Execution on a feature branch/worktree (`git worktree add .worktrees/tap-engine -b feature/rust-port-tap-engine`). All paths relative to the worktree root.

## Verified FFI Reference (Allowed APIs)

Everything the spike already exercised is documented by the spike source itself — `spikes/tap-spike/src/ca.rs` (property getters, tap, aggregate) and `spikes/tap-spike/src/main.rs` (IOProc registration, start/stop, teardown). Symbols verified against `objc2-core-audio` 0.3.2 (ca.rs:2). **New symbols for this stage, verified in the registry source** (`.../objc2-core-audio-0.3.2/src/generated/AudioHardware.rs`, line refs):

- `AudioObjectSetPropertyData(AudioObjectID, NonNull<AudioObjectPropertyAddress>, u32, *const c_void, u32, NonNull<c_void>) -> OSStatus` — AudioHardware.rs:856
- `AudioObjectAddPropertyListener(AudioObjectID, NonNull<AudioObjectPropertyAddress>, AudioObjectPropertyListenerProc, *mut c_void) -> OSStatus` — AudioHardware.rs:886
- `AudioObjectRemovePropertyListener(same shape)` — AudioHardware.rs:914
- `AudioObjectPropertyListenerProc = Option<unsafe extern "C-unwind" fn(AudioObjectID, u32, NonNull<AudioObjectPropertyAddress>, *mut c_void) -> OSStatus>` — AudioHardware.rs:660
- `kAudioDevicePropertyBufferFrameSize` (u32 property) — AudioHardware.rs:1310; `kAudioDevicePropertyBufferFrameSizeRange` — :1312; `kAudioDevicePropertyDeviceIsAlive` — :381; `kAudioDevicePropertyStreamConfiguration` — :1319
- `rtrb 0.3`: `RingBuffer::<T>::new(cap) -> (Producer<T>, Consumer<T>)`, `Producer::push -> Result<(), PushError<T>>`, `Consumer::pop -> Result<T, PopError>`, `Producer::slots`/`Consumer::slots`; both halves are `Send` (registry rtrb-0.3.4/src/lib.rs:138,336,367,540,572,646).

**Anti-patterns (do NOT do):**
- The spike's `&mut EqState` cross-thread alias (main.rs:47 vs 225-237) and `buffers_of` returning `&'static mut` (main.rs:32-36) — spike doc line 30 forbids both.
- Adding the tap to an aggregate **after** creation (delivers zero-filled buffers — ca.rs:172-174); `IsPrivate=true` is required for `TapAutoStart` (ca.rs:218).
- Trusting `AudioHardwareCreateProcessTap`'s OSStatus as a permission check, or gating "running" on `AudioDeviceStart` returning (spike doc lines 26-27).
- `AudioStreamBasicDescription` has no `Default` in objc2-core-audio-types 0.3.2 — explicit zero-init (ca.rs:143-156).
- `AudioHardwareCreateAggregateDevice` takes the *untyped* `&CFDictionary` — downcast via `dict.as_ref()` (ca.rs:227-230).
- Per-sample output-layout probing (spike write_sample, main.rs:106-130) — resolve layout once per callback.
- Don't port `spikes/tap-spike/src/biquad.rs` — production uses `paraeq-dsp`.

## File Structure (end state)

```
crates/paraeq-dsp/src/fr.rs              # modified: Result-returning entry points (Task 3)
crates/paraeq-dsp/src/targets.rs         # modified: parse-time validation (Task 3)
crates/paraeq-dsp/tests/test_fr.rs       # modified for new signatures
crates/paraeq-dsp/tests/test_targets.rs  # modified: rejection tests
crates/paraeq-dsp/tests/test_props.rs    # modified: shelf/notch stability (Task 2)
crates/paraeq-dsp/DIVERGENCES.md         # modified: new entries (Task 3)
crates/paraeq-engine/src/
├── lib.rs            # + pub mod backend; chain; controller; shared; status; EngineError
├── backend.rs        # AudioBackend trait, StreamInfo, BackendEvent          (Task 7)
├── chain.rs          # Correction, RealtimeChain, build_correction           (Task 4)
├── controller.rs     # EngineController, EngineHandle, EngineCommand,
│                     # CorrectionConfig, EngineState                         (Task 7)
├── convolver.rs      # modified: unified output contract                     (Task 1)
├── iir.rs            # modified: unified output contract                     (Task 1)
├── shared.rs         # RtShared, links() rings, RtProcessor                  (Task 5)
└── status.rs         # Watchdog, WatchdogConfig, EngineStatus                (Task 6)
crates/paraeq-engine/tests/
├── common/mod.rs     # (exists; gains `pub mod mock_backend;`)
├── common/mock_backend.rs  # shared MockBackend for controller tests        (Task 7)
├── test_chain.rs  test_controller.rs  test_convolver.rs (mod)
├── test_iir.rs (mod)  test_shared.rs  test_status.rs
crates/paraeq-coreaudio/src/
├── lib.rs            # pub mod backend; error; ioproc; listeners; properties; tap;
├── backend.rs        # TapBackend: AudioBackend impl                         (Task 12)
├── error.rs          # CaError + check()                                     (Task 8)
├── ioproc.rs         # IoProcHandle + callback-scoped buffer views           (Task 10)
├── listeners.rs      # PropertyListener RAII → mpsc events                   (Task 11)
├── properties.rs     # device/tap property getters + buffer-frame-size       (Task 8)
└── tap.rs            # create_tap, create_aggregate, TapSystem RAII          (Task 9)
crates/paraeq-coreaudio/tests/
├── test_buffers.rs   # pure AudioBufferList view tests (no HAL)              (Task 10)
└── test_hardware.rs  # #[ignore] hardware suite                              (Tasks 8-12)
crates/paraeq-coreaudio/examples/tap_engine.rs                                (Task 13)
```

Reference sources (read them; they are the spec for each pattern): `spikes/tap-spike/src/ca.rs` + `main.rs` (validated FFI), `docs/spikes/2026-07-tap-spike.md` (obligations), `crates/paraeq-engine/src/{convolver,iir}.rs` (existing processors), `docs/CONTEXT.md:109-114` (carry-forwards).

---

### Task 1: Unify the engine processors' output-buffer contracts (carry-forward)

**Files:**
- Modify: `crates/paraeq-engine/src/convolver.rs`, `crates/paraeq-engine/src/iir.rs`
- Modify: `crates/paraeq-engine/tests/test_convolver.rs`, `crates/paraeq-engine/tests/test_iir.rs`

**Interfaces:**
- The unified contract (goes verbatim into both docs): *"`output[ch].len()` must equal `input[ch].len()` on entry (asserted); contents are fully overwritten; the call never allocates."* Length-assert instead of clear+extend means a misuse is a loud panic, never a silent realtime allocation.
- Convolver additionally keeps its existing `block.len() == block_size` assert.
- Consumed by: Task 4 (`RealtimeChain` maintains output lengths via `resize` within capacity).

- [ ] **Step 1: Failing tests.** Append to `tests/test_iir.rs`:

```rust
#[test]
#[should_panic(expected = "output[ch] length")]
fn iir_rejects_mis_sized_output() {
    let mut p = IIRProcessor::new();
    p.set_sos(0, vec![[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]]);
    let input = [1.0f64; 64];
    let mut out = vec![Vec::new()]; // wrong: empty, old contract allowed this
    p.process(&[&input], &mut out);
}
```

And to `tests/test_convolver.rs`:

```rust
#[test]
#[should_panic(expected = "output[ch] length")]
fn convolver_rejects_mis_sized_output() {
    let mut c = OverlapAddConvolver::new(vec![vec![1.0]], 64);
    let input = [0.0f64; 64];
    let mut out = vec![vec![0.0; 32]]; // wrong: shorter than block_size
    c.process(&[&input], &mut out);
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p paraeq-engine` — the IIR test FAILS (no panic today: clear+extend accepts an empty Vec); the convolver test panics for the wrong reason (slice-index) — the `expected` substring won't match, so it fails too.

- [ ] **Step 3: Implement.** In `iir.rs` replace `out.clear(); out.extend_from_slice(block);` (iir.rs:44-45) with:

```rust
assert_eq!(
    out.len(),
    block.len(),
    "output[ch] length must equal input[ch] length (fully overwritten)"
);
out.copy_from_slice(block);
```

In `convolver.rs`, next to the existing block-size assert (convolver.rs:98), add:

```rust
assert_eq!(
    output[ch].len(),
    self.block_size,
    "output[ch] length must equal input[ch] length (fully overwritten)"
);
```

Update BOTH doc comments to the unified contract wording. Fix any existing tests that relied on clear+extend (pre-size with `vec![0.0; block]`).

- [ ] **Step 4: Run to verify pass.** `cargo test -p paraeq-engine` — all green, including the two fixture suites (behavior for correctly-sized callers is unchanged).

- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "refactor(engine): unify processor output contracts (length assert, no hidden allocation)"
```

---

### Task 2: Stability proptests for low_shelf / high_shelf / notch (carry-forward)

**Files:**
- Modify: `crates/paraeq-dsp/tests/test_props.rs`

**Interfaces:**
- Consumes `paraeq_dsp::biquad::{low_shelf, high_shelf, notch}` — NOTE `notch(fc, q, sample_rate)` has NO gain parameter (biquad.rs:56).
- Same pole criterion as the existing `peaking_is_stable` (test_props.rs:8-15): `|a2| < 1 && |a1| < 1 + a2` with the same epsilons.

- [ ] **Step 1: Write the three proptests** inside the existing `proptest!` block, mirroring `peaking_is_stable` strategies (`fc in 20.0..20000.0, gain in -24.0..24.0, q in 0.1..20.0`, sr 48 kHz; notch omits gain):

```rust
/// Designed shelf/notch filters are stable: poles inside the unit circle.
#[test]
fn low_shelf_is_stable(fc in 20.0f64..20000.0, gain in -24.0f64..24.0, q in 0.1f64..20.0) {
    let sos = biquad::low_shelf(fc, gain, q, 48000.0);
    let (a1, a2) = (sos[4], sos[5]);
    prop_assert!(a2.abs() < 1.0 + 1e-12);
    prop_assert!(a1.abs() < 1.0 + a2 + 1e-9);
}
```

(likewise `high_shelf_is_stable`; `notch_is_stable(fc, q)` calling `biquad::notch(fc, q, 48000.0)`).

- [ ] **Step 2: Run.** `cargo test -p paraeq-dsp --test test_props`. Expected: pass immediately (RBJ designs are stable by construction). **If proptest finds a counterexample, do not widen tolerances** — pin the failing (fc, gain, q) against the Python oracle (`prototype/paraeq/correction/biquad.py` + scipy pole check) first; a real instability is a bug report, not a test problem.

- [ ] **Step 3: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "test(dsp): extend filter-stability proptests to low_shelf/high_shelf/notch"
```

---

### Task 3: Input-wiring hardening in paraeq-dsp (carry-forward)

**Files:**
- Modify: `crates/paraeq-dsp/src/fr.rs`, `crates/paraeq-dsp/src/targets.rs`
- Modify: `crates/paraeq-dsp/tests/test_fr.rs`, `crates/paraeq-dsp/tests/test_targets.rs`, `crates/paraeq-dsp/DIVERGENCES.md`

**Interfaces:**
- `pub fn compute_frequency_response(...) -> Result<(Vec<f64>, Vec<f64>), DspError>` — `Err(InvalidInput)` when effective `n == 0` (covers `Some(0)` and empty `ir` with `None`); previously panicked at fr.rs:18.
- `pub fn average_measurements(...) -> Result<Vec<f64>, DspError>` — `Err(InvalidInput)` on empty slice (previously index-panic at fr.rs:57) and on mismatched inner lengths (previously silently zipped short; numpy would raise on ragged input, so erroring matches oracle *conditions*).
- `parse_target_csv` keeps its signature but now rejects: fewer than 2 data rows, and non-strictly-increasing frequencies — `Err(DspError::Parse)`. This removes the parse-accepts-then-`interpolate`-panics trap (targets.rs:29-30). `TargetCurve::interpolate`'s `.expect` stays: after this task it is unreachable for parsed curves; hand-constructed `TargetCurve`s panicking there is a programmer-error contract, documented.
- Callers today are tests only (stage 4 wiring lands later) — update them; no engine impact.

- [ ] **Step 1: Failing tests.** `tests/test_fr.rs` additions:

```rust
#[test]
fn average_of_nothing_is_an_error() {
    assert!(fr::average_measurements(&[]).is_err());
}

#[test]
fn ragged_average_is_an_error() {
    assert!(fr::average_measurements(&[vec![0.0; 4], vec![0.0; 3]]).is_err());
}

#[test]
fn zero_length_fft_is_an_error() {
    assert!(fr::compute_frequency_response(&[], 48000, None).is_err());
    assert!(fr::compute_frequency_response(&[1.0], 48000, Some(0)).is_err());
}
```

`tests/test_targets.rs` additions:

```rust
#[test]
fn non_monotonic_target_csv_is_a_parse_error() {
    assert!(targets::parse_target_csv("100,0.0\n50,1.0\n", "bad").is_err());
    assert!(targets::parse_target_csv("100,0.0\n100,1.0\n", "dup").is_err());
}

#[test]
fn single_row_target_csv_is_a_parse_error() {
    assert!(targets::parse_target_csv("100,0.0\n", "one").is_err());
}
```

- [ ] **Step 2: Run to verify failure.** The fr tests fail to compile (`Result` not returned yet) or panic; the targets tests fail (currently `Ok`).

- [ ] **Step 3: Implement.** Guards at the top of each function returning `DspError::InvalidInput`/`Parse` with actionable messages; thread `Result` through the existing fixture tests (`.unwrap()` at call sites — golden values unchanged). In `targets.rs`, after the parse loop add the `< 2 rows` check and `frequencies.windows(2).any(|w| w[1] <= w[0])` check.

- [ ] **Step 4: DIVERGENCES.md entries** (append, keeping the numbering):

```markdown
12. **`parse_target_csv` validates at parse time.** The Python loader accepts
    single-row / non-monotonic CSVs and only fails later inside
    scipy CubicSpline; Rust rejects them at parse with `DspError::Parse`.
    Same inputs fail in both worlds — different layer and mechanism.
13. **`fr::compute_frequency_response` and `fr::average_measurements` return
    `Result`.** The Python equivalents raise (ValueError / IndexError) on
    n_fft=0, empty input, or ragged measurement lists.
```

- [ ] **Step 5: Run to verify pass.** `cargo test -p paraeq-dsp` — all green including untouched golden values.

- [ ] **Step 6: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-dsp
git commit -m "feat(dsp): validate wiring-layer inputs (fr Results, target CSV monotonicity) + divergence log"
```

---

### Task 4: engine::chain — Correction + RealtimeChain

**Files:**
- Create: `crates/paraeq-engine/src/chain.rs`, `crates/paraeq-engine/tests/test_chain.rs`
- Modify: `crates/paraeq-engine/src/lib.rs` (`pub mod chain;` + `EngineError`)

**Interfaces:**
- Produces:
  - `pub enum Correction { Fir(OverlapAddConvolver), Iir(IIRProcessor) }`
  - `pub struct RealtimeChain` with:
    - `pub fn new(channels: usize, block_size: usize) -> RealtimeChain` — 1 ≤ channels ≤ 2 asserted (the parity tap is stereo; spike confirmed 2 ch); preallocates f64 scratch (channels × block_size)
    - `pub fn set_correction(&mut self, c: Option<Correction>) -> Option<Correction>` — plain move, rt-safe (used by Task 5's ring poll)
    - `pub fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], bypass: bool, gain: f32) -> ChainOutcome` — per spec line 83: `bypass? → correction → trim gain → safety clamp(±1.0)`
  - `pub struct ChainOutcome { pub corrected: bool, pub frame_mismatch: bool }`
  - `pub fn build_correction(config: &CorrectionConfig, channels: usize, block_size: usize) -> Correction` — control-plane-only constructor: builds the processor AND warms it up (one silent `process` call at the real channel count — satisfies both processors' warm-up contracts, convolver.rs:14-20 / iir.rs:32-35), then `reset()`s it. (`CorrectionConfig` lands in Task 7; until then take the enum by fields — see Step 3.)
- `EngineError` in lib.rs: `#[derive(Debug, thiserror::Error)] pub enum EngineError { #[error("backend: {0}")] Backend(String), #[error("invalid config: {0}")] InvalidConfig(String) }`.
- Semantics that MUST hold (each is a test):
  - Bypass skips correction only; gain and clamp still apply (gain is the user's volume trim).
  - On the bypassed→active edge, correction state is `reset()` (no stale-transient thump). Track `prev_bypass` internally.
  - **Uniform frame guard — the chain NEVER panics on a HAL-supplied frame count.** Correction applies only when the frame count fits: `Fir` requires exactly `block_size` frames (OverlapAddConvolver contract); `Iir` accepts any `frames <= block_size` (chain `resize`s its f64 scratch within capacity). Any other frame count → pass through (with gain+clamp, pure f32 path, no scratch needed), set `frame_mismatch: true`. No `assert!` on data-dependent HAL values anywhere on the rt path — the HAL occasionally resizes; persistent changes arrive as FormatChanged events and trigger a rebuild.
  - Zero allocation in `process` (scratch preallocated in `new`; `&[&[f64]]` views built as a stack array `[&[f64]; 2]` sliced to `channels` — never a `Vec` of refs).

- [ ] **Step 1: Failing tests** (`tests/test_chain.rs`), synthetic blocks:
  - `no_correction_is_identity_with_gain_and_clamp`: input 0.5, gain 2.0 → output 1.0 (clamped from 1.0 exactly), input 0.9 gain 2.0 → 1.0 (clamped), gain 0.5 → 0.25.
  - `iir_path_matches_direct_processor_across_blocks`: 3 blocks through chain-with-Iir vs a hand-driven `IIRProcessor` (f32→f64→f32 with same casts) — bitwise equal.
  - `fir_path_matches_direct_convolver`: same shape with a short FIR.
  - `bypass_passes_through_and_edge_resets_state`: loud block → bypass on (output == gain·input) → bypass off → next block equals a FRESH processor's output (state was reset).
  - `fir_frame_mismatch_passes_through`: Fir correction, feed block_size/2 frames → `frame_mismatch == true`, output == gain·input.
  - `oversized_frames_pass_through_for_any_correction`: Iir correction, feed 2×block_size frames → no panic, `frame_mismatch == true`, output == gain·input (clamped).
- [ ] **Step 2: Run to verify failure** — module not found.
- [ ] **Step 3: Implement `chain.rs`.** Notes: split borrows before building input views (`let Self { correction, in_f64, out_f64, .. } = self;`); `build_correction` initially exposed as two fns `build_fir(firs, channels, block_size)` / `build_iir(sos_per_channel, ...)` — Task 7 wraps them behind `CorrectionConfig`. Casting: `f32 as f64` up, `y as f32` down, clamp with `f32::clamp(-1.0, 1.0)` AFTER gain.
- [ ] **Step 4: Run to verify pass.** `cargo test -p paraeq-engine --test test_chain`.
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "feat(engine): realtime chain (bypass -> correction -> gain -> clamp) with FIR frame guard"
```

---

### Task 5: engine::shared — RtShared atomics, swap/retire rings, RtProcessor

**Files:**
- Create: `crates/paraeq-engine/src/shared.rs`, `crates/paraeq-engine/tests/test_shared.rs`
- Modify: `crates/paraeq-engine/src/lib.rs` (`pub mod shared;`), `crates/paraeq-engine/Cargo.toml` (nothing new — `rtrb` already declared)

**Interfaces:**
- Produces:
  - `pub struct RtShared` — all-atomic telemetry + params (the spike's proven pattern minus the aliasing bug, spike main.rs:17-27): `bypass: AtomicBool`, `gain_bits: AtomicU32` (f32 bits, linear), `callbacks: AtomicU64`, `peak_in_bits: AtomicU32`, `zero_blocks: AtomicU64`, `nonzero_blocks: AtomicU64`, `sample_time_delta_bits: AtomicU64` (f64 bits), `frame_mismatch_blocks: AtomicU64`, `skipped_blocks: AtomicU64` (backend-level oversize skips, Task 12). Helpers: `gain()/set_gain(f32)`, `sample_time_delta()/store_sample_time_delta(f64)`, `Default` = gain 1.0, bypass false. All loads/stores `Ordering::Relaxed` (telemetry + single-writer params; no cross-variable invariants).
  - `pub enum RtMsg { Correction(Option<Correction>) }` and `pub struct Retired(pub Option<Correction>)` — moved by value through the rings; the enum is shallow (Vec pointers), so a slot move is a small memcpy, zero allocation.
  - `pub fn links(capacity: usize) -> (ControlLink, RtLink)` — two `rtrb` rings: control→rt (`RtMsg`) and rt→control (`Retired`).
  - `impl ControlLink`: `pub fn send(&mut self, msg: RtMsg) -> Result<(), EngineError>` (ring-full = error, controller retries next tick), `pub fn drain_retired(&mut self)` (drops retired configs on the control thread — spec line 85's "retired configs are freed off-thread").
  - `impl RtLink`: `pub fn poll(&mut self, chain: &mut RealtimeChain)` — **swap is gated on retire-ring space**: if `retire.slots() == 0`, defer (leave the message queued) so the realtime side can never be forced to drop (deallocate) a processor.
  - `pub struct RtProcessor` — the one realtime entry point every backend drives: owns `Arc<RtShared>` + `RtLink` + `RealtimeChain`; `pub fn process_block(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], out_in_sample_delta: f64)` does, in order: counters (callbacks += 1, delta store) → input scan (peak, all-zero → zero/nonzero_blocks — the watchdog's raw signal) → `link.poll(chain)` → read bypass/gain → `chain.process` → `frame_mismatch_blocks` bump if flagged. Also `pub fn shared(&self) -> Arc<RtShared>`.
- Consumed by: Task 7 (controller builds it), Task 12 (TapBackend drives it).

- [ ] **Step 1: Failing tests** (`tests/test_shared.rs`):
  - `swap_under_churn_is_clean`: spawn a control thread pushing 200 alternating Iir/None corrections through `ControlLink` while the test thread pumps 10_000 synthetic blocks through `RtProcessor::process_block`, polling each block; control side drains retired continuously. Assert: never panics, all output samples finite, and total retired received == total swaps accepted (nothing leaked or dropped on the rt side).
  - `swap_defers_when_retire_ring_full` (capacity-1 links; note `send` errors when the control→rt ring is full, so the sends MUST interleave with pumps): send #1 → pump (swap #1 lands, retire ring now 1/1 full) → send #2 (fits, control ring was drained by the pump) → pump (swap #2 DEFERRED — output still shows correction #1) → `drain_retired()` → pump (swap #2 lands, observable via output).
  - `zero_and_nonzero_blocks_counted`: silent block bumps `zero_blocks`, signal block bumps `nonzero_blocks`, `peak_in_bits` tracks max.
- [ ] **Step 2: Run to verify failure** — module not found.
- [ ] **Step 3: Implement `shared.rs`.**
- [ ] **Step 4: Run to verify pass.**
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "feat(engine): lock-free rt state (atomics + swap/retire rings) and RtProcessor entry point"
```

---

### Task 6: engine::status — silence watchdog state machine

**Files:**
- Create: `crates/paraeq-engine/src/status.rs`, `crates/paraeq-engine/tests/test_status.rs`
- Modify: `crates/paraeq-engine/src/lib.rs` (`pub mod status;`), `crates/paraeq-engine/Cargo.toml` (add `serde = { workspace = true }`)

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, PartialEq, serde::Serialize)] pub enum EngineStatus { Stopped, Starting { since_ms: u64 }, NoInputDetected { since_ms: u64 }, Running, InputSilent { since_ms: u64 }, Stalled { since_ms: u64 }, Failed { reason: String } }`
  - `pub struct WatchdogConfig { pub engage_tolerance_ms: u64 /* 5000 */, pub silence_window_ms: u64 /* 3000 */, pub stall_window_ms: u64 /* 2000 */ }` with `Default`.
  - `pub struct Watchdog` (pure — **no `Instant::now` inside**; the caller supplies `now_ms`): `pub fn new(config: WatchdogConfig) -> Watchdog`, `pub fn started(&mut self, now_ms: u64)`, `pub fn stopped(&mut self)`, `pub fn observe(&mut self, now_ms: u64, callbacks: u64, nonzero_blocks: u64) -> &EngineStatus`.
- Transition semantics (spike obligations, doc lines 26-27 — each is a test):
  - `started()` → `Starting`. First observed increase in `nonzero_blocks` → `Running` (**gate on first nonzero input, never on start returning**).
  - `Starting` beyond `engage_tolerance_ms` with callbacks flowing but zero nonzero → `NoInputDetected` (this is the "check System Audio Recording permission **or** play some audio" UI hint — TCC failure is *indistinguishable* from silence, so this state is informational and never auto-fails).
  - `Running` with no `nonzero_blocks` increase for `silence_window_ms` → `InputSilent`; any nonzero increase → back to `Running`. From `NoInputDetected`, nonzero → `Running` too.
  - **Stall detection is gated on the first observed `callbacks` increase since `started()`.** The spike measured up to ~4 s with NO callbacks at all before the tap engages on a healthy first launch (doc line 27) — before the first callback, the ONLY slow-start transition is `Starting → NoInputDetected` at `engage_tolerance_ms` (informational, no rebuild). AFTER callbacks have been seen at least once, no `callbacks` increase for `stall_window_ms` → `Stalled` (the IOProc died; the controller reacts — Task 7). This prevents a rebuild loop on a healthy slow engage.
  - `Failed` is only ever set by the controller (backend errors), not by the watchdog.
- Consumed by: Task 7.

- [ ] **Step 1: Failing tests** — drive `observe` with synthetic timelines: engage at 4 s stays `Starting`→`Running` (within tolerance); zeros past 5 s → `NoInputDetected`; recovery; running→silence→running; **callbacks frozen at 0 for 4 s after `started()` stays `Starting`, NOT `Stalled`** (slow-engage gate); **callbacks frozen at 0 past `engage_tolerance_ms` → `NoInputDetected`, still not `Stalled`**; callbacks flowing then frozen for `stall_window_ms` → `Stalled`; `stopped()` → `Stopped` from anywhere. Use ms integers, no sleeping, no real clock.
- [ ] **Step 2: Verify failure**, **Step 3: implement**, **Step 4: verify pass** (`cargo test -p paraeq-engine --test test_status`).
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "feat(engine): silence watchdog state machine (first-nonzero gating, 5s engage tolerance, stall detect)"
```

---

### Task 7: engine::backend trait + controller with state snapshots

**Files:**
- Create: `crates/paraeq-engine/src/backend.rs`, `crates/paraeq-engine/src/controller.rs`, `crates/paraeq-engine/tests/common/mock_backend.rs`, `crates/paraeq-engine/tests/test_controller.rs`
- Modify: `crates/paraeq-engine/src/lib.rs` (`pub mod backend; pub mod controller;`), `crates/paraeq-engine/tests/common/mod.rs` (add `pub mod mock_backend;` — the `common/` directory convention keeps cargo from compiling the mock as its own zero-test integration target, which would fail the dead_code clippy gate; `test_controller.rs` reaches it via `mod common;` + `use common::mock_backend::MockBackend;`)

**Interfaces:**
- `backend.rs` produces:
  - `#[derive(Clone, Debug, serde::Serialize)] pub struct StreamInfo { pub sample_rate: f64, pub channels: usize, pub buffer_frames: usize, pub device_uid: String }`
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub enum BackendEvent { DefaultOutputChanged, DeviceDied, FormatChanged }`
  - `pub trait AudioBackend: Send { fn start(&mut self, proc: RtProcessor, requested_buffer_frames: Option<usize>) -> Result<StreamInfo, EngineError>; fn stop(&mut self) -> Result<(), EngineError>; fn poll_event(&mut self) -> Option<BackendEvent>; }` — `start` moves the `RtProcessor` into the backend's realtime context; `stop` runs the backend's full teardown and must be idempotent.
- `controller.rs` produces:
  - `#[derive(Clone, Debug, serde::Serialize)] pub enum CorrectionConfig { Fir { firs: Vec<Vec<f64>> }, Iir { sos_per_channel: Vec<Vec<[f64; 6]>> } }` (+ `build_correction(&CorrectionConfig, channels, block_size)` now wraps Task 4's builders)
  - `pub enum EngineCommand { ClearCorrection, Disable, Enable, SetBufferFrames(usize), SetBypass(bool), SetCorrection(CorrectionConfig), SetGainDb(f32), Shutdown }`
  - `#[derive(Clone, Debug, serde::Serialize)] pub struct EngineState { pub status: EngineStatus, pub bypass: bool, pub gain_db: f32, pub correction: Option<String> /* short descriptor e.g. "iir:5-band" */, pub stream: Option<StreamInfo>, pub latency_ms: Option<f64>, pub input_peak: f32 }` — the spec's "one serialized snapshot pushed on every change" (spec line 135), stage 4 serializes it over Tauri.
  - `pub struct EngineConfig { pub watchdog: WatchdogConfig, pub tick_ms: u64 /* 250 */, pub ring_capacity: usize /* 4 */, pub requested_buffer_frames: Option<usize> }` with `Default`.
  - `pub struct EngineHandle` — `pub fn spawn<B: AudioBackend + 'static>(backend: B, config: EngineConfig) -> EngineHandle`; `pub fn send(&self, cmd: EngineCommand)`; `pub fn state(&self) -> Arc<EngineState>` (via `arc_swap::ArcSwap` — latest snapshot, lock-free reads); `pub fn subscribe(&self) -> std::sync::mpsc::Receiver<Arc<EngineState>>` (every published change); `Drop` sends `Shutdown` and joins.
- Controller loop semantics (each is a MockBackend test):
  - **Start:** build `RtShared` + links + chain (the backend needs the `RtProcessor` up front, but `StreamInfo` only comes back from `start`. Resolution: the controller builds the chain provisionally for 2 channels (the parity tap is stereo — spike doc:15) and `block_size = requested_buffer_frames.unwrap_or(512)`; after `start` returns, **if `StreamInfo.buffer_frames` OR `StreamInfo.channels` differs from the chain's build parameters, renegotiate: one stop + start cycle with the corrected sizes**. Document this loudly; MockBackend tests cover both renegotiation triggers.)
  - Commands: `SetBypass`/`SetGainDb` → atomic store + publish (dB→linear `10f64.powf(db/20.0) as f32`). `SetCorrection` → `build_correction` (alloc + warm-up HERE, control plane) → `ControlLink::send`; on ring-full retry next tick (keep pending). Controller retains the last `CorrectionConfig` as source of truth. `ClearCorrection` → send `RtMsg::Correction(None)` + drop the stored config. `Enable` → start from the stored config (idempotent no-op when already running). `SetBufferFrames(n)` → update the stored request + one stop/start renegotiation cycle when running (just the stored request when stopped).
  - Tick (`recv_timeout(tick_ms)`): read `RtShared` counters → `watchdog.observe(now_ms, ...)` → publish on change; `drain_retired()`; `poll_event()`.
  - **Rebuild-on-change** (spec line 93): on any `BackendEvent` or watchdog `Stalled`: `stop()` → rebuild fresh links/chain/corrections from stored config → `start()` → watchdog `started()`. On repeated immediate failure (2 consecutive), publish `Failed` and stay stopped (don't rebuild-loop a dead device).
  - **Disable/panic safety** (spec lines 92/94): `Disable` → `stop()` + status `Stopped` (full disable: tap destroyed, device unmuted — "system exactly as if ParaEQ never ran"). The controller thread wraps its loop so ANY exit path (Shutdown, panic unwinding, error) runs `backend.stop()` — implement as a `struct StopGuard<'a, B>(…)` with `Drop`.
  - Latency: `latency_ms = shared.sample_time_delta() / stream.sample_rate * 1000.0` in every snapshot.
- `common/mock_backend.rs` (test support): implements `AudioBackend`; **`#[derive(Clone)]` with ALL state behind `Arc<...>`** — `spawn` consumes the backend by value, so tests construct one, keep a clone, and pass the other in; assertions and helpers run on the kept clone. Holds the `RtProcessor` in `Arc<Mutex<Option<RtProcessor>>>` (test-only lock, fine); test helper `pump(&self, frames: usize, amplitude: f32)` invokes `process_block` synchronously; scriptable `queue_event(BackendEvent)`; records call sequence (`start`/`stop` order, `requested_buffer_frames`), configurable `StreamInfo` and start-failure injection.

- [ ] **Step 1: Failing tests** (`tests/test_controller.rs`, shortened watchdog windows e.g. engage 200 ms / tick 20 ms so the suite stays fast):
  - `engages_on_first_nonzero_input`: spawn; pump silence → status stays `Starting` then `NoInputDetected`; pump signal → snapshot becomes `Running`.
  - `bypass_and_gain_commands_reach_the_rt_side`: send commands; pump; assert output amplitudes.
  - `set_correction_swaps_and_retires_off_thread`: send an Iir config; pump; corrected output; send another; retired drained (no growth).
  - `backend_event_triggers_stop_start_rebuild`: queue `DefaultOutputChanged`; assert stop-then-start call order and corrections re-applied after rebuild.
  - `stall_triggers_rebuild_then_failed`: stop pumping (callbacks frozen) with start-failure injection on the 2nd rebuild → `Failed`.
  - `disable_stops_backend_and_drop_is_clean`: `Disable` → backend stopped; drop handle → `stop` called exactly once more at most (idempotent), thread joined.
  - `buffer_frames_renegotiation`: MockBackend reports effective 256 ≠ requested 128 → one stop+start cycle, final chain block_size 256 (observable: FIR at 256 works, no frame_mismatch).
  - `channel_count_renegotiation`: MockBackend reports a mono `StreamInfo` (channels 1) → one stop+start cycle, final chain runs 1-channel (pump 1-channel blocks, corrected output, no panic).
  - `slow_first_callback_does_not_rebuild`: don't pump at all for > stall_window after spawn (callbacks frozen at 0) → NO rebuild happens (`start` called exactly once), status reaches `NoInputDetected` not `Stalled`.
  - `disable_enable_roundtrip`: `Disable` → backend stopped; `Enable` → started again with the stored correction re-applied (pump shows corrected output).
- [ ] **Step 2: Verify failure**, **Step 3: implement `backend.rs` + `controller.rs`**, **Step 4: verify pass** (`cargo test -p paraeq-engine`).
- [ ] **Step 5: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-engine
git commit -m "feat(engine): AudioBackend seam + controller (commands, snapshots, rebuild-on-change, fail-safe stop)"
```

---

### Task 8: coreaudio::error + properties

**Files:**
- Create: `crates/paraeq-coreaudio/src/error.rs`, `crates/paraeq-coreaudio/src/properties.rs`, `crates/paraeq-coreaudio/tests/test_hardware.rs`
- Modify: `crates/paraeq-coreaudio/src/lib.rs` (`#![warn(clippy::undocumented_unsafe_blocks)]`, `pub mod error; pub mod properties;`)

**Interfaces:**
- `error.rs`: `#[derive(Debug, thiserror::Error)] #[error("{ctx}: OSStatus {status} ('{fourcc}')")] pub struct CaError { pub status: i32, pub fourcc: String, pub ctx: String }`; `pub fn check(status: i32, ctx: &str) -> Result<(), CaError>` — port of spike `ca.rs:35-45` (fourcc decode) with the typed error instead of `String`.
- `properties.rs` — port the spike getters VERBATIM apart from error type (they are validated code; cites are the spec):
  - `pub fn default_output_device() -> Result<AudioObjectID, CaError>` — ca.rs:47-62
  - `pub fn device_uid(dev: AudioObjectID) -> Result<String, CaError>` — ca.rs:64-82 (+1 retained CFStringRef gotcha; convert to `String` at the boundary)
  - `pub fn nominal_sample_rate(dev: AudioObjectID) -> Result<f64, CaError>` — ca.rs:84-99
  - `pub fn translate_pid(pid: i32) -> Result<AudioObjectID, CaError>` — ca.rs:102-118
  - `pub fn tap_format(tap: AudioObjectID) -> Result<AudioStreamBasicDescription, CaError>` — ca.rs:142-170 (explicit zero-init, no Default)
  - NEW: `pub fn buffer_frame_size(dev: AudioObjectID) -> Result<u32, CaError>` (Get, `kAudioDevicePropertyBufferFrameSize`) and `pub fn set_buffer_frame_size(dev: AudioObjectID, frames: u32) -> Result<(), CaError>` via `AudioObjectSetPropertyData` (signature in the FFI reference above). Verify `AudioValueRange` in objc2-core-audio-types before using `BufferFrameSizeRange`; if absent, skip the range getter (set + read-back is sufficient).
- Every `unsafe` block gets `// SAFETY:`.

- [ ] **Step 1: Unit tests first** (pure, in `error.rs` `#[cfg(test)]`): fourcc decode of `0x216f626a` → `"!obj"`, of small negative ints → `'?'` padding; `check(0, _)` is `Ok`.
- [ ] **Step 2: Hardware tests** (`tests/test_hardware.rs`, ALL `#[ignore = "requires audio hardware + TCC grant"]`): `default_output_device()` returns nonzero id; `device_uid` non-empty; `nominal_sample_rate > 0`; `buffer_frame_size` read/set/read-back cycle on the default device restoring the original value afterward.
- [ ] **Step 3: Implement**, run `cargo test -p paraeq-coreaudio` (unit green, ignored skipped) AND `cargo test -p paraeq-coreaudio -- --ignored` locally (needs the TCC-granted terminal; if the grant is missing these fail loudly — that's fine, note it in the task report).
- [ ] **Step 4: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-coreaudio
git commit -m "feat(coreaudio): typed OSStatus errors + device/tap property helpers (incl. buffer frame size)"
```

---

### Task 9: coreaudio::tap — TapSystem RAII (tap + aggregate lifecycle)

**Files:**
- Create: `crates/paraeq-coreaudio/src/tap.rs`
- Modify: `crates/paraeq-coreaudio/src/lib.rs` (`pub mod tap;`), `crates/paraeq-coreaudio/tests/test_hardware.rs`
- Modify: `Cargo.toml` (root: add `log = "0.4"` to `[workspace.dependencies]` — first use is this task's `log::warn!`/`log::error!`), `crates/paraeq-coreaudio/Cargo.toml` (add `log = { workspace = true }`)

**Interfaces:**
- Port from spike (validated): `create_tap(excluded) -> Result<(AudioObjectID, Retained<CATapDescription>), CaError>` — ca.rs:122-140 (stereo global tap excluding processes, MutedWhenTapped, private; name `"paraeq-engine"`); `create_aggregate(out_uid, tap_uuid) -> Result<AudioObjectID, CaError>` — ca.rs:175-233 verbatim including the iqualize composition rules (tap in the CREATION dict; IsPrivate=true; TapAutoStart=true; drift compensation on the sub-tap) and the untyped-CFDictionary downcast; agg UID `"com.paraeq.engine.<pid>"`.
- Produces `pub struct TapSystem { pub tap: AudioObjectID, pub aggregate: AudioObjectID, pub format: AudioStreamBasicDescription, pub device: AudioObjectID, pub device_uid: String, /* private: */ desc, torn_down: bool }`:
  - `pub fn create() -> Result<TapSystem, CaError>` — the spike's exact setup order (main.rs:163-182): default output → uid → rate → translate own pid → tap → format → aggregate. **Self-exclusion strategy** (spike gap, main.rs:170-174): if `translate_pid` returns 0, sleep 200 ms and retry once; if still 0, proceed with empty exclusion and `log::warn!` (feedback risk is spike-documented, non-fatal).
  - `pub fn teardown(&mut self) -> Vec<CaError>` — idempotent; `AudioHardwareDestroyAggregateDevice` then `AudioHardwareDestroyProcessTap` (the tail of the invariant order; the IOProc stop/destroy head belongs to the layer above — Task 12 composes them in full order); every failure collected AND `log::error!`ed, never discarded (spike teardown_step pattern, main.rs:159-161).
  - `impl Drop` — runs `teardown` if not already torn down.
- **No IOProc here** — this task is pure lifecycle; a `TapSystem` alone captures nothing.

- [ ] **Step 1: Hardware test first** (`#[ignore]`): `tap_system_create_teardown_roundtrip` — create; assert `format.mSampleRate > 0.0` and `mChannelsPerFrame >= 1`; explicit `teardown()` returns empty error vec; a second `teardown()` is a no-op; then a create→drop cycle. (Requires TCC grant; without it creation still succeeds — the silent-zeros mode — so this test validates lifecycle, not capture.)
- [ ] **Step 2: Implement `tap.rs`.** Compile-verify: `cargo test -p paraeq-coreaudio` (non-ignored suite green) + run the ignored test locally.
- [ ] **Step 3: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-coreaudio Cargo.toml
git commit -m "feat(coreaudio): TapSystem RAII (tap + private aggregate, ordered teardown, self-exclusion retry)"
```

---

### Task 10: coreaudio::ioproc — sound IOProc wrapper + buffer views

**Files:**
- Create: `crates/paraeq-coreaudio/src/ioproc.rs`, `crates/paraeq-coreaudio/tests/test_buffers.rs`
- Modify: `crates/paraeq-coreaudio/src/lib.rs` (`pub mod ioproc;`), `crates/paraeq-coreaudio/tests/test_hardware.rs`

**Interfaces:**
- This task REPLACES the spike's two unsound shortcuts. Produces:
  - `pub struct IoBlock<'a> { pub input: BufferList<'a>, pub output: BufferListMut<'a>, pub in_sample_time: f64, pub out_sample_time: f64 }` — lifetimes bound to the trampoline's stack frame (the sound replacement for `buffers_of`'s `&'static mut`, spike main.rs:32-36).
  - `BufferList<'a>` / `BufferListMut<'a>`: zero-alloc views over an `AudioBufferList` (buffer count from `mNumberBuffers`, per-buffer `&[f32]`/`&mut [f32]` from `mData`/`mDataByteSize/4`, channel count from `mNumberChannels`). Layout facts to encode (spike main.rs:30-31, 65-93): a stereo stream may arrive as ONE interleaved 2-channel buffer or TWO mono buffers (DGR Labs gotcha). Provide `pub fn buffers(&self) -> impl Iterator<Item = (&[f32], usize /* channels */)>` (and mut equivalent) — layout policy (de/interleaving) belongs to the caller (Task 12), views stay dumb and allocation-free.
  - `pub type IoCallback = Box<dyn FnMut(IoBlock<'_>) + Send>;`
  - `pub struct IoProcHandle` — `pub fn register(device: AudioObjectID, cb: IoCallback) -> Result<IoProcHandle, CaError>` (double-box `Box::new(cb)` → `Box::into_raw` as the client pointer; trampoline reconstructs `&mut IoCallback` — **sound because**: the HAL serializes invocations of a single IOProc, the handle never touches the box while registered, and the box is freed only AFTER `AudioDeviceDestroyIOProcID` returns); `pub fn start(&mut self) -> Result<(), CaError>`; `pub fn stop(&mut self)` — idempotent `AudioDeviceStop` + `AudioDeviceDestroyIOProcID` + reclaim/drop the box, statuses logged; `impl Drop` calls `stop`.
  - Trampoline: the spike's `io_proc` signature (main.rs:38-46); body builds `IoBlock` from the four `NonNull` params and invokes the closure **inside `std::panic::catch_unwind(AssertUnwindSafe(..))`; on `Err` it calls `std::process::abort()`**; returns 0. **Nothing else** — no counters, no DSP (those live in `RtProcessor`). Why the explicit catch: the IOProc ABI is `extern "C-unwind"` (AudioHardware.rs:1185-1195), which *propagates* unwinds into CoreAudio's foreign HAL frames — it does NOT abort like plain `extern "C"`. An unwind into HAL frames could wedge the process with the tap alive (system muted). The catch+abort makes the safe behavior guaranteed: process dies → macOS drops the tap mute (spike-validated, doc line 13).
- Consumed by: Task 12.

- [ ] **Step 1: Pure failing tests** (`tests/test_buffers.rs`, NO HAL — hand-construct `AudioBufferList` values over local `Vec<f32>` storage; for the 2-buffer case build a `#[repr(C)] struct TwoBufferList { list: AudioBufferList, extra: AudioBuffer }` and view through `NonNull`):
  - interleaved single-buffer stereo exposes 1 buffer × 2 channels × N frames with correct data;
  - deinterleaved two-buffer stereo exposes 2 buffers × 1 channel;
  - mut views write through to the backing storage;
  - byte sizes not divisible by 4 truncate (`mDataByteSize as usize / 4`, spike behavior).
- [ ] **Step 2: Verify failure, implement, verify pass** (`cargo test -p paraeq-coreaudio --test test_buffers`).
- [ ] **Step 3: Hardware test** (`#[ignore]`): register a counting closure (AtomicU64 via Arc, closure moves the clone) on a `TapSystem`'s aggregate, `start`, sleep 1 s, assert callbacks > 50 (94/s expected at 512 frames — spike doc line 16), stop, then full teardown in order. This exercises the real trampoline.
- [ ] **Step 4: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-coreaudio
git commit -m "feat(coreaudio): IOProc wrapper with callback-scoped buffer views (no 'static lifetime lies)"
```

---

### Task 11: coreaudio::listeners — property watchers → events

**Files:**
- Create: `crates/paraeq-coreaudio/src/listeners.rs`
- Modify: `crates/paraeq-coreaudio/src/lib.rs` (`pub mod listeners;`), `crates/paraeq-coreaudio/tests/test_hardware.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq)] pub struct ListenerEvent { pub object: AudioObjectID, pub selector: u32 }`
  - `pub struct PropertyListener` — `pub fn watch(object: AudioObjectID, selector: u32, tx: std::sync::mpsc::Sender<ListenerEvent>) -> Result<PropertyListener, CaError>`; `impl Drop` → `AudioObjectRemovePropertyListener` then frees the ctx box.
  - Trampoline is an `AudioObjectPropertyListenerProc` (verified signature above); ctx = `Box<ListenerCtx { tx: Sender<ListenerEvent> }>`. HAL listener callbacks may fire on arbitrary (possibly concurrent) notification threads — that is fine: since Rust 1.72, `std::sync::mpsc::Sender<T: Send>` is `Sync`, so concurrent `send`s through a shared `&Sender` are safe (no mutex needed; these are non-realtime threads anyway — our selectors are dispatched async, never from the IO context). Send failures (receiver dropped) are ignored — the listener is being torn down. Trampoline body also wraps in `catch_unwind` + `abort` (same `C-unwind` rationale as Task 10).
  - Selectors used by Task 12: `kAudioHardwarePropertyDefaultOutputDevice` (on `kAudioObjectSystemObject`), `kAudioDevicePropertyDeviceIsAlive`, `kAudioDevicePropertyNominalSampleRate`, `kAudioDevicePropertyStreamConfiguration` (on the physical device).
- Safety notes in code: the ctx box must outlive registration (freed in Drop AFTER removal returns); removal must pass the SAME proc + ctx pointer used at add time (AudioHardware.rs:914 contract).

- [ ] **Step 1: Implement + compile-level test.** Pure-logic surface is thin; the meaningful assertions are hardware ones. Non-ignored test: `watch` + immediate `drop` on `kAudioObjectSystemObject` — registration/removal round-trip returns Ok on a mac (this does NOT need TCC; keep it non-ignored but `#[cfg(target_os = "macos")]`... it IS hardware-touching but harmless; mark `#[ignore]` anyway for CI-safety and parity with the suite).
- [ ] **Step 2: Hardware test** (`#[ignore]`, manual-interaction variant documented in the test's doc comment): register on default-output selector, print "switch your output device in System Settings within 10 s", assert an event arrives if switched — this one is `#[ignore = "manual: requires switching output device"]` and is allowed to be skipped in the normal ignored run; the register/drop roundtrip test above is the automated gate.
- [ ] **Step 3: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-coreaudio
git commit -m "feat(coreaudio): RAII property listeners (default-output, device-alive, format) to mpsc events"
```

---

### Task 12: coreaudio::backend — TapBackend implements engine AudioBackend

**Files:**
- Create: `crates/paraeq-coreaudio/src/backend.rs`
- Modify: `crates/paraeq-coreaudio/src/lib.rs` (`pub mod backend;`), `crates/paraeq-coreaudio/Cargo.toml` (add `paraeq-engine = { path = "../paraeq-engine" }`), `crates/paraeq-coreaudio/tests/test_hardware.rs`

**Interfaces:**
- Produces `pub struct TapBackend` implementing `paraeq_engine::backend::AudioBackend`:
  - `start(proc, requested_buffer_frames)`:
    1. `TapSystem::create()`;
    2. if requested, `set_buffer_frame_size(aggregate, frames)` then read back the effective size (this is the spike's latency work item, doc line 28);
    3. preallocate de/interleave scratch: `Vec<Vec<f32>>` in/out, `channels × max_frames` where `max_frames = effective_frames.max(4096)` — if a callback ever delivers MORE frames than scratch capacity, **zero all output buffers, leave silence for that block, and bump `RtShared::skipped_blocks`** (a layout-blind buffer-to-buffer copy is wrong: input/tap and output/device layouts are independent — spike main.rs:65-93 vs 106-130; one silent >4096-frame block is acceptable and near-unreachable). Never allocate in-callback;
    4. build the `IoCallback` closure: move `proc` (RtProcessor) + scratch in; per callback — resolve input layout ONCE (interleaved vs split, from `BufferList::buffers()`), deinterleave into scratch, build the input views from **`StreamInfo.channels`** (1 or 2 — sliced stack array, matching the chain's build parameter after Task 7's channel renegotiation), `proc.process_block(input, output_views, out_in_delta)` where `out_in_delta = out_sample_time - in_sample_time` (spike main.rs:49), then interleave scratch_out back into the output buffers (zero-fill any output space beyond our channels — spike zeroes first, main.rs:56-60: keep that order: zero ALL output buffers, then write);
    5. `IoProcHandle::register` on the aggregate + `start()`;
    6. register listeners: default-output (system), device-alive + rate + stream-config (physical device) → internal `mpsc`;
    7. return `StreamInfo { sample_rate: format.mSampleRate, channels: format.mChannelsPerFrame.min(2) as usize, buffer_frames: effective, device_uid }`.
  - `stop()`: **full invariant order in one place** — `IoProcHandle::stop()` (AudioDeviceStop + DestroyIOProcID) → `TapSystem::teardown()` (DestroyAggregate → DestroyTap); idempotent; all `CaError`s logged and folded into `EngineError::Backend` if any. Struct field order = drop order backup: `io: Option<IoProcHandle>` declared BEFORE `system: Option<TapSystem>` (Rust drops in declaration order) so even a bare Drop respects the invariant.
  - `poll_event()`: drain the internal listener rx; map default-output → `DefaultOutputChanged`, device-alive → `DeviceDied`, rate/config → `FormatChanged`. Dedup within one poll.
  - Mono devices: report `StreamInfo.channels = 1`; Task 7's controller renegotiates (one stop+start) and hands back a 1-channel chain — the backend never adapts channel count unilaterally.
- Error mapping: `impl From<CaError> for EngineError` (→ `Backend(err.to_string())`) lives here (coreaudio side), keeping engine free of coreaudio types.

- [ ] **Step 1: Hardware tests first** (`#[ignore]`, in `test_hardware.rs`):
  - `tap_backend_full_engine_boot`: `EngineHandle::spawn(TapBackend::new(), EngineConfig::default())`; sleep 2 s; snapshot: `stream.is_some()`, `callbacks flowing` (status is `Starting`/`NoInputDetected`/`Running` — any of these proves the IOProc runs; `Running` requires actual system audio, don't require it); `handle.send(Disable)`; drop — audio must be back to normal (manual ear-check documented).
  - `latency_delta_is_reported`: boot, sleep 2 s, assert `snapshot.latency_ms.unwrap_or(0.0) > 0.0`; print it (the number feeds Task 13's tuning table).
- [ ] **Step 2: Implement `backend.rs`.** `cargo test -p paraeq-coreaudio` (non-ignored green — the crate must still build warning-free with the new engine dep) + run ignored tests locally.
- [ ] **Step 3: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-coreaudio
git commit -m "feat(coreaudio): TapBackend — production tap engine behind the AudioBackend seam"
```

---

### Task 13: Example binary + latency tuning measurements

**Files:**
- Create: `crates/paraeq-coreaudio/examples/tap_engine.rs`
- Modify: `crates/paraeq-coreaudio/Cargo.toml` (`[dev-dependencies]` add `ctrlc = "3.5"`, `env_logger = "0.11"`, `paraeq-dsp = { path = "../paraeq-dsp" }` — all example/test-only; examples see dev-deps, and the example designs its SOS/FIR via paraeq-dsp because porting the spike's biquad is banned). The `log` workspace dep already exists (added in Task 9).

**Interfaces:**
- `tap_engine` example: the stage-3 acceptance artifact and the spike's successor. CLI: `--fc HZ --gain DB --q Q` (default 80/+9/1 — same audible test as the spike), `--fir` (optional: synthesize a min-phase FIR from the same peaking response via paraeq-dsp, to exercise the convolver path end-to-end), `--frames N` (requested buffer size), `--bypass-every SECS` (A/B toggle), `--secs N`. Builds the `CorrectionConfig`, spawns `EngineHandle` with `TapBackend`, subscribes to snapshots, prints one status line per change + a 1 Hz telemetry line (status, peak, latency_ms, callbacks/s). Ctrl-C → `Disable` → drop → exit. `env_logger::init()` so `RUST_LOG=paraeq_coreaudio=debug` shows lifecycle logs.
- Measurement protocol (executor runs what's automatable; the rest is the owner's manual checklist — record BOTH in the task report and Task 14's docs update):
  1. `cargo run -p paraeq-coreaudio --example tap_engine` at `--frames 512`, `256`, `128` → record `latency_ms` for each (spike baseline: 62.3 ms at defaults). Target: 20–30 ms (spec line 90); if the floor can't reach it, record the honest floor (spike doc line 28 — "expose the number honestly").
  2. CPU (the spike deferred formal profiling to this stage, doc line 17): during each frames-sweep run, sample `ps -o %cpu= -p <pid>` a few times for BOTH the IIR path and the `--fir` path; record steady-state %CPU alongside the latency numbers.
  3. Manual (owner): music audibly EQ'd; volume keys + HUD native; `--bypass-every 5` A/B clean; Ctrl-C restores audio; `kill -9` restores audio.

- [ ] **Step 1: Implement the example** (no TDD — it's a dev tool; it must compile under `--all-targets`).
- [ ] **Step 2: Run the automated part locally** (needs TCC-granted terminal): frames sweep, capture latency numbers.
- [ ] **Step 3: Gates + commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/paraeq-coreaudio Cargo.toml
git commit -m "feat(coreaudio): tap_engine example (production-stack spike successor) + latency sweep"
```

---

### Task 14: Docs, divergences check, final green

**Files:**
- Modify: `docs/CONTEXT.md` (stage-3 status), `crates/paraeq-dsp/DIVERGENCES.md` (only if implementation surfaced new ones), `docs/specs/2026-07-02-rust-port-design.md` (line 94 wording), `CLAUDE.md` (teardown bullet wording)

- [ ] **Step 1: CONTEXT.md.** Replace the stage-3 "Next:" sentence and the "Stage-3 plan brief must include" block (CONTEXT.md:107-114) with a completion entry: stage 3 complete — production tap engine (`TapBackend` + `EngineController`), watchdog semantics (first-nonzero gating, 5 s engage tolerance, `NoInputDetected` permission hint), measured latency + CPU table from Task 13 (per buffer size, vs the 62.3 ms spike baseline and 20–30 ms budget), remaining manual validation items for the owner (the Task 13 checklist), and "Next: stage 4 — Tauri shell + EQ tab". Keep the carried knowledge (unsafe-pattern bans now enforced in code) as a one-line pointer to this plan.

- [ ] **Step 1b: Teardown-wording reconciliation.** The spec (line 94) and CLAUDE.md say "destroys the tap first"; the spike-validated call order destroys the tap as the *final* call of the sequence (stop → destroy IOProc → destroy aggregate → destroy tap). Update both docs to say what is actually meant and built: *"every exit path runs the full teardown sequence ending in tap destruction — the system must never be left muted"* — so no future session "fixes" the validated order to match the stale wording.
- [ ] **Step 2: Final whole-workspace gates.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
.venv/bin/pytest prototype/tests -q   # oracle untouched and green (128 passed)
```

- [ ] **Step 3: Local hardware sweep** — `cargo test -p paraeq-coreaudio -- --ignored` one final time on the branch tip; paste results into the task report.
- [ ] **Step 4: Commit.**

```bash
git add docs/CONTEXT.md docs/specs/2026-07-02-rust-port-design.md CLAUDE.md crates/paraeq-dsp/DIVERGENCES.md
git commit -m "docs: stage-3 status (latency/CPU numbers) + teardown-order wording reconciled with spike"
```

---

## Completion Checklist (whole plan)

- [ ] All five CONTEXT.md:109-114 carry-forwards landed: watchdog + 5 s engage tolerance (Task 6), buffer-size latency tuning with honest numbers (Tasks 12-13), no spike unsafe patterns (Tasks 5/10 — grep the branch for `&'static mut` and `as *mut EqState`-style casts: zero hits outside spikes/), unified processor contracts + convolver assert (Task 1), shelf/notch proptests (Task 2), input-wiring hardening (Task 3).
- [ ] Teardown invariant encoded exactly once (TapBackend::stop) and respected by every Drop path; no OSStatus discarded.
- [ ] Realtime lane: no locks, no allocation, no logging (review `RtProcessor::process_block` + the TapBackend closure + `RtLink::poll` line by line against this).
- [ ] `paraeq-engine` has no coreaudio/Tauri deps; `paraeq-coreaudio` is the only crate with `unsafe` FFI; every unsafe block has a SAFETY comment.
- [ ] CI green without hardware (all HAL tests `#[ignore]`); local `-- --ignored` suite run and reported.
- [ ] New deps limited to: `log` (workspace), `serde` (engine), `ctrlc`/`env_logger`/`paraeq-dsp` (coreaudio dev-deps only). No ndarray/scirs2/fundsp.
- [ ] Engine lifecycle scenarios from spec line 209 (hot-plug, device switch, stall) covered by MockBackend tests.
- [ ] Merge per superpowers:finishing-a-development-branch; next plan = stage 4 (Tauri shell + EQ tab; carry forward: sample-rate-change coefficient redesign responsibility lives upstream of the engine — the controller republishes state, stage 4 must re-send corrections on rate change).
