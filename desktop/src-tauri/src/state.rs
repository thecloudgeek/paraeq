//! The app's in-memory state model and the serializable snapshot the UI
//! renders.
//!
//! `AppState` is serialized whole and pushed to the frontend on every change
//! (the `app-state` event) and returned by the `get_app_state` command. Its
//! wire shape -- together with `EqState` and `OutputDeviceInfo` -- is pinned
//! by the `app_state_wire_format_is_pinned` golden test below.
//!
//! `desktop/ui/src/ipc/types.ts` mirrors these shapes BY HAND (no codegen),
//! exactly as it mirrors `paraeq-engine`'s `test_wire_format.rs` and
//! `paraeq-dsp`'s golden band JSON. A field rename here fails that golden test
//! in the same PR, so the TS types can never silently drift from the Rust
//! source of truth.

use crate::settings::Settings;
use paraeq_coreaudio::backend::ExclusionWitness;
use paraeq_dsp::peq::EQBand;
use paraeq_engine::controller::{EngineHandle, EngineState};
use std::path::PathBuf;
use std::sync::Mutex;

/// A selectable output device, projected from `paraeq_coreaudio::devices::
/// OutputDevice` into a serde-friendly shape (the coreaudio crate stays
/// serde-free by crate-boundary rule, and its live HAL object id is not part
/// of the UI contract).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct OutputDeviceInfo {
    pub name: String,
    pub uid: String,
}

/// The EQ the UI shows: the current band set plus the preamp trim.
#[derive(Clone, Debug, serde::Serialize)]
pub struct EqState {
    pub bands: Vec<EQBand>,
    pub preamp_db: f64,
}

/// One diagnostic the Verify screen shows, with the copy the user acts on.
///
/// A flat projection rather than the typed enum: `code` is the stable wire
/// number and `remedy` is the plain-language fix, both rendered in Rust. The
/// UI is a text field, not an author -- a second remedy vocabulary in
/// TypeScript is a second place for the wrong advice to be given.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct VerifyDiagnostic {
    pub code: u16,
    pub remedy: String,
    /// `"refuse"` or `"warn"`. Snake case because everything else on this wire
    /// is; the Rust enum is `paraeq_decide::Severity`.
    pub severity: String,
    pub summary: String,
}

/// What a graded verification pass produced.
///
/// Both `gate_db` and `residual_rms_db` are `Option` because `decide()` only
/// produces them when it got far enough to grade: a bundle it refuses on
/// routing or on a dropped band carries diagnostics and no residual, and
/// showing `0.0` there would read as a perfect result.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct VerifyReport {
    /// The abort budget THIS run computed: one helper block + the 5 ms ramp +
    /// the engine's reported latency + slack. Reported, never asserted -- no
    /// process can guarantee the acoustic tail.
    pub abort_acoustic_budget_ms: f64,
    pub diagnostics: Vec<VerifyDiagnostic>,
    /// The threshold `residual_rms_db` was compared against.
    pub gate_db: Option<f64>,
    /// The engine's OWN armed preamp during the pass, dB. **The preamp
    /// disclosure is mandatory**, not decorative: the user is being told the
    /// number their music is now being played through.
    pub installed_preamp_db: f64,
    /// `L_verify`, sweep-span RMS, dBFS.
    pub level_dbfs: f64,
    /// RMS of `residual_vs_prediction` over the authority band, as the WORST
    /// capture channel. Never the mean: one bad ear must not be rescued by a
    /// good one.
    pub residual_rms_db: Option<f64>,
    /// `"proceed"`, `"proceed_with_warnings"` or `"refuse"`.
    pub verdict: String,
}

