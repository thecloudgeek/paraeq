# Rust Port Foundation (Stages 0–1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the Core Audio process-tap architecture with a standalone spike (go/no-go gate), then restructure the repo into a Rust workspace with the Python prototype demoted to `prototype/` as a golden-fixture oracle, with CI green.

**Architecture:** Per `docs/specs/2026-07-02-rust-port-design.md`. Stage 0 builds `spikes/tap-spike/` — a standalone Rust binary: global process tap (muted-when-tapped, self-excluded) → aggregate device containing the real output device → one IOProc runs a biquad and writes the device's output buffers. Stage 1 moves the Python to `prototype/`, generates `fixtures/` from it, scaffolds the `crates/` workspace + `desktop/` Tauri app, and adds CI. **Stage 1 does not depend on the spike's outcome** — fixtures, workspace, and scaffold are needed even in the fallback (BlackHole+aggregate) world.

**Tech Stack:** Rust stable (1.96+), `objc2-core-audio` 0.3, Tauri 2, React 19 + TypeScript + Vite, Tailwind 4, Python 3.13 venv (oracle only).

## Global Constraints

- macOS 14.4 is the minimum supported OS (`bundle.macOS.minimumSystemVersion = "14.4"`).
- Realtime audio code: no locks, no allocation in IOProc callbacks (spike may preallocate scratch buffers in its state struct).
- Teardown ordering invariant: `AudioDeviceStop` → `AudioDeviceDestroyIOProcID` → `AudioHardwareDestroyAggregateDevice` → `AudioHardwareDestroyProcessTap`. A dead ParaEQ must never leave the system muted.
- DSP design math is f64; realtime samples are f32 (CoreAudio native).
- Forbidden dependencies: `ndarray`, `scirs2` (any crate of that family), `fundsp` (in the EQ core).
- Dependency version specs (verified 2026-07-02): `objc2 = "0.6"`, `objc2-core-audio = "0.3"`, `objc2-core-audio-types = "0.3"`, `objc2-core-foundation = "0.3"`, `objc2-foundation = "0.3"`, `rustfft = "6.4"`, `realfft = "3.5"`, `hound = "3.5"`, `approx = "0.5"`, `proptest = "1"`, `arc-swap = "1"`, `rtrb = "0.3"`, `serde = "1"`, `serde_json = "1"`, `thiserror = "2"`, `tauri = "2"`, `tauri-build = "2"`, `libc = "0.2"`, `ctrlc = "3"`. npm: `@tauri-apps/cli@^2`, `@tauri-apps/api@^2`, `vite@^8`, `react@^19`, `typescript@^6`, `tailwindcss@^4`, `@tailwindcss/vite@^4`.
- Every commit message ends with the two trailers required by the harness:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and the `Claude-Session:` link line the harness supplies.
- All commands below run from the repo root `/Users/ronakpatel/code/paraeq` unless a `cd` is shown. The Python venv is `.venv/` at repo root (`source .venv/bin/activate`).
- Execution happens on a feature branch (worktree per superpowers:using-git-worktrees), e.g. `feature/rust-port-foundation`.

## File Structure (end state of this plan)

```
paraeq/
├── Cargo.toml                    # NEW workspace root
├── rust-toolchain.toml           # NEW
├── .taurignore                   # NEW (tauri dev watcher excludes)
├── .github/workflows/ci.yml     # NEW
├── crates/
│   ├── paraeq-dsp/               # NEW stub crate + fixture-path smoke test
│   ├── paraeq-coreaudio/         # NEW stub crate
│   └── paraeq-engine/            # NEW stub crate
├── desktop/
│   ├── src-tauri/                # NEW Tauri backend (workspace member)
│   └── ui/                       # NEW React+TS+Vite frontend
├── spikes/tap-spike/             # NEW standalone cargo project (NOT a workspace member)
│   └── src/{main.rs, biquad.rs, ca.rs}
├── docs/spikes/2026-07-tap-spike.md   # NEW findings + go/no-go record
├── fixtures/                     # NEW Python-generated goldens (committed)
├── targets/                      # unchanged location (single source of truth)
└── prototype/                    # ← paraeq/, app/, tests/, pyproject.toml move here
    ├── targets -> ../targets     # NEW symlink (keeps TARGETS_DIR + test paths working)
    └── tools/generate_fixtures.py # NEW oracle dump script
```

---

# STAGE 0 — TAP SPIKE (go/no-go gate)

### Task 1: Spike crate scaffold + biquad module (TDD)

**Files:**
- Create: `spikes/tap-spike/Cargo.toml`
- Create: `spikes/tap-spike/src/main.rs` (placeholder)
- Create: `spikes/tap-spike/src/biquad.rs`

**Interfaces:**
- Produces: `biquad::Coeffs { b0, b1, b2, a1, a2: f64 }`, `biquad::peaking(fs: f64, fc: f64, gain_db: f64, q: f64) -> Coeffs`, `biquad::Df2t::new(c: Coeffs)`, `Df2t::process(&mut self, x: f32) -> f32`. Task 3's IOProc consumes these exact names.

- [ ] **Step 1: Create the crate**

`spikes/tap-spike/Cargo.toml`:

```toml
[package]
name = "tap-spike"
version = "0.1.0"
edition = "2021"
publish = false

# Standalone project — deliberately NOT part of the repo workspace (Task 7 adds
# an explicit exclude). Spike code is throwaway; findings are the deliverable.

[dependencies]
ctrlc = "3"
libc = "0.2"
objc2 = "0.6"
objc2-core-audio = "0.3"
objc2-core-audio-types = "0.3"
objc2-core-foundation = "0.3"
objc2-foundation = "0.3"

[profile.release]
debug = true
```

`spikes/tap-spike/src/main.rs` (placeholder so it compiles):

```rust
mod biquad;

fn main() {
    println!("tap-spike: see Task 2/3");
}
```

- [ ] **Step 2: Write the failing biquad tests**

`spikes/tap-spike/src/biquad.rs`:

```rust
//! RBJ Audio EQ Cookbook peaking biquad — mirrors prototype/paraeq/correction/biquad.py.
//! Coefficients f64, samples f32, filter state f64 (spec: Sample formats).

#[derive(Clone, Copy, Debug)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// |H(e^{jw})| in dB, evaluated directly from the transfer function.
    fn mag_db(c: &Coeffs, f: f64, fs: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * f / fs;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let nr = c.b0 + c.b1 * c1 + c.b2 * c2;
        let ni = -(c.b1 * s1 + c.b2 * s2);
        let dr = 1.0 + c.a1 * c1 + c.a2 * c2;
        let di = -(c.a1 * s1 + c.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }

    #[test]
    fn peaking_gain_at_fc_is_exact() {
        // RBJ property: |H(fc)| in dB == gain_db exactly.
        let c = peaking(48000.0, 1000.0, 6.0, 1.0);
        assert!((mag_db(&c, 1000.0, 48000.0) - 6.0).abs() < 1e-9);
        let cut = peaking(48000.0, 250.0, -4.5, 2.0);
        assert!((mag_db(&cut, 250.0, 48000.0) + 4.5).abs() < 1e-9);
    }

    #[test]
    fn peaking_is_flat_far_from_fc() {
        let c = peaking(48000.0, 1000.0, 6.0, 1.0);
        assert!(mag_db(&c, 20.0, 48000.0).abs() < 0.1);
        assert!(mag_db(&c, 20000.0, 48000.0).abs() < 0.5);
    }

    #[test]
    fn df2t_amplifies_sine_at_fc_by_gain() {
        let c = peaking(48000.0, 1000.0, 6.0, 1.0);
        let mut f = Df2t::new(c);
        let n = 48000;
        let mut in_rms = 0.0f64;
        let mut out_rms = 0.0f64;
        for i in 0..n {
            let x = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48000.0).sin() as f32;
            let y = f.process(x);
            if i >= 4800 {
                // skip transient
                in_rms += (x as f64) * (x as f64);
                out_rms += (y as f64) * (y as f64);
            }
        }
        let gain_db = 10.0 * (out_rms / in_rms).log10();
        assert!((gain_db - 6.0).abs() < 0.05, "measured {gain_db}");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path spikes/tap-spike/Cargo.toml`
