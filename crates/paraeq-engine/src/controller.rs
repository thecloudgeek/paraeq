//! Engine controller: a control-plane thread that owns an [`AudioBackend`],
//! drives the silence watchdog, applies commands, and publishes serialized
//! [`EngineState`] snapshots on every change.
//!
//! ## Source of truth across rebuilds
//!
//! `Watchdog::started()` re-baselines against ZEROED counters, so **every**
//! backend start (initial, renegotiation, rebuild-on-change, re-enable)
//! constructs a fresh `RtShared` + ring pair + chain/`RtProcessor`, and the
//! controller re-applies its retained config (bypass, gain, correction,
//! requested buffer frames) to the fresh state. Nothing on the realtime
//! side survives a restart.
//!
//! ## Provisional start + renegotiation
//!
//! The backend needs the `RtProcessor` up front, but the effective
//! [`StreamInfo`] only comes back from `start`. The controller therefore
//! builds the chain provisionally for 2 channels (the parity tap is stereo)
//! and `block_size = requested_buffer_frames.unwrap_or(512)`; if the
//! reported `buffer_frames` OR `channels` differ, it renegotiates with a
//! stop+start cycle that rebuilds the chain at the last-REPORTED geometry
//! and requests exactly the last-reported buffer frames -- looping until
//! the report matches the chain, capped at 3 total starts. If the report
//! is *still* moving after the cap, the mismatch is accepted with a
//! warning: the chain's uniform frame guard degrades gracefully
//! (pass-through + `frame_mismatch` telemetry) until a `FormatChanged`
//! event triggers the next rebuild.
//!
//! ## Realtime discipline
//!
//! This module is control-plane only: allocation is fine here, but nothing
//! in it may block the realtime side. All cross-thread communication is the
//! `Arc<RtShared>` atomics and the SPSC rings -- no locks are shared with
//! `process_block`. Correction processors are built AND warmed up here,
//! then moved through the ring; retired processors come back through the
//! retire ring and are dropped here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use paraeq_dsp::peq::{EQBand, ParametricEQ};

use crate::backend::{AudioBackend, StreamInfo};
use crate::chain::{build_fir, build_iir, Correction, RealtimeChain};
use crate::preamp;
use crate::shared::{links, ControlLink, RtMsg, RtProcessor, RtShared};
use crate::status::{EngineStatus, Watchdog, WatchdogConfig};
use crate::EngineError;

/// Maximum absolute per-band gain, in dB. `|gain_db|` above this is rejected.
pub const GAIN_LIMIT_DB: f64 = 30.0;
/// Maximum band Q. Q above this (or `< Q_MIN`) is rejected.
pub const Q_MAX: f64 = 100.0;
/// Minimum band Q. Q below this is rejected. Set far above the subnormal
/// overflow floor (~1e-309, where `alpha = sin(w0)/(2*q)` overflows to +inf
/// and yields NaN SOS coefficients) while still admitting the full realistic
/// audio Q range (~0.1-100).
pub const Q_MIN: f64 = 0.1;

/// Serializable correction description -- the controller-retained source of
/// truth. Rebuilt into a live [`Correction`] (with warm-up) for the current
/// stream geometry, AT THE LIVE STREAM RATE, on every apply.
///
/// ## `design_rate` means two different things (R1-6 / Open Q1)
///
/// [`CorrectionConfig::Peq`] carries design *intent* (bands), so
/// [`build_correction`] re-derives coefficients at whatever rate the stream
/// reports and a correction SURVIVES a rate switch -- a 47 Hz mode filter
/// stays at 47 Hz across an AirPods 44.1<->48 kHz handoff. There its
/// `design_rate` is **provenance only** and is never compared.
///
/// [`CorrectionConfig::Fir`] and [`CorrectionConfig::Iir`] carry baked
/// coefficients, which cannot be re-derived (`design_fir_correction` takes a
/// positional magnitude vector with no Hz grid; an SOS row has forgotten the
/// band it came from). There `design_rate` is a **compare key**: a mismatch
/// against the live rate is refused and the chain fails open to flat
/// pass-through, because audibly un-EQ'd beats audibly wrong-EQ'd.
///
/// See `docs/decisions/2026-07-21-decision-engine-open-questions.md` §Q1 and
/// `docs/specs/2026-07-15-engine-hardening-design.md:403`.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub enum CorrectionConfig {
    Fir {
        design_rate: f64,
        firs: Vec<Vec<f64>>,
    },
    Iir {
        design_rate: f64,
        sos_per_channel: Vec<Vec<[f64; 6]>>,
    },
    Peq {
        /// Per channel; the last set is broadcast to any remaining channels.
        bands: Vec<Vec<EQBand>>,
        design_rate: f64,
    },
}

impl CorrectionConfig {
    /// Short human-readable descriptor for state snapshots.
    fn descriptor(&self) -> String {
        match self {
            CorrectionConfig::Fir { firs, .. } => {
                format!("fir:{}-tap", firs.iter().map(Vec::len).max().unwrap_or(0))
            }
            CorrectionConfig::Iir {
                sos_per_channel, ..
            } => format!(
                "iir:{}-band",
                sos_per_channel.iter().map(Vec::len).max().unwrap_or(0)
            ),
            CorrectionConfig::Peq { bands, .. } => {
                format!("peq:{}-band", bands.iter().map(Vec::len).max().unwrap_or(0))
            }
        }
    }
}

/// What [`build_correction`] had to do to make a config installable.
/// Control-plane telemetry: nothing here is on the realtime path.
///
/// No `Eq`: `preamp_db` is an f64 (R1-1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BuildReport {
    /// Bands that are not designable at the live stream rate (at or above
    /// the new Nyquist, or otherwise out of range) and were dropped. The
    /// rest of the set still designs -- R1-3's rule, "the auto front-end
    /// must never be bricked by one bad band".
    pub bands_dropped: usize,
    /// R1-1's auto-preamp for the correction that was actually installed, in
    /// dB, always `<= 0`. Published as [`EngineState::auto_preamp_db`] -- the
    /// number the Advanced drawer has to be able to explain (spec `:106`:
    /// "The auto front-end must never apply a number it cannot explain") --
    /// and `10^(preamp_db/20)` is what rides into the chain as
    /// [`Correction::preamp_lin`].
    pub preamp_db: f64,
    /// SOS rows the R1-3 Jury funnel replaced with the identity section.
    pub sections_substituted: usize,
}

/// Rate-INDEPENDENT band sanity, the guard's owner-decided home
/// (`docs/decisions/2026-07-22-owner-value-calls.md`: "`validate_bands` guard
/// location | `paraeq-engine` (next to `validate_correction`) | Keeps
/// `paraeq-dsp` free of policy").
///
/// Checks everything that does not depend on the sample rate: `fc` finite and
/// `> 0`, `q` finite and in `[Q_MIN, Q_MAX]`, `gain_db` finite and within
/// `GAIN_LIMIT_DB`. The Nyquist bound is rate-DEPENDENT and lives in
/// [`validate_band_at`], because a band legal at 96 kHz can be above Nyquist
/// at 44.1 kHz and that verdict can only be reached once the stream rate is
/// known.
///
/// The message names the offending field but NOT the band index: callers hold
/// the index and prefix it (`desktop/src-tauri/src/eq.rs` surfaces these
/// strings verbatim in the UI).
pub fn validate_band(band: &EQBand) -> Result<(), String> {
    if !band.fc.is_finite() {
        return Err("fc must be a finite number".to_string());
    }
    if band.fc <= 0.0 {
        return Err(format!("fc {} Hz must be greater than 0", band.fc));
    }
    if !band.q.is_finite() {
        return Err("q must be a finite number".to_string());
    }
    if band.q < Q_MIN || band.q > Q_MAX {
        return Err(format!("q {} must be between {Q_MIN} and {Q_MAX}", band.q));
    }
    if !band.gain_db.is_finite() {
        return Err("gain_db must be a finite number".to_string());
    }
    if band.gain_db.abs() > GAIN_LIMIT_DB {
        return Err(format!(
            "gain_db {} must be within +/-{GAIN_LIMIT_DB} dB",
            band.gain_db
        ));
    }
    Ok(())
}

/// [`validate_band`] plus the rate-dependent Nyquist bound: `fc` must lie in
/// the OPEN interval `(0, sample_rate / 2)`. At `fc >= sample_rate / 2` the
/// biquad math is finite but meaningless (aliased around Nyquist -- spec
/// `:188`), which `paraeq_dsp::biquad::is_stable` does not catch, so it is
/// checked explicitly here.
pub fn validate_band_at(band: &EQBand, sample_rate: f64) -> Result<(), String> {
    let nyquist = sample_rate / 2.0;
    if band.fc.is_finite() && (band.fc <= 0.0 || band.fc >= nyquist) {
        return Err(format!(
            "fc {} Hz must be between 0 and Nyquist ({nyquist} Hz), exclusive",
            band.fc
        ));
    }
    validate_band(band)
}