/// Where a verification pass is, as the Verify screen sees it.
///
/// Internally tagged on `phase`, mirroring `EngineStatus`'s `kind` tagging so
/// the UI reads one discriminated-union convention and not two.
///
/// `Armed` is its own phase because MS-18 makes it one: the acknowledgement
/// names the device and the projected SPL, it is a deliberate action rather
/// than a default-focused button, and **no sweep is reachable without it**.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub enum VerifyState {
    /// No pass exists. Nothing has been armed and nothing can play.
    ///
    /// `#[default]`, and it must stay so: a slot that defaulted to any other
    /// phase would claim a pass exists before one has been armed.
    #[default]
    Idle,
    /// Gates 1-6 passed and the level is decided. Awaiting MS-18.
    Armed {
        device_name: String,
        /// `L_verify`, sweep-span RMS, dBFS.
        level_dbfs: f64,
        /// The SPL this level projects at the mic -- the number the
        /// acknowledgement must show and the number the user acknowledges.
        projected_spl_db: f64,
    },
    /// Acknowledged: the helper is spawning, playing, or the capture is being
    /// analysed. One phase rather than three because the pass is one blocking
    /// call and reporting a sub-phase we cannot observe would be a guess.
    Running,
    /// The pass refused, or a seam failed. `code` is the stable
    /// `MeasurementDiagnostic` number when the failure had one.
    Failed {
        code: Option<u16>,
        remedy: Option<String>,
        summary: String,
    },
    /// The pass completed and `decide()` graded it.
    Complete { report: VerifyReport },
}

/// THE snapshot the UI renders. Serialized whole on every change (`app-state`
/// event) and returned by `get_app_state`. NO permission field -- TCC is
/// undetectable; `engine.status` carries the honest proxies.
#[derive(Clone, Debug, serde::Serialize)]
pub struct AppState {
    pub active_profile: Option<String>,
    pub default_output_uid: Option<String>,
    pub devices: Vec<OutputDeviceInfo>,
    pub engine: EngineState,
    pub eq: EqState,
    pub profiles: Vec<String>,
    pub setup_complete: bool,
    /// The verification pass, if one has been armed this session. Rides the
    /// existing `app-state` event rather than minting a second channel: the UI
    /// already has exactly one subscription and one snapshot to reconcile.
    pub verification: VerifyState,
}