Expected: FAIL — `cannot find function 'peaking'`, `cannot find struct 'Df2t'`.

- [ ] **Step 4: Implement `peaking` and `Df2t`**

Append to `spikes/tap-spike/src/biquad.rs` (above the `tests` module):

```rust
/// RBJ peaking EQ. Matches biquad_peaking() in the Python prototype:
/// a_lin = 10^(gain/40), w0 = 2*pi*fc/fs, alpha = sin(w0)/(2q), normalized by a0.
pub fn peaking(fs: f64, fc: f64, gain_db: f64, q: f64) -> Coeffs {
    let a_lin = 10f64.powf(gain_db / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * fc / fs;
    let alpha = w0.sin() / (2.0 * q);
    let a0 = 1.0 + alpha / a_lin;
    Coeffs {
        b0: (1.0 + alpha * a_lin) / a0,
        b1: (-2.0 * w0.cos()) / a0,
        b2: (1.0 - alpha * a_lin) / a0,
        a1: (-2.0 * w0.cos()) / a0,
        a2: (1.0 - alpha / a_lin) / a0,
    }
}

/// Direct Form II Transposed runner — f64 state, f32 samples.
pub struct Df2t {
    c: Coeffs,
    z1: f64,
    z2: f64,
}

impl Df2t {
    pub fn new(c: Coeffs) -> Self {
        Self { c, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.c.b0 * x + self.z1;
        self.z1 = self.c.b1 * x - self.c.a1 * y + self.z2;
        self.z2 = self.c.b2 * x - self.c.a2 * y;
        y as f32
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path spikes/tap-spike/Cargo.toml`
Expected: `test result: ok. 3 passed`

- [ ] **Step 6: Commit**

```bash
git add spikes/tap-spike
git commit -m "spike(tap): scaffold tap-spike crate with TDD'd RBJ peaking biquad"
```

---

### Task 2: CoreAudio helpers — devices, tap, format (`ca.rs`)

**Files:**
- Create: `spikes/tap-spike/src/ca.rs`
- Modify: `spikes/tap-spike/src/main.rs`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces (Task 3 consumes these exact signatures):
  - `ca::check(status: i32, ctx: &str) -> Result<(), String>` — OSStatus → Err with fourcc decode
  - `ca::default_output_device() -> Result<AudioObjectID, String>`
  - `ca::device_uid(dev: AudioObjectID) -> Result<CFRetained<CFString>, String>`
  - `ca::nominal_sample_rate(dev: AudioObjectID) -> Result<f64, String>`
  - `ca::translate_pid(pid: i32) -> Result<AudioObjectID, String>` (0 = unknown)
  - `ca::create_tap(excluded: &[AudioObjectID]) -> Result<(AudioObjectID, Retained<CATapDescription>), String>`
  - `ca::tap_format(tap: AudioObjectID) -> Result<AudioStreamBasicDescription, String>`
  - `ca::create_aggregate(out_uid: &CFString, tap_uuid: &NSString) -> Result<AudioObjectID, String>`

**Note for the implementer:** every symbol below was verified against `objc2-core-audio` 0.3.2 generated source on 2026-07-02. The CF bridging calls (`CFDictionary::from_slices`, `CFArray::from_CFTypes`, `.as_ref()` upcasts to `&CFType`) are correct in shape but may need small compiler-guided adjustments (`&*` derefs, explicit type ascription) — the API surface itself is right; do not switch crates if the borrow checker complains, just adjust the plumbing.

- [ ] **Step 1: Write `ca.rs`**

```rust
//! Thin unsafe wrappers over the CoreAudio HAL + process-tap API.
//! Symbols verified against objc2-core-audio 0.3.2 (2026-07-02).

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::AnyThread; // provides ::alloc(); named AllocAnyThread in some 0.6.x — adjust import if needed
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyNominalSampleRate,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};

fn addr(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Decode an OSStatus into Ok/Err with the fourcc shown (e.g. '!obj', 'who?').
pub fn check(status: i32, ctx: &str) -> Result<(), String> {
    if status == 0 {
        return Ok(());
    }
    let b = status.to_be_bytes();
    let fourcc: String = b
        .iter()
        .map(|&c| if c.is_ascii_graphic() { c as char } else { '?' })
        .collect();
    Err(format!("{ctx}: OSStatus {status} ('{fourcc}')"))
}

pub fn default_output_device() -> Result<AudioObjectID, String> {
    let mut dev: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject,
            &addr(kAudioHardwarePropertyDefaultOutputDevice),
            0,
            std::ptr::null(),
            &mut size,
            NonNull::from(&mut dev).cast::<c_void>(),
        )
    };
    check(status, "get default output device")?;
    Ok(dev)
}

pub fn device_uid(dev: AudioObjectID) -> Result<CFRetained<CFString>, String> {
    // Out-param is a +1 retained CFStringRef.
    let mut uid: *const CFString = std::ptr::null();
    let mut size = size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            &addr(kAudioDevicePropertyDeviceUID),
            0,
            std::ptr::null(),
            &mut size,
            NonNull::from(&mut uid).cast::<c_void>(),
        )
    };
    check(status, "get device UID")?;
    NonNull::new(uid.cast_mut())
        .map(|p| unsafe { CFRetained::from_raw(p) })
        .ok_or_else(|| "device UID was NULL".into())
}

pub fn nominal_sample_rate(dev: AudioObjectID) -> Result<f64, String> {
    let mut rate: f64 = 0.0;
    let mut size = size_of::<f64>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            dev,
            &addr(kAudioDevicePropertyNominalSampleRate),
            0,
            std::ptr::null(),
            &mut size,
            NonNull::from(&mut rate).cast::<c_void>(),
        )
    };
    check(status, "get nominal sample rate")?;
    Ok(rate)
}

/// PID -> HAL process object. Returns 0 (kAudioObjectUnknown) if not found.
pub fn translate_pid(pid: i32) -> Result<AudioObjectID, String> {
    let mut obj: AudioObjectID = 0;
    let mut size = size_of::<AudioObjectID>() as u32;
    let pid: libc::pid_t = pid;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject,
            &addr(kAudioHardwarePropertyTranslatePIDToProcessObject),
            size_of::<libc::pid_t>() as u32,
            (&raw const pid).cast(),
            &mut size,
            NonNull::from(&mut obj).cast::<c_void>(),
        )
    };
    check(status, "translate PID to process object")?;
    Ok(obj)
}

/// Global stereo tap excluding `excluded` process objects; MutedWhenTapped; private.
/// First AudioHardwareCreateProcessTap call triggers the TCC prompt.
pub fn create_tap(
    excluded: &[AudioObjectID],
) -> Result<(AudioObjectID, Retained<CATapDescription>), String> {
    let nums: Vec<Retained<NSNumber>> =
        excluded.iter().map(|&o| NSNumber::new_u32(o)).collect();
    let arr = NSArray::from_retained_slice(&nums);
    let desc = unsafe {
        CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &arr)
    };
    unsafe {
        desc.setMuteBehavior(CATapMuteBehavior::MutedWhenTapped);
        desc.setPrivate(true);
        desc.setName(&NSString::from_str("paraeq-tap-spike"));
    }
    let mut tap: AudioObjectID = 0;
    let status = unsafe { AudioHardwareCreateProcessTap(&desc, &mut tap) };
    check(status, "AudioHardwareCreateProcessTap (TCC-gated)")?;
    Ok((tap, desc))
}

pub fn tap_format(tap: AudioObjectID) -> Result<AudioStreamBasicDescription, String> {
    let mut asbd = AudioStreamBasicDescription::default();
    let mut size = size_of::<AudioStreamBasicDescription>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            tap,
            &addr(kAudioTapPropertyFormat),
            0,
            std::ptr::null(),
            &mut size,
            NonNull::from(&mut asbd).cast::<c_void>(),
        )
    };
    check(status, "get tap format")?;
    Ok(asbd)
}

/// Aggregate with the real output device as sub-device AND the tap in the
/// creation dictionary (iqualize gotcha: adding the tap after creation to an
/// aggregate that has sub-devices delivers zero-filled buffers).
pub fn create_aggregate(
    out_uid: &CFString,
    tap_uuid: &NSString,
) -> Result<AudioObjectID, String> {
    let key = |c: &std::ffi::CStr| CFString::from_str(c.to_str().unwrap());

    // sub-device entry: { uid: <output device UID> }
    let sub_dev: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[&*key(kAudioSubDeviceUIDKey)],
        &[out_uid.as_ref() as &CFType],
    );
    // sub-tap entry: { uid: <CATapDescription UUID string>, drift: true }
    let tap_uid_cf = CFString::from_str(&tap_uuid.to_string());
    let sub_tap: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioSubTapUIDKey),
            &*key(kAudioSubTapDriftCompensationKey),
        ],
        &[
            tap_uid_cf.as_ref() as &CFType,
            CFBoolean::new(true).as_ref() as &CFType,
        ],
    );
    let sub_devs = CFArray::from_CFTypes(&[sub_dev.as_ref()]);
    let taps = CFArray::from_CFTypes(&[sub_tap.as_ref()]);
    let agg_uid = CFString::from_str(&format!("com.paraeq.tap-spike.{}", std::process::id()));
    let name = CFString::from_str("paraeq-tap-spike-agg");

    let dict: CFRetained<CFDictionary<CFString, CFType>> = CFDictionary::from_slices(
        &[
            &*key(kAudioAggregateDeviceNameKey),
            &*key(kAudioAggregateDeviceUIDKey),
            &*key(kAudioAggregateDeviceMainSubDeviceKey),
            &*key(kAudioAggregateDeviceIsPrivateKey),
            &*key(kAudioAggregateDeviceIsStackedKey),
            &*key(kAudioAggregateDeviceTapAutoStartKey),
            &*key(kAudioAggregateDeviceSubDeviceListKey),
            &*key(kAudioAggregateDeviceTapListKey),
        ],
        &[
            name.as_ref() as &CFType,
            agg_uid.as_ref() as &CFType,
            out_uid.as_ref() as &CFType,
            CFBoolean::new(true).as_ref() as &CFType,  // private (required for tapautostart)
            CFBoolean::new(false).as_ref() as &CFType, // stacked
            CFBoolean::new(true).as_ref() as &CFType,  // tapautostart
            sub_devs.as_ref() as &CFType,
            taps.as_ref() as &CFType,
        ],
    );

    let mut agg: AudioObjectID = 0;
    let status = unsafe { AudioHardwareCreateAggregateDevice(&dict, &mut agg) };
    check(status, "AudioHardwareCreateAggregateDevice")?;
    Ok(agg)
}
```

