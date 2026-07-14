//! AutoEq database client: `results/INDEX.md` parsing + cache-first fetch of
//! per-model ParametricEq presets, with a jsDelivr CDN primary and a
//! raw.githubusercontent.com fallback.
//!
//! Behavioral oracle: `prototype/paraeq/correction/autoeq_db.py`. Where the
//! plan sketch and the prototype disagreed, the prototype won — notably the
//! filename-casing order (`ParametricEq.txt` BEFORE `ParametricEQ.txt`,
//! autoeq_db.py:115) and the INDEX.md line grammar
//! (`- [name](./source/rig/name) by source on rig`, leading `./` stripped).
//!
//! # Async design (Task 9, Step 1 — verified against reqwest 0.12 via context7)
//!
//! We took the **async-trait** option, not the sync-seam-plus-`spawn_blocking`
//! one. [`HttpFetch::get`] returns `impl Future + Send` (native RPITIT, no
//! `async-trait` crate) and [`AutoEqClient`] is generic (never `dyn`), so all
//! the testable logic — URL construction, the casing × (primary, fallback)
//! fetch order, cache-first reads, and atomic cache writes — lives inside
//! `AutoEqClient` and is exercised with a no-network [`MockFetch`], while the
//! real [`ReqwestFetch`] awaits an async `reqwest::Client` directly on Tauri's
//! tokio runtime from the `autoeq_*` async commands.
//!
//! Why not `reqwest::blocking` inside `tokio::task::spawn_blocking`: reqwest's
//! blocking wait (`src/blocking/wait.rs::enter`) panics when a tokio runtime is
//! already present in the calling context, and spawn_blocking threads still
//! carry the runtime handle — so the blocking client is unusable here. The
//! async client sidesteps that entirely.
//!
//! [`AutoEqClient`] holds NO Tauri types (crate-boundary rule); the command
//! layer resolves `app_data_dir()/autoeq/` and builds the client.

use paraeq_dsp::peq::{parse_autoeq, ParsedPreset};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};
use std::future::Future;
use std::path::{Path, PathBuf};

/// The DTO the `autoeq_fetch_preset` command returns. Structurally identical to
/// the DSP `ParsedPreset` (`{ bands, preamp_db }`) and serialized by the same
/// serde derive — aliased here so the command signature reads at the wire
/// contract the UI's `ParsedPresetDto` mirrors.
pub type ParsedPresetDto = ParsedPreset;

/// Relative path (below the repo root) of the model index.
const INDEX_REL: &str = "results/INDEX.md";

/// Preset filename casings, in the prototype's try order (autoeq_db.py:115):
/// lowercase-`q` first, then the capital-`Q` spelling the live repo uses.
const FILENAME_CASINGS: [&str; 2] = ["ParametricEq.txt", "ParametricEQ.txt"];

/// Percent-encode set matching Python's `urllib.parse.quote(s, safe="/")`
/// (autoeq_db.py:153,175): encode everything NON_ALPHANUMERIC except the
/// always-unreserved marks `-._~` and the path separator `/`. Spaces, `#`,
/// `+`, `&` etc. are encoded; `/` is preserved so path structure survives.
const PATH_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'/')
    .remove(b'_')
    .remove(b'~');

/// One entry of AutoEq's INDEX.md. The SAME model is measured by multiple
/// sources/rigs, so bare names are AMBIGUOUS: `source`/`rig` disambiguate and
/// fetches are keyed by `path` (the oracle's `rel_path`), never by name.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct IndexEntry {
    pub name: String,
    pub path: String,
    pub rig: String,
    pub source: String,
}

/// Network seam, identical in spirit to the prototype's patchable `_http_get`.
/// Returns `impl Future + Send` (native async-fn-in-trait) so a generic
/// [`AutoEqClient`] can `await` it without pulling in `async-trait`.
pub trait HttpFetch: Send + Sync {
    fn get(&self, url: String) -> impl Future<Output = Result<String, String>> + Send;
}