/// Everything mutable the app owns. `data` is the ONE Mutex commands normally
/// touch (they are rare and cheap), and the other four slots each own their own
/// lock for a reason stated on the field. **There is no lock ORDER to get
/// wrong, because there is no nesting** — every field below states the rule
/// that keeps it that way, and `verify`'s is the newest. The `EngineHandle`
/// lives in its own slot so `RunEvent::Exit` can `.take()` it and drive
/// teardown to completion.
///
/// # There is deliberately no `MeasurementLease` slot here
///
/// `paraeq_engine::controller::MeasurementLease` (wizard-design.md:418)
/// suspends the engine's fail-open auto-disable so the 15 s watchdog cannot
/// tear the tap down mid-measurement -- a measurement legitimately drives the
/// tap to all zeros, because `Direct` captures are tap-excluded by design.
/// It is not a field of `AppShared` and should not become one: a token parked
/// in long-lived state is not released by a panic, and a leaked token disables
/// fail-open **forever**, so a TCC denial then holds the user's system muted
/// with no auto-recovery. It is an RAII token; it belongs to the wizard-run
/// scope, which does not exist in this crate yet. When it lands:
///
/// - **Acquire** at the spine's `Probe` step, beside the device/rate/TCC checks
///   (wizard:39-40 lists "engine lease" there), from the `EngineHandle` in
///   [`AppShared::engine`]: `handle.acquire_measurement_lease()`. `None` means
///   a measurement is already running -- refuse this one and say so.
/// - **Check engine status first, and separately.** The lease knows nothing
///   about status, so it succeeds against a stopped engine. A stopped engine is
///   `paraeq_measure::MeasurementDiagnostic::EngineNotRunning` (wire code 24),
///   whose remedy is "turn ParaEQ on" -- not "wait for the other measurement",
///   and not `EngineFailed`'s "restart". The same diagnostic is what
///   `AbortReason::EngineStopped` maps to when the engine goes away mid-run.
/// - **Release** by letting the token drop: at Result/Save, at cancel, and on
///   every error exit. Declare it BEFORE the `MeasurementSession`s it covers:
///   Rust drops locals in REVERSE declaration order, so the lease declared
///   first is released last -- strictly after MS-14's restore sequence (abort
///   ramp, sink stop, volume restore) has run on the sessions declared after
///   it. (Declaration order is only load-bearing on the panic path; `finish()`
///   and `abort_now()` both run the restore eagerly at the call site.)
///
/// One lease spans the whole run, not one per sweep: a session is one sweep, so
/// a nine-position capture is nine sessions under a single lease.
pub struct AppShared {
    pub data: Mutex<AppData>,
    pub engine: Mutex<Option<EngineHandle>>,
    /// The MS-6 self-exclusion witness, cloned off the `TapBackend` in
    /// [`engine_bridge::spawn_engine`](crate::engine_bridge::spawn_engine)
    /// BEFORE the backend was moved into the controller thread -- the only
    /// moment it can be taken. It stays live and truthful for the life of that
    /// backend, tracking every start and stop, so the measurement runtime can
    /// poll the tap's CURRENT state at every gate that precedes emission
    /// instead of reading a snapshot that is up to a controller tick stale.
    /// No lock: it is an atomic cell behind an `Arc`.
    ///
    /// Its first reader is
    /// [`verify_seam::tap_status`](crate::verify_seam::tap_status), which hands
    /// it to `MeasurementSession::begin` as the `TapStatus` half of
    /// `SessionSeam` -- so the `#[allow(dead_code)]` this field used to carry
    /// is gone, as that attribute's own note instructed ("delete the
    /// attribute, not the field, when the wizard lands").
    pub exclusion_witness: ExclusionWitness,
    /// In-memory mirror of the durable [`Settings`] currently on disk, seeded at
    /// setup from the loaded file. `publish` compares the freshly-composed
    /// durable subset against this to decide whether to write, so a snapshot
    /// that leaves the durable subset unchanged (engine-only ticks: status,
    /// peak, latency) never touches the disk -- not even to read. Its own lock,
    /// held only across the compare-and-save inside `publish`, never nested with
    /// `data`/`engine`.
    pub persisted: Mutex<Settings>,
    /// The first-launch chime probe (child process + loop thread bookkeeping).
    /// Owns its own synchronization; never nested with `data`/`engine`. `stop`
    /// is called on wizard completion/cancel and on app exit so no `afplay`
    /// helper ever outlives its purpose.
    pub probe: crate::setup::ProbeState,
    pub profiles_dir: PathBuf,
    pub settings_path: PathBuf,
    /// The single verification slot: at most one armed or running pass, plus
    /// the abort trigger and the worker that runs it.
    ///
    /// **Never nested with `data`: read into a value and released before the
    /// data lock is taken.** `engine_bridge.rs`'s snapshot path is where that
    /// matters — it needs both the verification state and the model in one
    /// frame, and it takes `verify`, clones the state out, drops the guard, and
    /// only then takes `data`. Holding this across a `data` acquisition would
    /// be the first ordering constraint in the app, and the worker thread
    /// publishes into exactly these two locks from the other direction.
    ///
    /// Unlike the `MeasurementLease` this deliberately IS parked in shared
    /// state, and the distinction is the reason the paragraph above gives. A
    /// lease parked here is not released by a panic and would disable fail-open
    /// forever; a verification pass parked here holds its lease inside itself
    /// as an RAII token, so the only way to drop the pass -- which `abort` and
    /// `shutdown` both do explicitly -- is also the way the lease is released.
    /// Nothing here outlives a run: `Idle` is the state between passes.
    pub verify: Mutex<crate::verify::VerifyRuntime>,
}