- [ ] **Step 2: Add a `--probe` mode to `main.rs`**

Replace `spikes/tap-spike/src/main.rs`:

```rust
mod biquad;
mod ca;

fn probe() -> Result<(), String> {
    let dev = ca::default_output_device()?;
    let uid = ca::device_uid(dev)?;
    let rate = ca::nominal_sample_rate(dev)?;
    println!("default output: id={dev} uid={uid} rate={rate}");

    let own = ca::translate_pid(std::process::id() as i32)?;
    println!("own process object: {own} (0 = not registered with HAL yet; exclusion list will be empty)");

    let excluded = if own != 0 { vec![own] } else { vec![] };
    let (tap, _desc) = ca::create_tap(&excluded)?;
    let f = ca::tap_format(tap)?;
    println!(
        "tap created: id={tap} format: {} Hz, {} ch, {} bits, flags={:#x}",
        f.mSampleRate, f.mChannelsPerFrame, f.mBitsPerChannel, f.mFormatFlags
    );
    unsafe { objc2_core_audio::AudioHardwareDestroyProcessTap(tap) };
    println!("tap destroyed. probe OK");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("--probe") => probe(),
        _ => {
            eprintln!("usage: tap-spike --probe   (run loop lands in Task 3)");
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 3: Compile**

Run: `cargo build --manifest-path spikes/tap-spike/Cargo.toml`
Expected: compiles (warnings OK). If CF bridging lines error, adjust derefs/casts per the note above — the function and constant names are correct.

- [ ] **Step 4: Manual probe run — THE TCC MOMENT**

Run **from macOS Terminal.app** (not iTerm — known gotcha: some terminals never show the TCC prompt and the tap silently delivers zeros):

```bash
cargo run --manifest-path spikes/tap-spike/Cargo.toml -- --probe
```

Expected: macOS shows the "System Audio Recording" permission prompt (grant it); output prints the default device, `own process object`, and a tap format line like `48000 Hz, 2 ch, 32 bits`. If no prompt appears and later stages read silence: System Settings → Privacy & Security → Screen & System Audio Recording → add the terminal.

- [ ] **Step 5: Commit**

```bash
git add spikes/tap-spike
git commit -m "spike(tap): CoreAudio helpers — device/UID/rate, PID translation, tap create + format probe"
```

---

### Task 3: Aggregate + IOProc passthrough with EQ (the actual spike)

**Files:**
- Modify: `spikes/tap-spike/src/main.rs`

**Interfaces:**
- Consumes: `ca::*` from Task 2, `biquad::*` from Task 1.
- Produces: a runnable binary — `tap-spike run [--bypass] [--fc HZ] [--gain DB] [--q Q] [--secs N]`. Findings feed Task 4's doc; no code consumes this.

- [ ] **Step 1: Implement the run mode**

Replace `spikes/tap-spike/src/main.rs` in full:

```rust
mod biquad;
mod ca;

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectID,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};

/// Shared with the realtime callback. No locks; atomics + preallocated filters only.
struct EqState {
    filters: [biquad::Df2t; 2],
    bypass: bool,
    callbacks: AtomicU64,
    /// f32 bits of the max |sample| seen on input (AtomicU32 as f32 bits).
    peak_in_bits: AtomicU32,
    zero_blocks: AtomicU64,
    nonzero_blocks: AtomicU64,
    /// (output sample time - input sample time) of the latest callback.
    sample_time_delta: AtomicU64, // f64 bits
}

/// Copy an AudioBufferList's buffers into per-channel access without allocating:
/// we iterate buffers in place. Layout is probed at runtime (may be one
/// interleaved stereo buffer, or two mono buffers — DGR Labs gotcha).
unsafe fn buffers_of(list: NonNull<AudioBufferList>) -> &'static mut [objc2_core_audio_types::AudioBuffer] {
    let l = list.as_ptr();
    let n = (*l).mNumberBuffers as usize;
    std::slice::from_raw_parts_mut((*l).mBuffers.as_mut_ptr(), n)
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let state = &mut *(client as *mut EqState);
    state.callbacks.fetch_add(1, Ordering::Relaxed);
    let dt = (*output_time.as_ptr()).mSampleTime - (*input_time.as_ptr()).mSampleTime;
    state.sample_time_delta.store(dt.to_bits(), Ordering::Relaxed);

    let in_bufs = buffers_of(input);
    let out_bufs = buffers_of(output);

    // Zero all output buffers first (whatever their layout).
    for ob in out_bufs.iter_mut() {
        let n = ob.mDataByteSize as usize / 4;
        let data = std::slice::from_raw_parts_mut(ob.mData as *mut f32, n);
        data.fill(0.0);
    }

    let mut peak = f32::from_bits(state.peak_in_bits.load(Ordering::Relaxed));
    let mut all_zero = true;

    // Process channel-by-channel. `ch` is the logical stereo channel index.
    // Input layout cases: 1 buffer with mNumberChannels==2 (interleaved) or
    // 2 buffers with mNumberChannels==1 (deinterleaved) or mono.
    let mut logical_ch = 0usize;
    for ib in in_bufs.iter() {
        let ch_in_buf = ib.mNumberChannels.max(1) as usize;
        let n = ib.mDataByteSize as usize / 4;
        let data = std::slice::from_raw_parts(ib.mData as *const f32, n);
        for interleave in 0..ch_in_buf {
            let ch = (logical_ch + interleave).min(1);
            let filt = &mut state.filters[ch];
            let mut i = interleave;
            while i < n {
                let x = data[i];
                if x != 0.0 {
                    all_zero = false;
                }
                let ax = x.abs();
                if ax > peak {
                    peak = ax;
                }
                let y = if state.bypass { x } else { filt.process(x) };
                // Write into the matching output slot, probing output layout.
                write_sample(out_bufs, ch, i / ch_in_buf, ch_in_buf, y);
                i += ch_in_buf;
            }
        }
        logical_ch += ch_in_buf;
    }

    state.peak_in_bits.store(peak.to_bits(), Ordering::Relaxed);
    if all_zero {
        state.zero_blocks.fetch_add(1, Ordering::Relaxed);
    } else {
        state.nonzero_blocks.fetch_add(1, Ordering::Relaxed);
    }
    0
}

