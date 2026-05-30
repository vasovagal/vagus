//! Directory-scoped result exclusion.
//!
//! Walks UP the directory tree from the current working directory collecting `.vagus/config.json`
//! files (with a flat `.vagus.json` fallback), unions their excluded words ("inherited config"),
//! and drops search hits whose vault-relative path contains an excluded word (case-insensitive
//! substring). A `"root": true` config seals a directory from its ancestors. The scope is inert
//! when no config is found, and **any** error (missing CWD, unreadable file, invalid JSON) degrades
//! to an inert scope — a bad scope file must never fail a search.
//!
//! These config files live in the user's *code* directories (e.g. `~/code/viasat/.vagus/`), never
//! in the iCloud vault (invariant: only Markdown lives in the vault).

use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// On-disk shape of a `.vagus/config.json`. Unknown keys are ignored so old binaries tolerate new
/// fields; every field defaults so a `{}` (or partial) file is valid.
#[derive(Debug, Default, Deserialize)]
struct ScopeFile {
    #[serde(default)]
    exclude: Vec<String>,
    /// Stop walking up at this directory (`.editorconfig`-style seal).
    #[serde(default)]
    root: bool,
}

/// A compiled set of exclusion words plus where they came from.
#[derive(Debug, Default, Clone)]
pub struct Scope {
    /// Lowercased, trimmed, deduped excluded words. Empty => inert (excludes nothing).
    words: Vec<String>,
    /// The nearest config file found while walking up (for an optional diagnostic). `None` if none.
    pub source: Option<PathBuf>,
}

impl Scope {
    /// Inert scope — excludes nothing. Used for `--all`, for filing, and when no config is found.
    pub fn none() -> Self {
        Self::default()
    }

    /// Discover by walking up from the process CWD to `$HOME` (inclusive) or the filesystem root.
    ///
    /// Never errors in practice: a missing CWD or unreadable/invalid config yields an inert scope.
    pub fn discover() -> Result<Self> {
        let Ok(start) = std::env::current_dir() else {
            return Ok(Self::none());
        };
        Ok(Self::discover_from(&start, dirs::home_dir().as_deref()))
    }

    /// Testable core of [`Scope::discover`]. Walks `start` and its ancestors, unioning each config's
    /// `exclude` list. Stops *after* visiting `stop` (inclusive), at the filesystem root, or at the
    /// first config declaring `"root": true`.
    pub fn discover_from(start: &Path, stop: Option<&Path>) -> Self {
        let mut words: Vec<String> = Vec::new();
        let mut source: Option<PathBuf> = None;
        for dir in start.ancestors() {
            if let Some((sf, path)) = read_config(dir) {
                if source.is_none() {
                    source = Some(path); // nearest config is the reported source
                }
                words.extend(sf.exclude);
                if sf.root {
                    break;
                }
            }
            if stop == Some(dir) {
                break;
            }
        }
        Self::from_words(words, source)
    }

    /// Hermetic constructor — no filesystem, no CWD. Lowercases, trims, drops empties, dedups.
    pub fn from_words(words: impl IntoIterator<Item = String>, source: Option<PathBuf>) -> Self {
        let mut out: Vec<String> = Vec::new();
        for w in words {
            let w = w.trim().to_ascii_lowercase();
            if !w.is_empty() && !out.contains(&w) {
                out.push(w);
            }
        }
        Self { words: out, source }
    }

    /// True when there are no exclusion words (the scope excludes nothing).
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// True when `vault_rel` contains any excluded word (case-insensitive substring on the path).
    pub fn is_excluded(&self, vault_rel: &str) -> bool {
        if self.words.is_empty() {
            return false;
        }
        let p = vault_rel.to_ascii_lowercase();
        self.words.iter().any(|w| p.contains(w.as_str()))
    }
}