/// The mutable app model. Persisted fields (`active_profile`, `bands`,
/// `engine_enabled`, `preamp_db`, `setup_complete`) round-trip through
/// [`Settings`]; runtime-discovered fields (`default_output_uid`, `devices`,
/// `profiles`) are re-derived on startup and device changes, never persisted.
#[derive(Clone, Debug)]
pub struct AppData {
    pub active_profile: Option<String>,
    pub bands: Vec<EQBand>,
    pub default_output_uid: Option<String>,
    pub devices: Vec<OutputDeviceInfo>,
    pub engine_enabled: bool, // persisted intent (survives fail-open)
    pub preamp_db: f64,
    pub profiles: Vec<String>,
    pub setup_complete: bool,
}

impl AppData {
    /// Seed the model from persisted settings; runtime-discovered fields start
    /// empty and are filled in by the first device enumeration.
    pub fn from_settings(s: &Settings) -> AppData {
        AppData {
            active_profile: s.active_profile.clone(),
            bands: s.bands.clone(),
            default_output_uid: None,
            devices: Vec::new(),
            engine_enabled: s.engine_enabled,
            preamp_db: s.preamp_db,
            profiles: Vec::new(),
            setup_complete: s.setup_complete,
        }
    }

    /// Apply the setup wizard's terminal choice, factored out pure so it is
    /// unit-testable without a Tauri app. `enable` is the user's ACTUAL decision,
    /// decoupled from the probe's *temporary* enable (`setup_probe_start` flips
    /// `engine_enabled` on the moment the diagnostic runs, before any grant is
    /// verified): the opt-in path (a verified-`Running` probe, then Finish)
    /// passes `true`; "Skip for now" / completing without a successful enable
    /// passes `false`. Records `enable` as the persisted `engine_enabled` intent
    /// (which survives fail-open across launches -- decision 1) and marks setup
    /// complete. Returns `true` when the engine must be told to `Disable` to
    /// revert a probe's temporary enable -- i.e. exactly when the user declined,
    /// so a skipping user is never re-muted on future launches.
    pub fn complete_setup(&mut self, enable: bool) -> bool {
        self.engine_enabled = enable;
        self.setup_complete = true;
        !enable
    }

    /// Project the persisted subset back out for saving.
    pub fn to_settings(&self) -> Settings {
        Settings {
            active_profile: self.active_profile.clone(),
            bands: self.bands.clone(),
            engine_enabled: self.engine_enabled,
            preamp_db: self.preamp_db,
            setup_complete: self.setup_complete,
        }
    }