/// Write sample y for logical channel `ch`, frame `frame`, into whatever
/// layout the output buffers use.
unsafe fn write_sample(
    out_bufs: &mut [objc2_core_audio_types::AudioBuffer],
    ch: usize,
    frame: usize,
    _in_ch_per_buf: usize,
    y: f32,
) {
    if out_bufs.len() == 1 {
        let ob = &mut out_bufs[0];
        let ch_n = ob.mNumberChannels.max(1) as usize;
        let n = ob.mDataByteSize as usize / 4;
        let idx = frame * ch_n + ch.min(ch_n - 1);
        if idx < n {
            let data = std::slice::from_raw_parts_mut(ob.mData as *mut f32, n);
            data[idx] = y;
        }
    } else if ch < out_bufs.len() {
        let ob = &mut out_bufs[ch];
        let n = ob.mDataByteSize as usize / 4;
        if frame < n {
            let data = std::slice::from_raw_parts_mut(ob.mData as *mut f32, n);
            data[frame] = y;
        }
    }
}

struct Cli {
    bypass: bool,
    fc: f64,
    gain: f64,
    q: f64,
    secs: u64,
}

fn parse_cli(args: &[String]) -> Cli {
    let mut c = Cli { bypass: false, fc: 80.0, gain: 6.0, q: 1.0, secs: 0 };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bypass" => c.bypass = true,
            "--fc" => { i += 1; c.fc = args[i].parse().unwrap(); }
            "--gain" => { i += 1; c.gain = args[i].parse().unwrap(); }
            "--q" => { i += 1; c.q = args[i].parse().unwrap(); }
            "--secs" => { i += 1; c.secs = args[i].parse().unwrap(); }
            other => eprintln!("ignoring arg: {other}"),
        }
        i += 1;
    }
    c
}

fn run(cli: Cli) -> Result<(), String> {
    // 1-2. devices + tap
    let dev = ca::default_output_device()?;
    let uid = ca::device_uid(dev)?;
    let rate = ca::nominal_sample_rate(dev)?;
    println!("default output: id={dev} uid={uid} rate={rate}");

    let own = ca::translate_pid(std::process::id() as i32)?;
    let excluded = if own != 0 { vec![own] } else { vec![] };
    if excluded.is_empty() {
        println!("WARN: own process not in HAL registry yet — no self-exclusion (watch for feedback)");
    }
    let (tap, desc) = ca::create_tap(&excluded)?;
    let fmt = ca::tap_format(tap)?;
    println!("tap: id={tap}, {} Hz, {} ch", fmt.mSampleRate, fmt.mChannelsPerFrame);

    // 3. aggregate (tap must be in the creation dict)
    let tap_uuid = unsafe { desc.UUID().UUIDString() };
    let agg = ca::create_aggregate(&uid, &tap_uuid)?;
    println!("aggregate: id={agg}");

    // 4. EQ state + IOProc
    let coeffs = biquad::peaking(rate, cli.fc, cli.gain, cli.q);
    let mut state = Box::new(EqState {
        filters: [biquad::Df2t::new(coeffs), biquad::Df2t::new(coeffs)],
        bypass: cli.bypass,
        callbacks: AtomicU64::new(0),
        peak_in_bits: AtomicU32::new(0),
        zero_blocks: AtomicU64::new(0),
        nonzero_blocks: AtomicU64::new(0),
        sample_time_delta: AtomicU64::new(0),
    });
    println!(
        "EQ: peaking fc={} gain={} q={} bypass={}",
        cli.fc, cli.gain, cli.q, cli.bypass
    );

    let mut proc_id: AudioDeviceIOProcID = None;
    let status = unsafe {
        AudioDeviceCreateIOProcID(
            agg,
            Some(io_proc),
            (&mut *state as *mut EqState).cast::<c_void>(),
            &mut proc_id,
        )
    };
    ca::check(status, "AudioDeviceCreateIOProcID")?;
    ca::check(unsafe { AudioDeviceStart(agg, proc_id) }, "AudioDeviceStart")?;
    println!("RUNNING — play music. Ctrl-C to stop. (--secs {} means {})",
        cli.secs, if cli.secs == 0 { "until Ctrl-C".to_string() } else { format!("{}s", cli.secs) });

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .map_err(|e| e.to_string())?;
    }

    let started = std::time::Instant::now();
    let mut last_cb = 0u64;
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let cb = state.callbacks.load(Ordering::Relaxed);
        let dt = f64::from_bits(state.sample_time_delta.load(Ordering::Relaxed));
        println!(
            "cb/s={:4}  peak_in={:.3}  zero_blocks={}  nonzero={}  out-in sample delta={} ({:.1} ms)",
            cb - last_cb,
            f32::from_bits(state.peak_in_bits.load(Ordering::Relaxed)),
            state.zero_blocks.load(Ordering::Relaxed),
            state.nonzero_blocks.load(Ordering::Relaxed),
            dt,
            1000.0 * dt / rate,
        );
        last_cb = cb;
        state.peak_in_bits.store(0, Ordering::Relaxed);
        if cli.secs > 0 && started.elapsed().as_secs() >= cli.secs {
            break;
        }
    }

    // Teardown — ORDER IS THE INVARIANT: stop → destroy proc → destroy agg → destroy tap.
    println!("tearing down…");
    unsafe {
        AudioDeviceStop(agg, proc_id);
        AudioDeviceDestroyIOProcID(agg, proc_id);
        AudioHardwareDestroyAggregateDevice(agg);
        AudioHardwareDestroyProcessTap(tap);
    }
    println!("done — system audio should be back to normal.");
    Ok(())
}

