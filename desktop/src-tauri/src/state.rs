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
}

/// Everything mutable the app owns, behind ONE Mutex (commands are rare and
/// cheap; no lock ordering to get wrong). The `EngineHandle` lives in its own
/// slot so `RunEvent::Exit` can `.take()` it and drive teardown to completion.
pub struct AppShared {
    pub data: Mutex<AppData>,
    pub engine: Mutex<Option<EngineHandle>>,
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

    /// Compose the UI snapshot from this model plus the latest engine state.
    pub fn app_state(&self, engine: &EngineState) -> AppState {
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
            bypass: false,
            correction: Some("peq:1-band".into()),
            correction_rate_mismatch: None,
            enabled: true,
            frame_mismatch_blocks: 0,
            gain_db: -3.0,
            input_peak: 0.25,
            latency_ms: Some(62.3),
            status: EngineStatus::Running,
            stream: Some(StreamInfo {
                buffer_frames: 512,
                channels: 2,
                device_uid: "uid-1".into(),
                sample_rate: 48000.0,
            }),
        };

        assert_eq!(
            serde_json::to_value(data.app_state(&engine)).unwrap(),
            serde_json::json!({
                "active_profile": "hd650",
                "default_output_uid": "uid-1",
                "devices": [
                    { "name": "MacBook Pro Speakers", "uid": "uid-1" }
                ],
                "engine": {
                    "bypass": false,
                    "correction": "peq:1-band",
                    "correction_rate_mismatch": null,
                    "enabled": true,
                    "frame_mismatch_blocks": 0,
                    "gain_db": -3.0,
                    "input_peak": 0.25,
                    "latency_ms": 62.3,
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
                "setup_complete": true
            })
        );
    }
}