    /// Compose the UI snapshot from this model plus the latest engine state
    /// and the verification slot.
    ///
    /// `verification` is passed in rather than read here for the same reason
    /// `engine` is: this type is the persisted model, and both of those live
    /// behind their own locks in `AppShared`. Threading them through keeps this
    /// function pure and unit-testable without an app.
    pub fn app_state(&self, engine: &EngineState, verification: VerifyState) -> AppState {
        AppState {
            active_profile: self.active_profile.clone(),
            default_output_uid: self.default_output_uid.clone(),
            devices: self.devices.clone(),
            engine: engine.clone(),
            eq: EqState {
                bands: self.bands.clone(),
                preamp_db: self.preamp_db,
            },
            profiles: self.profiles.clone(),
            setup_complete: self.setup_complete,
            verification,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paraeq_dsp::peq::FilterType;
    use paraeq_engine::backend::StreamInfo;
    use paraeq_engine::status::EngineStatus;

    fn sample_settings() -> Settings {
        Settings {
            active_profile: Some("hd650".into()),
            bands: vec![EQBand {
                filter_type: FilterType::Peaking,
                fc: 3200.0,
                gain_db: -2.0,
                q: 1.4,
            }],
            engine_enabled: true,
            preamp_db: -3.0,
            setup_complete: true,
        }
    }

    #[test]
    fn from_settings_to_settings_round_trip() {
        let settings = sample_settings();
        let data = AppData::from_settings(&settings);
        assert_eq!(data.to_settings(), settings);
    }

    /// "Skip for now" / finishing without a verified enable: the terminal
    /// choice must persist `engine_enabled = false` (so future launches do NOT
    /// re-engage the tap and mute audio while the grant is still missing) AND
    /// request an engine `Disable` to revert the probe's temporary enable.
    #[test]
    fn complete_setup_skip_persists_disabled_and_requests_disable() {
        // Model the mid-wizard state a skip hits: the probe left the engine
        // enabled, setup is not yet complete.
        let mut data = AppData::from_settings(&sample_settings());
        data.engine_enabled = true;
        data.setup_complete = false;

        let must_disable = data.complete_setup(false);

        assert!(must_disable, "skip must request an engine Disable");
        assert!(
            !data.engine_enabled,
            "skip must persist engine_enabled = false"
        );
        assert!(data.setup_complete, "setup must be marked complete");
    }

    /// Explicit opt-in (a verified-`Running` probe, then Finish): persist
    /// `engine_enabled = true` and do NOT request a Disable. This is the intent
    /// that survives fail-open across launches (decision 1).
    #[test]
    fn complete_setup_opt_in_persists_enabled_and_no_disable() {
        let mut data = AppData::from_settings(&sample_settings());
        data.engine_enabled = true;
        data.setup_complete = false;

        let must_disable = data.complete_setup(true);

        assert!(!must_disable, "opt-in must NOT request a Disable");
        assert!(
            data.engine_enabled,
            "opt-in must persist engine_enabled = true"
        );
        assert!(data.setup_complete, "setup must be marked complete");
    }

    #[test]
    fn from_settings_leaves_runtime_fields_empty() {
        let data = AppData::from_settings(&sample_settings());
        assert_eq!(data.default_output_uid, None);
        assert!(data.devices.is_empty());
        assert!(data.profiles.is_empty());
    }

    /// Golden wire-format test. `desktop/ui/src/ipc/types.ts` mirrors this
    /// `AppState`/`EqState`/`OutputDeviceInfo` shape BY HAND, exactly as it
    /// mirrors `paraeq-engine`'s `test_wire_format.rs`. A field rename in
    /// `state.rs` fails HERE instead of silently breaking the UI.
    #[test]
    fn app_state_wire_format_is_pinned() {
        let data = AppData {
            active_profile: Some("hd650".into()),
            bands: vec![EQBand {
                filter_type: FilterType::Peaking,
                fc: 3200.0,
                gain_db: -2.0,
                q: 1.4,
            }],
            default_output_uid: Some("uid-1".into()),
            devices: vec![OutputDeviceInfo {
                name: "MacBook Pro Speakers".into(),
                uid: "uid-1".into(),
            }],
            engine_enabled: true,
            preamp_db: -3.0,
            profiles: vec!["hd650".into(), "hd800".into()],
            setup_complete: true,
        };
        let engine = EngineState {
            auto_preamp_db: Some(-9.5),
            bands_dropped: 1,
            bypass: false,
            clipped_samples: 3,
            correction: Some("peq:1-band".into()),
            correction_rate_mismatch: None,
            enabled: true,
            frame_mismatch_blocks: 0,
            gain_db: -3.0,
            input_peak: 0.25,
            input_peak_session: 0.75,
            invalid_samples: 2,
            latency_ms: Some(62.3),
            output_peak: 1.5,
            sections_substituted: 2,
            self_excluded: true,
            status: EngineStatus::Running,
            stream: Some(StreamInfo {
                buffer_frames: 512,
                channels: 2,
                device_uid: "uid-1".into(),
                sample_rate: 48000.0,
            }),
        };

        assert_eq!(
            serde_json::to_value(data.app_state(
                &engine,
                VerifyState::Complete {
                    report: VerifyReport {
                        abort_acoustic_budget_ms: 21.7,
                        diagnostics: vec![VerifyDiagnostic {
                            code: 41,
                            remedy: "Re-run the measurement.".into(),
                            severity: "refuse".into(),
                            summary: "VerificationResidual (2.9)".into(),
                        }],
                        gate_db: Some(2.0),
                        installed_preamp_db: -6.0,
                        level_dbfs: -21.0,
                        residual_rms_db: Some(2.9),
                        verdict: "refuse".into(),
                    },
                },
            ))
            .unwrap(),
            serde_json::json!({
                "active_profile": "hd650",
                "default_output_uid": "uid-1",
                "devices": [
                    { "name": "MacBook Pro Speakers", "uid": "uid-1" }
                ],
                "engine": {
                    "auto_preamp_db": -9.5,
                    "bands_dropped": 1,
                    "bypass": false,
                    "clipped_samples": 3,
                    "correction": "peq:1-band",
                    "correction_rate_mismatch": null,
                    "enabled": true,
                    "frame_mismatch_blocks": 0,
                    "gain_db": -3.0,
                    "input_peak": 0.25,
                    "input_peak_session": 0.75,
                    "invalid_samples": 2,
                    "latency_ms": 62.3,
                    "output_peak": 1.5,
                    "sections_substituted": 2,
                    "self_excluded": true,
                    "status": { "kind": "running" },
                    "stream": {
                        "buffer_frames": 512,
                        "channels": 2,
                        "device_uid": "uid-1",
                        "sample_rate": 48000.0
                    }
                },
                "eq": {
                    "bands": [
                        {
                            "filter_type": "peaking",
                            "fc": 3200.0,
                            "gain_db": -2.0,
                            "q": 1.4
                        }
                    ],
                    "preamp_db": -3.0
                },
                "profiles": ["hd650", "hd800"],
                "setup_complete": true,
                "verification": {
                    "phase": "complete",
                    "report": {
                        "abort_acoustic_budget_ms": 21.7,
                        "diagnostics": [
                            {
                                "code": 41,
                                "remedy": "Re-run the measurement.",
                                "severity": "refuse",
                                "summary": "VerificationResidual (2.9)"
                            }
                        ],
                        "gate_db": 2.0,
                        "installed_preamp_db": -6.0,
                        "level_dbfs": -21.0,
                        "residual_rms_db": 2.9,
                        "verdict": "refuse"
                    }
                }
            })
        );
    }

    /// Every `VerifyState` phase on the wire, because `desktop/ui/src/ipc/
    /// types.ts` mirrors this union BY HAND and the golden above only exercises
    /// one arm of it. A phase whose tag or payload drifts fails here, in the
    /// same PR as the TypeScript that has to change with it.
    #[test]
    fn every_verify_phase_is_pinned_on_the_wire() {
        let json = |state: VerifyState| serde_json::to_value(state).unwrap();

        assert_eq!(
            json(VerifyState::Idle),
            serde_json::json!({"phase": "idle"})
        );
        assert_eq!(
            json(VerifyState::Armed {
                device_name: "AirPods Max".into(),
                level_dbfs: -21.0,
                projected_spl_db: 78.0,
            }),
            serde_json::json!({
                "phase": "armed",
                "device_name": "AirPods Max",
                "level_dbfs": -21.0,
                "projected_spl_db": 78.0
            })
        );
        assert_eq!(
            json(VerifyState::Running),
            serde_json::json!({"phase": "running"})
        );
        assert_eq!(
            json(VerifyState::Failed {
                code: Some(24),
                remedy: Some("Turn ParaEQ on, then measure again.".into()),
                summary: "EngineNotRunning".into(),
            }),
            serde_json::json!({
                "phase": "failed",
                "code": 24,
                "remedy": "Turn ParaEQ on, then measure again.",
                "summary": "EngineNotRunning"
            })
        );
        // A seam failure has a message and NO code and NO remedy -- and both
        // must serialize as `null` rather than being omitted, or the hand-
        // mirrored TypeScript reads them as optional keys.
        assert_eq!(
            json(VerifyState::Failed {
                code: None,
                remedy: None,
                summary: "seam failure: cannot spawn the helper".into(),
            }),
            serde_json::json!({
                "phase": "failed",
                "code": null,
                "remedy": null,
                "summary": "seam failure: cannot spawn the helper"
            })
        );
    }
}