/// Production [`HttpFetch`]: an async `reqwest::Client` (rustls-tls,
/// default-features off) awaited on Tauri's tokio runtime.
pub struct ReqwestFetch {
    client: reqwest::Client,
}

impl ReqwestFetch {
    /// Build the shared async client. Fails only if TLS/backing setup fails.
    pub fn new() -> Result<ReqwestFetch, String> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("ParaEQ/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(ReqwestFetch { client })
    }
}

impl HttpFetch for ReqwestFetch {
    fn get(&self, url: String) -> impl Future<Output = Result<String, String>> + Send {
        let client = self.client.clone();
        async move {
            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("request to {url} failed: {e}"))?;
            let status = resp.status();
            if !status.is_success() {
                return Err(format!("HTTP {status} for {url}"));
            }
            resp.text()
                .await
                .map_err(|e| format!("reading body of {url} failed: {e}"))
        }
    }
}

/// Cache-first access to AutoEq's index and per-model presets. Free of Tauri
/// types; the command layer supplies `cache_dir`.
pub struct AutoEqClient<F: HttpFetch> {
    base_override: Option<String>,
    cache_dir: PathBuf,
    fetch: F,
    pinned_rev: Option<String>,
}

impl<F: HttpFetch> AutoEqClient<F> {
    /// `base_override` replaces the jsDelivr primary base (tests point it at a
    /// non-network host); the raw-GitHub fallback is always present.
    /// `pinned_rev` pins the commit/tag/branch (default `master`).
    pub fn new(
        fetch: F,
        cache_dir: PathBuf,
        base_override: Option<String>,
        pinned_rev: Option<String>,
    ) -> AutoEqClient<F> {
        AutoEqClient {
            base_override,
            cache_dir,
            fetch,
            pinned_rev,
        }
    }

    /// Cache-first: serve `<cache_dir>/INDEX.md` when present unless `force`;
    /// otherwise fetch (primary then fallback), write the cache atomically, and
    /// parse. A forced sync always refetches and rewrites.
    pub async fn sync_index(&self, force: bool) -> Result<Vec<IndexEntry>, String> {
        let cache_file = self.cache_dir.join("INDEX.md");
        if !force {
            if let Ok(text) = std::fs::read_to_string(&cache_file) {
                return Ok(parse_index(&text));
            }
        }
        let text = self.fetch_first(&[INDEX_REL.to_string()]).await?;
        write_cache_atomic(&cache_file, &text)?;
        Ok(parse_index(&text))
    }

    /// Case-insensitive substring match on entry `name` (prototype parity).
    /// Full entries are returned (borrowed) so callers can disambiguate the
    /// same model measured under multiple sources/rigs.
    pub fn search<'a>(entries: &'a [IndexEntry], query: &str) -> Vec<&'a IndexEntry> {
        let needle = query.to_lowercase();
        entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&needle))
            .collect()
    }

    /// Fetch preset TEXT (cache-first). Parsing is the caller's job. Tries the
    /// URL order casing-A primary → casing-B primary → casing-A fallback →
    /// casing-B fallback (casing inner, base outer), stopping at the first
    /// success. Path segments are percent-encoded; `/` is preserved.
    pub async fn fetch_preset(&self, entry: &IndexEntry) -> Result<String, String> {
        let cache_file = self
            .cache_dir
            .join(format!("{}.txt", entry.path.replace('/', "__")));
        if let Ok(text) = std::fs::read_to_string(&cache_file) {
            return Ok(text);
        }
        let path = entry.path.trim_end_matches('/');
        let model = path.rsplit('/').next().unwrap_or(path);
        let rels: Vec<String> = FILENAME_CASINGS
            .iter()
            .map(|casing| format!("results/{path}/{model} {casing}"))
            .collect();
        let text = self.fetch_first(&rels).await?;
        write_cache_atomic(&cache_file, &text)?;
        Ok(text)
    }

    /// Revision (pinned commit/tag/branch) or the default branch.
    fn rev(&self) -> &str {
        self.pinned_rev.as_deref().unwrap_or("master")
    }

    /// jsDelivr CDN primary (or the test/base override).
    fn primary_base(&self) -> String {
        self.base_override.clone().unwrap_or_else(|| {
            format!(
                "https://cdn.jsdelivr.net/gh/jaakkopasanen/AutoEq@{}",
                self.rev()
            )
        })
    }

    /// raw.githubusercontent.com fallback.
    fn fallback_base(&self) -> String {
        format!(
            "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/{}",
            self.rev()
        )
    }

    /// Try each `rel` under the primary base, then each under the fallback,
    /// returning the first success. `rel` values are percent-encoded per
    /// segment. The last error is surfaced if all attempts fail.
    async fn fetch_first(&self, rels: &[String]) -> Result<String, String> {
        let bases = [self.primary_base(), self.fallback_base()];
        let mut last_err = String::from("no URLs were attempted");
        for base in &bases {
            for rel in rels {
                let url = format!("{}/{}", base.trim_end_matches('/'), encode_path(rel));
                match self.fetch.get(url).await {
                    Ok(text) => return Ok(text),
                    Err(e) => last_err = e,
                }
            }
        }
        Err(last_err)
    }
}