/// `Some(reason)` for the shapes that would PANIC the underlying builders
/// (an empty FIR list, an empty FIR, an empty SOS-set list, an empty
/// band-set list). Separated from [`validate_correction`] because
/// [`build_correction`] must be panic-free for any caller, while bad band
/// *values* it drops one by one rather than refusing wholesale.
fn structural_defect(config: &CorrectionConfig) -> Option<String> {
    match config {
        CorrectionConfig::Fir { firs, .. } => {
            if firs.is_empty() {
                Some("Fir config has no FIRs".into())
            } else if firs.iter().any(Vec::is_empty) {
                Some("Fir config contains an empty (0-tap) FIR".into())
            } else {
                None
            }
        }
        CorrectionConfig::Iir {
            sos_per_channel, ..
        } => {
            if sos_per_channel.is_empty() {
                Some("Iir config has no SOS sets".into())
            } else {
                None
            }
        }
        CorrectionConfig::Peq { bands, .. } => {
            if bands.is_empty() {
                Some("Peq config has no band sets".into())
            } else {
                None
            }
        }
    }
}

/// `None` when `config` is acceptable to retain; `Some(reason)` for a
/// degenerate shape ([`structural_defect`]) or a band that is nonsense at
/// ANY rate ([`validate_band`]). The controller checks this before every
/// `SetCorrection` so a malformed command can never panic its thread and a
/// junk band is rejected at the seam where a user-facing error still exists.
///
/// Deliberately rate-INDEPENDENT: a `SetCorrection` may arrive before any
/// stream exists, and under R1-6 the rate-dependent verdicts (Nyquist, the
/// baked variants' `design_rate` compare) belong at build time, where the
/// live rate is in hand.
fn validate_correction(config: &CorrectionConfig) -> Option<String> {
    if let Some(reason) = structural_defect(config) {
        return Some(reason);
    }
    if let CorrectionConfig::Peq { bands, .. } = config {
        for (set, set_bands) in bands.iter().enumerate() {
            for (index, band) in set_bands.iter().enumerate() {
                if let Err(reason) = validate_band(band) {
                    return Some(format!("Peq set {set} band {index}: {reason}"));
                }
            }
        }
    }
    None
}

/// Build + warm up a correction for the given stream geometry AT
/// `stream_rate`. Control plane only (allocates); never panics.
///
/// `Peq` re-derives every band's SOS at `stream_rate` (its `design_rate` is
/// ignored). A band that is not designable there -- at or above the new
/// Nyquist -- is DROPPED and counted in the [`BuildReport`] rather than
/// failing the whole set, mirroring R1-3's "never bricked by one bad band"
/// (spec `:216`); the config is refused only when nothing survives.
///
/// `Fir` / `Iir` carry baked coefficients that cannot be re-derived, so they
/// are refused unless `design_rate == stream_rate` -- an EXACT comparison,
/// not a tolerance (spec `:420`: device-reported rates round-trip exactly in
/// f64 and a fuzzy compare would silently accept a genuinely different rate).
///
/// On `Err` the caller sends no correction at all: flat pass-through, the
/// same fail-open philosophy as the `AutoDisabledNoInput` path.
///
/// R1-1: every arm also computes its auto-preamp here, from the SAME realized
/// cascade that reaches the chain, and hands it to `build_iir`/`build_fir` so
/// it swaps atomically with the coefficients it protects (spec `:101`). It is
/// computed AFTER the Nyquist drop and from the surviving bands, because the
/// headroom that matters is the headroom the installed filters need. `Peq`
/// calls `ParametricEQ::preamp_db()` verbatim -- one implementation shared
/// with the AutoEQ export, so spec `:117`'s export/engine agreement holds and
/// the band-`fc` grid union stays intact; the two baked arms use
/// [`crate::preamp`], which is the same idea without the bands. A
/// multi-channel config takes the WORST channel's preamp.
pub fn build_correction(
    config: &CorrectionConfig,
    channels: usize,
    block_size: usize,
    stream_rate: f64,
) -> Result<(Correction, BuildReport), EngineError> {
    if !stream_rate.is_finite() || stream_rate <= 0.0 {
        return Err(EngineError::InvalidConfig(format!(
            "stream sample rate {stream_rate} is not a usable rate"
        )));
    }
    // STRUCTURAL guard only -- the builders assert on these, and this
    // function must never panic even when called outside the controller's
    // `SetCorrection` pre-screen. Bad *values* are dropped band by band
    // below, not refused wholesale (R1-3, spec `:216`).
    if let Some(reason) = structural_defect(config) {
        return Err(EngineError::InvalidConfig(reason));
    }
    match config {
        CorrectionConfig::Fir { design_rate, firs } => {
            refuse_on_rate_mismatch(*design_rate, stream_rate, "Fir")?;
            let preamp_db = preamp::fir_preamp_db(firs);
            Ok((
                build_fir(
                    firs.clone(),
                    channels,
                    block_size,
                    preamp::preamp_lin(preamp_db),
                ),
                BuildReport {
                    bands_dropped: 0,
                    preamp_db,
                    sections_substituted: 0,
                },
            ))
        }
        CorrectionConfig::Iir {
            design_rate,
            sos_per_channel,
        } => {
            refuse_on_rate_mismatch(*design_rate, stream_rate, "Iir")?;
            let preamp_db = preamp::sos_preamp_db(sos_per_channel, stream_rate);
            let (correction, substituted) = build_iir(
                sos_per_channel.clone(),
                channels,
                block_size,
                preamp::preamp_lin(preamp_db),
            );
            warn_on_substitutions(substituted);
            Ok((
                correction,
                BuildReport {
                    bands_dropped: 0,
                    preamp_db,
                    sections_substituted: substituted,
                },
            ))
        }
        CorrectionConfig::Peq { bands, .. } => {
            let mut bands_dropped = 0;
            let mut kept = 0;
            let kept_bands: Vec<Vec<EQBand>> = bands
                .iter()
                .map(|set| {
                    set.iter()
                        .filter(|band| match validate_band_at(band, stream_rate) {
                            Ok(()) => {
                                kept += 1;
                                true
                            }
                            Err(_) => {
                                bands_dropped += 1;
                                false
                            }
                        })
                        .cloned()
                        .collect()
                })
                .collect();
            let sos_per_channel: Vec<Vec<[f64; 6]>> = kept_bands
                .iter()
                .map(|set| set.iter().map(|band| band.to_sos(stream_rate)).collect())
                .collect();
            if kept == 0 && bands_dropped > 0 {
                return Err(EngineError::InvalidConfig(format!(
                    "no band of {bands_dropped} is designable at {stream_rate} Hz"
                )));
            }
            if bands_dropped > 0 {
                log::warn!(
                    "{bands_dropped} band(s) are at or above Nyquist at {stream_rate} Hz \
                     and were dropped; the rest of the correction still applies"
                );
            }
            // R1-1, spec `:101`: `ParametricEQ::preamp_db()` VERBATIM, on the
            // surviving bands at the live rate -- the same call the AutoEQ
            // export makes, so the two numbers agree exactly (spec `:117`)
            // and the band-`fc` grid union that makes high-Q peaks exact
            // stays intact. The worst channel wins: a stereo config must not
            // clip on the loud side because the quiet side needed less
            // headroom.
            let preamp_db = kept_bands
                .iter()
                .map(|set| {
                    ParametricEQ {
                        bands: set.clone(),
                        sample_rate: stream_rate,
                    }
                    .preamp_db()
                })
                .fold(0.0f64, f64::min);
            let (correction, substituted) = build_iir(
                sos_per_channel,
                channels,
                block_size,
                preamp::preamp_lin(preamp_db),
            );
            warn_on_substitutions(substituted);
            Ok((
                correction,
                BuildReport {
                    bands_dropped,
                    preamp_db,
                    sections_substituted: substituted,
                },
            ))
        }
    }
}

/// R1-6's last-resort net for the baked (un-re-derivable) variants.
fn refuse_on_rate_mismatch(
    design_rate: f64,
    stream_rate: f64,
    kind: &str,
) -> Result<(), EngineError> {
    if design_rate == stream_rate {
        return Ok(());
    }
    Err(EngineError::InvalidConfig(format!(
        "{kind} config carries baked coefficients designed at {design_rate} Hz, \
         which cannot be re-derived for the live {stream_rate} Hz stream"
    )))
}

