//! The per-position impulse-response store: raw IRs as f32 WAV, one file per
//! position per channel, plus the derived storage window.
//!
//! Test tier: **Tier 3 (analytic/policy — no oracle)**. `tests/test_store.rs`
//! pins the window derivation at its clamps, the taper's two invariants, a
//! byte-level round-trip through a real temporary directory, and the refusals.
//!
//! # Why this exists at all
//!
//! `Reanalyze` is only a 200 ms operation if the impulse responses are still
//! there. The prototype appends only smoothed post-compensation curves
//! (`measurement_wizard.py:483`) and keeps exactly **one** `_current_ir`
//! regardless of N (`:446`, `:522`), so changing the cal file discards every
//! measurement (`:389-391`) — even though compensation is applied *after* the
//! FFT (`:467`) and could be re-applied for free. **That discard is a storage
//! bug wearing physics as a costume**, and without cached IRs every row of the
//! Reanalyze tier collapses into Recapture, which turns the Advanced drawer
//! into a reason to re-measure. The Rust wizard must not inherit it.
//!
//! # Scope: the IR store, not the profile store
//!
//! The measurement-suite architecture diagram splits these deliberately —
//! `crates/paraeq-measure` owns "per-pos IR store", `desktop/src-tauri` owns
//! the "profile store" (the JSON manifest, the verbatim cal, `Overrides`, the
//! last `DecisionSet`, and the app-data directory layout). This module is the
//! first half only. It is `paraeq-measure`'s because the bytes come off the
//! capture path, and it stops short of the manifest because that composes
//! `paraeq-decide` types and belongs above both crates. The desktop wiring is
//! sequenced post-merge with the rest of it.

use crate::MeasureError;
use paraeq_dsp::gating::ImpulseResponse;
use paraeq_dsp::sweep::apply_fade;
use std::path::{Path, PathBuf};

/// Pre-peak extent of the stored window, ms.
///
/// **Cross-spec reconciliation (2026-07-25).** decision-engine says
/// `[peak − 100 ms, …]`; wizard says `[peak − min(64 ms, peak_index), …]`.
/// Taking the larger is free — 36 ms of f32 at 48 kHz is 7 KB per channel —
/// and it is the one that satisfies both specs' stated reason: 100 ms
/// comfortably covers the whole `left_window_ms` domain. Both agree it is
/// clamped by the peak index, and that clamp is not a fallback but the normal
/// case: `deconvolve` puts the IR peak at only ~46–64 ms here (tap latency
/// plus propagation), which is also why REW's 125 ms left window is
/// *physically impossible* on this architecture.
///
/// The Farina harmonic products are the reason there is any pre-peak bound at
/// all: for a 5 s 20 Hz–20 kHz sweep, H2 arrives `T·ln2/ln(f2/f1) = 502 ms`
/// **before** the peak, so any window this side of half a second excludes the
/// distortion products from what is claimed to be the linear response.
pub const STORE_PRE_MS: f64 = 100.0;

/// Post-peak extent of the stored window, ms.
///
/// **Cross-spec reconciliation (2026-07-25).** decision-engine says
/// `+1100 ms` (the `right_window_ms` domain ceiling of 1000 ms plus margin);
/// wizard says `+1500 ms`, derived from the drawer's FDW domain — the largest
/// post-peak extent any in-scope decision can request is `n_c / f_min`, so
/// 1.5 s bounds the cycles domain at `n_c ≤ 20 × 1.5 = 30`, comfortably
/// containing the 15-cycle default and REW's usable range. The wizard's
/// derivation binds a domain the decision-engine's does not, and the larger
/// number satisfies both, so 1500 wins.
///
/// The wizard marks the figure **OPEN \[NEEDS DATA\]** — it should be
/// re-derived against real room IR decay, and if a bass-heavy untreated room
/// needs more, the drawer's `n_c` ceiling moves with it. That stays open.
/// Cost at this figure: 1.6 s × 48 kHz × 4 B ≈ 307 KB per position per
/// channel, so a 9-position stereo room profile is ~5.5 MB.
pub const STORE_POST_MS: f64 = 1500.0;

