//! Vault walk + incremental diff (mtime then sha256), persisting files + chunks.
//!
//! tantivy and embeddings are layered on in later steps; this module owns the change detection and
//! the SQLite side. Paths are stored **vault-relative** so the index is portable and matches the
//! "Brain/ holds only markdown" model.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use walkdir::{DirEntry, WalkDir};

use chrono::{Local, NaiveDateTime, TimeZone};

use crate::chunk::{chunk_markdown, parse_frontmatter};
use crate::config::{CHUNK_VERSION, Config, EMBED_DIMS, EMBED_MODEL};
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

/// Per-step wall-clock timings (milliseconds) for the index sub-steps, accumulated across every
/// changed/new file in a run. Surfaced by `vagus file --stats` so the embedding bottleneck is
/// visible. The final `commit_ms` covers the single post-loop tantivy commit (+ merge wait).
#[derive(Debug, Default, Serialize)]
pub struct IndexTimings {
    /// Markdown chunking (`chunk_markdown`).
    pub chunk_ms: f64,
    /// SQLite chunk-row replacement (`db.replace_chunks`).
    pub replace_chunks_ms: f64,
    /// Building + adding tantivy docs (`lex.replace_file`).
    pub tantivy_add_ms: f64,
    /// Computing embeddings (`emb.embed_documents`) — the usual bottleneck.
    pub embed_ms: f64,
    /// Inserting embedding vectors (`db.set_embedding` loop).
    pub insert_embedding_ms: f64,
    /// The single tantivy `writer.commit()` (+ `wait_merging_threads`) after the loop.
    pub commit_ms: f64,
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

/// Note-level `created_at` (unix secs) for the `--since` filter (ADR 0017): the frontmatter `created`
/// value parsed as `%Y-%m-%dT%H:%M` in **local** time (matching how notes.rs writes it), or — when
/// the key is absent, empty, or unparseable — a **G3 fallback to the file mtime** so a bare,
/// frontmatter-free note is still `--since`-filterable.
fn created_at_secs(created: Option<&str>, mtime: f64) -> i64 {
    if let Some(raw) = created
        && let Ok(naive) = NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%dT%H:%M")
        && let Some(dt) = Local.from_local_datetime(&naive).single()
    {
        return dt.timestamp();
    }
    mtime as i64 // G3 mtime fallback
}

/// Run an incremental index (or full rebuild when `reindex`).
///
/// Thin wrapper over [`run_timed`] for callers that don't want the per-step timing breakdown.
pub fn run(cfg: &Config, reindex: bool) -> Result<IndexStats> {
    run_timed(cfg, reindex, None)
}

/// Like [`run`], but when `timings` is `Some`, accumulates per-step wall-clock durations
/// (milliseconds) into it. Passing `None` skips the (negligible) bookkeeping entirely.
pub fn run_timed(
    cfg: &Config,
    reindex: bool,
    mut timings: Option<&mut IndexTimings>,
) -> Result<IndexStats> {
    if !cfg.vault.exists() {
        bail!(
            "vault not found: {} (set VAGUS_VAULT or create the vault + ~/brain symlink)",
            cfg.vault.display()
        );
    }
    cfg.ensure_dirs()?;
    let db = Db::open(&cfg.db_path())?;

    // A chunker change reshapes every chunk; force a one-time rebuild so old indexes self-heal.
    let mut reindex = reindex;
    let mut auto_reindex = false;
    if !reindex {
        reindex = match db.meta_get("chunk_version")? {
            Some(v) => v != CHUNK_VERSION,
            None => db.count("SELECT count(*) FROM chunks")? > 0, // pre-versioning index
        };
        auto_reindex = reindex;
    }
    if auto_reindex {
        // The first run after an upgrade re-embeds the whole vault — say so, so a `vagus search`
        // (which calls this incrementally) isn't silently slow on its first post-upgrade invocation.
        eprintln!("vagus: embedding/chunk format changed — reindexing the whole vault (one-time)…");
    }
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
    db.meta_set("chunk_version", CHUNK_VERSION)?;

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
        // Note-level indexed filters (ADR 0017): `created_at` (frontmatter `created`, else mtime — G3)
        // and `source` (frontmatter `source`, else NULL), attached to every chunk of this note.
        let fm = parse_frontmatter(&text);
        let created_at = created_at_secs(fm.created.as_deref(), mtime);

        let t0 = Instant::now();
        let chunks = chunk_markdown(&rel, &text);
        if let Some(t) = timings.as_mut() {
            t.chunk_ms += elapsed_ms(t0);
        }

        let t0 = Instant::now();
        db.replace_chunks(&rel, &chunks, Some(created_at), fm.source.as_deref())?;
        if let Some(t) = timings.as_mut() {
            t.replace_chunks_ms += elapsed_ms(t0);
        }

        let t0 = Instant::now();
        lex.replace_file(&writer, &rel, &chunks)?;
        if let Some(t) = timings.as_mut() {
            t.tantivy_add_ms += elapsed_ms(t0);
        }

        if !chunks.is_empty() {
            if embedder.is_none() {
                embedder = Some(Embedder::new(&cfg.cache_dir)?);
            }
            let emb = embedder.as_mut().unwrap();
            let bodies: Vec<String> = chunks.iter().map(|c| c.body.clone()).collect();

            let t0 = Instant::now();
            let vecs = emb.embed_documents(bodies)?;
            if let Some(t) = timings.as_mut() {
                t.embed_ms += elapsed_ms(t0);
            }

            let t0 = Instant::now();
            for (c, v) in chunks.iter().zip(vecs) {
                db.set_embedding(&c.id, &v)?;
            }
            if let Some(t) = timings.as_mut() {
                t.insert_embedding_ms += elapsed_ms(t0);
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

    let t0 = Instant::now();
    writer.commit()?;
    // Let tantivy's merge policy finish any scheduled merges so segments stay bounded instead of
    // accumulating across per-file commits (the writer would otherwise drop before they run).
    writer.wait_merging_threads()?;
    if let Some(t) = timings.as_mut() {
        t.commit_ms += elapsed_ms(t0);
    }
    Ok(stats)
}

/// Milliseconds since `start`, as `f64`.
fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_timings_serializes_with_stable_keys() {
        let t = IndexTimings {
            chunk_ms: 1.0,
            replace_chunks_ms: 2.0,
            tantivy_add_ms: 3.0,
            embed_ms: 4.0,
            insert_embedding_ms: 5.0,
            commit_ms: 6.0,
        };
        let v: serde_json::Value = serde_json::to_value(&t).unwrap();
        let obj = v.as_object().unwrap();
        // Stable shape (G13): exactly these keys, no more, no less.
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "chunk_ms",
                "commit_ms",
                "embed_ms",
                "insert_embedding_ms",
                "replace_chunks_ms",
                "tantivy_add_ms",
            ]
        );
        assert_eq!(obj["embed_ms"], serde_json::json!(4.0));
    }

    #[test]
    fn elapsed_ms_is_nonnegative() {
        assert!(elapsed_ms(Instant::now()) >= 0.0);
    }
}