fn warn_on_substitutions(substituted: usize) {
    if substituted > 0 {
        // Stability backstop (spec R1-3): the dropped bands do nothing
        // rather than destabilize the chain; surface the count. Control
        // plane only -- never the realtime path.
        log::warn!(
            "correction contained {substituted} unstable SOS section(s); \
             each was replaced with the identity section (band dropped)"
        );
    }
}

/// Commands accepted by [`EngineHandle::send`].
pub enum EngineCommand {
    ClearCorrection,
    Disable,
    Enable,
    SetBufferFrames(usize),
    SetBypass(bool),
    SetCorrection(CorrectionConfig),
    SetGainDb(f32),
    Shutdown,
}

/// One serialized engine snapshot, published on every change (spec line
/// 135); stage 4 pushes it over Tauri as-is.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct EngineState {
    /// R1-1's computed auto-preamp for the correction currently installed, in
    /// dB and always `<= 0`; `None` when no correction is running. The
    /// Advanced drawer explains it ("we pulled you down 9.4 dB to make room
    /// for the 45 Hz boost" -- spec `:106`), and decision-engine `:340` gives
    /// the user-facing wording verbatim.
    ///
    /// Session-scoped, like `correction_rate_mismatch` and for the same
    /// reason: it names what the LIVE chain is applying, so with no chain
    /// there is nothing being applied. Distinct from the user's manual trim
    /// (`gain_db` / `EqState.preamp_db`), which it COMPOSES with -- they
    /// multiply in linear and add in dB -- and which is never overwritten by
    /// it.
    pub auto_preamp_db: Option<f32>,
    pub bypass: bool,
    /// Output samples the +-1.0 clamp engaged on (R1-8), counted per sample
    /// per channel over both chain paths. Retained across a torn-down session
    /// so a `Disable` does not blank the count the user is looking at.
    ///
    /// A nonzero value while `auto_preamp_db` is active is a BUG SIGNAL and
    /// the spec says so (`:571`): it is R1-1's falsifier.
    pub clipped_samples: u64,
    /// Short descriptor of the retained correction, e.g. `"iir:5-band"`.
    pub correction: Option<String>,
    /// `Some(stream_rate)` when the retained correction could NOT be built
    /// for the live stream and the chain is therefore running flat (R1-6):
    /// the rate it must be redesigned for. `None` whenever a correction is
    /// installed, cleared, or there is no session.
    ///
    /// Session-scoped by construction -- it describes the LIVE stream's rate,
    /// so a teardown clears it. The desktop keys its redesign off this field
    /// rather than diffing rates itself, so the daemon seam inherits the
    /// behavior and the refusal lands in the same call that used to install
    /// stale coefficients (spec `:424`, gap 3 at `:407`).
    pub correction_rate_mismatch: Option<f64>,
    /// The controller's current enabled flag: `true` between an
    /// [`EngineCommand::Enable`] (or an enabled spawn) and the next
    /// [`EngineCommand::Disable`]/fail-open auto-disable. Lets the UI tell a
    /// user-disabled `Stopped` apart from a pre-start or failed one.
    pub enabled: bool,
    /// Cumulative count of realtime blocks the chain passed through instead
    /// of correcting because their frame count did not match the built
    /// geometry (degraded pass-through telemetry). Read from `RtShared` at
    /// snapshot time; retains the last observed count after a `Disable`
    /// tears the session down, and is re-zeroed by the fresh `RtShared` on
    /// every start.
    pub frame_mismatch_blocks: u64,
    pub gain_db: f32,
    /// Input METER: the largest recent input |sample|, released at the
    /// broadcast-standard 20 dB / 1.7 s (R1-8, spec `:515`). It answers "how
    /// loud is the music right now"; `input_peak_session` answers "how loud
    /// did it ever get".
    ///
    /// Consequence, priced and accepted: this changes on nearly every tick
    /// while audio plays, so `publish` fires at the full tick rate (4 Hz by
    /// default) instead of going quiet a few seconds into a session. That is
    /// a Tauri `app-state` event and a React re-render 4x/s; it does NOT
    /// touch disk (`desktop/src-tauri/src/state.rs` compares the durable
    /// subset first, so engine-only deltas never reach `settings.json`, reads
    /// included). If it bites, the fix is a separate lighter meters event,
    /// not a slower meter.
    pub input_peak: f32,
    /// Maximum input |sample| since this session started (R1-8's session
    /// statistic, spec `:552`). `input_peak` is the meter; this one never
    /// decays.
    pub input_peak_session: f32,
    /// Non-finite samples zeroed at the capture boundary and by the chain's
    /// output backstop (R1-2). Already counted in `RtShared`; R1-8 surfaces
    /// it, so the drawer can show what the tick previously only logged.
    /// Retained across a torn-down session, like `clipped_samples`.
    pub invalid_samples: u64,
    /// `sample_time_delta / sample_rate * 1000` for the live stream.
    pub latency_ms: Option<f64>,
    /// Output METER, taken PRE-clamp, so "we peaked at 0.87, 1.2 dB of
    /// margin" is answerable and a preamp that is not working reads as a
    /// number over 1.0 rather than as a saturated 1.0 (see
    /// [`crate::chain::ChainOutcome::peak_out`] for the spec-text
    /// reconciliation this pins). Released by the same coefficient as
    /// `input_peak` so the two read comparably; see
    /// [`crate::shared::RtShared::peak_out_bits`] for why the spec's
    /// input-only decay is applied here too.
    pub output_peak: f32,
    /// The MS-6 self-exclusion witness: whether the LIVE capture currently
    /// keeps ParaEQ's own audio out of ParaEQ's own tap, exactly as the
    /// backend reports it (measurement-safety `:336`, wizard `:412`). The
    /// measurement wizard refuses to begin a `Direct` capture when it is
    /// `false` -- an "uncorrected" baseline that was silently corrected is
    /// worse than no baseline, and feedback is live into a coupler that may be
    /// on someone's head.
    ///
    /// Read it as a PAIR with `stream`, never alone:
    /// - `false` + `stream == None` -- nothing is running, so nothing is
    ///   witnessed. The remedy is to enable the EQ.
    /// - `false` + `stream == Some(_)` -- the documented fail-open path fired
    ///   (`tap.rs`: `translate_pid` returned 0 twice, the tap's exclusion list
    ///   went out empty, and ParaEQ's own audio IS being captured). The remedy
    ///   is to restart ParaEQ.
    ///
    /// That pair is why the spec's literal `bool` suffices and no
    /// `Option<bool>` is needed.
    ///
    /// **The rebuild window is a legitimate `false`.** `rebuild` is
    /// `stop_session` + `try_start`, and `start_once` may stop+start up to
    /// three times while renegotiating geometry, so a device change drops this
    /// to `false` for a moment. A `SelfExclusionUnavailable` refusal right
    /// after a device swap is that window, not a bug -- and in practice the
    /// same [`BackendEvent`] that forces the rebuild is already an
    /// `OutputDeviceChanged` abort for any session in flight.
    ///
    /// Telemetry, not the gate. This is published at most once per tick and is
    /// what the UI renders; `paraeq-measure` polls the backend's own live
    /// witness (`paraeq_coreaudio::backend::ExclusionWitness`) at every gate
    /// that precedes emission, so a safety check is never up to a tick stale.
    /// Both come from the same cell, so they cannot disagree.
    pub self_excluded: bool,
    pub status: EngineStatus,
    pub stream: Option<StreamInfo>,
}

/// Controller tuning. Tests shorten the watchdog windows and tick so the
/// suite runs in real time without real waits.
#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    /// Whether the controller starts the backend on spawn. `true` (the
    /// [`Default`]) preserves the historical spawn-and-start behavior; the
    /// desktop passes `false` pre-wizard so the tap is not engaged (and the
    /// system not muted) until the user explicitly enables the EQ. Either
    /// way, [`EngineCommand::Enable`]/[`EngineCommand::Disable`] flip it at
    /// runtime.
    pub enabled: bool,
    /// Fail-open window: how long [`EngineStatus::NoInputDetected`] may
    /// persist before the controller auto-disables (same path as
    /// [`EngineCommand::Disable`]: backend stopped, tap destroyed, device
    /// unmuted) and publishes [`EngineStatus::AutoDisabledNoInput`].
    /// `NoInputDetected` only occurs before the FIRST nonzero input since
    /// start -- the TCC silent-failure signature (the tap mutes the device
    /// but delivers zeros) -- so failing open restores the user's un-EQ'd
    /// audio instead of holding the system muted with silence indefinitely.
    /// Post-`Running` states (`Idle`, `InputSilent`) never fail open:
    /// pausing music must never disable the EQ. Measured engage worst case
    /// is ~4-5 s, so the 15 s default is comfortably conservative. `None`
    /// disables the behavior.
    pub fail_open_after_ms: Option<u64>,
    /// Buffer-frame-size hint passed to the backend (user-settable via
    /// [`EngineCommand::SetBufferFrames`]).
    pub requested_buffer_frames: Option<usize>,
    /// Slots per SPSC ring (swap + retire). Must be >= 1.
    pub ring_capacity: usize,
    /// Controller tick period (watchdog observe + snapshot publish +
    /// retired drain + event poll).
    pub tick_ms: u64,
    pub watchdog: WatchdogConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fail_open_after_ms: Some(15_000),
            requested_buffer_frames: None,
            ring_capacity: 4,
            tick_ms: 250,
            watchdog: WatchdogConfig::default(),
        }
    }
}

