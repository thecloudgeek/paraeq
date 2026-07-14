//! The minimal profile store: named EQ presets persisted as one JSON file per
//! profile under `<app-data>/profiles/`, keyed by a slug of the display name.
//!
//! Stage-4 minimal store (stage 5 adds the Profiles tab + WAV impulses on top
//! of THIS format -- do not change it casually).
//!
//! Pure filesystem module -- no Tauri types, fully unit-testable with a
//! tempdir.

use paraeq_dsp::peq::EQBand;
use std::path::{Path, PathBuf};

/// A saved EQ preset: the band set, its preamp trim, and the display name it
/// was saved under (the file itself is named by `slugify(&name)`, but the
/// display name is preserved verbatim inside the file).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Profile {
    pub bands: Vec<EQBand>,
    pub name: String,
    pub preamp_db: f64,
}

/// Turn a display name into a filesystem-safe slug: lowercase, `[a-z0-9]`
/// kept, everything else collapsed to a single `-`, leading/trailing `-`
/// trimmed. An all-punctuation/unicode name that strips to nothing becomes
/// `"profile"` rather than an empty (invalid) filename.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_lowercase() || lc.is_ascii_digit() {
            out.push(lc);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "profile".to_string()
    } else {
        trimmed.to_string()
    }
}

fn profile_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.json", slugify(name)))
}

/// Save `p` to `<dir>/<slug(p.name)>.json`, atomically (write to a `.tmp`
/// sibling, then rename over the destination -- a reader never observes a
/// partial file). Two names that collide on the same slug overwrite each
/// other; last write wins (stage 5 owns collision UX).
pub fn save_profile(dir: &Path, p: &Profile) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = profile_path(dir, &p.name);
    let json = serde_json::to_vec_pretty(p)?;
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp = PathBuf::from(tmp_os);
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)
}

/// Load the profile saved under `slugify(name)`, or `None` if the file is
/// missing or corrupt (corruption is logged via `log::warn`, never panics).
pub fn load_profile(dir: &Path, name: &str) -> Option<Profile> {
    let path = profile_path(dir, name);
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(profile) => Some(profile),
        Err(err) => {
            log::warn!("corrupt profile at {}: {err}", path.display());
            None
        }
    }
}

/// The display names of every valid profile file in `dir`, sorted. A missing
/// `dir` yields an empty list (not an error -- there's simply nothing saved
/// yet). Corrupt files are skipped, not panicked on.
pub fn list_profiles(dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let bytes = std::fs::read(entry.path()).ok()?;
            match serde_json::from_slice::<Profile>(&bytes) {
                Ok(profile) => Some(profile.name),
                Err(err) => {
                    log::warn!("corrupt profile at {}: {err}", entry.path().display());
                    None
                }
            }
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use paraeq_dsp::peq::FilterType;

    fn sample_profile(name: &str) -> Profile {
        Profile {
            bands: vec![EQBand {
                filter_type: FilterType::Peaking,
                fc: 3200.0,
                gain_db: -2.0,
                q: 1.4,
            }],
            name: name.to_string(),
            preamp_db: -3.0,
        }
    }

    #[test]
    fn slugify_lowercases_and_dashes_punctuation() {
        assert_eq!(slugify("My AirPods Pro!"), "my-airpods-pro");
    }

    #[test]
    fn slugify_all_punctuation_falls_back_to_profile() {
        assert_eq!(slugify("---"), "profile");
    }

    #[test]
    fn slugify_strips_unicode() {
        // Non-ASCII letters are not in [a-z0-9]; they collapse to '-' and get
        // trimmed away, leaving only the ASCII remainder.
        assert_eq!(slugify("Café Übermix"), "caf-bermix");
    }

    #[test]
    fn slugify_collapses_repeated_separators() {
        assert_eq!(slugify("a   b--c"), "a-b-c");
    }

    #[test]
    fn save_list_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let profile = sample_profile("HD650");
        save_profile(dir.path(), &profile).unwrap();

        assert_eq!(list_profiles(dir.path()), vec!["HD650".to_string()]);
        assert_eq!(load_profile(dir.path(), "HD650"), Some(profile));
    }

    #[test]
    fn load_by_display_name_resolves_through_slug() {
        let dir = tempfile::tempdir().unwrap();
        let profile = sample_profile("My AirPods Pro!");
        save_profile(dir.path(), &profile).unwrap();

        assert_eq!(
            load_profile(dir.path(), "My AirPods Pro!"),
            Some(profile.clone())
        );
        // Loading by the raw slug also resolves -- same file.
        assert_eq!(load_profile(dir.path(), "my-airpods-pro"), Some(profile));
    }

    #[test]
    fn load_missing_profile_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_profile(dir.path(), "nope"), None);
    }

    #[test]
    fn load_corrupt_profile_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("hd650.json"), b"not json").unwrap();
        assert_eq!(load_profile(dir.path(), "hd650"), None);
    }

    #[test]
    fn list_missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert_eq!(list_profiles(&missing), Vec::<String>::new());
    }

    #[test]
    fn list_skips_corrupt_files_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        save_profile(dir.path(), &sample_profile("Good")).unwrap();
        std::fs::write(dir.path().join("bad.json"), b"not json").unwrap();
        assert_eq!(list_profiles(dir.path()), vec!["Good".to_string()]);
    }

    #[test]
    fn list_is_sorted() {
        let dir = tempfile::tempdir().unwrap();
        save_profile(dir.path(), &sample_profile("Zebra")).unwrap();
        save_profile(dir.path(), &sample_profile("Alpha")).unwrap();
        assert_eq!(
            list_profiles(dir.path()),
            vec!["Alpha".to_string(), "Zebra".to_string()]
        );
    }

    #[test]
    fn colliding_slugs_last_write_wins() {
        let dir = tempfile::tempdir().unwrap();
        save_profile(dir.path(), &sample_profile("HD 650")).unwrap();
        save_profile(dir.path(), &sample_profile("HD-650")).unwrap();
        // Both slugify to "hd-650" -- one file, the second write's name wins.
        assert_eq!(list_profiles(dir.path()), vec!["HD-650".to_string()]);
    }
}