/// Parse fetched preset text into a [`ParsedPresetDto`]. Thin delegate to the
/// DSP oracle-verified parser (`paraeq_dsp::peq::parse_autoeq`) so the AutoEq
/// module owns the whole fetch→parse concern.
pub fn parse_preset(text: &str) -> ParsedPresetDto {
    parse_autoeq(text)
}

/// Percent-encode a repo-relative path, preserving `/`.
fn encode_path(path: &str) -> String {
    percent_encoding::utf8_percent_encode(path, PATH_ENCODE_SET).to_string()
}

/// Parse an AutoEq `results/INDEX.md` into entries. Each recognised line is
/// `- [name](./source/rig/name) by source on rig`; the leading `./` is
/// stripped from the path and non-matching lines (headings, prose, blanks) are
/// skipped. Duplicates (same model, different source/rig) are kept distinct.
/// Oracle: autoeq_db.py:54-79 (regex `_INDEX_LINE`).
pub fn parse_index(md: &str) -> Vec<IndexEntry> {
    md.lines().filter_map(parse_index_line).collect()
}

fn parse_index_line(line: &str) -> Option<IndexEntry> {
    let rest = line.trim().strip_prefix('-')?.trim_start();
    // [name]
    let rest = rest.strip_prefix('[')?;
    let name_end = rest.find(']')?;
    let name = rest[..name_end].trim().to_string();
    // (path)
    let rest = rest[name_end + 1..].trim_start().strip_prefix('(')?;
    let path_end = rest.find(')')?;
    let raw_path = rest[..path_end].trim();
    let path = raw_path.strip_prefix("./").unwrap_or(raw_path).to_string();
    // by <source> on <rig>
    let rest = strip_keyword(rest[path_end + 1..].trim_start(), "by")?;
    let (source, rig) = split_at_keyword(rest, "on")?;
    if name.is_empty() || path.is_empty() || source.is_empty() || rig.is_empty() {
        return None;
    }
    Some(IndexEntry {
        name,
        path,
        rig,
        source,
    })
}

/// If `s` begins with the whitespace-delimited word `kw`, return the remainder
/// with leading whitespace trimmed; else `None`. Mirrors `\s+by\s+`.
fn strip_keyword<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(kw)?;
    rest.starts_with(char::is_whitespace)
        .then(|| rest.trim_start())
}