/// Handle to a spawned engine controller thread.
///
/// Dropping the handle sends [`EngineCommand::Shutdown`] and joins the
/// controller thread (which stops the backend on every exit path).
pub struct EngineHandle {
    cmd_tx: Sender<EngineCommand>,
    join: Option<JoinHandle<()>>,
    /// The fail-open suspension cell shared with the controller thread:
    /// `true` while a [`MeasurementLease`] is outstanding. Deliberately a
    /// shared atomic rather than an [`EngineCommand`] -- see
    /// [`EngineHandle::acquire_measurement_lease`].
    measurement_lease: Arc<AtomicBool>,
    state: Arc<ArcSwap<EngineState>>,
    subscribers: Arc<Mutex<Vec<SyncSender<Arc<EngineState>>>>>,
}

impl EngineHandle {
    /// Spawn the controller thread; it starts the backend immediately when
    /// `config.enabled` (the default), or waits for [`EngineCommand::Enable`]
    /// when spawned disabled.
    pub fn spawn<B: AudioBackend + 'static>(backend: B, config: EngineConfig) -> EngineHandle {
        assert!(config.ring_capacity >= 1, "ring_capacity must be >= 1");
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let initial = Arc::new(EngineState {
            auto_preamp_db: None,
            bypass: false,
            clipped_samples: 0,
            correction: None,
            correction_rate_mismatch: None,
            enabled: config.enabled,
            frame_mismatch_blocks: 0,
            gain_db: 0.0,
            input_peak: 0.0,
            input_peak_session: 0.0,
            invalid_samples: 0,
            latency_ms: None,
            output_peak: 0.0,
            self_excluded: false,
            status: EngineStatus::Stopped,
            stream: None,
        });
        let measurement_lease = Arc::new(AtomicBool::new(false));
        let state = Arc::new(ArcSwap::new(Arc::clone(&initial)));
        let subscribers: Arc<Mutex<Vec<SyncSender<Arc<EngineState>>>>> =
            Arc::new(Mutex::new(Vec::new()));

        let thread_lease = Arc::clone(&measurement_lease);
        let thread_state = Arc::clone(&state);
        let thread_subscribers = Arc::clone(&subscribers);
        let join = std::thread::Builder::new()
            .name("paraeq-engine-controller".into())
            .spawn(move || {
                Controller {
                    auto_disabled: None,
                    auto_preamp_db: None,
                    backend: StopGuard(backend),
                    bypass: false,
                    clipped_samples: 0,
                    cmd_rx,
                    consecutive_start_failures: 0,
                    correction: None,
                    correction_rate_mismatch: None,
                    enabled: config.enabled,
                    epoch: Instant::now(),
                    fail_open_after_ms: config.fail_open_after_ms,
                    failed: None,
                    frame_mismatch_blocks: 0,
                    gain_db: 0.0,
                    invalid_samples: 0,
                    last_tick: Instant::now(),
                    measurement_lease: thread_lease,
                    measurement_lease_seen: false,
                    measurement_lease_warned: false,
                    published: initial,
                    requested_buffer_frames: config.requested_buffer_frames,
                    ring_capacity: config.ring_capacity,
                    session: None,
                    state: thread_state,
                    subscribers: thread_subscribers,
                    swap_pending: false,
                    tick: Duration::from_millis(config.tick_ms),
                    watchdog: Watchdog::new(config.watchdog),
                }
                .run();
            })
            .expect("spawn paraeq-engine-controller thread");

        EngineHandle {
            cmd_tx,
            join: Some(join),
            measurement_lease,
            state,
            subscribers,
        }
    }

    /// Queue a command for the controller (fire-and-forget; ignored after
    /// shutdown).
    pub fn send(&self, cmd: EngineCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// The latest published snapshot (lock-free read).
    pub fn state(&self) -> Arc<EngineState> {
        self.state.load_full()
    }

    /// Subscribe to snapshots published from now on.
    ///
    /// The channel is BOUNDED (64 snapshots): a subscriber that stops
    /// reading gets updates dropped, not queued without limit -- the latest
    /// state is always available via [`state`](Self::state). A dropped
    /// receiver prunes the subscription on the next publish.
    pub fn subscribe(&self) -> Receiver<Arc<EngineState>> {
        let (tx, rx) = mpsc::sync_channel(64);
        self.subscribers
            .lock()
            .expect("subscriber list lock")
            .push(tx);
        rx
    }

    /// Suspend the fail-open auto-disable for the duration of a measurement
    /// run. `None` when a lease is already outstanding -- one measurement at a
    /// time. See [`MeasurementLease`] for what is and is not suspended, and
    /// for the (important) question of what "a measurement" means here.
    ///
    /// Lock-free and immediate (one compare-exchange on a cell shared with the
    /// controller thread), deliberately NOT an [`EngineCommand`]: commands are
    /// fire-and-forget with no reply plumbing anywhere in the engine, and the
    /// caller needs the answer before it emits a single sample. The controller
    /// reads the cell once per tick on the control thread; the realtime lane
    /// never sees it.
    ///
    /// It knows nothing about engine status: acquiring against a stopped,
    /// disabled or `Failed` engine succeeds. Callers gate on
    /// [`EngineState::status`] / [`EngineState::stream`] themselves, because
    /// "the engine is not running" is a different refusal, with a different
    /// remedy, than "a measurement is already running".
    pub fn acquire_measurement_lease(&self) -> Option<MeasurementLease> {
        if self
            .measurement_lease
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            log::warn!("measurement lease refused: one is already outstanding");
            return None;
        }
        log::warn!(
            "measurement lease acquired -- fail-open auto-disable is suspended until it is \
             released"
        );
        Some(MeasurementLease {
            acquired: Instant::now(),
            held: Arc::clone(&self.measurement_lease),
        })
    }
}

