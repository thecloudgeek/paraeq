# Room DSP Core — Design Spec

**Date:** 2026-07-15
**Status:** Draft — rescope *direction* approved by the project owner (2026-07-15); this document is pending owner review
**Amends:** `docs/specs/2026-07-02-rust-port-design.md` — the port table (line 100–114), the DSP dependency list (line 116), and the "averaging stays in dB domain" decision (line 104), which is hereby scoped to the coupler path only.

## Summary

ParaEQ rescopes from "headphone correction EQ built around a miniDSP EARS jig" to a general measurement + correction suite for macOS: any measurement device with a cal file, all four transducer types (headphones, IEMs, bookshelf, floorstanding) in one release, behind a unified wizard that branches to a **coupler path** or a **room path**. The Core Audio process-tap architecture is unchanged and correct; the Mac is always the source.

This spec defines the `paraeq-dsp` room modules. Room depth this release is **gated + spatially averaged**: no time alignment, no subwoofer integration, no crossovers — but with clean seams for each.

Three decisions carry the design:

1. **Gating is frequency-dependent, not a fixed gate.** A fixed gate short enough to reject a first reflection (3–6 ms) is blind below 167–333 Hz — blind to exactly the modal region where correction has the most authority. REW's frequency-dependent window (FDW) resolves this with one parameter, and an FDW of half-amplitude width `n_c/f` is provably **exactly** a constant-Q Gaussian smoothing of the *complex* spectrum. That identity turns an O(F·N) per-frequency loop into an O(N log N) convolution on a log-f axis and is the highest-leverage result in the workstream.
2. **Correction authority is confidence-derived, not threshold-derived.** "Full authority below 200 Hz" is unsafe as a blanket rule, because the premise behind it — that rooms are minimum-phase below the transition frequency — is false (see [Authority](#authorityrs--new)). ParaEQ instead derives authority from σ(f), the inter-position standard deviation, which measures the same thing operationally, reproduces the ~200 Hz rule in an untreated room *without hardcoding it*, and adapts to a treated one. No competitor automates this.
3. **Everything new is additive.** The ten existing `paraeq-dsp` modules keep their golden fixtures at their current tolerances. Room behaviour arrives as new functions and new modules, never as modified ones. The single exception is `compensation.rs`, where the fix must land in the Rust and the Python oracle in one commit.

## Decisions Log

| Decision | Choice | Alternatives rejected |
|---|---|---|
| Gating strategy | Frequency-dependent window (FDW) as constant-Q complex Gaussian on a log-f axis, `n_c` cycles | Fixed single gate (blind in the modal region); two-window + splice (kept as `splice.rs` fallback only) |
| FDW implementation | O(N log N) convolution on the log-f axis via the exact identity | O(F·N) per-frequency window loop (3–4 orders of magnitude slower); magnitude smoothing (a different, wrong operation) |
| Correction authority | σ(f)-derived confidence ceiling × Trinnov excursion curve | Hardcoded frequency threshold ("full authority below 200 Hz" — unsafe, see Authority); excess group delay per region (correct, but deferred — see Out of Scope) |
| Spatial averaging | Power/RMS, gated behind a type-level `AlignedSet` that only `align_spl` can construct | dB averaging (retained, coupler path only, fixture-pinned); vector/coherent averaging (rejected + guarded with a hard error) |
| Existing dB averaging | Untouched, coupler path only, `test_fr.rs::averaging_is_in_db_domain` stays green | Unifying the two averaging paths (the discriminating test exists precisely to stop this) |
| Smoothing kernel | Alvarez–Mazorra recursive Gaussian, O(N·K); new modes additive | Replacing the existing boxcar (kernel change ⇒ exact fixture parity impossible); keeping O(N²) boxcar for rooms (2586 ms at 65536 bins; room IRs are 10–100× longer) |
| Target class safety | `TransducerClass` promoted to a **required** argument of `match_closest_target` | Leaving `category: Option<String>` as an advisory field (a speaker measurement can return `harman_oe_2018` and double-apply 11.3 dB of ear gain at 3 kHz); UI-level default (not compiler-enforced) |
| Room target | `rbj_low_shelf(105 Hz, Q=0.71)` + linear tilt (default −0.9 dB/oct, range −0.0…−1.5), plus bundled `bk_1974.csv` | X-curve (explicitly **refused** — scoped to rooms > 125 m³; its −3 dB/oct is 3× the domestic consensus) |
| Cal parsing | REW's rule: "only lines which begin with a number are loaded, others are ignored" | Quote-sniff dispatch + hardcoded `skip(2)` (current; silently drops the first data row on single-header files) |
| Cal normalization | **Never.** Curves are applied as-is, with a regression test | Normalize-to-0-dB-at-1-kHz (would bake the EARS 2.1 dB L/R capsule offset in as a permanent channel imbalance) |
| `deconvolve` shape | Returns `ImpulseResponse { samples, peak, sample_rate }` | Bare `Vec<f64>` (current; takes `_sample_rate` and discards it, so there is no time axis and no gating operation is expressible) |
| Per-channel API | `PerChannel<T>` lands **first** in R3, before any policy code | Retrofitting after authority/autofit are written against a mono API (touches everything twice) |
| Oracle strategy | Four tiers (frozen / scipy-direct / analytic physics / REW characterization) | Writing room DSP in Python first (fake provenance — new Python written the same afternoon for the same purpose is not an independent oracle); skipping fixtures (leaves the highest-risk new code untested) |

## Oracle Strategy and Test Tiers

The four-tier oracle methodology is **decided in the master spec**
(`docs/specs/2026-07-15-measurement-suite-design.md`, *The Oracle and Testing
Methodology*); this section applies it to the room modules and assigns each a tier
in the Module Table below. This is a **clarification of CLAUDE.md, not an amendment.** The rules are not in conflict, and the precedent is already merged: `crates/paraeq-dsp/src/spline.rs` (commit 309d71d) is a `paraeq-dsp` module with no `fixtures/` directory, zero mentions in `generate_fixtures.py`, and no prototype addition — verified instead by analytic invariants (`passes_through_knots`, `n2_is_linear_and_n3_is_parabola`) plus inline pinned scipy values. "Fixtures are sacred" is a **provenance** rule whose own text sanctions editing the generator ("regenerate and commit script + output together"); it is not a freeze on the script.

One honest caveat on the precedent: `spline.rs:1–5` cites `scipy.interpolate.CubicSpline` alone, but even that is not an *independent third-party* oracle — the prototype uses `CubicSpline` too (`prototype/paraeq/correction/target_curves.py:8`). (`fir.rs:1–4` says "Oracles:" **plural** and lists `prototype/paraeq/correction/fir_filter.py` first; scipy is a transitive dependency of the prototype there.) The honest statement of the precedent is: **cite the library the prototype itself delegates to.** For genuinely new room math there is no such library, which is what Tier 3 exists for.

| Tier | Regime | Applies to |
|---|---|---|
| **1 — Frozen** | Existing prototype fixtures at current tolerances (1e-12 coefficient math, ~1e-9 single-FFT). Not regenerated for room work. | The ten existing modules. `test_fr.rs::averaging_is_in_db_domain` is the contract that stops a future "unification" of the two averaging paths. |
| **2 — scipy-direct fixtures** | New fixtures from `generate_fixtures.py` **without importing `prototype/paraeq`**. The generator stays the sole writer; the manifest still records versions. | Windows (`scipy.signal.windows.*`), Alvarez–Mazorra Gaussian (vs `scipy.ndimage.gaussian_filter1d`), RMS averaging, Schroeder backward integration, log-f resampling. |
| **3 — Analytic physics** | Closed-form assertions. **Cannot inherit an oracle's bugs**, which makes this a strict upgrade over parity testing for code we have no oracle for. | Gating, FDW, splice, authority, `PerChannel`. |
| **4 — REW characterization** | One real IR as WAV + REW's own gated/smoothed FR as text; assert within 0.1–0.5 dB. **Never assert bit parity** — REW is closed-source with undocumented window defaults and ships betas weekly. | End-to-end regression only. Does not gate any module. |

**Fix the real pinning defect while we are here.** `fixtures/manifest.json` is `{"dtype": "<f8", "numpy": "2.5.0", "python": "3.13.7", "scipy": "1.18.0"}` — a **generated provenance record**, not a pin: `generate_fixtures.py:235–239` writes `scipy.__version__` at runtime. The only dependency *declaration* is `prototype/pyproject.toml:16` → `"scipy>=1.10"`, a floor. Nothing enforces 1.18.0, and regenerating under a newer scipy silently rewrites the manifest. Since Tier 2 makes scipy a first-class oracle rather than an incidental transitive dep, this becomes load-bearing: **pin `scipy==1.18.0`, `numpy==2.5.0` in `prototype/pyproject.toml`** (a `[project.optional-dependencies] fixtures` extra), and have `generate_fixtures.py` **fail loudly** if the running versions differ from the manifest's rather than overwriting it.

**Note for a future session:** `generate_fixtures.py` imports **eleven** of the prototype's seventeen modules. Never imported: `measurement/noise.py`, `correction/autoeq_db.py`, `audio/*`, `profiles/profile.py`.

## Dependency Graph

`logf.rs` gates FDW, smoothing, splice, and targets. It must land first even though it looks like plumbing. `PerChannel<T>` must land first for a different reason: the policy code in `authority.rs` / `autofit.rs` is the largest new surface, and writing it mono means rewriting it.

```
  PerChannel<T> (lib.rs) ─────────────┐   compensation.rs (rewrite) ── independent;
       │  lands first                 │        feeds measured curves in
       │                              │
  window.rs        logf.rs            │
       │              │               │
       ▼              │               │
  gating.rs ◄─────────┤               │
   (ImpulseResponse,  │               │
    GateSpec, peak)   │               │
       │              │               │
       ├──────────────┼───────────┐   │
       ▼              │           ▼   │
  deconvolution.rs    │        room.rs│
   (reshape)          │      (T60, f_s range —
                      │       informational only)
       ┌──────────────┼──────────────┬───────────────┐
       ▼              ▼              ▼               ▼
    fdw.rs         fr.rs         targets.rs      splice.rs
  (complex       (rms avg,     (TransducerClass,  (FALLBACK —
   Gaussian)      sigma(f),     room target)       fdw.rs subsumes it)
       │          smoothing)         │
       └──────────────┼──────────────┘
                      ▼
                authority.rs ◄──── biquad.rs, peq.rs (existing)
                      │
                      ▼
                autofit.rs  (auto_fit_room — per-channel)
                      │
                      ▼
             peq.rs / fir.rs  (per-channel entry points)
```

## Module Table

| Module | Algorithm | Test tier | Effort | Blocks release |
|---|---|---|---|---|
| `PerChannel<T>` (lib.rs) | Non-empty ordered per-channel container; engine channel index | 3 | hours | **yes** |
| `logf.rs` | 96 pts/oct log grid + 1/48-oct anti-comb decimation prefilter | 2 + 3 | days | **yes** |
| `window.rs` | Rect / Hann / Tukey(α=0.25) / Blackman-Harris, independent left/right | 2 + 3 | hours | **yes** |
| `gating.rs` | Peak detect (argmax + parabolic, 0.5·max-crossing fallback); `GateSpec`; left clamp; resolution + Farina bounds | 3 | days | **yes** |
| `fdw.rs` | Constant-Q Gaussian on the **complex** spectrum, O(N log N) on the log-f axis; brute force as test oracle | 3 | days | **yes** |
| `splice.rs` | Klippel AN39: mean-dB level match → raised-cosine blend → 1-oct RMS auto-tune | 3 | days | no (fallback) |
| `room.rs` | T60 via Schroeder backward integration; `f_s = 2000·√(T60/V)`; returns a **range** | 2 + 3 | days | no |
| `fr.rs` (rework) | `average_measurements_rms`, `align_spl`, `sigma_db`, Alvarez–Mazorra variable smoothing | 2 + 3 | days | **yes** |
| `targets.rs` (rework) | `TransducerClass` required arg; room target = RBJ low-shelf + linear tilt; ship `bk_1974.csv` | 1 + 3 | days | **yes** |
| `authority.rs` | σ(f)-weighted excursion ceiling + REW boost-Q cap + jury stability guard | 3 | days | **yes** |
| `compensation.rs` (rewrite) | REW leading-numeric rule; `CalFile` metadata; outlier validator | 1 + 2 + 3 | hours | **yes** |
| `deconvolution.rs` (reshape) | Return `ImpulseResponse`; Wiener core numerically unchanged | 1 + 3 | hours | **yes** |

---

## `PerChannel<T>` — new, in `lib.rs`

**Purpose.** Every `paraeq-dsp` design function takes **one mono curve** today, and `ParametricEQ` has no channel field (`peq.rs:66–69`: `pub struct ParametricEQ { pub bands: Vec<EQBand>, pub sample_rate: f64 }`). The **engine is already per-channel capable** (`CorrectionConfig::Iir { sos_per_channel }`, per-channel `zi`). So the mono/multi seam sits exactly at the DSP/engine boundary, and the DSP side is the one that must move. This lands **first in R3**: the policy code in `authority.rs` and `autofit.rs` is the largest new surface in the workstream, and writing it against a mono API then retrofitting means touching everything twice.

```rust
/// Ordered per-channel payload. Index is the ENGINE's channel index
/// (0 = left, 1 = right for stereo). Non-empty by construction.
#[derive(Clone, Debug)]
pub struct PerChannel<T>(Vec<T>);

impl<T> PerChannel<T> {
    pub fn new(values: Vec<T>) -> Result<Self, DspError>;          // Err on empty
    pub fn splat(value: T, channels: usize) -> Result<Self, DspError> where T: Clone;
    pub fn channels(&self) -> usize;
    pub fn get(&self, ch: usize) -> Option<&T>;
    pub fn iter(&self) -> std::slice::Iter<'_, T>;
    pub fn map<U>(&self, f: impl FnMut(&T) -> U) -> PerChannel<U>;
    pub fn try_map<U, E>(&self, f: impl FnMut(&T) -> Result<U, E>) -> Result<PerChannel<U>, E>;
    pub fn as_slice(&self) -> &[T];
}
```

New per-channel entry points. **Mono originals are retained verbatim** (Tier 1, fixture-pinned):

| Mono (retained, Tier 1) | Per-channel (new) |
|---|---|
| `autofit::auto_fit_parametric_eq(correction_db, freqs, sr, max_bands, min_gain_db) -> Vec<EQBand>` | `autofit::auto_fit_room(correction: &PerChannel<Vec<f64>>, grid, sr, authority, max_bands, min_gain_db) -> PerChannel<Vec<EQBand>>` |
| `targets::compute_correction(measured_db, target_db) -> Vec<f64>` | `targets::compute_correction_multi(measured: &PerChannel<Vec<f64>>, target_db: &[f64]) -> Result<PerChannel<Vec<f64>>, DspError>` |
| `fir::design_fir_correction(correction_db, freqs, n_taps, sr, phase) -> Vec<f64>` | `fir::design_fir_correction_multi(correction: &PerChannel<Vec<f64>>, …) -> Result<PerChannel<Vec<f64>>, DspError>` |
| `peq::ParametricEQ { bands, sample_rate }` | `peq::MultiChannelPeq { channels: PerChannel<ParametricEQ> }` |

**The target is mono by design.** `compute_correction_multi` takes **one** `target_db` shared across channels. That is the point: correcting every channel to a common target is what removes L/R imbalance. It is also why cal curves must never be normalized — the cal file's per-channel offset is the truth the shared target corrects *against*, and normalizing it away would make the imbalance invisible and therefore permanent. These two decisions are load-bearing on each other.

`MultiChannelPeq::to_sos_per_channel(&self) -> Vec<Vec<[f64; 6]>>` is the exact shape `CorrectionConfig::Iir { sos_per_channel }` wants (`controller.rs:105–116`), so the engine seam needs no change.

**Tests (Tier 3).** `PerChannel::new(vec![])` errors. `splat` → `map` round-trips. `auto_fit_room` on two *identical* curves returns two identical band lists (channel independence); on two *different* curves returns different lists (no accidental sharing). `compute_correction_multi` errors on ragged channel lengths.

**Effort:** hours. **Blocks release:** yes.

---

## `logf.rs` — new

**Purpose.** The shared frequency axis for everything downstream. FDW, smoothing, splice, targets, and authority all operate on it. It lands first.

**Algorithm.** The grid is `f_i = f_min · 2^(i/ppo)` for `i ∈ [0, N)`, with

```
N = floor(ppo · log2(f_max / f_min)) + 1
```

For the standard 20 Hz – 20 kHz at 96 pts/oct: `log2(1000) = 9.965784`, so `N = floor(956.72) + 1 = 957` points, and `f[956] = 20 · 2^(956/96) = 19 910 Hz`. **The last bin falls short of `f_max` by less than one bin spacing.** That is deliberate — the grid is the largest set of `ppo`-spaced points that does not exceed `f_max`. Documented here so nobody re-derives it or "fixes" it into an off-by-one.

**The anti-comb prefilter is a decimation filter, not smoothing.** A linear FFT axis has constant `Δf = sr/n_fft`. At 20 kHz with `n_fft = 65536, sr = 48 kHz`, `Δf = 0.73 Hz` while adjacent log bins are **145 Hz** apart — so naive point-sampling picks one linear bin out of ~200 and aliases the comb structure of reflections into the log curve. (At the other end the log grid is *finer* than `Δf` — 0.145 Hz spacing at 20 Hz — which is harmless oversampling.) Before decimating, average the linear-axis data over each log bin's 1/48-octave neighbourhood. This is exactly a lowpass-before-downsample, and 1/48 octave is chosen because it is REW's finest variable-smoothing setting: it **cannot erase anything the downstream variable smoothing would keep**, making it information-preserving relative to the coarsest operation we perform.

```rust
pub struct LogGrid { /* freqs, f_min, f_max, points_per_octave */ }

impl LogGrid {
    pub const DEFAULT_PPO: u32 = 96;
    pub fn new(f_min: f64, f_max: f64, points_per_octave: u32) -> Result<Self, DspError>;
    pub fn standard() -> Self;                 // 20 Hz .. 20 kHz @ 96 ppo -> 957 points
    pub fn freqs(&self) -> &[f64];
    pub fn len(&self) -> usize;
    pub fn f_min(&self) -> f64;
    pub fn f_max(&self) -> f64;
    pub fn points_per_octave(&self) -> u32;
    /// Octave offset of bin i from f_min: i / ppo. The UNIFORM axis fdw.rs
    /// and fr.rs convolve on -- this is why a log grid buys O(N log N).
    pub fn octave_axis(&self) -> Vec<f64>;
}

#[derive(Clone, Copy, Debug)]
pub enum Prefilter {
    None,
    /// Average over each bin's 1/`fraction`-octave neighbourhood. Default 48.
    AntiComb { fraction: u32 },
}

/// Complex resample -- the ONLY correct input to fdw.rs.
pub fn resample_complex_to_log_grid(
    freqs_linear: &[f64],
    spectrum: &[Complex<f64>],
    grid: &LogGrid,
    prefilter: Prefilter,
) -> Result<Vec<Complex<f64>>, DspError>;

/// Magnitude resample -- for display and for dB-domain operations only.
pub fn resample_db_to_log_grid(
    freqs_linear: &[f64],
    magnitude_db: &[f64],
    grid: &LogGrid,
    prefilter: Prefilter,
) -> Result<Vec<f64>, DspError>;
```

`Complex<f64>` is `num_complex::Complex`, already in the tree via `realfft`. Pure math, no platform dependency — the crate boundary holds.

**Tests.**
- Tier 2: `resample_db_to_log_grid` with `Prefilter::None` matches `np.interp` on `np.logspace` to 1e-12.
- Tier 3 (analytic): `f[0] == f_min` exactly; `f[i+1]/f[i] == 2^(1/ppo)` to 1e-15 for all `i`; `LogGrid::standard().len() == 957`; a flat input resamples flat (any prefilter); a synthetic comb `|1 + 0.7·e^{−jωτ}|` resampled at 20 kHz has bounded ripple with `AntiComb` and demonstrably aliased ripple without it (the test asserts the *difference*, which is the whole reason the prefilter exists).

**Effort:** days. **Blocks release:** yes.

---

## `window.rs` — new

**Purpose.** Taper functions for `gating.rs`, with **independent left and right** kinds — the left window rejects pre-peak junk (harmonic distortion products, deconvolution pre-ringing) while the right window sets frequency resolution. They have different jobs and must be independently selectable.

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowKind {
    BlackmanHarris,
    Hann,
    Rect,
    Tukey { alpha: f64 },
}

impl WindowKind {
    pub const TUKEY_DEFAULT: WindowKind = WindowKind::Tukey { alpha: 0.25 };
    /// Half-window taper of length `n`, index 0 at the gate's INNER edge
    /// (nearest the peak, value 1.0) running to the taper's outer end.
    pub fn half_taper(&self, n: usize) -> Vec<f64>;
}

/// Independent left/right half-windows around the IR peak.
#[derive(Clone, Copy, Debug)]
pub struct WindowSpec { pub left: WindowKind, pub right: WindowKind }

impl Default for WindowSpec {
    // Tukey { alpha: 0.25 } on both sides.
}
```

**Formulas** (scipy `sym=True` convention, denominator `n−1` — matching the existing `fir.rs:92` Hann, which is `0.5 − 0.5·cos(2πi/(n−1))` per `np.hanning`; do not mix conventions within the crate):

| Kind | `w[i]` |
|---|---|
| Rect | `1` |
| Hann | `0.5 − 0.5·cos(2πi/(n−1))` |
| Blackman-Harris | `a₀ − a₁cos(2πi/(n−1)) + a₂cos(4πi/(n−1)) − a₃cos(6πi/(n−1))`, `a = [0.35875, 0.48829, 0.14128, 0.01168]` |
| Tukey(α) | Flat over the central `(1−α)` fraction; cosine-tapered over the outer `α/2` at each end (scipy definition) |

Tukey α = 0.25 is the default because it is the standard compromise for gating: a rectangular gate rings badly in frequency, while a full Hann discards half the gated energy and widens the effective window.

**Tests.**
- Tier 2: fixtures from `scipy.signal.windows.{blackmanharris, hann, tukey}` at `n ∈ {8, 9, 64, 4096}` to 1e-12 (odd and even lengths both — the `n−1` denominator is where off-by-ones live).
- Tier 3 (analytic): symmetry `w[i] == w[n−1−i]`; `Rect` is all-ones; `Tukey{alpha: 0.0} == Rect`; `Tukey{alpha: 1.0} == Hann` (scipy's own identity); `half_taper(1) == [1.0]`; `half_taper(0)` is empty, not a panic.

**Effort:** hours. **Blocks release:** yes.

---

## `gating.rs` — new

**Purpose.** Establish `t = 0` on a measured IR and apply a fixed time gate around it, reporting honestly what the gate can and cannot resolve. This is where `ImpulseResponse` lives.

```rust
/// A measured impulse response with a known time origin.
#[derive(Clone, Debug)]
pub struct ImpulseResponse {
    pub samples: Vec<f64>,
    /// Fractional sample index of the direct-sound peak (parabolic-refined).
    pub peak: f64,
    pub sample_rate: u32,
}

impl ImpulseResponse {
    pub fn peak_index(&self) -> usize;    // peak.round()
    pub fn peak_time_s(&self) -> f64;     // peak / sample_rate
}

#[derive(Clone, Copy, Debug)]
pub struct SweepParams { pub duration_s: f64, pub f1: f64, pub f2: f64 }

#[derive(Clone, Copy, Debug)]
pub struct GateSpec {
    pub left_ms: f64,
    pub right_ms: f64,
    /// Sweep parameters when known, so the left window can be bounded by the
    /// Farina H2 arrival. `None` skips that check (synthetic IRs, MLS).
    pub sweep: Option<SweepParams>,
    pub window: WindowSpec,
}

#[derive(Clone, Debug)]
pub struct GateReport {
    pub applied_left_ms: f64,
    pub applied_right_ms: f64,
    pub clamped_left: Option<LeftClamp>,
    /// 1 / T_right. The ABSOLUTE floor.
    pub min_valid_freq_hz: f64,
    /// The honest, 1/N-octave-aware limit. Always >= min_valid_freq_hz.
    pub resolution_limit_hz: f64,
    pub harmonic_bound_ms: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub enum LeftClamp { PeakTooEarly { peak_ms: f64 }, HarmonicBound { dt2_ms: f64 } }

pub fn detect_peak(samples: &[f64]) -> Result<f64, DspError>;
pub fn apply_gate(ir: &ImpulseResponse, spec: &GateSpec) -> Result<(Vec<f64>, GateReport), DspError>;
pub fn min_valid_freq(right_ms: f64) -> f64;                        // 1000.0 / right_ms
pub fn resolution_limit_hz(t_s: f64, fraction: u32) -> f64;
pub fn farina_h2_bound_s(sweep: &SweepParams) -> f64;
```

**Peak detection.** Primary: `argmax|h|`, then parabolic vertex refinement on `(i−1, i, i+1)`:

```
δ = 0.5·(y[i−1] − y[i+1]) / (y[i−1] − 2y[i] + y[i+1]),   peak = i + δ
```

with `δ` clamped to `[−0.5, 0.5]` and `δ = 0` when the denominator is within 1e-30 of zero.

Fallback: the **0.5·max crossing** — the first index where `|h[i]| ≥ 0.5·max|h|`. A room IR with a strong early reflection or a smeared LF-heavy direct arrival can put `argmax` on a *reflection* rather than the direct sound. Rule: if the first 0.5·max crossing precedes the argmax by **more than 1 ms**, take the crossing (parabolic-refined) as the peak and emit a structured warning `gate.peak_fallback`. More than 1 ms of half-amplitude energy before the argmax means the argmax is not the direct sound.

**The left clamp is mandatory, and this is a ParaEQ-specific trap.** `deconvolve()` puts the IR peak at only **~46–64 ms** (tap latency plus acoustic propagation). REW's 125 ms default left window is therefore **physically impossible** here — without a clamp the gate's start index goes negative and the slice is empty. So:

```
applied_left_ms = min(requested_left_ms, peak_time_ms, farina_h2_bound_ms)
```

with the binding constraint reported in `clamped_left`.

**The two resolution numbers, and why both are reported.** A window of length `T` resolves nothing below about `1/T`. The stricter, honest criterion for 1/N-octave validity is

```
f = (1/T) / (2^(1/2N) − 2^(−1/2N))
```

A 10 ms gate is only 1/6-octave-valid above **~865 Hz**. A gate short enough to reject a first reflection (3–6 ms) is blind below **167–333 Hz** — blind to exactly the modal region where correction has the most authority. This is precisely why "gated + Schroeder-aware" is self-contradictory if "gated" means one fixed gate, and why `fdw.rs` exists. `GateReport` reports both numbers because the `1/T` figure is the absolute floor and the `1/N` figure is the one a user should believe.

**The Farina harmonic bound.** For an exponential sweep, the *k*-th harmonic's impulse response arrives *before* the linear one by

```
dt₂ = T · ln2 / ln(f₂/f₁)          (second harmonic)
```

A 5 s, 20 Hz–20 kHz sweep puts H2 only **502 ms** before the peak (`5 · 0.6931 / 6.9078`). A long left window folds harmonic distortion into the "linear" response. `apply_gate` clamps `left_ms` to `dt₂` whenever `spec.sweep` is `Some`, and reports it.

**Consumers must respect `min_valid_freq_hz`.** `authority.rs` intersects its fit mask with it — never fit a band below the frequency the gate can resolve. This is the link that makes "gated + spatially averaged" an honest claim rather than a slogan: the gate publishes its own limit and the corrector obeys it.

**Tests (Tier 3 — analytic physics, no oracle can be wrong here).**
- **The two-path identity.** `h = δ(t₀) + g·δ(t₀+τ)` has ungated `|H| = |1 + g·e^{−jωτ}|`: a comb with peaks `1+g`, nulls `1−g`, spacing `1/τ`. Assert the ungated response reproduces it to 1e-12, and that a **right window shorter than τ returns it exactly flat** (Rect, to 1e-12).
- `detect_peak` on a synthetic fractional-delay sinc recovers the peak within 0.01 sample.
- Left clamp: an IR with its peak at 46 ms and a requested 125 ms left window returns `applied_left_ms == 46.0`, `clamped_left == Some(PeakTooEarly { .. })`, and a **non-empty** slice.
- `min_valid_freq(6.0) ≈ 166.67`; `resolution_limit_hz(0.010, 6) ≈ 865`; `farina_h2_bound_s(&SweepParams { duration_s: 5.0, f1: 20.0, f2: 20000.0 }) ≈ 0.5017`.
- The 0.5·max fallback fires on an IR whose argmax is a reflection 2 ms after a smaller direct arrival, and does **not** fire on a clean one.

**Effort:** days. **Blocks release:** yes.

---

## `fdw.rs` — new. **The key module.**

**Purpose.** Frequency-dependent windowing: fine frequency resolution in the bass (where modes are real, correctable, and need detail) and quasi-anechoic gating in the treble (where reflections are position-specific junk) — from **one parameter**.

### The identity

An FDW whose half-amplitude width is `n_c` cycles at frequency `f` — a window of duration `n_c/f` seconds centred on the IR peak, re-evaluated per frequency — is **exactly** a constant-Q Gaussian smoothing of the **complex** spectrum:

```
σ_f / f  =  √(2 ln 2) / (π · n_c)

FWHM     =  4 / (π · n_c)          octaves

n_c      =  4N / π                 for a 1/N-octave equivalent
```

Verified numerically to **< 2e-15 dB** against the brute-force per-frequency window. The cycles↔octave conversion is exact, which explains REW's 15-cycle default as `N = π·15/4 = 11.78`, i.e. **1/11.8 ≈ 1/12 octave**, and lets the Advanced drawer present whichever unit the user thinks in.

At 15 cycles the window is **750 ms at 20 Hz** (effectively ungated — full modal detail) and **1.5 ms at 10 kHz** (quasi-anechoic). One number delivers the entire room-depth scope item.

### Why a log-f axis makes it O(N log N)

Because `σ_f/f` is constant, the kernel is **shift-invariant in octave coordinates** `u = log2(f/f_min)`. Since `du/df = 1/(f·ln2)`, a Gaussian in `f` of width `σ_f` centred at `f` maps to a Gaussian in `u` of constant standard deviation

```
σ_u = (σ_f/f) / ln2 = √(2 ln 2) / (π · n_c · ln 2)     octaves
```

Consistency check: `FWHM_u = 2√(2ln2)·σ_u = 4/(π·n_c)` octaves ✓ — the two closed forms agree exactly.

So on `logf::LogGrid::octave_axis()` (a **uniform** axis) the operation is a plain convolution with a fixed real Gaussian kernel of

```
σ_bins = σ_u · ppo
```

At `n_c = 15, ppo = 96`: `σ_u = 1.1774 / (π·15·0.6931) = 0.03605` octaves → **σ_bins = 3.46**. A well-sampled ~3.5-bin kernel on a 957-point axis. FFT-convolve: **O(N log N)** with `N ≈ 957`, versus the naive O(F·N) loop's 957 windows × a 65536-point FFT each — three to four orders of magnitude.

**Grid-resolution guard (derive this once, here).** The kernel must be sampled: require `σ_bins ≥ 1.0`, i.e.

```
n_c ≤ ppo · √(2 ln 2) / (π · ln 2) = 0.5407 · ppo = ppo / 1.849
```

(The denominator is `π·ln2 / √(2 ln 2) = 1.849`, **not** `π·ln2 = 2.177` — dropping the `√(2 ln 2) = 1.177` numerator would wrongly enforce `σ_bins ≥ 1.177` and reject valid `n_c` in `(44, 51]` at `ppo = 96`.) At `ppo = 96` that caps `n_c ≤ 51`. `apply_fdw` returns a structured error when `σ_bins < 1.0`, naming the minimum `ppo` required. The Tier-3 brute-force test therefore runs at **`ppo = 192`** (supporting `n_c ≤ 103`) to cover `n_c` up to 61 without tripping its own guard.

### It MUST operate on the complex spectrum

Magnitude smoothing is a **different, wrong operation**. It discards the phase-dependent cancellation that makes a comb a comb: smoothing `|H|` of a two-path IR fills the nulls symmetrically, while smoothing `H` and *then* taking `|·|` reproduces exactly what a time window does. This is not a refinement — it is the difference between implementing FDW and implementing something else that looks like it on a plot.

**Derotate first.** Phase-rotate the spectrum by `e^{+j2πf·t_peak}` (remove the bulk delay) **before** the log-f resample. Without it, the complex spectrum at 20 kHz with a 50 ms delay winds ~1000 full turns and **no log grid could sample it** — the Gaussian would smooth a rapidly rotating phasor to zero. Derotation is what makes 96 ppo sufficient. Re-apply the rotation afterwards only if a caller needs absolute phase (correction design does not).

### Asymmetry — an honest question, now decided

A symmetric FDW at low frequency needs pre-peak data ParaEQ does not have: 15 cycles at 50 Hz is a 300 ms window, 150 ms of it *before* the peak, against the ~46–64 ms available. So `FdwSpec` carries independent pre/post cycle counts (Acourate's convention):

```rust
#[derive(Clone, Copy, Debug)]
pub struct FdwSpec {
    /// Cycles of window BEFORE the peak. A noise gate on pre-peak artifacts,
    /// NOT a resolution control -- see the causality argument below.
    pub pre_cycles: f64,
    /// Cycles AFTER the peak. This is the one that sets frequency resolution.
    pub post_cycles: f64,
}

impl FdwSpec {
    /// REW's 15-cycle default, made asymmetric to fit ParaEQ's ~46-64 ms of
    /// pre-peak headroom.
    pub const DEFAULT: FdwSpec = FdwSpec { pre_cycles: 3.0, post_cycles: 15.0 };
    pub fn from_octave_fraction(n: f64) -> Self;      // n_c = 4N/pi
    pub fn post_octave_fraction(&self) -> f64;        // N = pi*n_c/4
    pub fn sigma_over_f(cycles: f64) -> f64;          // sqrt(2 ln2)/(pi*cycles)
    pub fn fwhm_octaves(cycles: f64) -> f64;          // 4/(pi*cycles)
    /// The time-domain left gate gating.rs should apply for this spec.
    pub fn left_gate_s(&self, f_min: f64) -> f64;     // pre_cycles / f_min
}

pub fn apply_fdw(
    spectrum: &[Complex<f64>],   // derotated, on `grid`
    grid: &LogGrid,
    spec: &FdwSpec,
) -> Result<Vec<Complex<f64>>, DspError>;

/// The O(F*N) reference: for each grid frequency, window the IR with a
/// symmetric Gaussian of half-amplitude width `n_c/f` and DFT at that one
/// frequency. TEST ORACLE for `apply_fdw`. Never call in product code.
pub fn apply_fdw_bruteforce(
    ir: &ImpulseResponse,
    grid: &LogGrid,
    spec: &FdwSpec,
) -> Result<Vec<Complex<f64>>, DspError>;
```

**The identity is proven for the *symmetric* case.** A time-asymmetric window has a complex (Hermitian-asymmetric) Fourier transform, so its frequency kernel applies a phase shift as well as a smoothing — it is *not* a two-piece Gaussian on the log-f axis, and specifying it as one would be fabrication.

The justification for applying it anyway with `n_c = post_cycles`: **a deconvolved IR is causal.** `h(t) ≈ 0` before the peak — the only things living there are tap-latency noise, deconvolution pre-ringing, and Farina harmonic products. The pre-side of the window therefore multiplies near-zero data and contributes negligibly to the effective kernel. This reframes `pre_cycles` correctly: it is a **noise gate on pre-peak artifacts, not a resolution parameter**, and it is applied in the **time domain** by `gating.rs` as a fixed left gate at `min(pre_cycles/f_min, peak_time, farina_h2_bound)` — frequency-independent, which is exactly what a fixed time gate can express.

**DECIDED (default) [NEEDS DATA]:** retain the asymmetry, default `n_c_pre = 3.0` (see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q3 — the lowest-confidence knob, first to collapse to symmetric if real rooms show no effect). The magnitude of the residual error from treating the asymmetric window as symmetric is not established analytically; it is bounded empirically by the test below, and if that test shows > 0.1 dB, revisit before release.

**Tests (Tier 3 — the brute force *is* the oracle).**
- **`apply_fdw` matches `apply_fdw_bruteforce` to < 2e-15 dB** across `n_c ∈ {3, 5, 15, 30, 61}` at `ppo = 192`, on a synthetic multi-reflection IR. Write the brute force first; ship the fast path against it.
- `n_c → large` (well past any physical window) returns the ungated response.
- Two-path comb + small `n_c` returns flat (agrees with `gating.rs`'s fixed-gate result on the same IR).
- Guard: `apply_fdw` with `σ_bins < 1.0` returns `Err` naming the minimum `ppo`, and does **not** silently produce a wrong answer.
- Complex-vs-magnitude discrimination: smoothing `|H|` of the two-path comb and smoothing `H` then taking `|·|` differ by > 3 dB at the null frequencies. This test exists to make the wrong implementation fail loudly.
- **Asymmetry error bound:** full brute-force asymmetric-window FDW vs (fixed pre-gate + symmetric fast FDW) agree within **0.1 dB** over 20 Hz–20 kHz on an IR carrying realistic pre-peak H2 energy.

**Effort:** days — the highest-value days in the workstream. **Blocks release:** yes.

---

## `splice.rs` — new. **FALLBACK ONLY.**

**Status: build `fdw.rs` first and ship splice only if FDW misses.** FDW subsumes it and tracks perception better: Toole's three-zone model has the ear integrating the first ~50 ms into timbre between 200 Hz and 1 kHz, which a cycles-based window follows naturally (75 ms at 200 Hz, 15 ms at 1 kHz) and a two-window splice cannot. `splice.rs` exists because a two-window pipeline is the conventional fallback, not because it is the plan.

```rust
pub struct SpliceSpec { pub overlap_lo_hz: f64, pub overlap_hi_hz: f64 }

pub struct SpliceReport {
    pub level_offset_db: f64,
    pub rms_error_1oct_db: f64,
}

pub fn splice(
    low: &[f64],    // dB on grid -- the long-window (fine LF) curve
    high: &[f64],   // dB on grid -- the short-window (gated HF) curve
    grid: &LogGrid,
    spec: &SpliceSpec,
) -> Result<(Vec<f64>, SpliceReport), DspError>;
```

**Algorithm (Klippel AN39).**
1. **Level match — MANDATORY, not cosmetic.** `offset = mean_dB(low) − mean_dB(high)` over `[overlap_lo, overlap_hi]`; then `high += offset`. An unmatched splice produced a **13.28 dB step** in testing.
2. **Raised-cosine blend** over the overlap in log-f: for `u ∈ [0,1]` across the overlap in octaves, `wt = 0.5·(1 − cos(π·u))`; `out = (1−wt)·low + wt·high_matched`.
3. **Auto-tune objective:** 1-octave RMS error between the spliced curve and each source inside the overlap; the crossover placement minimizing it wins.

**Tests (Tier 3).** Two curves offset by a known constant splice with zero discontinuity after level matching (max bin-to-bin `|Δ|` beyond the underlying curve's own slope < 1e-12); the *unmatched* splice reproduces the step (proving the level match is load-bearing); blend weights sum to 1 at every overlap bin; `wt(0) == 0` and `wt(1) == 1` exactly.

**Effort:** days. **Blocks release:** no.

---

## `room.rs` — new

**Purpose.** Estimate decay time and report the transition region — **for display and sanity-checking only.** This module does **not** feed authority. That separation is the whole point and is stated again below.

```rust
pub struct DecayEstimate {
    pub t60_s: f64,
    /// The range actually fitted (-5..-25 dB), extrapolated x3 to 60 dB.
    pub t20_s: f64,
    pub fit_r2: f64,
    /// false when SNR or curvature make the fit untrustworthy.
    pub usable: bool,
}

pub enum TransitionSource { Measured { t60_s: f64, volume_m3: f64 }, Fallback }

pub struct TransitionRange {
    pub low_hz: f64,      // 0.5 * f_s
    pub center_hz: f64,   // f_s
    pub high_hz: f64,     // 2.0 * f_s
    pub source: TransitionSource,
}

pub fn schroeder_decay_db(ir: &[f64]) -> Vec<f64>;
pub fn estimate_t60(ir: &ImpulseResponse, band: Option<(f64, f64)>) -> Result<DecayEstimate, DspError>;
pub fn schroeder_frequency(t60_s: f64, volume_m3: f64) -> f64;    // 2000*sqrt(T60/V)
pub fn transition_range(t60_s: Option<f64>, volume_m3: Option<f64>) -> TransitionRange;
```

**Schroeder backward integration.** `E(t) = ∫_t^∞ h²(τ) dτ`, reported as `10·log10(E(t)/E(0))`. Implemented as a reverse cumulative sum of `h²` normalized to its own maximum (which is at `t = 0`). T60 is obtained by fitting a line to the decay curve over **[−5, −25] dB** (T20) and extrapolating ×3, per standard ISO 3382 practice. Set `usable = false` when `fit_r2 < 0.95` or the noise floor is reached before −25 dB.

**Schroeder frequency.** `f_s = 2000·√(T60/V)` (SI: T60 in seconds, V in m³). Example: `T60 = 0.4 s, V = 50 m³` → `2000·√0.008 = 178.9 Hz`.

**Return a RANGE, not a number.** `0.5·f_s … 2·f_s`, because the Schroeder frequency is a statistical boundary whose leading constant is a convention (the 2000 coefficient varies across the literature); a single number implies precision the physics does not have. **Fallback:** center 200 Hz, range 100–400 Hz, when T60 or volume is unknown.

**This does NOT gate authority — read this before wiring it.** `TransitionRange` is a display value and a sanity check (the UI can say "your room's transition is around 180 Hz"; an implausible σ(f) profile can be flagged against it). Authority comes from σ(f) and nothing else. This is precisely the seam through which the refuted minimum-phase reasoning would otherwise creep back in: the transition frequency marks **modal vs. statistical dominance — not minimum-phase vs. non-minimum-phase behaviour.** See [Authority](#authorityrs--new).

**Tests.**
- Tier 2: `schroeder_decay_db` matches a numpy reverse-cumsum reference to 1e-12.
- Tier 3 (analytic): a synthetic IR `h = noise · exp(−t·6.908/T60)` (since −60 dB = `exp(−6.908)`) with a **known** T60 recovers it within 5%; `schroeder_frequency(0.4, 50.0) ≈ 178.9`; `transition_range(None, None)` returns center 200 Hz with `source: Fallback`; an IR that is pure noise returns `usable: false`.

**Effort:** days. **Blocks release:** no.

---

## `fr.rs` — rework (strictly additive)

**Hard constraint.** `compute_frequency_response`, `fractional_octave_smooth`, `average_measurements`, and `normalize_to_reference_band` keep their **exact current behaviour and fixtures**. `test_fr.rs::averaging_is_in_db_domain` must keep passing untouched — that test is the contract that stops a future session from "unifying" the two averaging paths, and the fixture genuinely discriminates (`fixtures/fr/average.*.f64` has `b = a + 6.0` dB at every bin; the dB mean is `a+3.000` where a linear-domain mean would be `a+3.508`, a gap ~5e11× the 1e-12 tolerance).

### Why RMS for rooms and dB for couplers — the honest version

The two estimators are **not** "identical except on position-dependent features." By the power-mean inequality (Jensen / AM–GM):

```
RMS_dB  ≥  dB_avg     ALWAYS,   with equality iff magnitude is IDENTICAL at every position
RMS_dB − dB_avg  ≈  0.1151 · σ_dB²
```

They diverge for **any** feature with spread, including one present at every position. That variance scaling is exactly what makes the choice path-dependent rather than cosmetic:

- A coherent mode with `σ = 0.8 dB` diverges by ~0.07 dB — negligible, which is *why* it looks identical and is safe to treat as such.
- A position-dependent null with `σ = 10 dB` diverges by ~11.5 dB.

**Coupler path keeps dB averaging.** Repeated reseats on one jig produce smooth, low-σ variance and no interference nulls. It is fixture-pinned, oracle-documented as perceptually intuitive for EQ work, and it sits in **REW's own endorsed regime**: ParaEQ smooths 1/6-octave *before* averaging (`prototype/app/wizard/measurement_wizard.py:481–484`), and REW's actual words are *"dB averaging may be useful when averaging smoothed traces to derive an EQ target, with unsmoothed data the dips would have a disproportionate effect on the result."* REW does **not** call dB averaging inappropriate. Note also that `fr::average_measurements` has **zero non-test callers in Rust** today — the issue is a *design decision for new room code*, not a live bug.

**Room path uses power/RMS.** Positions differ by wavelengths; σ reaches ~10 dB at interference nulls, and dB averaging tracks a deep null linearly and unbounded.

Corrected numbers: a **−30 dB null at one of five** positions → **dB-avg = −6.0 dB, power-avg = −0.97 dB**. A −40 dB null at one of three → **−13.3 dB vs −1.76 dB**.

**Power averaging is null-RESISTANT, not null-immune.** With `k` of `N` positions nulled by `d` dB relative to the survivors:

```
P_dB = 10·log10( 1 − (k/N)·(1 − 10^(d/10)) )
```

This decreases monotonically toward the floor `10·log10((N−k)/N)` as `d → −∞` but **never attains it**. The floor is reached within 0.01 dB only by about **−20 dB** depth; at −6 dB depth (N=5, k=1) the value is **−0.705 dB**, not the −0.969 floor. And for **k = N** — a null common to every position (floor bounce, SBIR, a driver notch) — the power average passes the null through **exactly**, which is correct and desirable: a null at every seat is a real feature, not spatial scatter. The bound is benign only because `k/N` is normally small.

### API

```rust
/// Spatially-aligned measurement set. Constructible ONLY via `align_spl`,
/// so a caller cannot power-average unaligned data.
pub struct AlignedSet {
    pub measurements_db: Vec<Vec<f64>>,
    pub offsets_db: Vec<f64>,
    pub reference_band: (f64, f64),
}

/// Remove overall level differences due to different source distances.
/// MANDATORY before any spatial average. Default band: (200.0, 2000.0).
pub fn align_spl(
    measurements_db: &[Vec<f64>],
    freqs: &[f64],
    band: (f64, f64),
) -> Result<AlignedSet, DspError>;

/// Power/RMS spatial average. Room path ONLY.
///   out_dB[i] = 10*log10( (1/N) * sum_j 10^(m_j[i]/10) )
pub fn average_measurements_rms(set: &AlignedSet) -> Vec<f64>;

/// Per-frequency inter-position standard deviation, dB.
/// The confidence signal authority.rs consumes.
pub fn sigma_db(set: &AlignedSet) -> Vec<f64>;

/// Coherent (vector) average. ALWAYS Err for n > 1 -- a tripwire, kept so the
/// error is discoverable rather than the operation reinvented.
pub fn average_measurements_vector(
    measurements: &[Vec<Complex<f64>>],
) -> Result<Vec<Complex<f64>>, DspError>;

pub enum Smoothing {
    /// Bit-exact legacy path: the existing boxcar. Coupler path.
    Fixed(u32),
    /// Constant-Q Gaussian, O(N*K) recursive.
    Gaussian { fraction: f64 },
    /// REW's variable profile: 1/48 oct <100 Hz, 1/6 at 1 kHz, 1/3 >10 kHz.
    Variable,
    None,
}

pub fn smooth(magnitude_db: &[f64], grid: &LogGrid, mode: Smoothing) -> Result<Vec<f64>, DspError>;
```

**`align_spl` is enforced at the type level.** `average_measurements_rms` and `sigma_db` take an `AlignedSet`, which only `align_spl` can construct. REW: *"it is usually best to first use Align SPL to remove overall level differences due to different source distances."* Skipping it does not merely tilt the mean — near positions dominate the power average **and inflate σ(f)**, corrupting the confidence metric `authority.rs` depends on, which would silently make ParaEQ back off from features that are genuinely correctable. That failure is invisible on a plot, so the compiler prevents it instead.

Algorithm: `offset_j = band_mean_j − mean_of_all_band_means`; `m_j −= offset_j`. This removes *relative* level differences while preserving the ensemble's absolute level. The 200–2000 Hz default band sits above the modal region (so position-dependent modal scatter cannot drive the alignment) and below the directivity/air-absorption region. `normalize_to_reference_band` is the existing primitive `align_spl` reuses for the band mean — but it *zeroes* the band mean rather than aligning to the ensemble mean, so `align_spl` is a new function, not a rename.

**Vector averaging is rejected and guarded.** It collapses toward the incoherent floor `−10·log10(N)` once position spread approaches a wavelength (**−10.94 dB at 1.5 kHz for ±40 cm**). `average_measurements_vector` returns `Err` unconditionally for `len > 1`.

**σ(f) is the single best differentiator in this workstream.** One extra pass over data already held. It separates correctable-everywhere features (σ ≈ 0.6–0.8 dB) from position-dependent junk (σ ≈ 10 dB), and it rises toward the diffuse-field asymptote of **5.57 dB** above the Schroeder frequency — so confidence-derived authority **reproduces the ~200 Hz rule without hardcoding it** and adapts to a treated room. No competitor automates this.

### Smoothing: Alvarez–Mazorra recursive Gaussian

The current `fractional_octave_smooth` (`fr.rs:34–59`) is a rectangular boxcar with an **O(N²) inner scan** — it re-scans the entire `freqs` array for every bin. Measured **2586 ms for 65536 bins**. It never bit because headphone IRs are short; room IRs are 10–100× longer and will visibly hang the wizard.

Alvarez–Mazorra gives the correct Gaussian kernel in **O(N·K)**:

```
λ = q² / (2K)
ν = (1 + 2λ − √(1 + 4λ)) / (2λ)
K ≈ 4 passes,  with Getreuer's q-correction mapping desired σ (in bins) to q
```

with published boundary handling. On the uniform `octave_axis`, a constant σ_bins Gaussian **is** constant-Q smoothing — the same coordinate trick `fdw.rs` uses.

**The kernel changes from boxcar to Gaussian, so exact fixture parity is impossible.** Therefore: `Fixed(n)` routes to the **existing boxcar code path**, bit-exact, fixtures untouched. `Gaussian` and `Variable` are new and additive, Tier-2 tested against a Gaussian reference on the log-f axis — a "matches to within the method's accuracy" test, not a parity test, because the AM recursion is by construction an approximation to a true Gaussian. **Budget corrected (Stage 4, from measurement):** the original **1e-3 dB** bar is unattainable *at any K* — the K-pass composite transfer function `(1 + 2λ(1−cos ω))^−K` carries a K-independent second-difference discretization floor against scipy's sampled Gaussian `exp(−σ²ω²/2)` (worst ~3.2% at K=4, still 4–8e-3 dB even at K=64 with a numerically optimal q). The shipped `Gaussian` budgets are the measured method floor **0.03 / 0.06 / 0.13 dB for σ = 2 / 8 / 32 bins at K=4** (Getreuer's `gaussian_conv_am`), and they still discriminate — dropping the q-correction or an off-by-one pass count fails them. `Variable` (a per-bin σ, which no single-σ scipy primitive expresses) is pinned Tier-2 against an independent numpy re-implementation of its exact truncated per-bin convolution at **1e-9 dB** (parity, since both sides run the same closed form), and Tier-3 by a spike-response σ(f) width check at 300 Hz / 1 kHz / 3 kHz.

**The Variable profile is the authority mechanism.** 1/48 oct below 100 Hz, 1/6 at 1 kHz, 1/3 above 10 kHz, log-interpolated in between (fraction as a function of `log f`). REW: variable smoothing *"is recommended for responses that are to be equalised."* Note it is **fine in the bass and coarse in the treble — the inverse of psychoacoustic smoothing.** That inversion is the entire point: it hands the corrector fine detail exactly where the corrector has authority (modal peaks, which are locally minimum-phase poles) and hides detail where it does not (the direct/reflected interference field above transition, where any detail is position-specific).

**Tests.**
- Tier 2: `average_measurements_rms` vs a numpy `10*log10(mean(10**(m/10)))` reference to 1e-12; `sigma_db` vs `np.std(..., ddof=0)`; AM `Gaussian` vs `scipy.ndimage.gaussian_filter1d` at the measured method floor (0.03/0.06/0.13 dB for σ=2/8/32 bins at K=4 — the 1e-3 dB bar is unattainable at any K; see the smoothing section); `Variable` vs an independent numpy per-bin truncated-Gaussian reference at 1e-9 dB.
- Tier 3 (analytic): **a −30 dB null at one of five positions gives dB-avg = −6.0 and power-avg = −0.97** (this test pins both estimators and the corrected numbers); the power-mean inequality `RMS_dB ≥ dB_avg` holds over 10 000 random 5-position vectors with **zero** violations; `k = N` (null at every position, −20 dB) passes through the power average **exactly**; the `0.1151·σ²` gap formula holds to 1% for σ ∈ {0.5, 1, 2, 5}; `average_measurements_vector` on 2 measurements returns `Err`; `Fixed(n)` is byte-identical to `fractional_octave_smooth`; smoothing preserves a flat spectrum exactly in every mode; `align_spl` on inputs differing by a constant returns identical aligned curves and offsets summing to zero.

**Effort:** days. **Blocks release:** yes.

---

## `targets.rs` — rework

### The category error (this is a bug, not a gap)

`match_closest_target` (`targets.rs:193–211`) iterates **every** curve with **no category filter**, despite `category: Option<String>` existing (`targets.rs:18`) and being parsed (`targets.rs:76`). Verified: its only Rust caller today is `crates/paraeq-dsp/tests/test_targets.rs:67`.

Add room targets to `targets/` and it will happily return `harman_oe_2018` for a speaker measurement. Verified against the shipped CSV: `targets/harman_oe_2018.csv` reads `2983.19, 8.30` — **+8.3 dB at 3 kHz**, with a +0.22 dB/oct tilt. B&K's room curve is **−3.0 dB at 3 kHz** with a −0.84 dB/oct tilt. **The 3 kHz gap is 11.3 dB.** That peak is **ear gain**: a headphone bypasses the outer ear and must synthesize it, while a loudspeaker delivers it acoustically via the listener's own ear. Applying a headphone target to a speaker measurement double-applies ~11 dB at the ear's most sensitive frequency. This is a category error, and the type system must make it unrepresentable.

```rust
// TransducerClass is the SHARED cross-crate type. Its canonical definition is
// owned by the decision-engine spec (it is `decide()`'s `bundle.class` input):
// the four physical device kinds, NOT a coarser {InEar, OverEar, Room}. A room
// target is legal for BOTH room kinds; there is no single `Room` variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransducerClass { Bookshelf, Floorstander, InEar, OverEar }

pub struct TargetCurve {
    // ... existing fields ...
    /// Classes this curve is legal for. A SET, not an Option: `flat` is legal
    /// for all four, `diffuse_field` for both headphone classes, `bk_1974` for
    /// both room classes (Bookshelf, Floorstander). Parsed from the comma-
    /// separated `# classes:` header (which supersedes `# category:` — see the
    /// decision-engine spec's Target selection); `any` expands to every class.
    pub classes: Vec<TransducerClass>,
    /// Raw `# category:` header text, retained for round-tripping unknown
    /// categories. `classes` is the typed view.
    pub category: Option<String>,
}

/// REQUIRED class argument -- the compiler enforces the filter.
pub fn match_closest_target<'a>(
    class: TransducerClass,
    measured_freqs: &[f64],
    measured_db: &[f64],
    targets: &'a [TargetCurve],
) -> Result<&'a TargetCurve, DspError>;   // Err if no curve matches `class`
```

**A set, not an `Option`, because `reference` is not a class.** The six bundled CSVs are tagged `reference` (flat, diffuse_field), `over-ear`, and `in-ear`. Mapping `reference → None → legal everywhere` would be wrong: `diffuse_field.csv`'s own description says *"based on B&K Type 5128 head-and-torso simulator"* — it is a **head-measured curve and carries ear gain**, so it is emphatically not legal for rooms. Only `flat` is genuinely class-agnostic (0 dB is 0 dB). Required `# classes:` header, one line each (this is the decision-engine spec's `# classes:` scheme; `Room` is not a class — a room curve names both room kinds explicitly):

| File | New `# classes:` |
|---|---|
| `flat.csv` | `any` (→ Bookshelf, Floorstander, InEar, OverEar) |
| `diffuse_field.csv` | `InEar, OverEar` |
| `harman_oe_2018.csv`, `harman_oe_2018_without_bass.csv` | `OverEar` |
| `harman_ie_2019.csv`, `harman_ie_2019_without_bass.csv` | `InEar` |
| `bk_1974.csv` **(new)** | `Bookshelf, Floorstander` |

The signature change is breaking **on purpose**: making `class` a required positional argument forces every call site to be revisited. There is exactly one Rust call site (a test), so the blast radius is tiny — take the free win now, before room code multiplies the call sites.

**Two fixture consequences a future session will otherwise hit as red tests.**
1. **`test_targets.rs:65` asserts `all.len() == 6`** ("builtin count"; line 64 is the `list_targets` load call). Shipping `bk_1974.csv` makes it **7**. Update the assertion deliberately in the same commit.
2. **`generate_fixtures.py:129` calls `match_closest_target(dense, measured, targets)`** and pins `expected_name`. Adding `bk_1974.csv` grows the Python's `targets` list to 7 and **regenerates `fixtures/targets/match_closest`**. The fixture's `measured` is `harman.interpolate(dense) + noise` around **Harman In-Ear 2019**, so passing `TransducerClass::InEar` on the Rust side keeps `expected_name` unchanged — but that is **luck, not design**: the Python oracle has no class filter at all. Therefore add a **Tier-3** test that cannot come from the oracle: `match_closest_target(TransducerClass::Bookshelf, …)` over the *same* measured data must **not** return a Harman curve. That test is the actual contract.

### Room target generator

```rust
pub struct RoomTargetSpec {
    pub shelf_hz: f64,          // default 105.0
    pub shelf_gain_db: f64,     // default +4.0
    pub shelf_q: f64,           // default 0.71
    pub tilt_db_per_oct: f64,   // default -0.9, valid -0.0 ..= -1.5
    pub pivot_hz: f64,          // default 1000.0
}

impl Default for RoomTargetSpec { /* 105 Hz, +4.0 dB, Q 0.71, -0.9 dB/oct, 1 kHz */ }

pub fn build_room_target(spec: &RoomTargetSpec, freqs: &[f64]) -> Result<TargetCurve, DspError>;
```

Algorithm:

```
gains_db[i] = shelf_db(f_i) + tilt_db_per_oct · log2(f_i / pivot_hz)
```

where `shelf_db` is `biquad::sos_frequency_response_db(&[biquad::low_shelf(shelf_hz, shelf_gain_db, shelf_q, 48000.0)], freqs, 48000.0)` — **reusing the existing, fixture-pinned RBJ designer** rather than introducing a second closed form. The 48 kHz rate is fixed and documented: a target is a *curve*, not a filter, and bilinear warping at a 105 Hz corner is ~0.0002% — utterly negligible, and the shelf is flat by 20 kHz where warping would matter. `build_room_target` returns `Err` if `tilt_db_per_oct` is outside `−1.5 ..= 0.0`.

Ship **`targets/bk_1974.csv`** (`# classes: Bookshelf, Floorstander`) as the bundled measured room curve.

### The X-curve is explicitly REFUSED

Not an omission — a documented refusal, so a future session does not "helpfully" add it. The X-curve (SMPTE ST 202) is scoped to rooms **above 125 m³** (dubbing stages, cinemas), and its **−3 dB/oct is 3× the domestic consensus** (−0.9 dB/oct). Bundling it as a selectable domestic room target would be a category error of exactly the same kind the `TransducerClass` work exists to prevent. Users can import a CSV; ParaEQ will not bundle it or suggest it.

**Tests.** Tier 1: existing target fixtures stay green (with the two deliberate updates above). Tier 3: `match_closest_target(Bookshelf, …)` never returns a Harman curve on Harman-shaped data; `match_closest_target` with a class no bundled curve serves returns `Err`, not a wrong curve; `build_room_target` with `tilt = 0` and `shelf_gain_db = 0` is flat to 1e-9; tilt of −0.9 dB/oct measured between 1 kHz and 2 kHz is −0.9 dB to 1e-9; `build_room_target` rejects `tilt = −2.0`; `flat.csv` parses to all four classes.

**Effort:** days. **Blocks release:** yes.

---

## `authority.rs` — new

**Purpose.** Convert a measured curve plus its confidence into a **frequency-indexed ceiling** on what the corrector is allowed to do, and bound filter Q for both musical and numerical-stability reasons.

### Why authority is confidence-derived, not threshold-derived

The tempting rule is "full authority below 200 Hz, none above." **It is unsafe**, and the reason is precise.

The premise behind it — *"room resonances below the transition frequency are minimum-phase, so correcting amplitude corrects phase"* — is **false as a blanket claim**. It conflates two different objects:

- An **individual room mode** is a single pole pair and **is** minimum-phase. True.
- The **measured in-room response** is the **sum** of many modal contributions plus direct sound. **Summing minimum-phase systems does not preserve minimum phase** — the sum can hit zero where contributions are equal in magnitude and opposite in phase, placing zeros outside the unit circle. Real in-room responses are therefore **mixed-phase throughout the modal region**.

Since it is the *sum* you equalize, "correcting amplitude also corrects phase" does not follow.

**The transition frequency is not the boundary.** REW's help — the source usually cited for the claim — says directly: *"we cannot simply say a response is minimum phase below some specific cutoff"*, and documents **non-minimum-phase regions at 44–56 Hz (below transition)** and **minimum-phase regions at 300–500 Hz (above it)**. Toole's 2015 JAES paper attributes minimum-phase behaviour to loudspeaker **transducers**, never to rooms. The transition frequency marks **modal vs. statistical dominance — not minimum-phase vs. non-minimum-phase behaviour.**

**What survives, and what ParaEQ builds on:**

| Feature | Phase character | ParaEQ policy |
|---|---|---|
| Modal **peak** | A pole; locally minimum-phase | Correct it. A matched parametric filter (matched f₀ and Q) places a zero on the pole, correcting amplitude **and** phase together and reducing ringing **at the measurement position**. |
| Modal **dip** | Destructive interference; **non**-minimum-phase | **Never fill it.** Not correctable by EQ; boosting only wastes headroom. |

Authority must be established **per-region**, never assumed from a frequency threshold. The rigorous per-region test is **excess group delay** (flat ⇒ minimum phase). ParaEQ uses **σ(f)** instead, and the reason it works is that σ(f) measures the same thing *operationally*, with data already in hand:

- **Low σ** ⇒ the feature is present at every position ⇒ it is a property of the loudspeaker, or of a mode dominating the whole listening area ⇒ worth correcting.
- **High σ** ⇒ the feature is an interference artifact of one microphone position ⇒ correcting it makes every other seat **worse**.

And because σ(f) rises toward the diffuse-field asymptote of **5.57 dB** above the Schroeder frequency, a σ-derived ceiling **reproduces the ~200 Hz rule in an untreated room without hardcoding it**, and correctly extends authority higher in a well-treated one. Excess group delay remains available as a post-release refinement (see Out of Scope).

Note also that EQ validity is **single-position**: it attenuates the energy driving a mode rather than changing the room's decay physically. `room.rs`'s `TransitionRange` is display-only and must never be wired into this module.

### API

```rust
pub struct AuthorityPolicy {
    /// Trinnov's SHIPPED excursion curve as (freq_hz, max_abs_db) breakpoints,
    /// log-f interpolated. Default: [(20,10), (150,10), (500,2), (20000,2)]
    /// -- +-10 dB at or below 150 Hz, tapering to +-2 dB by 500 Hz, +-2 dB above.
    pub excursion: Vec<(f64, f64)>,
    /// sigma (dB) at or below which authority is FULL. Default 1.0.
    pub sigma_full_db: f64,
    /// sigma (dB) at or above which authority is ZERO. Default 6.0.
    pub sigma_none_db: f64,
    /// Boosts capped at this fraction of the cut ceiling: cut peaks freely,
    /// fill dips grudgingly. Default 0.5.
    pub boost_ratio: f64,
    /// Never fill a dip narrower than this, at any gain. Default 1/6 octave.
    pub min_dip_width_oct: f64,
}

pub struct AuthorityCurve {
    pub freqs: Vec<f64>,
    pub max_boost_db: Vec<f64>,
    pub max_cut_db: Vec<f64>,
    pub max_q: Vec<f64>,
}

pub fn build_authority(
    grid: &LogGrid,
    sigma_db: &[f64],
    policy: &AuthorityPolicy,
) -> Result<AuthorityCurve, DspError>;

/// REW's gain-dependent boost-Q cap, derived from a 500 ms T60 rule:
///     Q_max = 0.227 * f0 / A,     A = 10^(G/40)
/// Verified against ParaEQ's own biquad.rs coefficients. Doubles as the
/// runtime stability guard.
pub fn max_q_for_boost(f0: f64, gain_db: f64) -> f64;

/// Jury stability test on a designed SOS row: |a2| < 1 && |a1| < a2 + 1.
pub fn is_stable(sos: &[f64; 6]) -> bool;

pub enum Clamp {
    GainToExcursion { from: f64, to: f64 },
    GainToSigma { from: f64, to: f64, sigma_db: f64 },
    QToBoostCap { from: f64, to: f64 },
    DipRefused { width_oct: f64 },
}

/// Clamp a band to the authority curve, reporting every change so the
/// Advanced drawer can explain WHY (each decision visible + overridable).
pub fn clamp_band(band: &EQBand, authority: &AuthorityCurve) -> (EQBand, Vec<Clamp>);
```

**Composition — the order matters:**

1. **Excursion ceiling** `e(f)`: log-f interpolation of the Trinnov breakpoints — **±10 dB at or below 150 Hz, tapering to ±2 dB by 500 Hz, ±2 dB above.**
2. **Confidence weight** `w(f) = clamp((σ_none − σ(f)) / (σ_none − σ_full), 0, 1)`. σ ≤ 1 dB → `w = 1`; σ ≥ 6 dB → `w = 0`.
3. `max_cut(f) = e(f) · w(f)`.
4. `max_boost(f) = boost_ratio · e(f) · w(f)` — **asymmetric by construction.**
5. `max_q(f) = max_q_for_boost(f, max_boost(f))` for boosts. Cuts keep the existing `0.5..=20.0` clamp (`autofit.rs:71`).
6. **Narrow-dip refusal is a pick-time veto, not a gain clamp** — see below.

**The runtime stability guard ParaEQ is missing.** The jury test `|a2| < 1 && |a1| < a2 + 1` exists today **only as build-time proptests** (`tests/test_props.rs:7–42` — four tests × 64 cases, asserting `a2.abs() < 1.0 + 1e-12` and `a1.abs() < 1.0 + a2 + 1e-9`). There is **no runtime guard in library code**: `biquad.rs`'s designers return `[f64; 6]` **infallibly** — no Q clamp, no Nyquist/fc validation, no `Result`. `authority.rs` supplies the missing guard without changing `biquad.rs`'s fixture-pinned signatures.

Worked values: `max_q_for_boost(100.0, 6.0)` → `A = 10^0.15 = 1.4125`, `Q_max = 0.227·100/1.4125 = 16.07`. `max_q_for_boost(100.0, 0.0)` → `A = 1`, `Q_max = 22.7`.

### `autofit.rs` changes (additive)

Today `auto_fit_parametric_eq` picks bands by **symmetric** `residual[i].abs()` (`autofit.rs:19`), applies a global Q clamp of `0.5..=20.0` (`autofit.rs:71`), has **no gain limit at all**, and hardcodes a 20–20000 Hz mask (`autofit.rs:13`). Pointed at a room curve it does exactly what Toole warns automated algorithms do: **it fills nulls.**

```rust
pub fn auto_fit_room(
    correction: &PerChannel<Vec<f64>>,
    grid: &LogGrid,
    sample_rate: f64,
    authority: &AuthorityCurve,
    max_bands: usize,
    min_gain_db: f64,
) -> PerChannel<Vec<EQBand>>;
```

`auto_fit_parametric_eq` stays **byte-identical** for the coupler path (fixture-pinned). `auto_fit_room` changes four things:

**Sign convention — get this right.** `compute_correction(measured, target) = target − measured` (`targets.rs:140–146`). So `residual > 0` means *boost* (measured is **below** target — filling a measured **dip** — dangerous), and `residual < 0` means *cut* (measured is **above** target — a measured **peak** — safe).

1. **Asymmetric picking.** Score `s(i) = |residual[i]|` when `residual[i] < 0` (a cut), and `s(i) = boost_ratio · residual[i]` when `residual[i] > 0` (a boost). A peak of a given size therefore always outranks a dip of the same size.
2. **Narrow-dip veto.** If `residual[idx] > 0`, measure the dip's width by reusing `estimate_q`'s half-amplitude crossing logic (`autofit.rs:50–75`) to get `f_low`/`f_high`; `width_oct = log2(f_high/f_low)`. If `width_oct < policy.min_dip_width_oct`, veto the band and continue to the next candidate.
3. **Gain and Q clamps.** `gain = residual[idx].clamp(−max_cut[idx], max_boost[idx])`; for boosts `q = q.min(max_q_for_boost(fc, gain))`.
4. **Mask.** The hardcoded 20–20000 Hz mask becomes `grid.f_min()..=grid.f_max()`, **intersected with `GateReport::min_valid_freq_hz`** — never fit a band below the frequency the gate can resolve. *This is the link that makes "gated + spatially averaged" honest: the gate publishes its own limit and the corrector obeys it.*

**Belt and braces.** After `EQBand::to_sos`, assert `is_stable(&sos)`; on failure back Q off by 10% and retry up to 8 times, then drop the band and log `authority.band_dropped`. With the Q cap applied this should never fire — if it does, that is a bug, and the structured log says so.

**Tests (Tier 3).**
- **The discriminating test.** A synthetic residual that is a pure **−20 dB narrow dip at 60 Hz**: `auto_fit_room` returns **zero bands** (dip veto), while `auto_fit_parametric_eq` returns a **+20 dB boost**. This is the exact regression the module exists to prevent, and it pins both behaviours.
- σ = 10 dB everywhere → authority zero everywhere → zero bands.
- σ = 0.5 dB with a −8 dB peak at 60 Hz → one cut band, gain −8 dB (inside the ±10 dB excursion at ≤150 Hz).
- σ = 0.5 dB with a −8 dB peak at 2 kHz → one cut band clamped to **−2 dB** (excursion above 500 Hz).
- Excursion interpolation: `e(150) == 10.0`, `e(500) == 2.0`, `e(1000) == 2.0`; monotone non-increasing over the grid.
- `max_q_for_boost(100.0, 6.0) ≈ 16.07`; `max_q_for_boost(100.0, 0.0) ≈ 22.7`.
- **Proptest, 256 cases:** every band `auto_fit_room` returns satisfies `is_stable`, and every gain lies within `[−max_cut, max_boost]` at its centre frequency.
- Bands below `min_valid_freq_hz` are never emitted.

**Effort:** days. **Blocks release:** yes.

---

## `compensation.rs` — rewrite

**Status (2026-07-21):** landed in Stage 2 — the REW leading-numeric rewrite shipped in `compensation.rs` in lockstep with the Python oracle (`compensation.py`) in one commit, with a new single-header Tier-2 fixture pinning the fix (see docs/plans/2026-07-16-rescope-implementation.md, Stage 2 and fixture-plan item 7). The design below is retained as the rationale of record.

### The rule

**REW's own documented rule replaces all format sniffing:** *"Only lines which begin with a number are loaded, others are ignored."* That single rule handles EARS (2 quoted headers), UMIK-1 0-degree (1 header), UMIK-1 90-degree (2 headers), unquoted legacy files, and `*`-comments — uniformly, with **no dispatch at all**.

### Current state (verified by reading the file)

- `compensation.rs:11` dispatches on a leading double-quote: `if first.trim_start().starts_with('"')`.
- `compensation.rs:60` hardcodes `for line in content.lines().skip(2)`.
- UMIK-1 **0-degree files have ONE header line**; **90-degree files have TWO** (`"Sens Factor =…"` plus `"Auto-generated 90-degree calibration file"`).
- Consequence: the parser is **brittle to real-world header variation** and **silently drops the first data row on single-header files**, after which `linear_interp_edge_hold` edge-holds from the wrong point below it.

**Do not state a failure rate.** Verifiers disagreed on whether real UMIK-1 files ship quoted or bare, and the "80% hard-error" statistic is **not established**. The defects that *are* solid are the silent first-row drop on single-header files and general brittleness to header variation. That is enough to justify the rewrite; an invented statistic is not needed and would not survive review.

**The oracle shares the flaw.** `prototype/paraeq/measurement/compensation.py` does the same quote-sniff and `np.loadtxt(filepath, skiprows=2, comments="*", usecols=(0,1))`. **Both must change together in one commit**, or you manufacture an oracle divergence and a red fixture. This is the one place in this spec where a prototype edit is *required* — and it is exactly the workflow CLAUDE.md's *"regenerate and commit script + output together"* prescribes.

**The existing EARS fixture is invariant under the fix** (2 quoted headers + data → the new rule yields the same `n = 7`, and so does the fixed oracle). That is why the fix is safe — **and precisely why the fixture never caught the bug.** Add a **new Tier-2 fixture for a single-header file** to pin the fix.

### API

```rust
pub struct CalFile {
    pub freqs: Vec<f64>,
    /// dB. NEVER normalized -- see below.
    pub gains_db: Vec<f64>,
    /// `Sens Factor =-0.421dB` -> Some(-0.421). Informational; NOT applied.
    pub sens_factor_db: Option<f64>,
    /// `AGain =18dB` -> Some(18.0). Informational; NOT applied.
    pub again_db: Option<f64>,
    /// `SERNO: 7103798` -> Some("7103798").
    pub serial: Option<String>,
    /// Every line NOT loaded, verbatim, in file order.
    pub ignored_lines: Vec<String>,
}

pub fn parse_cal(content: &str) -> Result<CalFile, DspError>;
pub fn load_cal(path: &Path) -> Result<CalFile, DspError>;

pub struct CalWarning { pub freq_hz: f64, pub kind: CalWarningKind }

pub enum CalWarningKind {
    Outlier { value_db: f64, neighbours_db: (f64, f64) },
    /// Exact 0.0000 between non-zero neighbours -- the vendor's known bug.
    SuspectZero { neighbours_db: (f64, f64) },
    NonMonotonicFreq,
    DuplicateFreq,
}

pub fn validate_cal(cal: &CalFile) -> Vec<CalWarning>;
```

**Algorithm.**
1. For each line: trim. If empty → ignore. **Attempt to parse column 1 as a float; on failure, push the raw line to `ignored_lines` and continue.** (This *is* the REW rule.)
2. Split on whitespace **or** comma — UMIK-1 rows are tab-separated, ParaEQ CSV is comma-separated, and one parser now serves both. Take columns 0 and 1; ignore column 2 (phase) when present.
3. Metadata: scan `ignored_lines` (quotes stripped) for `Sens Factor\s*=\s*([-+0-9.]+)`, `AGain\s*=\s*([-+0-9.]+)`, `SERNO:\s*(\S+)`. **Recorded, not applied** — Sens Factor is an absolute-SPL calibration constant and ParaEQ measures *relative* response.
4. Error only on: zero data rows, or a row whose column 1 parses but column 2 does not (a genuinely malformed row, not a header).

This unifies `parse_minidsp` and `parse_paraeq_csv` into one function; the quote-sniff dispatch disappears, and the documented `#`-comment divergence at `compensation.rs:20–24` dissolves with it. `parse_compensation` is retained as a deprecated shim returning `(Vec<f64>, Vec<f64>)` so the existing fixture test keeps its signature.

### NEVER normalize a cal curve

`apply_compensation` subtracts the curve **as-is** (`compensation.rs:109–121` — keep this behaviour exactly). Do **not** add a "helpful" normalize-to-0-dB-at-1-kHz step.

**Why:** the miniDSP EARS encodes a **2.1 dB L/R capsule offset in the curve** — the L and R cal files differ by a real, measured 2.1 dB because the two capsules genuinely differ in sensitivity. Normalizing each channel's curve to 0 dB at 1 kHz would erase that offset and **bake a 2.1 dB channel imbalance into every correction ParaEQ generates from an EARS measurement**. This is the mirror image of the `compute_correction_multi` decision: a shared mono target corrects channel imbalance *only if* the cal curve still carries it.

```rust
#[test]
fn cal_curves_are_never_normalized() {
    // Two curves differing by a constant 2.1 dB must STILL differ by 2.1 dB
    // after parse + apply. Guards the EARS L/R capsule offset.
}
```

### Outlier validator

A **shipping vendor file** (`7005770_90deg.txt`) contains a bogus **`0.0000` at 19.611 Hz** between neighbours of **−3.13** and **−3.11** — sitting squarely inside the band where authority is highest.

- **`Outlier`**: a point deviating from the linear interpolation of its two neighbours by more than `outlier_db` (default **1.0 dB**).
- **`SuspectZero`**: an exact `0.0000` whose neighbours are both non-zero and more than 1 dB from zero. A distinct kind, because exact-zero is the vendor's specific failure signature and deserves its own message.

**Warnings are returned, not raised.** The wizard shows them and offers to interpolate over them. A cal file is *user data* and ParaEQ must not refuse to load it. **Auto-repair is offered, never silent.**

**Tests.** Tier 1: the existing EARS fixture stays green unchanged (and the oracle's fixed parser agrees). Tier 2: new fixtures for a single-header file, a two-header file, tab-delimited and comma-delimited rows — the single-header case pins the dropped-row fix. Tier 3: `cal_curves_are_never_normalized`; the outlier detector fires on the `7005770` pattern (`-3.13, 0.0000, -3.11`) as `SuspectZero` and does **not** fire on a smooth curve; a file with only headers returns `Err`; metadata round-trips (`sens_factor_db`, `again_db`, `serial`); `ignored_lines` preserves every non-data line verbatim.

**Effort:** hours. **Blocks release:** yes.

---

## `deconvolution.rs` — reshape

**Purpose.** Give the deconvolved IR a **time axis**. Today `deconvolve(recorded, sweep, _sample_rate) -> Vec<f64>` (`deconvolution.rs:6`) takes the sample rate and **discards it** — the parameter is literally named `_sample_rate`. Without `t = 0`, **no gating operation is expressible**, which blocks `gating.rs`, `fdw.rs`, and `room.rs`.

```rust
pub fn deconvolve_ir(
    recorded: &[f64],
    sweep: &[f64],
    sample_rate: u32,
) -> Result<ImpulseResponse, DspError>;
```

**The Wiener core is unchanged** — regularized spectral division with `eps = 1e-10 · max power` (`deconvolution.rs:18–19`). This is a **reshape, not a numerical change**. The function computes `peak` via `gating::detect_peak` and packages the result.

`deconvolve` is **retained verbatim**, delegating to `deconvolve_ir(..)?.samples`, so the existing fixture (Tier 1) stays bit-exact and untouched. Mark it `#[deprecated(note = "use deconvolve_ir; this loses the time axis")]`.

`ImpulseResponse` lives in `gating.rs` (it is that module's central type) and is re-exported from the crate root, so `deconvolution` depends on `gating`.

**Tests.** Tier 1: existing deconvolution fixture unchanged, `deconvolve_ir(..)?.samples == deconvolve(..)` bit-for-bit. Tier 3: a synthetic recording built as `sweep ⊛ δ(t₀)` recovers `peak ≈ t₀` within 0.01 sample; `sample_rate` round-trips; `peak_time_s()` matches `t₀` to 1e-9.

**Effort:** hours. **Blocks release:** yes.

---

## Adjacent Work This Spec Hands Off

These are **not** `paraeq-dsp`, but the room path depends on them and a DSP spec that ignored them would leave a future session to rediscover them. Each needs a home in the engine/measurement specs.

| Item | What | Where |
|---|---|---|
| **NaN sanitization** | Copy CamillaDSP's `src/utils/conversions.rs:89–106` pattern — `if !value.is_finite() { invalid_values += 1; *value = 0.0; }` fused into the existing peak-detection pass (one branch/sample), plus a `warn!` with the count. **Do NOT fix this at the clamp**: Rust's `clamp` returning NaN for NaN is documented and intentional (infinity *is* clamped correctly). One NaN poisons DF2T feedback state (`iir.rs:56–60`) permanently; the FIR path flushes after the tail. A NaN-triggered `Correction::reset()` (`iir.rs:66` exists and is realtime-safe) is reasonable belt-and-braces, but **input sanitization at the capture boundary is the primary fix**. Note CamillaDSP's own live device paths pass `check_for_nan: false`, so it shares the exposure — but the pattern is in-tree, tested, and directly liftable. | `paraeq-coreaudio` capture path (`backend.rs`) |
| **Output-level policy** | The prototype's `SWEEP_AMPLITUDE = 0.5` (−6 dBFS) lives at `prototype/app/wizard/measurement_wizard.py:39` — the **playback layer, which is not ported**. `sweep.rs` faithfully matches its oracle `prototype/paraeq/measurement/sweep.py`, which is also unscaled; the peak-1.0 convention is **deliberate** (`noise.py`: *"normalized to a peak of 1.0 so the caller can apply any output amplitude"*), and CLAUDE.md scopes `paraeq-dsp` to pure math. The prototype's actual sweep is **−9 dBFS RMS = 3 dB above REW's −12 dBFS default and 6 dB below REW's −3 dBFS maximum.** This is **not a regression** — it is a **forward-looking requirement**: the −6 dBFS policy has no home in the Rust tree and stage 6 must create one. | Measurement runtime spec |
| **Closed-loop verification** | `tap.rs:26–48` and `:153–154` exclude ParaEQ's own process **so that a measurement sweep plays unprocessed** (spec line 147: *"correction state cannot contaminate the measurement"*). **Do not pre-convolve the sweep with the active correction** — that defeats the design. But measuring the **corrected** output requires the sweep to go *through* the engine, which self-exclusion prevents. The answer is the stage-4 carry-forward: **play the stimulus from a helper child process** (not excluded), per the `afplay` reference in `crates/paraeq-coreaudio/tests/test_hardware.rs`. Note self-exclusion has a documented **fail-open** fallback: if `translate_pid` returns 0 after a 200 ms retry, the exclusion list is empty and ParaEQ's own audio **is** tapped (`tap.rs:154–159`, logs *"watch for feedback"*). | Measurement runtime spec |
| **Spurious mic prompt** | `kAudioSubDeviceInputChannelsKey: 0` on the aggregate sub-device — *"otherwise running our IOProc counts as microphone access and macOS shows a mic permission prompt."* ParaEQ's sub-device dict (`tap.rs:57–61`) is a bare `{kAudioSubDeviceUIDKey}`. This dissolves the known limitation at `backend.rs:121–130` **and** removes a spurious prompt — which now matters **more**, because a spurious prompt is indistinguishable from ParaEQ's *legitimate* measurement-mic prompt. | `paraeq-coreaudio` |
| **Correction-swap clicks** | `chain.rs:89`'s `mem::replace` starts the new processor from **zeroed state**, discarding up to 4095 samples of FIR overlap tail. The reference pattern is `BiquadFilter.setCoefficients(_:setup:resetState:)` with `resetState: false` on incremental edits, which preserves delay state and avoids clicks **without** a crossfade. | Engine spec |

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| **Two-clock topology (biggest unscheduled cost in the rescope).** A UMIK-1 runs at its own fixed rate vs. the output device. The existing spec's *"the Farina method tolerates their small clock skew (prototype proved it)"* was proven for an **ungated coupler magnitude** measurement. **Gating needs a trustworthy `t = 0` and an undistorted IR shape — that claim does not transfer.** | Schedule it explicitly as a prerequisite, not an assumption. Measure skew directly: repeat the same sweep N times and track `detect_peak` drift across repeats. If drift exceeds ~1 sample over a sweep, resample the capture to the playback clock before deconvolution. Gate the room path on this experiment, not on hope. |
| **FDW asymmetry approximation.** The exact identity is proven for a symmetric window; ParaEQ applies it with `n_c = post_cycles` and justifies it by IR causality (`h(t) ≈ 0` before the peak). The residual error is not established analytically. | Bounded empirically by the Tier-3 asymmetry test (< 0.1 dB vs. full brute-force asymmetric windowing on an IR with realistic pre-peak H2 energy). **DECIDED (default) [NEEDS DATA]:** retain the asymmetry, default `n_c_pre = 3.0` (see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q3); revisit before release only if that test exceeds 0.1 dB. |
| **σ(f)-derived authority is novel; no competitor ships it.** If σ(f) proves noisy or unstable across realistic 5–9 position sets, authority becomes erratic. | σ(f) has a known expected shape (0.6–0.8 dB for coherent features; rising to the 5.57 dB diffuse-field asymptote above Schroeder). Validate against `room.rs`'s independently-computed `TransitionRange` as a **sanity check** — if σ(f) crosses its midpoint far from `0.5·f_s…2·f_s`, log `authority.sigma_implausible`. `align_spl` being type-enforced removes the dominant σ-inflation failure mode. |
| **Kernel change breaks the smoothing contract.** Boxcar → Gaussian means exact fixture parity is impossible. | `Fixed(n)` routes to the existing boxcar path bit-exact; new modes are additive and tested against a Gaussian reference at the method's measured accuracy (`Gaussian` 0.03/0.06/0.13 dB at K=4 — the 1e-3 dB target is unattainable at any K; `Variable` 1e-9 dB against an independent numpy reference), not at parity. Tier-1 fixtures never regenerate. |
| **`compensation.rs` oracle divergence.** Fixing `skip(2)` in Rust alone manufactures a red fixture. | Both sides change in **one commit**. The existing EARS fixture is invariant under the fix (verified by inspection); a new single-header fixture pins the actual change. |
| **Targets fixture churn.** Shipping `bk_1974.csv` breaks `test_targets.rs:65` (`all.len() == 6`) and regenerates `fixtures/targets/match_closest`. | Both are called out above as **deliberate** same-commit updates, with a Tier-3 test (`Room` never returns a Harman curve) as the real contract, since the Python oracle has no class filter to inherit one from. |
| **Room IR length.** Room IRs are 10–100× longer than headphone IRs; the O(N²) boxcar measured 2586 ms at 65536 bins and will visibly hang the wizard. | Alvarez–Mazorra O(N·K) for new modes; FDW on a 957-point log axis rather than a 65536-point linear one; the log grid is the performance architecture, not just a convenience. |
| **Competitive clock.** Dirac shipped Mac ART 2026-06-30 ($499/$899). Sonarworks SoundID Reference (€249) measures + corrects system-wide on Mac but **refuses USB microphones** (killing UMIK-1 and EARS). OnlyEQ, iQualize, and eqtune ship ParaEQ's exact tap architecture; **none measure.** The window before one bolts on a sweep is ~12–18 months. | The tap is table stakes, not a moat — measurement + σ(f)-derived authority is the moat. If the date is at risk, **cut room ambition rather than room presence**: correct below the transition only. Dirac sells exactly that as a Limited-Bandwidth tier, so the precedent is commercial, not apologetic. |
| **Latency over-claim.** Measured 46–62 ms with a ~41 ms fixed tap floor (fitting `40.96 ms + 2.0·buffer_period`; 1966-sample floor invariant across a 4× buffer sweep). But `backend.rs:195` computes `out_sample_time − in_sample_time`, which **includes the output device's own safety offset and DAC latency that exist with or without ParaEQ** — so "ParaEQ adds 41 ms" is an **over-claim**; the added latency is not established. Competitors' "~10 ms" claims are arithmetic (OnlyEQ's `estimatedLatency = ioBufferFrames*2/sampleRate`), not measurement; iQualize removed its "Low Latency" toggle because ring capacity *"did not meaningfully reduce latency."* | Do not publish a delta until it is measured against a no-ParaEQ baseline on the same device. Report the measured round-trip honestly in `EngineState.latency_ms`. |

## Open Questions

1. **FDW asymmetry (highest priority). DECIDED (default) [NEEDS DATA]:** retain the asymmetry, default `n_c_pre = 3.0` (see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q3 — the lowest-confidence knob, first to collapse to symmetric if real rooms show no effect); the residual-error bound stays gated on the Tier-3 fast-vs-brute-force asymmetry test. The symmetric-Gaussian identity applied with `n_c = post_cycles` is justified by IR causality, but the residual error is bounded only empirically. If that test exceeds 0.1 dB, either raise `ppo` and model the complex asymmetric kernel properly, or restrict `pre_cycles` further — resolve before release.
2. **Two-clock skew under gating. DECIDED (method) [NEEDS DATA]:** adopt REW's bracketed-timing-marker skew estimate + resample, default on (see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q6 — a known ~12 ppm port, not open research), with `Warn(TwoClock)` as the fallback when no estimate can be formed; confirm the magnitude on the owner's own rig via measurement-suite/9 before hardening. Does UMIK-1-vs-output-device skew perturb `detect_peak` or IR shape enough to matter at a 3–6 ms effective HF window? Still to be measured, but no longer open research. **Schedule the experiment the moment the Stage-4 aggregate exists.**
3. **σ_full / σ_none defaults (1.0 dB / 6.0 dB). DECIDED (starting value) [NEEDS DATA]:** ship as-is (see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q5 — 6.0 sits just above the 5.571 dB diffuse-field asymptote, keep the wide gap), validate on real multi-position sets. Chosen to bracket the known 0.6–0.8 dB coherent and ~10 dB position-dependent regimes. Expose in the Advanced drawer; revisit after the owner's ears-on run.
4. **`boost_ratio = 0.5` and `min_dip_width_oct = 1/6`. OPEN [OWNER]:** owner tuning; reconcile with engine-hardening R1-4 (BOOST_WEIGHT / narrow-dip Q / cut_limit) as ONE autofit-shape decision (see docs/plans/2026-07-16-rescope-implementation.md cross-spec question 2). Principled (cut freely, fill grudgingly) but not tuned. The Trinnov excursion curve is *shipped* and therefore trustworthy; these two numbers are ParaEQ's own.
5. **Room target defaults. OPEN [OWNER]:** owner's ears. Tilt −0.9 dB/oct is the domestic consensus midpoint; shelf +4.0 dB at 105 Hz / Q 0.71 is a starting point, not a measured preference.
6. **Number of measurement positions. DECIDED (2026-07-21):** 9 room / 5 coupler, ceiling 15, floor 5, hard-min 3 (see docs/decisions/2026-07-21-decision-engine-open-questions.md §Q7; the wizard owns the per-class defaults). The averaging math is specified for arbitrary N; the `10·log10((N−k)/N)` floor argues for N ≥ 5 (a single null costs ≤ 0.97 dB), and 9 lands on the knee of the mean-convergence curve while 15 sits near the half-wavelength decorrelation limit.
7. **`splice.rs` at all. OPEN [DESIGN — decided by the Stage-4 FDW test]:** build splice only if the FDW brute-force asymmetry test misses; do not build speculatively. If FDW lands clean, splice is dead code.

## Out of Scope (this release)

- **Time alignment, subwoofer integration, crossovers.** Explicitly deferred by the rescope; `PerChannel<T>` and `ImpulseResponse { peak }` are the seams they will need.
- **Excess-group-delay-based per-region authority.** The rigorous minimum-phase test (flat excess group delay ⇒ minimum phase). σ(f) is the shipping proxy; EGD is the post-release refinement. It requires phase data the FDW path already preserves, so the seam exists.
- **Psychoacoustic (ERB) smoothing.** `Smoothing::None` reserves the enum slot. The `Variable` profile is deliberately the *inverse* of psychoacoustic smoothing, because its job is authority, not perception modelling.
- **The X-curve.** Refused, not deferred — see Targets.
- **Multi-position optimization / MMM.** ParaEQ averages positions; it does not solve for a filter that is optimal across them. That is a different (and much larger) problem.
- **Modal decay correction beyond matched-parametric attenuation.** ParaEQ attenuates energy driving a mode at the measurement position. It does not claim to change the room's decay physically, and the UI must not imply otherwise.
- **Anything not played by the Mac.** The signal path is settled: the Mac is always the source.

## Research Basis

Corrected facts this spec is built on, each of which contradicts a claim that appeared in the research corpus and was adversarially refuted. **A future session must not reintroduce the refuted versions.**

| Refuted claim | Correct fact used here |
|---|---|
| "Rooms are minimum-phase below the transition frequency, so correcting amplitude corrects phase." | An individual mode is a pole pair and **is** minimum-phase, but the **measured** response is a **sum** of modes + direct sound, and summing minimum-phase systems does not preserve minimum phase. Real in-room responses are **mixed-phase throughout the modal region.** REW: *"we cannot simply say a response is minimum phase below some specific cutoff"* — non-minimum-phase at 44–56 Hz, minimum-phase at 300–500 Hz. Toole 2015 JAES attributes minimum-phase behaviour to loudspeaker **transducers**, never to rooms. Modal **peaks** are locally minimum-phase and correctable; modal **dips** are non-minimum-phase and are not. **"Full authority below 200 Hz" is unsafe as a blanket rule.** |
| "An omni mic at the listening position ≈ the pressure the ear receives below ~500 Hz." | At 500 Hz `ka ≈ 0.82` and the ipsilateral ear is **+2.3 dB** above free field. Honest thresholds: ≤0.5 dB needs `f ≲ 270 Hz`; ≤1 dB needs `f ≲ 340 Hz`. **Use ~300 Hz, not 500 Hz.** Also: the head **diffracts strongly** at low frequency — that is *why* ILD is small. Never write "the head does not diffract below 500 Hz." |
| "dB and RMS averaging are identical for any feature present at all positions." | They agree **iff** magnitude is identical at every position (zero variance). Gap ≈ `0.1151·σ_dB²`, and `RMS ≥ dB` always (power-mean inequality). A feature present everywhere **with spread** still diverges. |
| "Power averaging is null-immune, returning exactly `10log10((N−k)/N)`." | That is the **asymptotic floor** as depth → −∞, reached within 0.01 dB only by ~−20 dB. At −6 dB depth (N=5, k=1) the value is **−0.705 dB**, not the −0.969 floor. For **k = N** the power average passes the null through **exactly**. Power averaging is null-**resistant**. |
| "−30 dB null at one of five → −5.0 / −0.79 dB." | **−6.0 dB (dB-avg) / −0.97 dB (power-avg).** The −5.0/−0.79 pair is the **six**-position result. A −40 dB null at one of three → −13.3 vs −1.76. |
| "REW says dB averaging is inappropriate for unsmoothed data." | Overstated. REW: *"dB averaging may be useful when averaging smoothed traces to derive an EQ target, with unsmoothed data the dips would have a disproportionate effect on the result."* ParaEQ smooths 1/6-oct **before** averaging (`measurement_wizard.py:481–484`) — **REW's endorsed regime**. `fr::average_measurements` has **zero non-test callers** in Rust: the issue is **latent** (a design decision for new room code), not a live bug. The actionable neighbour is that **`autofit.rs` has no boost cap**. |
| "CLAUDE.md's rules are jointly unsatisfiable for room DSP." | False. `spline.rs` (commit 309d71d) is already a `paraeq-dsp` module with no fixtures dir, no `generate_fixtures.py` mention, and no prototype addition — verified by analytic invariants plus inline pinned scipy values. "Fixtures are sacred" is a **provenance** rule whose own text sanctions editing the generator. The tiered oracle is a **clarification**, not a resolution of a contradiction. |
| "`fir.rs` cites scipy directly as an oracle, bypassing the prototype." | `fir.rs:1–4` says *"Oracles:"* **plural** and lists `prototype/paraeq/correction/fir_filter.py` **first**; scipy is a **transitive** dep of the prototype. The real precedent is `spline.rs:1–5` (scipy alone) — and even that is not an independent oracle, since the prototype uses `CubicSpline` too. The honest precedent: **cite the library the prototype itself delegates to.** |
| "`fixtures/manifest.json` pins scipy 1.18.0." | It is a **generated provenance record** (`generate_fixtures.py:235–239` writes `scipy.__version__` at runtime). The only declaration is `prototype/pyproject.toml:16` → `"scipy>=1.10"`, a **floor**. **A real latent defect** — this spec requires an actual pin. |
| "`paraeq-dsp` contains no IR windowing." | `fir.rs:85` `windowed_ir()` applies a Hann window to a **designed** IR, live on both arms of `design_fir_correction`. What is true: **nothing time-gates a measured IR.** The module set is exactly: autofit, biquad, compensation, deconvolution, fir, fr, peq, **spline**, sweep, targets. |
| "The Rust port dropped `SWEEP_AMPLITUDE = 0.5` and runs at REW's maximum / 9 dB above default." | The 0.5 lives in the **playback layer** (`measurement_wizard.py:39`), which is **not ported**. `sweep.rs` faithfully matches its oracle, which is also unscaled; peak-1.0 is a **deliberate** convention. The prototype's actual sweep is **−9 dBFS RMS = 3 dB above** REW's default and **6 dB below** its maximum. The real issue is **forward-looking**: the −6 dBFS policy has no home in the Rust tree. |
| "A 20 Hz–20 kHz sweep spends 33.33% of its time in the 2–20 kHz band, per REW." | The **one-third figure is correct** (`ln(10)/ln(1000) = 1/3` exactly) but **do not attribute it to REW**: REW widens a requested range to half-start/twice-end capped at Nyquist, so its sweeps do not show this ratio. REW explicitly warns long sweeps risk tweeter overheating and are **not recommended** for loudspeakers. |
| "AutoEQ preamp = −(max cascaded response) with `PREAMP_HEADROOM = 0.2`." | AutoEQ's `ParametricEQ.txt` preamp is exactly `−compound.max_gain` with **no headroom** (`frequency_response.py:211`). `PREAMP_HEADROOM = 0.2` applies **only** to the GraphicEQ string and the min/linear-phase FIR IRs, subtracting from the normalized equalization **curve**. The README-quoted parametric preamp uses a **hardcoded 0.1**. |
| "ParaEQ cannot parse a real UMIK-1 cal file / 80% hard-error." | **Contested — do not state a failure rate.** What is solid: `compensation.rs:11` dispatches on a leading double-quote; `:60` hardcodes `.skip(2)`; 0-degree files have **one** header, 90-degree **two**; the parser is brittle to header variation and **silently drops the first data row on single-header files**. The oracle shares the flaw; **both must change together.** |
| "Fix NaN at the clamp." | Rust's `clamp` returning NaN for NaN is **documented and intentional**; infinity **is** clamped correctly. One NaN poisons DF2T state (`iir.rs:56–60`) permanently; the FIR path flushes. **The fix is input sanitization at the capture boundary** (CamillaDSP `conversions.rs:89–106`). |
| "Pre-convolve the verification sweep with the active correction." | **Backwards.** Tap self-exclusion (`tap.rs:26–48`, `:153–154`) exists **so that** the sweep plays **unprocessed** (spec line 147). Closed-loop verification uses a **helper child process** instead. Self-exclusion **fails open**: if `translate_pid` returns 0 after a 200 ms retry, the exclusion list is empty and ParaEQ's own audio **is** tapped. |

Verified facts the design depends on: the **FDW identity** (< 2e-15 dB); the **gate resolution limit** and **Farina H2 bound**; the **IR peak at ~46–64 ms** (making REW's 125 ms left window impossible); the **target category error** (11.3 dB at 3 kHz; `match_closest_target` has no filter); **smoothing performance** (2586 ms at 65536 bins) and **Alvarez–Mazorra**; **REW variable smoothing** *"recommended for responses that are to be equalised"*; **σ(f)** and the **5.57 dB diffuse-field asymptote**; **Align SPL first** (REW); **vector averaging** collapse (−10.94 dB at 1.5 kHz for ±40 cm); the **REW boost-Q cap** `Q_max = 0.227·f₀/A`; the **jury test** (proptests exist, runtime guard does not); **Trinnov's shipped excursion curve**; the **Klippel AN39 splice** (13.28 dB unmatched step); **Schroeder** `f_s = 2000·√(T60/V)`; the **EARS 2.1 dB L/R capsule offset** and the `7005770_90deg.txt` bogus `0.0000` at 19.611 Hz.
