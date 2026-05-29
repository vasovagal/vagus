//! Vault walk + incremental diff (mtime then sha256), persisting files + chunks.
//!
//! tantivy and embeddings are layered on in later steps; this module owns the change detection and
//! the SQLite side. Paths are stored **vault-relative** so the index is portable and matches the
//! "Brain/ holds only markdown" model.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use walkdir::{DirEntry, WalkDir};

use crate::chunk::chunk_markdown;
use crate::config::Config;
use crate::db::Db;
use crate::util::{now_unix, sha256_hex};

#[derive(Debug, Default)]
pub struct IndexStats {
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
}

fn is_hidden(e: &DirEntry) -> bool {
    e.file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

fn is_markdown(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("md")).unwrap_or(false)
}

/// Every `*.md` under the vault, skipping hidden dirs (`.obsidian`, `.git`, `.trash`, …).
pub fn walk_vault(vault: &Path) -> Vec<PathBuf> {
    WalkDir::new(vault)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && is_markdown(e.path()))
        .map(|e| e.into_path())
        .collect()
}

fn mtime_secs(path: &Path) -> Result<f64> {
    let modified = fs::metadata(path)?.modified()?;
    Ok(modified.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}

/// Run an incremental index (or full rebuild when `reindex`).
pub fn run(cfg: &Config, reindex: bool) -> Result<IndexStats> {
    if !cfg.vault.exists() {
        bail!(
            "vault not found: {} (set VAGUS_VAULT or create the vault + ~/brain symlink)",
            cfg.vault.display()
        );
    }
    cfg.ensure_dirs()?;
    let db = Db::open(&cfg.db_path())?;
    if reindex {
        db.clear_all()?;
    }

    let existing = db.existing_files()?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut stats = IndexStats::default();

    for abs in walk_vault(&cfg.vault) {
        let rel = abs
            .strip_prefix(&cfg.vault)
            .unwrap_or(&abs)
            .to_string_lossy()
            .to_string();
        seen.insert(rel.clone());

        let mtime = mtime_secs(&abs).with_context(|| format!("stat {}", abs.display()))?;
        if let Some((old_mtime, _)) = existing.get(&rel) {
            if (*old_mtime - mtime).abs() < f64::EPSILON {
                stats.unchanged += 1;
                continue; // fast path: mtime unchanged
            }
        }

        let bytes = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        let sha = sha256_hex(&bytes);
        let prior = existing.get(&rel);
        if let Some((_, old_sha)) = prior {
            if *old_sha == sha {
                // content identical (touch / checkout): just refresh mtime.
                db.upsert_file(&rel, mtime, &sha, now_unix())?;
                stats.unchanged += 1;
                continue;
            }
        }

        // New or changed content: persist the file row first (chunks FK-reference it), then chunks.
        db.upsert_file(&rel, mtime, &sha, now_unix())?;
        let text = String::from_utf8_lossy(&bytes);
        let chunks = chunk_markdown(&rel, &text);
        db.replace_chunks(&rel, &chunks)?;
        if prior.is_some() {
            stats.changed += 1;
        } else {
            stats.new += 1;
        }
    }

    // Deletions: indexed files no longer on disk.
    for path in existing.keys() {
        if !seen.contains(path) {
            db.delete_file(path)?;
            stats.removed += 1;
        }
    }

    Ok(stats)
}