/// Try `<dir>/.vagus/config.json`, then a flat `<dir>/.vagus.json`. Returns the first that exists and
/// parses; invalid JSON is reported to stderr and skipped (never fatal).
fn read_config(dir: &Path) -> Option<(ScopeFile, PathBuf)> {
    for cand in [dir.join(".vagus").join("config.json"), dir.join(".vagus.json")] {
        let Ok(text) = std::fs::read_to_string(&cand) else {
            continue;
        };
        match serde_json::from_str::<ScopeFile>(&text) {
            Ok(sf) => return Some((sf, cand)),
            Err(e) => eprintln!("vagus: ignoring invalid {}: {e}", cand.display()),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // --- predicate tests (hermetic: no filesystem) -----------------------------------------------

    fn scope(words: &[&str]) -> Scope {
        Scope::from_words(words.iter().map(|s| s.to_string()), None)
    }

    #[test]
    fn substring_matches_path() {
        let s = scope(&["scientist"]);
        assert!(s.is_excluded("10-Projects/scientist/q2.md"));
        assert!(s.is_excluded("00-Inbox/scientist-intro.md"));
        assert!(!s.is_excluded("10-Projects/viasat/q2.md"));
    }

    #[test]
    fn viasat_matches_internal() {
        let s = scope(&["viasat"]);
        assert!(s.is_excluded("30-Resources/viasat-internal/policy.md"));
        assert!(s.is_excluded("20-Areas/viasat/oncall.md"));
        assert!(!s.is_excluded("30-Resources/rust/tokio.md"));
    }

    #[test]
    fn case_insensitive() {
        let s = scope(&["Scientist"]);
        assert!(s.is_excluded("10-Projects/scientist/x.md"));
    }

    #[test]
    fn neutral_paths_kept() {
        let s = scope(&["viasat", "scientist"]);
        for p in [
            "00-Inbox/idea.md",
            "10-Projects/personal/p.md",
            "20-Areas/health/h.md",
            "30-Resources/rust/r.md",
            "40-Archive/old.md",
        ] {
            assert!(!s.is_excluded(p), "should keep {p}");
        }
    }

    #[test]
    fn empty_or_none_is_noop() {
        assert!(Scope::none().is_empty());
        assert!(!Scope::none().is_excluded("10-Projects/scientist/x.md"));
        let blanks = scope(&["  ", ""]);
        assert!(blanks.is_empty());
    }

    #[test]
    fn dedup_and_lowercase() {
        let s = scope(&["viasat", "VIASAT", " Viasat "]);
        assert_eq!(s.words, vec!["viasat".to_string()]);
        assert!(s.is_excluded("10-Projects/viasat/x.md"));
    }

    // --- discovery tests (real temp dirs, never the real CWD/$HOME) ------------------------------

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique temp dir, removed on drop.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("vagus_scope_{}_{}", std::process::id(), n));
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_config(dir: &Path, json: &str) {
        std::fs::create_dir_all(dir.join(".vagus")).unwrap();
        std::fs::write(dir.join(".vagus").join("config.json"), json).unwrap();
    }

    #[test]
    fn walk_up_finds_ancestor() {
        let t = TmpDir::new();
        let root = t.path();
        write_config(root, r#"{ "exclude": ["scientist"] }"#);
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        let s = Scope::discover_from(&deep, Some(root));
        assert!(s.is_excluded("10-Projects/scientist/x.md"));
        assert!(s.source.is_some());
    }

    #[test]
    fn merge_unions_words() {
        let t = TmpDir::new();
        let root = t.path();
        write_config(root, r#"{ "exclude": ["old"] }"#);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        write_config(&repo, r#"{ "exclude": ["scientist"] }"#);
        let s = Scope::discover_from(&repo, Some(root));
        assert!(s.is_excluded("10-Projects/scientist/x.md"));
        assert!(s.is_excluded("40-Archive/old.md"));
    }

    #[test]
    fn root_true_seals() {
        let t = TmpDir::new();
        let root = t.path();
        write_config(root, r#"{ "exclude": ["viasat"] }"#);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        write_config(&repo, r#"{ "exclude": ["scientist"], "root": true }"#);
        let s = Scope::discover_from(&repo, Some(root));
        assert!(s.is_excluded("10-Projects/scientist/x.md"));
        assert!(!s.is_excluded("10-Projects/viasat/x.md"), "ancestor word must be sealed off");
    }

    #[test]
    fn flat_fallback() {
        let t = TmpDir::new();
        let root = t.path();
        std::fs::write(root.join(".vagus.json"), r#"{ "exclude": ["scientist"] }"#).unwrap();
        let s = Scope::discover_from(root, Some(root));
        assert!(s.is_excluded("10-Projects/scientist/x.md"));
    }

    #[test]
    fn no_config_is_noop() {
        let t = TmpDir::new();
        let s = Scope::discover_from(t.path(), Some(t.path()));
        assert!(s.is_empty());
    }

    #[test]
    fn invalid_json_degrades() {
        let t = TmpDir::new();
        let root = t.path();
        write_config(root, "{ this is not json");
        let s = Scope::discover_from(root, Some(root));
        assert!(s.is_empty(), "invalid config must degrade to inert scope");
    }
}