/// Raised-cosine taper applied at each stored edge, ms.
///
/// **This is a duration, not a Tukey α, and the difference is not cosmetic.**
/// The wizard spec words the taper as "a Tukey α = 0.25 taper on the stored
/// edges". A Tukey α is a fraction of the *whole* window, so at these
/// proportions α = 0.25 over a 1.6 s store is a **200 ms** ramp at each end —
/// which would consume the entire 100 ms pre-peak region and another 100 ms
/// past the peak, destroying the direct arrival the window exists to centre
/// on. Any α large enough to matter is large enough to eat the peak, so the
/// taper has to be specified in absolute time. 10 ms is long enough that a
/// hard truncation cannot ring and short enough to stay clear of the peak;
/// [`StoredWindow::plan`] additionally clamps it to half the realized
/// pre-peak extent, so it can never reach the peak even when the IR starts
/// early.
///
/// It is belt-and-braces in normal use: the analysis gate's own
/// `right_window_ms` ceiling is 1000 ms, so a gate never reaches the stored
/// edge. It matters only when something FFTs the stored buffer directly.
pub const STORE_TAPER_MS: f64 = 10.0;

/// Bumped whenever the stored layout changes in a way a reader must notice.
/// Present from v1: a measurement bundle is the most expensive artifact a user
/// owns, and the one thing they must never be asked to regenerate.
pub const SCHEMA_VERSION: u32 = 1;

/// The realized storage window for one IR, in samples — what was actually
/// stored, after the clamps, so a reader can put the peak back where it was
/// without re-deriving anything.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct StoredWindow {
    /// First stored sample's index in the source IR.
    pub start: usize,
    /// Number of samples stored.
    pub len: usize,
    /// The peak's index **within the stored buffer**. This, not the source
    /// index, is what the analysis needs: `t = 0` of the stored file.
    pub peak_in_store: usize,
    /// Realized taper length at each edge, in samples (after clamping).
    pub taper: usize,
}

impl StoredWindow {
    /// Derive the window for an IR of `len` samples with its peak at
    /// `peak_index`, at `sample_rate`.
    ///
    /// Every extent is clamped to what the buffer actually holds — an IR that
    /// starts 40 ms before its peak stores 40 ms of pre-roll, not 100 ms of
    /// which 60 are invented.
    pub fn plan(len: usize, peak_index: usize, sample_rate: u32) -> Self {
        let ms_to_samples = |ms: f64| (ms / 1000.0 * f64::from(sample_rate)).round() as usize;
        let pre = ms_to_samples(STORE_PRE_MS).min(peak_index);
        let start = peak_index - pre;
        let post = ms_to_samples(STORE_POST_MS).min(len.saturating_sub(peak_index + 1));
        let stored = pre + 1 + post;
        // Half the realized pre-roll, so the leading taper can never reach the
        // peak. `min` with the post extent for the same reason at the far end,
        // though that one is never binding at these numbers.
        let taper = ms_to_samples(STORE_TAPER_MS).min(pre / 2).min(post / 2);
        Self {
            start,
            len: stored,
            peak_in_store: pre,
            taper,
        }
    }
}

/// One stored impulse response: the windowed, tapered samples plus everything
/// needed to interpret them.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredIr {
    pub channel: usize,
    pub position: usize,
    pub sample_rate: u32,
    pub samples: Vec<f64>,
    pub window: StoredWindow,
}

/// What can go wrong writing or reading a stored IR.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("stored WAV at {path} has {channels} channels; the store writes mono files")]
    NotMono { channels: u16, path: PathBuf },
    #[error(
        "stored WAV at {path} is {bits}-bit {format}; the store writes 32-bit float and will \
         not silently reinterpret anything else"
    )]
    NotFloat32 {
        bits: u16,
        format: String,
        path: PathBuf,
    },
    #[error("impulse response is empty, or its peak index {peak} is outside its {len} samples")]
    PeakOutOfRange { len: usize, peak: usize },
    #[error(
        "stored WAV at {path} holds {found} samples but the manifest window \
         describes {expected} — the manifest and the file disagree about which \
         capture this is"
    )]
    WindowMismatch {
        expected: usize,
        found: usize,
        path: PathBuf,
    },
    #[error("WAV I/O at {path}: {source}")]
    Wav {
        path: PathBuf,
        #[source]
        source: hound::Error,
    },
}

impl From<StoreError> for MeasureError {
    fn from(e: StoreError) -> Self {
        MeasureError::Capture(e.to_string())
    }
}

/// A directory of per-position IR WAVs.
///
/// One file per position per channel, named `pos<NN>_ch<N>.wav` — zero-padded
/// so a directory listing sorts in capture order, and flat rather than nested
/// because the manifest above this layer is what gives a position its meaning
/// (label, diagnostics, accepted/rejected). **Rejected captures are stored
/// too**: they cost nothing, they make "why did you throw position 4 away?"
/// answerable, and they let a user override a rejection after the fact as a
/// Reanalyze rather than a Recapture. This type does not know or care which is
/// which.
#[derive(Clone, Debug)]
pub struct IrStore {
    root: PathBuf,
}