/// Split `s` at the FIRST whitespace-delimited occurrence of `kw` (mirrors the
/// non-greedy `.+?\s+on\s+.+?` grammar), returning `(before, after)` trimmed.
fn split_at_keyword(s: &str, kw: &str) -> Option<(String, String)> {
    let mut from = 0;
    while let Some(pos) = s[from..].find(kw) {
        let idx = from + pos;
        let before = &s[..idx];
        let after = &s[idx + kw.len()..];
        if before.ends_with(char::is_whitespace) && after.starts_with(char::is_whitespace) {
            return Some((before.trim().to_string(), after.trim().to_string()));
        }
        from = idx + kw.len();
    }
    None
}

/// Write `text` to `path` via a temp file + rename, so an interrupted write
/// never leaves a truncated file a later run would serve as a valid cache hit.
/// Oracle: autoeq_db.py:128-133 `_atomic_write_text`.
fn write_cache_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cache mkdir failed: {e}"))?;
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| "cache path has no file name".to_string())?
        .to_string_lossy();
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("cache write failed: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cache rename failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const SAMPLE_INDEX: &str = include_str!("../tests/data/sample_index.md");
    const SAMPLE_PRESET: &str = include_str!("../tests/data/sample_preset.txt");

    /// No-network [`HttpFetch`]: canned responses keyed by exact URL, recording
    /// every requested URL in order so tests can assert the fetch sequence.
    struct MockFetch {
        responses: HashMap<String, Result<String, String>>,
        log: Mutex<Vec<String>>,
    }

    impl MockFetch {
        fn new(responses: HashMap<String, Result<String, String>>) -> MockFetch {
            MockFetch {
                responses,
                log: Mutex::new(Vec::new()),
            }
        }

        fn urls(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl HttpFetch for MockFetch {
        fn get(&self, url: String) -> impl Future<Output = Result<String, String>> + Send {
            self.log.lock().unwrap().push(url.clone());
            let result = self
                .responses
                .get(&url)
                .cloned()
                .unwrap_or_else(|| Err(format!("mock 404 for {url}")));
            async move { result }
        }
    }

    fn block_on<T>(fut: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn responses(pairs: &[(&str, Result<&str, &str>)]) -> HashMap<String, Result<String, String>> {
        pairs
            .iter()
            .map(|(url, r)| {
                let v = match r {
                    Ok(s) => Ok((*s).to_string()),
                    Err(s) => Err((*s).to_string()),
                };
                ((*url).to_string(), v)
            })
            .collect()
    }

    const PRIMARY: &str = "https://primary.test";
    const FALLBACK: &str = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/rev123";

    fn client(fetch: MockFetch, cache_dir: PathBuf) -> AutoEqClient<MockFetch> {
        AutoEqClient::new(
            fetch,
            cache_dir,
            Some(PRIMARY.to_string()),
            Some("rev123".to_string()),
        )
    }

    fn hd650_first() -> IndexEntry {
        IndexEntry {
            name: "Sennheiser HD 650".to_string(),
            path: "oratory1990/harman_over-ear_2018/Sennheiser HD 650".to_string(),
            rig: "harman_over-ear_2018".to_string(),
            source: "oratory1990".to_string(),
        }
    }

    // -- parse_index --------------------------------------------------------

    #[test]
    fn parse_index_extracts_name_source_rig_path() {
        let entries = parse_index(SAMPLE_INDEX);
        assert_eq!(entries[0], hd650_first());
        // Leading "./" stripped; spaces preserved in path.
        assert!(entries
            .iter()
            .all(|e| !e.path.starts_with("./") && !e.path.is_empty()));
    }

    #[test]
    fn parse_index_preserves_duplicate_models_as_distinct_entries() {
        let entries = parse_index(SAMPLE_INDEX);
        let hd650: Vec<&IndexEntry> = entries
            .iter()
            .filter(|e| e.name == "Sennheiser HD 650")
            .collect();
        assert_eq!(hd650.len(), 2, "same model under two source/rig pairs");
        assert_eq!(hd650[0].source, "oratory1990");
        assert_eq!(hd650[0].rig, "harman_over-ear_2018");
        assert_eq!(hd650[1].source, "crinacle");
        assert_eq!(hd650[1].rig, "GRAS 43AG-7");
        assert_ne!(hd650[0].path, hd650[1].path);
    }

    #[test]
    fn parse_index_skips_headings_and_prose() {
        // Sample has 8 entry lines; everything else (headings, prose, blanks)
        // must be skipped.
        assert_eq!(parse_index(SAMPLE_INDEX).len(), 8);
    }

    // -- search -------------------------------------------------------------

    #[test]
    fn search_is_case_insensitive_substring_on_name() {
        let entries = parse_index(SAMPLE_INDEX);
        let hits = AutoEqClient::<MockFetch>::search(&entries, "hd 650");
        assert_eq!(hits.len(), 2);
        assert!(AutoEqClient::<MockFetch>::search(&entries, "MOONDROP").len() == 1);
        assert!(AutoEqClient::<MockFetch>::search(&entries, "nonexistent").is_empty());
    }

    // -- sync_index ---------------------------------------------------------

    #[test]
    fn sync_index_fetches_primary_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("{PRIMARY}/results/INDEX.md");
        let fetch = MockFetch::new(responses(&[(&url, Ok(SAMPLE_INDEX))]));
        let c = client(fetch, dir.path().to_path_buf());

        let entries = block_on(c.sync_index(false)).unwrap();
        assert_eq!(entries.len(), 8);
        assert_eq!(c.fetch.urls(), vec![url]);
        // Cache written.
        assert!(dir.path().join("INDEX.md").exists());
    }

    #[test]
    fn sync_index_cache_hit_issues_zero_fetches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("INDEX.md"), SAMPLE_INDEX).unwrap();
        // Empty response table: any fetch would error, so a fetch would fail
        // the assertion below too.
        let fetch = MockFetch::new(HashMap::new());
        let c = client(fetch, dir.path().to_path_buf());

        let entries = block_on(c.sync_index(false)).unwrap();
        assert_eq!(entries.len(), 8);
        assert!(c.fetch.urls().is_empty(), "cache hit must not fetch");
    }

    #[test]
    fn sync_index_primary_fails_fallback_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let primary = format!("{PRIMARY}/results/INDEX.md");
        let fallback = format!("{FALLBACK}/results/INDEX.md");
        let fetch = MockFetch::new(responses(&[
            (&primary, Err("boom")),
            (&fallback, Ok(SAMPLE_INDEX)),
        ]));
        let c = client(fetch, dir.path().to_path_buf());

        let entries = block_on(c.sync_index(false)).unwrap();
        assert_eq!(entries.len(), 8);
        assert_eq!(c.fetch.urls(), vec![primary, fallback]);
    }

    #[test]
    fn sync_index_force_refetches_and_rewrites_cache() {
        let dir = tempfile::tempdir().unwrap();
        // Stale cache with a single, different entry.
        let stale = "- [Old Model](./s/r/Old Model) by s on r\n";
        std::fs::write(dir.path().join("INDEX.md"), stale).unwrap();
        let url = format!("{PRIMARY}/results/INDEX.md");
        let fetch = MockFetch::new(responses(&[(&url, Ok(SAMPLE_INDEX))]));
        let c = client(fetch, dir.path().to_path_buf());

        let entries = block_on(c.sync_index(true)).unwrap();
        assert_eq!(entries.len(), 8);
        assert_eq!(c.fetch.urls(), vec![url]);
        let on_disk = std::fs::read_to_string(dir.path().join("INDEX.md")).unwrap();
        assert_eq!(on_disk, SAMPLE_INDEX, "force must rewrite the cache");
    }

    // -- fetch_preset -------------------------------------------------------

    /// The four candidate preset URLs, in try order: casing-A primary,
    /// casing-B primary, casing-A fallback, casing-B fallback.
    fn preset_urls(entry: &IndexEntry) -> [String; 4] {
        let model = entry.path.rsplit('/').next().unwrap();
        let enc = |base: &str, casing: &str| {
            format!(
                "{base}/{}",
                encode_path(&format!("results/{}/{} {}", entry.path, model, casing))
            )
        };
        [
            enc(PRIMARY, "ParametricEq.txt"),
            enc(PRIMARY, "ParametricEQ.txt"),
            enc(FALLBACK, "ParametricEq.txt"),
            enc(FALLBACK, "ParametricEQ.txt"),
        ]
    }

    #[test]
    fn fetch_preset_percent_encodes_and_stops_at_first_success() {
        let dir = tempfile::tempdir().unwrap();
        let entry = hd650_first();
        let urls = preset_urls(&entry);
        // Only casing-A primary succeeds.
        let fetch = MockFetch::new(responses(&[(&urls[0], Ok(SAMPLE_PRESET))]));
        let c = client(fetch, dir.path().to_path_buf());

        let text = block_on(c.fetch_preset(&entry)).unwrap();
        assert_eq!(text, SAMPLE_PRESET);
        // Spaces percent-encoded, "/" preserved.
        assert!(urls[0].contains("Sennheiser%20HD%20650"));
        assert!(urls[0].contains("/results/oratory1990/"));
        // Stopped at the first success — one URL tried.
        assert_eq!(c.fetch.urls(), vec![urls[0].clone()]);
    }

    #[test]
    fn fetch_preset_tries_all_four_urls_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let entry = hd650_first();
        let urls = preset_urls(&entry);
        // Only the LAST candidate (casing-B fallback) succeeds.
        let fetch = MockFetch::new(responses(&[
            (&urls[0], Err("404")),
            (&urls[1], Err("404")),
            (&urls[2], Err("404")),
            (&urls[3], Ok(SAMPLE_PRESET)),
        ]));
        let c = client(fetch, dir.path().to_path_buf());

        let text = block_on(c.fetch_preset(&entry)).unwrap();
        assert_eq!(text, SAMPLE_PRESET);
        assert_eq!(c.fetch.urls(), urls.to_vec());
    }

    #[test]
    fn fetch_preset_encodes_hash_and_plus_and_ampersand() {
        let dir = tempfile::tempdir().unwrap();
        let entry = IndexEntry {
            name: "Test Model #1+2".to_string(),
            path: "oratory1990/harman_in-ear_2019/Test Model #1+2".to_string(),
            rig: "harman_in-ear_2019".to_string(),
            source: "oratory1990".to_string(),
        };
        let urls = preset_urls(&entry);
        let fetch = MockFetch::new(responses(&[(&urls[0], Ok(SAMPLE_PRESET))]));
        let c = client(fetch, dir.path().to_path_buf());

        block_on(c.fetch_preset(&entry)).unwrap();
        assert!(urls[0].contains("Test%20Model%20%231%2B2"));
    }

    #[test]
    fn fetch_preset_cache_hit_issues_zero_fetches() {
        let dir = tempfile::tempdir().unwrap();
        let entry = hd650_first();
        let cache = dir
            .path()
            .join(format!("{}.txt", entry.path.replace('/', "__")));
        std::fs::write(&cache, SAMPLE_PRESET).unwrap();
        let fetch = MockFetch::new(HashMap::new());
        let c = client(fetch, dir.path().to_path_buf());

        let text = block_on(c.fetch_preset(&entry)).unwrap();
        assert_eq!(text, SAMPLE_PRESET);
        assert!(c.fetch.urls().is_empty(), "cache hit must not fetch");
    }

    #[test]
    fn fetch_preset_all_fail_surfaces_last_error() {
        let dir = tempfile::tempdir().unwrap();
        let entry = hd650_first();
        let fetch = MockFetch::new(HashMap::new()); // every URL -> mock 404
        let c = client(fetch, dir.path().to_path_buf());

        let err = block_on(c.fetch_preset(&entry)).unwrap_err();
        assert!(err.contains("mock 404"));
        assert_eq!(c.fetch.urls().len(), 4, "all four candidates attempted");
    }
}