/// RAII token that suspends the fail-open auto-disable for the duration of a
/// measurement run (`docs/specs/2026-07-15-wizard-design.md:418`). Obtained
/// from [`EngineHandle::acquire_measurement_lease`].
///
/// # Why it exists
///
/// [`EngineConfig::fail_open_after_ms`] auto-disables the engine when
/// [`EngineStatus::NoInputDetected`] persists -- no nonzero sample has EVER
/// been captured since start, which is the TCC silent-failure signature. A
/// measurement walks straight into that uncovered case: the wizard's `Direct`
/// captures are tap-excluded **by design**, so the tap sees nothing but zeros,
/// and 15 s in the controller would tear the tap down *mid-session* -- leaving
/// the `Helper` verification capture with no engine to run through.
///
/// # What it suspends, and what it deliberately does not
///
/// Exactly one branch: the fail-open auto-disable (`enabled = false`,
/// `stop_session()`, [`EngineStatus::AutoDisabledNoInput`]). Everything else
/// runs untouched:
///
/// - **The chain.** It must -- closed-loop verification needs the designed
///   correction installed and running while the `Helper` stimulus plays.
/// - **Rebuilds on backend events.** A device change still rebuilds; the
///   measurement aborts on that event on its own evidence, and suppressing the
///   rebuild would only strand the engine on a dead device.
/// - **[`EngineCommand::Disable`] and [`EngineCommand::Shutdown`].** User
///   intent and app exit outrank a measurement; blocking `Shutdown` would hang
///   quit.
/// - **`try_start` and the start-failure `Failed` latch.**
/// - **The watchdog's *reporting*.** `NoInputDetected` is still produced and
///   still published -- wizard:418 says so in as many words. A genuine TCC
///   denial during a measurement is still a refusal; it just must not race the
///   wizard to the teardown.
///
/// # Hold it ABOVE `MeasurementSession`, never inside one
///
/// A `paraeq_measure::MeasurementSession` is **one sweep**: its phase machine
/// runs `Preflight -> Solved -> Acknowledged -> Swept -> Terminated` and never
/// loops back, so a 9-position capture is **nine sessions** and one lease spans
/// all of them. "For the duration of a session" in wizard:418 means the
/// duration of the *wizard run*: acquire at the spine's `Probe` step
/// (wizard:39-40 lists "engine lease" there, beside the device/rate/TCC
/// checks), and release at Result/Save, at cancel, and on every error exit.
/// A token parked inside a session seam is released between positions and
/// re-arms fail-open in the middle of a capture.
///
/// Declare it **before** the sessions it covers, which is what "above" means
/// in source order too: Rust drops LOCALS in REVERSE declaration order, so the
/// lease declared first is released last -- strictly after MS-14's restore
/// sequence (abort ramp, sink stop, volume restore) has run on the sessions
/// declared after it.
///
/// Do not reach for `TapBackend`'s field-order trick here: that is the
/// OPPOSITE rule (`backend.rs`, "Rust drops fields in declaration order"), and
/// it applies to struct fields, which this token must never be. The worked
/// example in the tree is
/// `crates/paraeq-coreaudio/tests/test_measure_hardware.rs`, which declares
/// the lease and then the session.
///
/// # Release
///
/// Dropping the token re-arms fail-open and **re-baselines** the window: if the
/// watchdog is sitting in `NoInputDetected`, the controller restamps `since_ms`
/// to now, so the auto-disable can only fire after a further FULL window. The
/// time accrued under the lease is not evidence of a TCC failure -- the tap was
/// *supposed* to see zeros -- and firing on release would auto-disable the
/// engine immediately after every successful measurement, which is exactly
/// backwards.
///
/// `Drop` runs during unwinding, so a panicking wizard run releases the lease.
/// The token holds only the shared cell, never the [`EngineHandle`], so it is
/// also safe to outlive the engine: the release is then a plain store nothing
/// reads.
///
/// # The hazard this type is shaped around
///
/// A token parked in long-lived state and never dropped disables fail-open
/// **forever**, and a TCC denial then holds the user's system muted with no
/// auto-recovery. That is the highest-severity failure mode in this feature.
/// Mitigations, all present: keep the token scope-local to the wizard run (a
/// field in a long-lived struct is not released by a panic); `WARN` on acquire
/// and on release with the held duration; and one `WARN` per lease at the
/// moment the suspended window would have fired. There is deliberately no
/// maximum hold time in v1 -- a `MEASUREMENT_LEASE_MAX_MS` break is additive
/// later and re-arming a safety net under a running measurement is an owner
/// call, not an implementation default.
pub struct MeasurementLease {
    acquired: Instant,
    held: Arc<AtomicBool>,
}

