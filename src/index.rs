//! Vault walk + incremental diff (mtime then sha256), persisting files + chunks.
//!
//! tantivy and embeddings are layered on in later steps; this module owns the change detection and
//! the SQLite side. Paths are stored **vault-relative** so the index is portable and matches the
//! "Brain/ holds only markdown" model.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use walkdir::{DirEntry, WalkDir};

use crate::chunk::chunk_markdown;
use crate::config::{Config, EMBED_DIMS, EMBED_MODEL};
use crate::db::Db;
use crate::embed::Embedder;
use crate::lex::Lex;
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
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
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
    Ok(modified
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0))
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
        let _ = std::fs::remove_dir_all(cfg.tantivy_dir());
    }
    let lex = Lex::open(&cfg.tantivy_dir())?;
    let mut writer = lex.writer()?;

    // Guardrail G4: pin / validate the embedding identity.
    let dims = EMBED_DIMS.to_string();
    if !reindex
        && let (Some(m), Some(d)) = (db.meta_get("embed_model")?, db.meta_get("embed_dims")?)
        && (m != EMBED_MODEL || d != dims)
    {
        bail!("embedding identity changed ({m} {d} -> {EMBED_MODEL} {dims}); run `vagus reindex`");
    }
    db.meta_set("embed_model", EMBED_MODEL)?;
    db.meta_set("embed_dims", &dims)?;
    db.meta_set("tantivy_version", "0.26")?;

    // Lazily loaded on the first changed file, so a no-op `index` never loads the model.
    let mut embedder: Option<Embedder> = None;

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
        if let Some((old_mtime, _)) = existing.get(&rel)
            && (*old_mtime - mtime).abs() < f64::EPSILON
        {
            stats.unchanged += 1;
            continue; // fast path: mtime unchanged
        }

        let bytes = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        let sha = sha256_hex(&bytes);
        let prior = existing.get(&rel);
        if let Some((_, old_sha)) = prior
            && *old_sha == sha
        {
            // content identical (touch / checkout): just refresh mtime.
            db.upsert_file(&rel, mtime, &sha, now_unix())?;
            stats.unchanged += 1;
            continue;
        }

        // New or changed content: persist the file row first (chunks FK-reference it), then chunks.
        db.upsert_file(&rel, mtime, &sha, now_unix())?;
        let text = String::from_utf8_lossy(&bytes);
        let chunks = chunk_markdown(&rel, &text);
        db.replace_chunks(&rel, &chunks)?;
        lex.replace_file(&writer, &rel, &chunks)?;
        if !chunks.is_empty() {
            if embedder.is_none() {
                embedder = Some(Embedder::new(&cfg.cache_dir)?);
            }
            let emb = embedder.as_mut().unwrap();
            let bodies: Vec<String> = chunks.iter().map(|c| c.body.clone()).collect();
            let vecs = emb.embed_documents(bodies)?;
            for (c, v) in chunks.iter().zip(vecs) {
                db.set_embedding(&c.id, &v)?;
            }
        }
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
            lex.delete_file(&writer, path);
            stats.removed += 1;
        }
    }

    writer.commit()?;
    Ok(stats)
}