impl IrStore {
    /// Open (creating if needed) the IR directory at `root`.
    pub fn create(root: impl Into<PathBuf>) -> Result<Self, MeasureError> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| MeasureError::Capture(format!("creating {}: {e}", root.display())))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path one position/channel is stored at.
    pub fn path_for(&self, position: usize, channel: usize) -> PathBuf {
        self.root.join(format!("pos{position:02}_ch{channel}.wav"))
    }

    /// Window, taper, and write one channel of one position.
    ///
    /// Returns the [`StoredIr`] as written — the caller records its
    /// [`StoredWindow`] in the manifest, because a stored file cannot say
    /// where in the original capture it came from.
    ///
    /// # Errors
    ///
    /// `PeakOutOfRange` on an empty IR or a peak outside it; `Wav` on I/O.
    pub fn write(
        &self,
        ir: &ImpulseResponse,
        position: usize,
        channel: usize,
    ) -> Result<StoredIr, StoreError> {
        let peak = ir.peak_index();
        if ir.samples.is_empty() || peak >= ir.samples.len() {
            return Err(StoreError::PeakOutOfRange {
                len: ir.samples.len(),
                peak,
            });
        }
        let window = StoredWindow::plan(ir.samples.len(), peak, ir.sample_rate);
        let mut samples = ir.samples[window.start..window.start + window.len].to_vec();
        // `apply_fade` is the repo's raised-cosine edge taper — the same
        // primitive `stimulus.rs` fades the sweep with — so there is one
        // edge-taper implementation, not two.
        apply_fade(&mut samples, window.taper, window.taper);

        let path = self.path_for(position, channel);
        let spec = hound::WavSpec {
            bits_per_sample: 32,
            channels: 1,
            sample_format: hound::SampleFormat::Float,
            sample_rate: ir.sample_rate,
        };
        let mut writer =
            hound::WavWriter::create(&path, spec).map_err(|source| StoreError::Wav {
                path: path.clone(),
                source,
            })?;
        for &v in &samples {
            // f32 is the storage format the spec chose: ~307 KB per position
            // per channel instead of 614, against an IR whose own dynamic
            // range is set by the capture's 24-bit converter. The cast is the
            // lossy step and it is the intended one.
            writer
                .write_sample(v as f32)
                .map_err(|source| StoreError::Wav {
                    path: path.clone(),
                    source,
                })?;
        }
        writer.finalize().map_err(|source| StoreError::Wav {
            path: path.clone(),
            source,
        })?;

        Ok(StoredIr {
            channel,
            position,
            sample_rate: ir.sample_rate,
            samples,
            window,
        })
    }

    /// Read one position/channel back.
    ///
    /// `window` comes from the manifest: the WAV carries samples and a rate,
    /// not a peak index, so the store refuses to guess one. Reading is
    /// strict about format — a file that is not mono 32-bit float is an error,
    /// never a silent reinterpretation, because misreading an IR's sample
    /// format produces a *plausible* wrong correction rather than an obvious
    /// failure.
    pub fn read(
        &self,
        position: usize,
        channel: usize,
        window: StoredWindow,
    ) -> Result<StoredIr, StoreError> {
        let path = self.path_for(position, channel);
        let mut reader = hound::WavReader::open(&path).map_err(|source| StoreError::Wav {
            path: path.clone(),
            source,
        })?;
        let spec = reader.spec();
        if spec.channels != 1 {
            return Err(StoreError::NotMono {
                channels: spec.channels,
                path,
            });
        }
        if spec.bits_per_sample != 32 || spec.sample_format != hound::SampleFormat::Float {
            return Err(StoreError::NotFloat32 {
                bits: spec.bits_per_sample,
                format: format!("{:?}", spec.sample_format),
                path,
            });
        }
        let samples: Result<Vec<f32>, hound::Error> = reader.samples::<f32>().collect();
        let samples = samples.map_err(|source| StoreError::Wav {
            path: path.clone(),
            source,
        })?;
        // The window comes from the manifest and the samples from the file;
        // if they disagree, `peak_in_store` points somewhere that is not the
        // peak and every gate downstream is applied to the wrong time origin.
        // A wrong t=0 produces a plausible correction, not a visible failure,
        // so this refuses rather than trusting either side.
        if samples.len() != window.len {
            return Err(StoreError::WindowMismatch {
                expected: window.len,
                found: samples.len(),
                path,
            });
        }
        Ok(StoredIr {
            channel,
            position,
            sample_rate: spec.sample_rate,
            samples: samples.into_iter().map(f64::from).collect(),
            window,
        })
    }
}