impl Drop for MeasurementLease {
    fn drop(&mut self) {
        // Release BEFORE logging: the store is the safety-relevant half, and
        // it must happen even if the log line cannot be emitted. Control plane
        // only -- a lease is never acquired or dropped on the realtime lane.
        self.held.store(false, Ordering::Release);
        log::warn!(
            "measurement lease released after {} ms -- fail-open re-armed (window \
             re-baselined from now)",
            self.acquired.elapsed().as_millis()
        );
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(EngineCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Stops the backend when dropped, so ANY controller-thread exit path --
/// Shutdown, channel disconnect, panic unwinding -- runs the backend's
/// (idempotent) full teardown. A dead ParaEQ must never leave the system
/// muted.
struct StopGuard<B: AudioBackend>(B);

impl<B: AudioBackend> Drop for StopGuard<B> {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

/// Per-start realtime session: the control halves of the fresh state built
/// for one backend start. Dropped (with any queued/retired corrections) on
/// the control thread at stop.
struct Session {
    /// Chain build geometry -- corrections must be built to match.
    block_size: usize,
    channels: usize,
    control: ControlLink,
    /// `invalid_samples` value already reported via log: the tick warns on
    /// the delta only (the rt side never logs; the tick period is the rate
    /// limit).
    invalid_seen: u64,
    shared: Arc<RtShared>,
    stream: StreamInfo,
}

struct Controller<B: AudioBackend> {
    /// `Some(after_ms)` after a fail-open auto-disable. Like `failed`, a
    /// controller-owned status override: `effective_status` keeps
    /// publishing `AutoDisabledNoInput` (not the watchdog's `Stopped`)
    /// until an explicit Enable/Disable clears it.
    auto_disabled: Option<u64>,
    /// Mirror of [`EngineState::auto_preamp_db`]: R1-1's computed preamp for
    /// the correction currently installed. Set by `send_correction` from the
    /// `BuildReport`, cleared on a refusal, a clear, and teardown.
    auto_preamp_db: Option<f32>,
    backend: StopGuard<B>,
    bypass: bool,
    /// Last `RtShared::clipped_samples` observed at snapshot time. Retained
    /// across a torn-down session (SHELL's `frame_mismatch_blocks` pattern):
    /// a `Disable` must not blank the clip count, because the count IS the
    /// disclosure -- and R1-1's falsifier.
    clipped_samples: u64,
    cmd_rx: Receiver<EngineCommand>,
    /// Consecutive `start` failures; at 2 the controller publishes `Failed`
    /// and stops retrying (no rebuild loop on a dead device).
    consecutive_start_failures: u32,
    correction: Option<CorrectionConfig>,
    /// Mirror of [`EngineState::correction_rate_mismatch`]: set when
    /// `build_correction` refuses the retained config at the live stream
    /// rate, cleared on every successful install, on `ClearCorrection`, and
    /// on teardown.
    correction_rate_mismatch: Option<f64>,
    enabled: bool,
    epoch: Instant,
    /// Fail-open window from [`EngineConfig::fail_open_after_ms`]
    /// (`None` = behavior disabled).
    fail_open_after_ms: Option<u64>,
    failed: Option<String>,
    /// Last `RtShared::frame_mismatch_blocks` observed at snapshot time.
    /// Retained across a torn-down session (so a `Disable` does not reset
    /// the surfaced count to 0); re-zeroed when the fresh `RtShared` of the
    /// next start reports 0.
    frame_mismatch_blocks: u64,
    gain_db: f32,
    /// Last `RtShared::invalid_samples` observed at snapshot time; retained
    /// across a torn-down session for the same reason `clipped_samples` is.
    invalid_samples: u64,
    /// When `on_tick` last ran. A command flood keeps `recv_timeout`
    /// returning `Ok` and would otherwise starve the tick work entirely
    /// (watchdog, event poll, retired drain, swap retry); `run` checks this
    /// after every command and runs the tick when it is due.
    last_tick: Instant,
    /// Fail-open suspension cell shared with [`EngineHandle`]: `true` while a
    /// [`MeasurementLease`] is outstanding. Read once per tick, on this
    /// thread; never touched by the realtime lane.
    measurement_lease: Arc<AtomicBool>,
    /// `measurement_lease` as of the last tick, so the release can be seen as
    /// an EDGE (that is what re-baselines the fail-open window). A lease taken
    /// and dropped entirely between two ticks is invisible here, which is
    /// correct: nothing was suspended, so nothing needs re-baselining.
    measurement_lease_seen: bool,
    /// One `WARN` per lease when the suspended window elapses, not one per
    /// tick. Reset on every observed edge.
    measurement_lease_warned: bool,
    published: Arc<EngineState>,
    requested_buffer_frames: Option<usize>,
    ring_capacity: usize,
    session: Option<Session>,
    state: Arc<ArcSwap<EngineState>>,
    subscribers: Arc<Mutex<Vec<SyncSender<Arc<EngineState>>>>>,
    /// A correction send hit a full ring; rebuild from the stored config
    /// and retry next tick.
    swap_pending: bool,
    tick: Duration,
    watchdog: Watchdog,
}

impl<B: AudioBackend> Controller<B> {
    fn run(mut self) {
        self.try_start();
        self.publish();
        loop {
            match self.cmd_rx.recv_timeout(self.tick) {
                Ok(EngineCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => {
                    self.handle(cmd);
                    // Tick starvation guard: commands arriving faster than
                    // the tick keep recv_timeout from ever timing out, so
                    // run the due tick work here too.
                    if self.last_tick.elapsed() >= self.tick {
                        self.on_tick();
                    }
                    self.publish();
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.on_tick();
                    self.publish();
                }
            }
        }
        // Orderly teardown; the StopGuard additionally covers panic paths.
        self.stop_session();
        self.publish();
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn handle(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::ClearCorrection => {
                self.correction = None;
                self.send_correction();
            }
            EngineCommand::Disable => {
                // Full disable: tap destroyed, device unmuted -- "system
                // exactly as if ParaEQ never ran". An explicit Disable also
                // supersedes a fail-open latch: the honest status is now
                // plain Stopped.
                self.auto_disabled = None;
                self.enabled = false;
                self.stop_session();
            }
            EngineCommand::Enable => {
                self.auto_disabled = None;
                self.enabled = true;
                self.failed = None;
                self.consecutive_start_failures = 0;
                self.try_start(); // no-op when already running
            }
            EngineCommand::SetBufferFrames(frames) => {
                self.requested_buffer_frames = Some(frames);
                if self.session.is_some() {
                    self.rebuild();
                }
            }
            EngineCommand::SetBypass(bypass) => {
                self.bypass = bypass;
                if let Some(s) = &self.session {
                    s.shared.bypass.store(bypass, Ordering::Relaxed);
                }
            }
            EngineCommand::SetCorrection(config) => {
                // Malformed configs would panic the builders (and take the
                // controller thread with them): warn + ignore, no state
                // change.
                if let Some(reason) = validate_correction(&config) {
                    log::warn!("SetCorrection ignored: {reason}");
                    return;
                }
                self.correction = Some(config);
                self.send_correction();
            }
            EngineCommand::SetGainDb(db) => {
                self.gain_db = db;
                if let Some(s) = &self.session {
                    s.shared.set_gain(db_to_linear(db));
                }
            }
            EngineCommand::Shutdown => unreachable!("Shutdown breaks the loop in run()"),
        }
    }

    fn on_tick(&mut self) {
        self.last_tick = Instant::now();
        // Retry a start deferred by a single earlier failure.
        if self.session.is_none() {
            self.try_start();
        }

        if let Some(s) = &mut self.session {
            let callbacks = s.shared.callbacks.load(Ordering::Relaxed);
            let nonzero = s.shared.nonzero_blocks.load(Ordering::Relaxed);
            let now = self.epoch.elapsed().as_millis() as u64;
            // Drives the informational status machine (Idle, InputSilent,
            // ...). No watchdog state is ever a rebuild trigger: the tap
            // aggregate's IOProc only cycles while the system renders audio
            // (hardware finding 2026-07-11), so a frozen callback counter
            // is normal idling -- rebuilding on it would loop forever while
            // the system is idle. Device death arrives as backend events.
            self.watchdog.observe(now, callbacks, nonzero);
            // Non-finite sanitization report, moved OFF the realtime thread
            // (the rt side only bumps the atomic): warn on the delta, at
            // most once per tick.
            let invalid = s.shared.invalid_samples.load(Ordering::Relaxed);
            if invalid > s.invalid_seen {
                log::warn!(
                    "ignored {} non-finite samples (zeroed at the capture/output guards)",
                    invalid - s.invalid_seen
                );
                s.invalid_seen = invalid;
            }
            // Free retired corrections on the control thread.
            s.control.drain_retired();
        }

        // Fail-open (spec: fail-safe ordering / TCC silent failure).
        // `NoInputDetected` means the tap has NEVER captured a nonzero
        // sample since start: either nothing is playing, or the TCC grant
        // is missing and the tap mutes the device while delivering zeros.
        // Holding that state forever risks a silent system, so past the
        // window the controller takes the same path as Disable -- backend
        // stopped, tap destroyed, device unmuted -- and latches
        // `AutoDisabledNoInput`. This can only fire from `NoInputDetected`
        // (pre-first-input by construction); post-`Running` states (`Idle`,
        // `InputSilent`) never reach here -- pausing music must never
        // disable the EQ. It does not touch the start-failure counter, and
        // there is no auto-retry (re-engaging would re-mute the system);
        // an explicit Enable starts again.
        //
        // A live `MeasurementLease` (wizard/1) suspends THIS BRANCH AND
        // NOTHING ELSE -- not the chain, not rebuilds, not Disable/Shutdown,
        // not the reporting above. A measurement legitimately drives the tap
        // to all zeros (the wizard's `Direct` captures are tap-excluded by
        // design), so without the lease the watchdog would tear the tap down
        // mid-session and the verification capture would have no engine to run
        // through.
        let lease_held = self.measurement_lease.load(Ordering::Acquire);
        if lease_held != self.measurement_lease_seen {
            // A RELEASE re-baselines the window instead of firing the disable
            // it deferred: the time accrued under the lease is not evidence of
            // a TCC failure, and firing here would auto-disable the engine
            // immediately after every successful measurement. This must run
            // BEFORE the branch below, or the very tick that observes the
            // release fires it.
            if !lease_held {
                let now = self.now_ms();
                self.watchdog.rebaseline_no_input(now);
            }
            self.measurement_lease_seen = lease_held;
            self.measurement_lease_warned = false;
        }

        if let (Some(window), &EngineStatus::NoInputDetected { since_ms }) =
            (self.fail_open_after_ms, self.watchdog.status())
        {
            let waited = self.now_ms().saturating_sub(since_ms);
            if waited >= window {
                if lease_held {
                    self.warn_fail_open_suspended(waited);
                } else {
                    log::warn!(
                        "fail-open: no input captured for {waited} ms -- auto-disabling \
                         (tap destroyed, un-EQ'd audio restored); send Enable to retry"
                    );
                    self.enabled = false;
                    self.stop_session();
                    self.auto_disabled = Some(waited);
                }
            }
        }

        // Rebuilds trigger ONLY on backend events (drain them all -> one
        // rebuild).
        let mut rebuild = false;
        while self.backend.0.poll_event().is_some() {
            rebuild = true;
        }

        if rebuild {
            self.rebuild();
        } else if self.swap_pending {
            self.send_correction();
        }
    }

    /// One `WARN` per lease, at the moment the fail-open window elapses while
    /// a [`MeasurementLease`] is held: the safety net is off and it just would
    /// have fired. Once per lease rather than once per tick, and the wording
    /// names the cost the spec accepts -- if this really IS a TCC denial, the
    /// device stays muted until the lease is released.
    fn warn_fail_open_suspended(&mut self, waited: u64) {
        if self.measurement_lease_warned {
            return;
        }
        self.measurement_lease_warned = true;
        log::warn!(
            "fail-open suspended: no input captured for {waited} ms, but a measurement lease \
             is held -- the tap stays up (and the device stays muted if this is a TCC denial) \
             until the lease is released"
        );
    }

    /// Start the backend if enabled, not running, and not failed. Two
    /// consecutive start failures latch `Failed`.
    fn try_start(&mut self) {
        if !self.enabled || self.failed.is_some() || self.session.is_some() {
            return;
        }
        match self.start_once() {
            Ok(()) => self.consecutive_start_failures = 0,
            Err(e) => {
                self.consecutive_start_failures += 1;
                if self.consecutive_start_failures >= 2 {
                    self.fail(format!("backend start failed twice: {e}"));
                }
                // else: retry on the next tick.
            }
        }
    }

    /// One start attempt: provisional geometry, then a renegotiation loop
    /// (capped at 3 total starts). Each retry rebuilds the chain at the
    /// last-REPORTED geometry and requests exactly the last-reported buffer
    /// frames; the loop breaks when the report matches the chain. If the
    /// report is still moving after the cap, the mismatch is accepted with
    /// a warning (the chain's uniform frame guard degrades gracefully).
    fn start_once(&mut self) -> Result<(), EngineError> {
        // Provisional: the parity tap is stereo; block from the stored
        // request. See the module docs ("Provisional start + renegotiation").
        const MAX_STARTS: u32 = 3;
        let mut channels = 2;
        let mut block_size = self.requested_buffer_frames.unwrap_or(512);
        let mut request = self.requested_buffer_frames;
        for attempt in 1..=MAX_STARTS {
            let info = self.start_with(channels, block_size, request)?;
            if info.buffer_frames == block_size && info.channels == channels {
                break;
            }
            if attempt == MAX_STARTS {
                log::warn!(
                    "renegotiation did not converge after {MAX_STARTS} starts \
                     (chain {channels} ch x {block_size} frames, reported {info:?}); \
                     accepting degraded pass-through until the next format event"
                );
                break;
            }
            // Renegotiate at the reported geometry. Stop errors are
            // non-fatal here (the backend contract keeps stop idempotent).
            //
            // This is a teardown, so it clears the same session-scoped
            // publications `stop_session` does. Without that, a retry `start`
            // that FAILS leaves `self.session` at `None` (`start_with`
            // assigns it only after `start` returns Ok) while the aborted
            // attempt's `send_correction` verdict is still standing, and the
            // very next `publish` emits `stream: None` next to an
            // `auto_preamp_db` nothing is applying and a
            // `correction_rate_mismatch` naming a stream that is gone --
            // which the desktop renders as "EQ paused, correction must be
            // redesigned" beside "Engine: Stopped".
            let _ = self.backend.0.stop();
            self.clear_session_scoped_publications();
            self.session = None;
            channels = info.channels;
            block_size = info.buffer_frames;
            request = Some(info.buffer_frames);
        }

        self.watchdog.started(self.now_ms());
        Ok(())
    }

    /// Build a FRESH `RtShared` + links + chain, hand the `RtProcessor` to
    /// the backend, and re-apply the retained bypass/gain/correction.
    /// `request` is the buffer-frame hint passed to the backend: the user's
    /// stored request on the first start, the last-reported size on
    /// renegotiation retries.
    ///
    /// Ordering is load-bearing (R1-6): bypass and gain go on the shared
    /// state BEFORE `start`, but the correction is built AFTER it returns,
    /// because only then is the stream's actual sample rate known.
    fn start_with(
        &mut self,
        channels: usize,
        block_size: usize,
        request: Option<usize>,
    ) -> Result<StreamInfo, EngineError> {
        let shared = Arc::new(RtShared::default());
        // Retained params re-applied to the fresh shared state BEFORE start.
        shared.bypass.store(self.bypass, Ordering::Relaxed);
        shared.set_gain(db_to_linear(self.gain_db));

        let (control, rt) = links(self.ring_capacity);
        let chain = RealtimeChain::new(channels, block_size);

        let processor = RtProcessor::new(Arc::clone(&shared), rt, chain);
        let stream = self.backend.0.start(processor, request)?;
        // R1-8's release coefficient, from the geometry the backend just
        // REPORTED -- the same reason the correction is built below rather
        // than above: only `start` knows the effective rate and buffer size.
        //
        // The spec (`:521`) says "before the backend starts -- so the RT
        // thread is not yet running and there is no race"; that ordering is
        // not achievable for a number derived from the negotiated stream, and
        // it does not need to be. This is a single relaxed store to a FRESH
        // `RtShared` whose default is 1.0, so the worst case is the handful of
        // callbacks between `start` returning and this line metering with no
        // release -- the meter reads a touch high for a few milliseconds and
        // then converges. Nothing else reads the coefficient, so there is no
        // cross-variable invariant to tear.
        shared.set_decay_per_block(decay_per_block(stream.buffer_frames, stream.sample_rate));
        self.session = Some(Session {
            block_size,
            channels,
            control,
            invalid_seen: 0,
            shared,
            stream: stream.clone(),
        });

        // R1-6 gap 3 (spec `:407`). The stored correction is re-applied
        // AFTER `start` returns, so it is built at the rate the backend just
        // REPORTED. Building it earlier is what made a rate switch re-send
        // coefficients designed for the old rate -- live and audible for a
        // whole snapshot round-trip (up to `tick_ms`, 250 ms by default)
        // before the desktop could even notice. Now the refusal happens in
        // the same call that used to install them.
        //
        // The cost is the few microseconds between `start` returning and the
        // ring send, during which the chain has no correction: flat
        // pass-through, the fail-open direction, and the same class of
        // transient the renegotiation loop above already produces. A fresh
        // ring (capacity >= 1) cannot be full, so this send never defers.
        self.send_correction();
        Ok(stream)
    }

    /// Stop the running session (if any): drain retired corrections, tear
    /// the backend down, mark the watchdog stopped.
    fn stop_session(&mut self) {
        self.swap_pending = false;
        self.clear_session_scoped_publications();
        if let Some(mut s) = self.session.take() {
            s.control.drain_retired();
            let _ = self.backend.0.stop();
        }
        self.watchdog.stopped();
    }

    /// Retract the two published statements that are only true of a LIVE
    /// stream: `correction_rate_mismatch` names the live stream's rate, and
    /// `auto_preamp_db` names what the live chain is applying. With no
    /// session both are stale, and the invariant is stated in each field's
    /// own doc on [`EngineState`].
    ///
    /// The COUNTERS are deliberately NOT cleared here -- see
    /// `Controller::clipped_samples`: a `Disable` must not blank the clip
    /// count the user is looking at.
    ///
    /// Called from every path that ends a session: `stop_session` and
    /// `start_once`'s renegotiation retry, which tears down without going
    /// through it.
    fn clear_session_scoped_publications(&mut self) {
        self.auto_preamp_db = None;
        self.correction_rate_mismatch = None;
    }

    /// Rebuild-on-change: full stop, then a fresh start from retained
    /// config (spec line 93).
    fn rebuild(&mut self) {
        self.stop_session();
        self.try_start();
    }

    /// Build the retained correction (or a clear) for the live session's
    /// geometry AND ITS REPORTED RATE, then send it through the ring.
    /// Ring-full keeps it pending; the next tick rebuilds from the stored
    /// config and retries.
    ///
    /// R1-6: when `build_correction` refuses the config at the live rate
    /// (baked coefficients from another rate, or a band set with nothing
    /// designable below the new Nyquist), the controller sends an explicit
    /// `Correction(None)` -- flat pass-through, not "leave the old one
    /// running" -- and publishes the rate it must be redesigned for.
    fn send_correction(&mut self) {
        let Some(s) = &mut self.session else {
            // Not running: nothing has been attempted at any rate, so there
            // is no refusal to report and no preamp is being applied. The
            // stored config is applied (and judged) at the next start.
            self.auto_preamp_db = None;
            self.swap_pending = false;
            self.correction_rate_mismatch = None;
            return;
        };
        let stream_rate = s.stream.sample_rate;
        let (msg, mismatch, auto_preamp_db) = match self.correction.as_ref() {
            None => (RtMsg::Correction(None), None, None),
            Some(config) => match build_correction(config, s.channels, s.block_size, stream_rate) {
                // `report`'s drop/substitution counts are already logged by
                // `build_correction`; its preamp is R1-1's `auto_preamp_db`
                // and is published (the drawer must be able to explain the
                // number the engine applied -- spec `:106`).
                Ok((correction, report)) => (
                    RtMsg::Correction(Some(correction)),
                    None,
                    Some(report.preamp_db as f32),
                ),
                Err(e) => {
                    log::warn!(
                        "correction refused at {stream_rate} Hz ({e}); \
                         running flat pass-through until it is redesigned"
                    );
                    (RtMsg::Correction(None), Some(stream_rate), None)
                }
            },
        };
        match s.control.send(msg) {
            Ok(()) => {
                self.swap_pending = false;
                self.auto_preamp_db = auto_preamp_db;
                self.correction_rate_mismatch = mismatch;
            }
            // Ring-full: the built correction was dropped (control plane --
            // safe); nothing reached the chain, so the published verdict is
            // left alone. Retry from the stored config next tick.
            Err(_) => self.swap_pending = true,
        }
    }

    /// Latch `Failed` (controller-owned status) and make sure the backend
    /// is fully stopped.
    fn fail(&mut self, reason: String) {
        self.stop_session();
        let _ = self.backend.0.stop();
        self.failed = Some(reason);
    }

    /// Controller-owned overrides first (`Failed`, then the fail-open
    /// `AutoDisabledNoInput` latch -- which must NOT be overwritten by the
    /// watchdog's `Stopped` after the auto-disable), else the watchdog's
    /// status.
    fn effective_status(&self) -> EngineStatus {
        if let Some(reason) = &self.failed {
            return EngineStatus::Failed {
                reason: reason.clone(),
            };
        }
        if let Some(after_ms) = self.auto_disabled {
            return EngineStatus::AutoDisabledNoInput { after_ms };
        }
        self.watchdog.status().clone()
    }

    /// Publish a snapshot iff it differs from the last published one.
    fn publish(&mut self) {
        let (stream, latency_ms, input_peak, input_peak_session, output_peak, counters) =
            match &self.session {
                Some(s) => (
                    Some(s.stream.clone()),
                    (s.stream.sample_rate > 0.0)
                        .then(|| s.shared.sample_time_delta() / s.stream.sample_rate * 1000.0),
                    s.shared.peak_in(),
                    s.shared.peak_in_session(),
                    s.shared.peak_out(),
                    Some((
                        s.shared.clipped_samples.load(Ordering::Relaxed),
                        s.shared.frame_mismatch_blocks.load(Ordering::Relaxed),
                        s.shared.invalid_samples.load(Ordering::Relaxed),
                    )),
                ),
                // The PEAKS go to zero with the session (nothing is flowing);
                // the COUNTERS are retained below.
                None => (None, None, 0.0, 0.0, 0.0, None),
            };
        // A live session owns the current counts (fresh RtShared -> 0 on each
        // start); a torn-down session retains the last observed ones, so a
        // `Disable` does not blank the clip count the user is looking at.
        if let Some((clipped, frame_mismatch, invalid)) = counters {
            self.clipped_samples = clipped;
            self.frame_mismatch_blocks = frame_mismatch;
            self.invalid_samples = invalid;
        }
        let next = EngineState {
            auto_preamp_db: self.auto_preamp_db,
            bypass: self.bypass,
            clipped_samples: self.clipped_samples,
            correction: self.correction.as_ref().map(CorrectionConfig::descriptor),
            correction_rate_mismatch: self.correction_rate_mismatch,
            enabled: self.enabled,
            frame_mismatch_blocks: self.frame_mismatch_blocks,
            gain_db: self.gain_db,
            input_peak,
            input_peak_session,
            invalid_samples: self.invalid_samples,
            latency_ms,
            output_peak,
            // Straight from the backend -- the controller never infers this.
            // A backend with no live capture answers `false` (trait contract),
            // which is what makes the `(self_excluded, stream)` pair honest
            // through a teardown and through a rebuild window.
            self_excluded: self.backend.0.self_excluded(),
            status: self.effective_status(),
            stream,
        };
        if effectively_equal(&next, &self.published) {
            return;
        }
        let next = Arc::new(next);
        self.state.store(Arc::clone(&next));
        self.published = Arc::clone(&next);
        let mut subscribers = self.subscribers.lock().expect("subscriber list lock");
        subscribers.retain(|tx| match tx.try_send(Arc::clone(&next)) {
            // Full: the subscriber stopped reading -- drop THIS update (it
            // can always read the latest via `state()`), keep the channel.
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

/// R1-8's per-block meter release coefficient for one stream geometry:
///
/// ```text
/// decay = 10 ^ ( -(20.0 / 1.7) * (buffer_frames / sample_rate) / 20.0 )
/// ```
///
/// the broadcast-standard 20 dB / 1.7 s release (spec `:525`), expressed per
/// BLOCK so the realtime lane applies it with one multiply and never needs a
/// clock -- which `shared.rs`'s realtime-lane contract forbids it, and which
/// the spec's own Decisions Log (`:28`) rejects by name along with the
/// alternative of decaying on the controller thread ("needs an atomic
/// swap-on-read that races the RT store").
///
/// Computed from the REPORTED geometry, not hardcoded, so the release is the
/// same wall-clock rate at every buffer size and sample rate: at 512 frames /
/// 48 kHz (10.67 ms/block) it is 0.1254902 dB/block -> 0.9856563, which is the
/// spec's stated 0.98565; at 64 frames / 44.1 kHz it is 0.9980363, and 1.7 s
/// of either is 20 dB.
///
/// Returns 1.0 (no decay -- a plain peak hold) for a degenerate geometry, so
/// a backend reporting 0 Hz or 0 frames leaves a meter that is merely stale
/// rather than one that empties instantly or reads NaN forever.
pub fn decay_per_block(buffer_frames: usize, sample_rate: f64) -> f32 {
    if buffer_frames == 0 || sample_rate <= 0.0 {
        return 1.0;
    }
    let db_per_block = (20.0 / 1.7) * (buffer_frames as f64 / sample_rate);
    10f64.powf(-db_per_block / 20.0) as f32
}

/// Change detection for `publish`, with the jittery telemetry quantized:
/// `latency_ms` compares at 0.1 ms and `input_peak` at 1e-3 resolution, so
/// HAL sample-time jitter does not publish a fresh snapshot every tick.
/// `frame_mismatch_blocks` is compared only as `> 0` (the boolean the UI
/// needs, Task 14): a degraded stretch increments it once per block, and
/// comparing it raw would emit a fresh snapshot + Tauri event every tick for
/// the whole stretch. `enabled` compares exactly so a flip alone publishes.
/// `correction_rate_mismatch` compares EXACTLY (R1-6): it is the desktop's
/// redesign trigger and the user's only disclosure that the EQ is paused, so
/// a change in the rate it names must publish. It has no chatter to prevent
/// -- in a healthy session it is pinned at `None`.
///
/// R1-8 (`:566`) says "the **counters compare exactly** -- a clip must
/// publish", which reads as a contradiction of the `> 0` rule above. Both
/// hold, scoped, and the asymmetry is deliberate: `clipped_samples` and
/// `invalid_samples` compare EXACTLY because in a healthy session they are
/// pinned at 0 (no chatter to prevent) and when they are not, the chatter IS
/// the signal -- a clip count that stops moving is what the drawer is for.
/// `frame_mismatch_blocks` keeps the `> 0` boolean: R1-8 never mentions it,
/// and a degraded stretch increments it every block.
///
/// `auto_preamp_db` also compares exactly: it changes only on a deliberate
/// rebuild, and a changed preamp must publish -- it is a number the app has
/// promised to be able to explain. The two new peaks go through the existing
/// 1e-3 quantization, exactly like `input_peak`.
fn effectively_equal(a: &EngineState, b: &EngineState) -> bool {
    let q_latency = |l: Option<f64>| l.map(|v| (v * 10.0).round() as i64);
    let q_peak = |p: f32| (f64::from(p) * 1000.0).round() as i64;
    a.auto_preamp_db == b.auto_preamp_db
        && a.bypass == b.bypass
        && a.clipped_samples == b.clipped_samples
        && a.correction == b.correction
        && a.correction_rate_mismatch == b.correction_rate_mismatch
        && a.enabled == b.enabled
        && (a.frame_mismatch_blocks > 0) == (b.frame_mismatch_blocks > 0)
        && a.gain_db == b.gain_db
        && q_peak(a.input_peak) == q_peak(b.input_peak)
        && q_peak(a.input_peak_session) == q_peak(b.input_peak_session)
        && a.invalid_samples == b.invalid_samples
        && q_latency(a.latency_ms) == q_latency(b.latency_ms)
        && q_peak(a.output_peak) == q_peak(b.output_peak)
        // EXACT, unlike the quantized meters above: a safety witness must
        // reach the UI on every change, and it only ever has two values.
        && a.self_excluded == b.self_excluded
        && a.status == b.status
        && a.stream == b.stream
}

/// dB -> linear amplitude (f64 math, f32 at the atomics boundary).
fn db_to_linear(db: f32) -> f32 {
    10f64.powf(f64::from(db) / 20.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot with every field at rest, so a test can move exactly one.
    fn at_rest() -> EngineState {
        EngineState {
            auto_preamp_db: None,
            bypass: false,
            clipped_samples: 0,
            correction: None,
            correction_rate_mismatch: None,
            enabled: true,
            frame_mismatch_blocks: 0,
            gain_db: 0.0,
            input_peak: 0.0,
            input_peak_session: 0.0,
            invalid_samples: 0,
            latency_ms: None,
            output_peak: 0.0,
            self_excluded: false,
            status: EngineStatus::Stopped,
            stream: None,
        }
    }

    /// Every EXACTLY-compared field must be able to publish on its own.
    ///
    /// `effectively_equal` returning true makes `publish` return early, so
    /// `self.state` is never stored and no subscriber event is emitted. The
    /// integration suite can only reach a field's compare through whatever
    /// state transition happens to move it, and several of these fields are
    /// only ever moved in lockstep with another one -- `send_correction`
    /// assigns `auto_preamp_db` and `correction_rate_mismatch` as one pair,
    /// so no controller-level script can isolate the second of them at all.
    /// Deleting either compare left the whole workspace green. This is the
    /// direct guard: one field moved, one verdict.
    #[test]
    fn every_exact_compare_publishes_on_its_own() {
        assert!(
            effectively_equal(&at_rest(), &at_rest()),
            "two identical snapshots must NOT publish, or the quantization is pointless"
        );
        /// One named single-field edit to an otherwise at-rest snapshot.
        type Move = (&'static str, fn(&mut EngineState));
        let moves: [Move; 6] = [
            // R1-1: a number the app has promised to be able to explain.
            ("auto_preamp_db", |s| s.auto_preamp_db = Some(-9.4)),
            // R1-8 (`:566`): "the counters compare exactly -- a clip must publish".
            ("clipped_samples", |s| s.clipped_samples = 1),
            ("invalid_samples", |s| s.invalid_samples = 1),
            // R1-6: the desktop's redesign trigger and the user's only
            // disclosure that the EQ is paused.
            ("correction_rate_mismatch", |s| {
                s.correction_rate_mismatch = Some(44_100.0);
            }),
            ("enabled", |s| s.enabled = false),
            // wizard/1: a safety witness must reach the UI on every change.
            ("self_excluded", |s| s.self_excluded = true),
        ];
        for (field, apply) in moves {
            let mut changed = at_rest();
            apply(&mut changed);
            assert!(
                !effectively_equal(&at_rest(), &changed),
                "a change to `{field}` ALONE must publish a fresh snapshot"
            );
        }
    }
}