fn probe() -> Result<(), String> {
    let dev = ca::default_output_device()?;
    let uid = ca::device_uid(dev)?;
    let rate = ca::nominal_sample_rate(dev)?;
    println!("default output: id={dev} uid={uid} rate={rate}");
    let own = ca::translate_pid(std::process::id() as i32)?;
    println!("own process object: {own}");
    let excluded = if own != 0 { vec![own] } else { vec![] };
    let (tap, _desc) = ca::create_tap(&excluded)?;
    let f = ca::tap_format(tap)?;
    println!(
        "tap created: id={tap} format: {} Hz, {} ch, {} bits, flags={:#x}",
        f.mSampleRate, f.mChannelsPerFrame, f.mBitsPerChannel, f.mFormatFlags
    );
    unsafe { objc2_core_audio::AudioHardwareDestroyProcessTap(tap) };
    println!("tap destroyed. probe OK");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--probe") => probe(),
        Some("run") => run(parse_cli(&args[1..])),
        _ => {
            eprintln!("usage: tap-spike --probe | run [--bypass] [--fc HZ] [--gain DB] [--q Q] [--secs N]");
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("FATAL: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 2: Compile + unit tests still green**

Run: `cargo test --manifest-path spikes/tap-spike/Cargo.toml`
Expected: `3 passed`. Then `cargo build --release --manifest-path spikes/tap-spike/Cargo.toml` — compiles.

- [ ] **Step 3: Manual verification protocol (run from Terminal.app, music playing)**

```bash
cargo run --release --manifest-path spikes/tap-spike/Cargo.toml -- run --fc 80 --gain 9 --q 1
```

Verify each, noting results for Task 4:
1. **Audio flows**: music audible; `nonzero` counter climbing, `peak_in > 0`. All-zero forever ⇒ TCC not granted (see Task 2 Step 4) — fix and rerun.
2. **EQ audible**: obvious bass boost at 80 Hz/+9 dB. Rerun with `--bypass` — boost gone.
3. **Volume keys**: press volume up/down/mute while running — HUD appears, level changes, mute works. **This is the whole point of the architecture.**
4. **Teardown**: Ctrl-C — "system audio back to normal" and unprocessed audio continues on the device.
5. **Kill -9 safety**: run again, `kill -9` the process — confirm system audio returns by itself (macOS removes the tap mute when the owning process dies). Note the result either way.
6. **Device switch**: while running, switch default output (System Settings) — note behavior (expected: our aggregate keeps the old device; audio silent on new device until restart — rebuild-on-change is engine work, stage 3).
7. **Sample-rate note**: if the device is not 48 kHz, note `rate` and whether EQ fc sounds correct.
8. **Latency + CPU**: record the printed `out-in sample delta` ms values and `top -pid $(pgrep tap-spike)` CPU%.

- [ ] **Step 4: Commit**

```bash
git add spikes/tap-spike
git commit -m "spike(tap): full run mode — tap -> aggregate -> single IOProc biquad EQ -> device output"
```

---

### Task 4: Findings doc + GO/NO-GO gate

**Files:**
- Create: `docs/spikes/2026-07-tap-spike.md`

- [ ] **Step 1: Write the findings doc from Task 3's protocol results**

Template (fill every `RESULT:` from the actual runs — no blanks left):

```markdown
# Tap Spike Findings — 2026-07

Spike code: `spikes/tap-spike/` (kept for reference; not a workspace member).
Protocol: docs/plans/2026-07-02-rust-port-foundation.md Task 3 Step 3.

| Check | Result |
|---|---|
| Tap capture works (TCC granted) | RESULT: |
| EQ audible / bypass clean | RESULT: |
| Volume keys + HUD native | RESULT: |
| Ctrl-C teardown restores audio | RESULT: |
| kill -9 restores audio by itself | RESULT: |
| Default-output switch behavior | RESULT: |
| Device rate + fc correctness | RESULT: |
| out→in sample delta (ms), steady state | RESULT: |
| CPU % steady state | RESULT: |
| Single-IOProc output design worked (vs needing ring+second IOProc) | RESULT: |

## Decision

GO / NO-GO for the process-tap architecture: RESULT
If NO-GO: engine stage (spec stage 3) switches AudioSource to the BlackHole+aggregate
fallback per docs/CONTEXT.md; stages 1-2 of the port are unaffected.

## Surprises / knowledge for the engine implementation

- RESULT: (buffer layouts seen, aggregate quirks, anything not in the plan)
```

- [ ] **Step 2: Commit and STOP for review**

```bash
git add docs/spikes/2026-07-tap-spike.md
git commit -m "docs(spike): tap spike findings + go/no-go decision"
```

**⛔ GATE: report the findings to the project owner before any engine (spec stage 3) work begins. Tasks 5–10 below may proceed regardless of the gate outcome.**

---

# STAGE 1 — REPO RESTRUCTURE, FIXTURES, SCAFFOLD, CI

### Task 5: Move the Python prototype to `prototype/`

**Files:**
- Move: `paraeq/`, `app/`, `tests/`, `pyproject.toml` → `prototype/`
- Create: symlink `prototype/targets` → `../targets`

**Interfaces:**
- Produces: importable `paraeq` package via editable install of `prototype/`; `pytest prototype/tests` green. Task 6's generator imports `paraeq.*` from this install.

Why the symlink: `paraeq/correction/target_curves.py:13` computes `TARGETS_DIR = Path(__file__).parent.parent.parent / "targets"` (resolves to `prototype/targets` after the move) and `tests/test_target_curves.py:13` looks for `targets/` as a sibling of `tests/`. The symlink satisfies both with **zero code changes to the oracle**, while `targets/` stays the single top-level source of truth per the spec.

- [ ] **Step 1: Move (history-preserving)**

```bash
mkdir prototype
git mv paraeq app tests pyproject.toml prototype/
ln -s ../targets prototype/targets
git add prototype/targets
```

- [ ] **Step 2: Re-point the editable install**

```bash
source .venv/bin/activate
pip install -e "./prototype[dev,gui]"
```

Expected: `Successfully installed paraeq-0.1.0`.

- [ ] **Step 3: Full oracle suite green**

Run: `pytest prototype/tests -v`
Expected: **128 passed** (pytest picks `prototype/pyproject.toml` as rootdir). Any failure = a missed path assumption; fix before committing (the known-sensitive spots are only TARGETS_DIR and the tests/targets sibling lookup, both covered by the symlink).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor: move Python prototype to prototype/ (oracle role; targets/ symlinked)"
```

---

### Task 6: Golden-fixture generator + committed fixtures

**Files:**
- Create: `prototype/tools/generate_fixtures.py`
- Create: `fixtures/` (generated output, committed)

**Interfaces:**
- Produces the fixture format all Rust DSP tests (stage 2 plan) consume:
  - `fixtures/<stage>/<case>.json` — `{"params": {...}, "scalars": {...}, "arrays": {"<key>": {"file": "<case>.<key>.f64", "len": N, "shape": [...]}}}`
  - `fixtures/<stage>/<case>.<key>.f64` — raw little-endian f64 bytes, C-order.
  - `fixtures/manifest.json` — `{"dtype": "<f8", "numpy": "...", "scipy": "...", "python": "..."}` (no timestamps — regeneration must be byte-identical).

- [ ] **Step 1: Write the generator**

`prototype/tools/generate_fixtures.py`:

```python
#!/usr/bin/env python3
"""Golden-fixture generator: dumps input->output pairs from the Python
prototype (the numerical oracle) for the Rust port's parity tests.

Run:  source .venv/bin/activate && python prototype/tools/generate_fixtures.py
Deterministic: seeded RNG, no timestamps. Rerunning must be byte-identical.
"""
import json
import platform
import shutil
from pathlib import Path

import numpy as np
import scipy

from paraeq.correction.auto_fit import auto_fit_parametric_eq
from paraeq.correction.biquad import (
    biquad_frequency_response,
    biquad_high_shelf,
    biquad_low_shelf,
    biquad_notch,
    biquad_peaking,
)
from paraeq.correction.fir_filter import design_fir_correction
from paraeq.correction.parametric_eq import EQBand, ParametricEQ
from paraeq.correction.target_curves import (
    ANCHOR_FREQS,
    build_anchor_target,
    list_builtin_targets,
    match_closest_target,
)
from paraeq.engine.convolver import OverlapAddConvolver
from paraeq.engine.iir_processor import IIRProcessor
from paraeq.measurement.compensation import apply_compensation
from paraeq.measurement.deconvolution import deconvolve
from paraeq.measurement.frequency_response import (
    average_measurements,
    compute_frequency_response,
    fractional_octave_smooth,
    normalize_to_reference_band,
)
from paraeq.measurement.sweep import generate_inverse_sweep, generate_sweep

ROOT = Path(__file__).resolve().parent.parent.parent  # repo root
OUT = ROOT / "fixtures"
SR = 48000


def save_case(stage: str, name: str, params: dict, arrays: dict, scalars: dict | None = None):
    d = OUT / stage
    d.mkdir(parents=True, exist_ok=True)
    case = {"params": params, "scalars": scalars or {}, "arrays": {}}
    for key, arr in arrays.items():
        arr = np.ascontiguousarray(np.asarray(arr, dtype=np.float64))
        fname = f"{name}.{key}.f64"
        (d / fname).write_bytes(arr.astype("<f8").tobytes())
        case["arrays"][key] = {"file": fname, "len": int(arr.size), "shape": list(arr.shape)}
    (d / f"{name}.json").write_text(json.dumps(case, indent=1, sort_keys=True) + "\n")


def gen_sweep():
    sweep = generate_sweep(0.25, SR)
    inverse = generate_inverse_sweep(sweep, SR)
    save_case("sweep", "basic", {"duration": 0.25, "sample_rate": SR, "f_start": 20.0, "f_end": 20000.0},
              {"sweep": sweep, "inverse": inverse})


def gen_deconvolution():
    rng = np.random.default_rng(42)
    sweep = generate_sweep(0.25, SR)
    ir_true = np.zeros(256)
    ir_true[32] = 1.0
    ir_true[33:97] = rng.standard_normal(64) * 0.05 * np.exp(-np.arange(64) / 16.0)
    recorded = np.convolve(sweep, ir_true)[: len(sweep)]
    ir_out = deconvolve(recorded, sweep, SR)
    save_case("deconvolution", "delta_plus_tail", {"sample_rate": SR},
              {"sweep": sweep, "recorded": recorded, "ir_true": ir_true, "ir_out": ir_out})


def gen_frequency_response():
    rng = np.random.default_rng(43)
    ir = rng.standard_normal((1024, 2)) * np.exp(-np.arange(1024) / 128.0)[:, None]
    freqs, mag_db = compute_frequency_response(ir, SR)
    save_case("fr", "stereo_decay", {"sample_rate": SR, "n_fft": 1024},
              {"ir": ir, "freqs": freqs, "mag_db": mag_db})

    mono = mag_db[:, 0]
    for fraction in (3, 6, 12):
        sm = fractional_octave_smooth(mono, freqs, fraction=fraction)
        save_case("fr", f"smooth_1_{fraction}", {"fraction": fraction},
                  {"freqs": freqs, "mag_db": mono, "smoothed": sm})

    a, b = mono, mono + 6.0
    save_case("fr", "average", {}, {"a": a, "b": b, "avg": average_measurements([a, b])})
    save_case("fr", "normalize", {"low_hz": 200.0, "high_hz": 1000.0},
              {"freqs": freqs, "mag_db": mono,
               "normalized": normalize_to_reference_band(freqs, mono)})


def gen_compensation():
    comp_freqs = np.array([20.0, 100.0, 1000.0, 10000.0, 20000.0])
    comp_gains = np.array([-2.0, 0.5, 0.0, 1.5, -3.0])
    grid = np.geomspace(10.0, 24000.0, 256)
    mag = np.zeros_like(grid)
    out = apply_compensation(mag, grid, comp_freqs, comp_gains)
    save_case("compensation", "edge_hold", {},
              {"comp_freqs": comp_freqs, "comp_gains": comp_gains, "grid": grid,
               "mag_db": mag, "compensated": out})


def gen_targets():
    targets = list_builtin_targets()
    harman = next(t for t in targets if t.name == "harman_ie_2019")
    dense = np.geomspace(20.0, 20000.0, 512)
    edges = np.array([0.5, 5.0, 15.0, 25000.0, 40000.0])  # clamp/extrapolation edges
    save_case("targets", "harman_ie_interp", {"target": "harman_ie_2019"},
              {"curve_freqs": harman.frequencies, "curve_gains": harman.gains_db,
               "dense": dense, "interp_dense": harman.interpolate(dense),
               "edges": edges, "interp_edges": harman.interpolate(edges)})

    rng = np.random.default_rng(44)
    offsets = np.round(rng.uniform(-4.0, 4.0, size=ANCHOR_FREQS.shape[0]), 3)
    custom = build_anchor_target(harman, ANCHOR_FREQS, offsets)
    save_case("targets", "anchor_deviation", {"base": "harman_ie_2019"},
              {"anchor_freqs": ANCHOR_FREQS, "offsets_db": offsets,
               "result_freqs": custom.frequencies, "result_gains": custom.gains_db})

    measured = harman.interpolate(dense) + rng.standard_normal(dense.shape[0]) * 0.75
    best = match_closest_target(dense, measured, targets)
    save_case("targets", "match_closest", {},
              {"measured_freqs": dense, "measured_db": measured},
              scalars={"expected_name": best.name})


def gen_fir():
    rng = np.random.default_rng(45)
    n_bins = 2049
    corr = np.cumsum(rng.standard_normal(n_bins)) * 0.05
    corr -= corr.mean()
    corr = np.clip(corr, -6.0, 6.0)
    for n_taps in (512, 4096):
        for phase in ("linear", "minimum"):
            taps = design_fir_correction(corr, np.array([]), n_taps=n_taps, phase=phase)
            save_case("fir", f"{phase}_{n_taps}", {"n_taps": n_taps, "phase": phase},
                      {"correction_db": corr, "taps": taps})


def gen_biquad():
    cases = []
    grid = np.geomspace(20.0, 20000.0, 128)
    for kind, fc, gain, q in [
        ("peaking", 1000.0, 6.0, 1.0), ("peaking", 80.0, -4.5, 2.0),
        ("peaking", 12000.0, 3.0, 0.7), ("low_shelf", 100.0, 4.0, 0.707),
        ("low_shelf", 250.0, -6.0, 1.0), ("high_shelf", 8000.0, -3.0, 0.707),
        ("high_shelf", 4000.0, 5.0, 0.5), ("notch", 60.0, 0.0, 10.0),
        ("notch", 1000.0, 0.0, 4.0),
    ]:
        if kind == "peaking":
            sos = biquad_peaking(fc, gain, q, SR)
        elif kind == "low_shelf":
            sos = biquad_low_shelf(fc, gain, q, SR)
        elif kind == "high_shelf":
            sos = biquad_high_shelf(fc, gain, q, SR)
        else:
            sos = biquad_notch(fc, q, SR)
        cases.append({"kind": kind, "fc": fc, "gain_db": gain, "q": q,
                      "sos": [float(x) for x in sos[0]]})
    resp_sos = biquad_peaking(1000.0, 6.0, 1.0, SR)
    save_case("biquad", "matrix", {"sample_rate": SR},
              {"resp_freqs": grid,
               "resp_db": biquad_frequency_response(resp_sos, grid, SR)},
              scalars={"cases": cases})


def gen_peq_autofit():
    bands = [EQBand("peaking", 100.0, 5.0, 1.2), EQBand("high_shelf", 9000.0, -3.5, 0.707)]
    peq = ParametricEQ(bands, SR)
    grid = np.geomspace(20.0, 20000.0, 256)
    save_case("peq", "two_band", {"sample_rate": SR},
              {"freqs": grid, "response_db": peq.frequency_response(grid)},
              scalars={"autoeq_export": peq.export_autoeq_format(),
                       "bands": [{"filter_type": b.filter_type, "fc": b.fc,
                                  "gain_db": b.gain_db, "q": b.q} for b in bands]})

    freqs = np.geomspace(20.0, 20000.0, 512)
    seed_bands = [EQBand("peaking", 150.0, 6.0, 2.0), EQBand("peaking", 3000.0, -5.0, 3.0)]
    corr = ParametricEQ(seed_bands, SR).frequency_response(freqs)
    fitted = auto_fit_parametric_eq(corr, freqs, SR, max_bands=4)
    save_case("autofit", "two_peaks", {"sample_rate": SR, "max_bands": 4},
              {"freqs": freqs, "correction_db": corr},
              scalars={"fitted": [{"filter_type": b.filter_type, "fc": float(b.fc),
                                   "gain_db": float(b.gain_db), "q": float(b.q)}
                                  for b in fitted]})


def gen_convolver():
    rng = np.random.default_rng(46)
    fir_l = rng.standard_normal(1024) * np.exp(-np.arange(1024) / 200.0)
    fir_r = fir_l * 0.5
    conv = OverlapAddConvolver([fir_l, fir_r], block_size=512)
    blocks_in = rng.standard_normal((8 * 512, 2))
    out = np.vstack([conv.process(blocks_in[i * 512:(i + 1) * 512]) for i in range(8)])
    save_case("convolver", "stereo_8_blocks", {"block_size": 512},
              {"fir_l": fir_l, "fir_r": fir_r, "input": blocks_in, "output": out})


def gen_iir():
    rng = np.random.default_rng(47)
    sos = np.vstack([biquad_peaking(80.0, 6.0, 1.0, SR),
                     biquad_peaking(1000.0, -4.0, 2.0, SR),
                     biquad_high_shelf(8000.0, 3.0, 0.707, SR)])
    proc = IIRProcessor(SR)
    proc.set_sos(sos, channel=0)
    proc.set_sos(sos, channel=1)
    blocks_in = rng.standard_normal((4 * 512, 2))
    out = np.vstack([proc.process(blocks_in[i * 512:(i + 1) * 512]) for i in range(4)])
    save_case("iir", "cascade_4_blocks", {"sample_rate": SR},
              {"sos": sos, "input": blocks_in, "output": out})


def main():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()
    gen_sweep()
    gen_deconvolution()
    gen_frequency_response()
    gen_compensation()
    gen_targets()
    gen_fir()
    gen_biquad()
    gen_peq_autofit()
    gen_convolver()
    gen_iir()
    (OUT / "manifest.json").write_text(json.dumps({
        "dtype": "<f8",
        "numpy": np.__version__,
        "python": platform.python_version(),
        "scipy": scipy.__version__,
    }, indent=1, sort_keys=True) + "\n")
    n = sum(1 for _ in OUT.rglob("*"))
    print(f"wrote {n} files under {OUT}")


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Run it, verify determinism**

```bash
source .venv/bin/activate
python prototype/tools/generate_fixtures.py
cp -r fixtures /tmp/fixtures-run1
python prototype/tools/generate_fixtures.py
diff -r /tmp/fixtures-run1 fixtures
```

Expected: `wrote N files…` twice; `diff` prints nothing (byte-identical). If diff shows differences, a case is non-deterministic — find the unseeded RNG and fix before committing.

- [ ] **Step 3: Sanity-check contents**

Run: `python -c "import json; m=json.load(open('fixtures/manifest.json')); print(m)"`
Expected: dtype `<f8` + the venv's numpy/scipy/python versions. Also `ls fixtures/` shows the 10 stage dirs.

- [ ] **Step 4: Commit (script + fixtures)**

```bash
git add prototype/tools/generate_fixtures.py fixtures
git commit -m "feat(fixtures): golden-fixture generator + committed oracle outputs for Rust parity tests"
```

---

### Task 7: Cargo workspace + three stub crates

**Files:**
- Create: `Cargo.toml` (workspace root), `rust-toolchain.toml`
- Create: `crates/paraeq-dsp/{Cargo.toml, src/lib.rs, tests/fixtures_smoke.rs}`
- Create: `crates/paraeq-engine/{Cargo.toml, src/lib.rs}`
- Create: `crates/paraeq-coreaudio/{Cargo.toml, src/lib.rs}`
- Modify: `.gitignore`

**Interfaces:**
- Produces: crate names `paraeq-dsp`, `paraeq-engine`, `paraeq-coreaudio` (lib targets `paraeq_dsp`, `paraeq_engine`, `paraeq_coreaudio`); the stage-2 plan adds real modules into these. The fixture-path convention `CARGO_MANIFEST_DIR/../../fixtures` is proven by the smoke test.

- [ ] **Step 1: Workspace root files**

`Cargo.toml`:

```toml
[workspace]
members = [
    "crates/paraeq-coreaudio",
    "crates/paraeq-dsp",
    "crates/paraeq-engine",
]
exclude = ["spikes/tap-spike"]
resolver = "2"

[workspace.package]
edition = "2021"
license = "MIT"
version = "0.1.0"

[workspace.dependencies]
approx = "0.5"
arc-swap = "1"
hound = "3.5"
libc = "0.2"
objc2 = "0.6"
objc2-core-audio = "0.3"
objc2-core-audio-types = "0.3"
objc2-core-foundation = "0.3"
objc2-foundation = "0.3"
proptest = "1"
realfft = "3.5"
rtrb = "0.3"
rustfft = "6.4"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
```

Append to `.gitignore`:

```
# Rust
/target
spikes/*/target
```

- [ ] **Step 2: paraeq-dsp with fixture smoke test**

`crates/paraeq-dsp/Cargo.toml`:

```toml
[package]
name = "paraeq-dsp"
edition.workspace = true
license.workspace = true
version.workspace = true

[dependencies]
realfft = { workspace = true }
rustfft = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
approx = { workspace = true }
proptest = { workspace = true }
serde_json = { workspace = true }
```

`crates/paraeq-dsp/src/lib.rs`:

```rust
//! Pure-math DSP core. Zero platform dependencies (spec constraint:
//! this crate must never import CoreAudio, Tauri, or any OS API).

/// Crate version, used by the desktop app's about dialog later.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
```

`crates/paraeq-dsp/tests/fixtures_smoke.rs`:

```rust
//! Proves the committed golden fixtures are reachable and well-formed from
//! Rust tests (path convention: CARGO_MANIFEST_DIR/../../fixtures).

use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[test]
fn manifest_is_readable_and_f64le() {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures_dir().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["dtype"], "<f8");
    assert!(manifest["numpy"].is_string());
}

#[test]
fn sweep_fixture_array_lengths_match_json() {
    let case: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures_dir().join("sweep/basic.json")).unwrap(),
    )
    .unwrap();
    let arr = &case["arrays"]["sweep"];
    let bin = std::fs::read(fixtures_dir().join("sweep").join(arr["file"].as_str().unwrap())).unwrap();
    assert_eq!(bin.len(), arr["len"].as_u64().unwrap() as usize * 8);
    // 0.25 s at 48 kHz
    assert_eq!(arr["len"].as_u64().unwrap(), 12000);
}
```

- [ ] **Step 3: engine + coreaudio stubs**

`crates/paraeq-engine/Cargo.toml`:

```toml
[package]
name = "paraeq-engine"
edition.workspace = true
license.workspace = true
version.workspace = true

[dependencies]
arc-swap = { workspace = true }
paraeq-dsp = { path = "../paraeq-dsp" }
rtrb = { workspace = true }
thiserror = { workspace = true }
```

`crates/paraeq-engine/src/lib.rs`:

```rust
//! Realtime engine graph (source -> correction -> gain -> sink).
//! Zero Tauri dependencies (spec constraint: daemon-ready seams).

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn depends_on_dsp_crate() {
        assert_eq!(paraeq_dsp::VERSION, super::VERSION);
    }
}
```

`crates/paraeq-coreaudio/Cargo.toml`:

```toml
[package]
name = "paraeq-coreaudio"
edition.workspace = true
license.workspace = true
version.workspace = true

[dependencies]
libc = { workspace = true }
objc2 = { workspace = true }
objc2-core-audio = { workspace = true }
objc2-core-audio-types = { workspace = true }
objc2-core-foundation = { workspace = true }
objc2-foundation = { workspace = true }
thiserror = { workspace = true }
```

`crates/paraeq-coreaudio/src/lib.rs`:

```rust
//! All unsafe CoreAudio FFI lives here — the ONLY macOS-specific crate
//! (spec constraint). Tap lifecycle, devices, listeners land in stage 3.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
```

- [ ] **Step 4: Workspace green**

Run: `cargo test --workspace`
Expected: `fixtures_smoke` 2 passed + engine 1 passed, 0 failed.
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml .gitignore crates
git commit -m "feat(workspace): cargo workspace with paraeq-dsp/engine/coreaudio stubs + fixture smoke test"
```

---

### Task 8: Tauri 2 + React scaffold under `desktop/`

**Files:**
- Create: `desktop/ui/` (Vite React-TS app + Tailwind 4)
- Create: `desktop/src-tauri/` (via `tauri init`, then edited)
- Create: `desktop/src-tauri/Info.plist`, `.taurignore` (repo root)
- Modify: root `Cargo.toml` (add member), `.gitignore`

**Interfaces:**
- Produces: `npm run tauri dev` / `tauri build` working from `desktop/ui`; crate `paraeq-desktop` in the workspace; `Info.plist` carrying `NSAudioCaptureUsageDescription`; min macOS 14.4. Stage-4 plan adds commands/state into `src-tauri`.

- [ ] **Step 1: Frontend scaffold**

```bash
mkdir -p desktop
cd desktop
npm create vite@latest ui -- --template react-ts
cd ui
npm install
npm install @tauri-apps/api
npm install -D @tauri-apps/cli tailwindcss @tailwindcss/vite
```

Then wire Tailwind 4: replace `desktop/ui/src/index.css` content with:

```css
@import "tailwindcss";
```

and add the plugin + fixed dev port in `desktop/ui/vite.config.ts`:

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
});
```

- [ ] **Step 2: Tauri init**

```bash
cd desktop/ui
npx tauri init \
  --directory .. \
  --app-name ParaEQ \
  --window-title "ParaEQ" \
  --frontend-dist ../ui/dist \
  --dev-url http://localhost:5173 \
  --before-dev-command "npm --prefix ../ui run dev" \
  --before-build-command "npm --prefix ../ui run build" \
  --ci
```

This creates `desktop/src-tauri/`. (`--frontend-dist` is relative to `src-tauri/`.)

- [ ] **Step 3: Edit the generated files**

`desktop/src-tauri/Cargo.toml` — set the package name and workspace inheritance (keep the generated `[build-dependencies]` and lib sections):

```toml
[package]
name = "paraeq-desktop"
edition.workspace = true
license.workspace = true
version.workspace = true

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
paraeq-engine = { path = "../../crates/paraeq-engine" }
serde = { workspace = true }
serde_json = { workspace = true }
tauri = { version = "2", features = ["tray-icon"] }
```

`desktop/src-tauri/tauri.conf.json` — ensure these keys (merge into the generated file; leave `build` as written by init):

```json
{
  "productName": "ParaEQ",
  "identifier": "com.paraeq.desktop",
  "bundle": {
    "active": true,
    "targets": ["app", "dmg"],
    "macOS": {
      "minimumSystemVersion": "14.4"
    }
  }
}
```

`desktop/src-tauri/Info.plist` (new file — Tauri merges it into the bundle's Info.plist):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>NSAudioCaptureUsageDescription</key>
  <string>ParaEQ captures system audio to apply your headphone EQ correction. Audio never leaves your Mac.</string>
</dict>
</plist>
```

Root `Cargo.toml` — add the member:

```toml
members = [
    "crates/paraeq-coreaudio",
    "crates/paraeq-dsp",
    "crates/paraeq-engine",
    "desktop/src-tauri",
]
```

`.taurignore` (repo root — keeps the `tauri dev` watcher off non-app paths; known watcher pitfall):

```
crates/**/target
desktop/ui/dist
docs/
fixtures/
prototype/
spikes/
targets/
```

Append to `.gitignore`:

```
# Frontend
desktop/ui/node_modules
desktop/ui/dist
```

- [ ] **Step 4: Verify build + dev smoke**

```bash
cargo test --workspace          # still green, now including paraeq-desktop compile
cd desktop/ui && npx tsc --noEmit && npm run build
```

Expected: workspace tests pass; `tsc` clean; Vite build emits `desktop/ui/dist`.

Manual smoke (requires display): `cd desktop/ui && npx tauri dev` — a "ParaEQ" window opens showing the Vite starter page. Ctrl-C to stop.

- [ ] **Step 5: Commit**

```bash
git add desktop .taurignore Cargo.toml Cargo.lock .gitignore
git commit -m "feat(desktop): Tauri 2 + React + Tailwind scaffold (min macOS 14.4, audio-capture usage string, tray feature)"
```

---

### Task 9: CI on GitHub Actions

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Write the workflow**

Pin `macos-15` explicitly (`macos-latest` is mid-migration to macOS 26 as of 2026-07 — don't ride that wave in CI).

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  rust:
    runs-on: macos-15
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test --workspace

  frontend:
    runs-on: macos-15
    defaults:
      run:
        working-directory: desktop/ui
    steps:
      - uses: actions/checkout@v6
      - uses: actions/setup-node@v6
        with:
          node-version: 24
          cache: npm
          cache-dependency-path: desktop/ui/package-lock.json
      - run: npm ci
      - run: npx tsc --noEmit
      - run: npm run build

  prototype-oracle:
    runs-on: macos-15
    steps:
      - uses: actions/checkout@v6
      - run: python3 -m venv .venv
      - run: .venv/bin/pip install -e "./prototype[dev]"
      - run: .venv/bin/pytest prototype/tests -q
```

- [ ] **Step 2: Local sanity + commit**

Run: `cargo fmt --all --check` and fix any formatting drift first (`cargo fmt --all`).

```bash
git add .github/workflows/ci.yml
git commit -m "ci: macOS workflow — rust fmt/clippy/test, frontend typecheck/build, python oracle suite"
```

Expected on push: all three jobs green (verify on the PR).

---

### Task 10: Rewrite CLAUDE.md + update CONTEXT.md

**Files:**
- Modify: `CLAUDE.md` (full rewrite)
- Modify: `docs/CONTEXT.md` (status + pointers; keep all captured knowledge)

- [ ] **Step 1: Replace `CLAUDE.md` content with:**

```markdown
# CLAUDE.md

Instructions for Claude when working in this repo.

## Project

ParaEQ — open-source headphone measurement and correction EQ for macOS.
**The product is the Rust port**: a Cargo workspace (`crates/`) + Tauri 2/React
app (`desktop/`), built on Core Audio process taps. The old Python/PyQt6
prototype lives in `prototype/` as the **numerical oracle** — it generates the
golden fixtures in `fixtures/` that the Rust DSP core must match. Don't add
features to the prototype.

Read **`docs/CONTEXT.md`** before making changes. Design spec:
**`docs/specs/2026-07-02-rust-port-design.md`**. Current plan:
**`docs/plans/2026-07-02-rust-port-foundation.md`**.

## Commands

```bash
# Rust: build + test everything
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Desktop app (dev)
cd desktop/ui && npx tauri dev

# Frontend only
cd desktop/ui && npx tsc --noEmit && npm run build

# Tap spike (standalone, not a workspace member)
cargo run --release --manifest-path spikes/tap-spike/Cargo.toml -- run

# Python oracle (only for fixtures / verifying prototype behavior)
source .venv/bin/activate
pytest prototype/tests -v
python prototype/tools/generate_fixtures.py   # regenerates fixtures/ — commit the diff deliberately
```

## Project Conventions

- **TDD**: Rust DSP modules are written against failing golden-fixture tests
  first (`crates/paraeq-dsp/tests/`). Engine code gets synthetic-block unit
  tests. GUI (`desktop/ui`) is typecheck + manual smoke.
- **Fixtures are sacred**: `fixtures/` is generated ONLY by
  `prototype/tools/generate_fixtures.py` (deterministic, seeded). Never edit
  fixtures by hand; regenerate and commit script + output together.
- **Crate boundaries** (spec constraints):
  - `paraeq-dsp`: pure math, zero platform deps — no CoreAudio, no Tauri.
  - `paraeq-coreaudio`: the ONLY crate with unsafe CoreAudio FFI.
  - `paraeq-engine`: no Tauri deps (daemon-ready). No locks/allocation on the
    realtime path. Teardown always destroys the tap first (never leave the
    system muted).
  - Forbidden deps: ndarray, scirs2-anything, fundsp.
- **Alphabetical ordering**: imports, dict keys, dep lists where order doesn't
  matter functionally.
- **Context7 for libraries**: verify 3rd-party API usage with
  `npx ctx7@latest library <name> "<question>"` before using/changing it.

## Worktrees

Worktree directory: `.worktrees/` (gitignored). Use for feature branches:
`git worktree add .worktrees/<name> -b feature/<name>`
```

- [ ] **Step 2: Update `docs/CONTEXT.md`**

Edits (keep everything else — especially the aggregate/volume knowledge, which is the documented fallback path):
1. `**Last updated:** 2026-07-02` → add ` (Rust port foundation)` and bump the date if executing later.
2. In **Phased Strategy**, append: `Phase 2 has begun — see docs/specs/2026-07-02-rust-port-design.md (process-tap architecture supersedes BlackHole+aggregate; the aggregate knowledge below is retained as the designed fallback).`
3. In **What's Left → Phase 2**, replace the four bullet sketch with: `In progress. Spec: docs/specs/2026-07-02-rust-port-design.md. Foundation plan: docs/plans/2026-07-02-rust-port-foundation.md. Repo restructured (Python → prototype/), fixtures committed, workspace + desktop scaffold + CI live. Tap spike findings: docs/spikes/2026-07-tap-spike.md.`
4. In **How to Pick Up the Work**, replace item 2's installer-pipeline recommendation with: `Continue the Rust port: next stage per the spec's port order (stage 2: paraeq-dsp against fixtures/).`

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/CONTEXT.md
git commit -m "docs: CLAUDE.md rewritten for the Rust-port world; CONTEXT.md points at spec/plan/spike"
```

---

## Completion Checklist (whole plan)

- [ ] Spike ran on the owner's machine; findings doc filled; GO/NO-GO recorded and reported (Task 4 gate).
- [ ] `pytest prototype/tests` → 128 passed from the new location.
- [ ] `fixtures/` committed, regeneration byte-identical.
- [ ] `cargo test --workspace` and `cargo clippy -D warnings` green.
- [ ] `npx tauri dev` opens a ParaEQ window; `npm run build` + `tsc --noEmit` clean.
- [ ] CI green on push.
- [ ] CLAUDE.md/CONTEXT.md reflect the new world.
- [ ] Merge per superpowers:finishing-a-development-branch; next plan = stage 2 (`paraeq-dsp` port against fixtures).
