//! Persisted user settings: a single JSON document loaded at startup and
//! rewritten atomically on every change.
//!
//! `#[serde(default)]` makes the format forward-tolerant: an older on-disk
//! file missing a field newer builds added deserializes fine (the missing
//! field takes its `Default`). A missing file or corrupt bytes both fall back
//! to `Settings::default()` rather than panicking, so a hand-mangled or
//! partially written config never bricks startup.

use paraeq_dsp::peq::EQBand;
use std::path::{Path, PathBuf};

/// The complete persisted app configuration. Runtime-discovered facts (the
/// live device list, the current default output) are NOT stored here -- only
/// the user's durable intent.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Settings {
    pub active_profile: Option<String>,
    pub bands: Vec<EQBand>,
    pub engine_enabled: bool,
    pub preamp_db: f64,
    pub setup_complete: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            active_profile: None,
            bands: Vec::new(),
            engine_enabled: false,
            preamp_db: 0.0,
            setup_complete: false,
        }
    }
}

/// Load settings from `path`. A missing file or corrupt JSON yields
/// `Settings::default()` (a `log::warn!` records corruption; this never
/// panics).
pub fn load(path: &Path) -> Settings {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Settings::default(),
    };
    match serde_json::from_slice(&bytes) {
        Ok(settings) => settings,
        Err(err) => {
            log::warn!(
                "corrupt settings at {}: {err}; falling back to defaults",
                path.display()
            );
            Settings::default()
        }
    }
}

/// Persist `s` to `path` atomically: create parent directories, write the
/// serialized document to `<path>.tmp`, then `rename` it over `path`. A reader
/// therefore only ever observes a complete, valid file.
pub fn save(path: &Path, s: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(s)?;
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp = PathBuf::from(tmp_os);
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use paraeq_dsp::peq::FilterType;

    fn sample_settings() -> Settings {
        Settings {
            active_profile: Some("hd650".into()),
            bands: vec![
                EQBand {
                    filter_type: FilterType::LowShelf,
                    fc: 105.0,
                    gain_db: 4.5,
                    q: 0.7,
                },
                EQBand {
                    filter_type: FilterType::Peaking,
                    fc: 3200.0,
                    gain_db: -2.0,
                    q: 1.4,
                },
            ],
            engine_enabled: true,
            preamp_db: -3.0,
            setup_complete: true,
        }
    }

    #[test]
    fn load_missing_path_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        assert_eq!(load(&path), Settings::default());
    }

    #[test]
    fn load_garbage_bytes_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"}{ this is not json <<<").unwrap();
        assert_eq!(load(&path), Settings::default());
    }

    #[test]
    fn save_load_round_trip_with_bands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = sample_settings();
        save(&path, &settings).unwrap();
        assert_eq!(load(&path), settings);
    }

    #[test]
    fn save_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/settings.json");
        let settings = sample_settings();
        save(&path, &settings).unwrap();
        assert_eq!(load(&path), settings);
    }

    #[test]
    fn serde_default_tolerates_old_file_missing_new_fields() {
        // An older config that predates the `engine_enabled` / `preamp_db` /
        // `setup_complete` fields must still load, with those fields defaulted.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, br#"{"active_profile":"legacy","bands":[]}"#).unwrap();
        let loaded = load(&path);
        assert_eq!(
            loaded,
            Settings {
                active_profile: Some("legacy".into()),
                bands: Vec::new(),
                engine_enabled: false,
                preamp_db: 0.0,
                setup_complete: false,
            }
        );
    }

    #[test]
    fn save_is_atomic_no_tmp_remains() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        save(&path, &sample_settings()).unwrap();

        // No leftover temp file, and the destination parses cleanly.
        let tmp = dir.path().join("settings.json.tmp");
        assert!(!tmp.exists(), "temp file should be renamed away");
        let bytes = std::fs::read(&path).unwrap();
        let _: Settings = serde_json::from_slice(&bytes).unwrap();
    }
}
