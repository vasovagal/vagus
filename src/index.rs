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

use crate::chunk::{Chunk, chunk_markdown, parse_frontmatter};
use crate::config::{CHUNK_VERSION, Config, EMBED_DIMS, EMBED_MODEL, VEC_INDEX_VERSION};
use crate::db::Db;
use crate::embed::Embedder;
use crate::lex::Lex;
use crate::util::{key_for, now_unix, sha256_hex};
use crate::vector::{UsearchIndex, VectorIndex};

/// How an index run treats the existing derived stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    /// Normal mtime+hash incremental reconciliation.
    Incremental,
    /// Wipe every derived row/store and rebuild the whole vault.
    Full,
    /// Run normal reconciliation, but force-refresh every existing note whose filesystem mtime is at
    /// or after `cutoff` even when its cached mtime/hash already match (ADR 0022).
    Since { cutoff: i64 },
}

impl IndexMode {
    fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }

    fn force_refresh(self, mtime: f64) -> bool {
        matches!(self, Self::Since { cutoff } if mtime >= cutoff as f64)
    }
}

#[derive(Debug, Default)]
pub struct IndexStats {
    pub scanned: usize,
    pub selected: usize,
    pub refreshed: usize,
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
    /// True for an explicit full rebuild or an identity/chunk-version auto-rebuild.
    pub full_reindex: bool,
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
    /// The single post-loop usearch persist: incremental `save()` or a full rebuild-from-BLOBs (ADR 0019).
    pub vector_ms: f64,
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
/// Returns a complete, sorted snapshot; a walk error is fatal rather than silently making an indexed
/// note look deleted.
pub fn walk_vault(vault: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in WalkDir::new(vault)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
    {
        let entry = entry.with_context(|| format!("walking vault {}", vault.display()))?;
        if entry.file_type().is_file() && is_markdown(entry.path()) {
            paths.push(entry.into_path());
        }
    }
    paths.sort();
    Ok(paths)
}

#[derive(Debug)]
struct VaultFile {
    abs: PathBuf,
    rel: String,
    mtime: f64,
}

/// Build the complete path+mtime list before mutating any derived store (ADR 0022). Besides giving
/// `--since` one stable selection snapshot, this prevents a late walk/stat failure from being
/// mistaken for deletions after an index run has already begun writing.
fn snapshot_vault(vault: &Path) -> Result<Vec<VaultFile>> {
    walk_vault(vault)?
        .into_iter()
        .map(|abs| {
            let rel = abs
                .strip_prefix(vault)
                .unwrap_or(&abs)
                .to_string_lossy()
                .to_string();
            let mtime = mtime_secs(&abs).with_context(|| format!("stat {}", abs.display()))?;
            Ok(VaultFile { abs, rel, mtime })
        })
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

/// Exact document text sent to the semantic index. Producer metadata is already represented as its
/// own bounded chunk, so this same path embeds it without hidden side channels or body mutation.
fn embedding_documents(chunks: &[Chunk]) -> Vec<String> {
    chunks.iter().map(|chunk| chunk.body.clone()).collect()
}

/// Reconcile the vault according to `mode`.
///
/// Thin wrapper over [`run_timed`] for callers that don't want the per-step timing breakdown.
pub fn run(cfg: &Config, mode: IndexMode) -> Result<IndexStats> {
    run_timed(cfg, mode, None)
}

/// Like [`run`], but when `timings` is `Some`, accumulates per-step wall-clock durations
/// (milliseconds) into it. Passing `None` skips the (negligible) bookkeeping entirely.
pub fn run_timed(
    cfg: &Config,
    mode: IndexMode,
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

    // Snapshot every vault path + mtime before any derived-store mutation. `--since` selects from
    // this list; all modes use it for complete deletion detection (ADR 0022/G26).
    let vault_files = snapshot_vault(&cfg.vault)?;

    // A chunker change reshapes every chunk; force a one-time rebuild so old indexes self-heal.
    let mut mode = mode;
    let mut auto_reindex = false;
    if !mode.is_full() {
        let needs_full = match db.meta_get("chunk_version")? {
            Some(v) => v != CHUNK_VERSION,
            None => db.count("SELECT count(*) FROM chunks")? > 0, // pre-versioning index
        };
        if needs_full {
            mode = IndexMode::Full;
            auto_reindex = true;
        }
    }
    if auto_reindex {
        // The first run after an upgrade re-embeds the whole vault — say so, so a `vagus search`
        // (which calls this incrementally) isn't silently slow on its first post-upgrade invocation.
        eprintln!("vagus: embedding/chunk format changed — reindexing the whole vault (one-time)…");
    }
    let full_reindex = mode.is_full();
    if full_reindex {
        db.clear_all()?;
        let _ = std::fs::remove_dir_all(cfg.tantivy_dir());
        // The usearch sidecar is a derived cache; `clear_all` doesn't touch it, so drop it explicitly
        // or a stale index would survive the rebuild (ADR 0019/G5).
        let _ = std::fs::remove_file(cfg.vector_path());
    }
    let lex = Lex::open(&cfg.tantivy_dir())?;
    let mut writer = lex.writer()?;

    // Guardrail G4: pin / validate the embedding identity.
    let dims = EMBED_DIMS.to_string();
    if !full_reindex
        && let (Some(m), Some(d)) = (db.meta_get("embed_model")?, db.meta_get("embed_dims")?)
        && (m != EMBED_MODEL || d != dims)
    {
        bail!("embedding identity changed ({m} {d} -> {EMBED_MODEL} {dims}); run `vagus reindex`");
    }
    db.meta_set("embed_model", EMBED_MODEL)?;
    db.meta_set("embed_dims", &dims)?;
    db.meta_set("tantivy_version", "0.26")?;
    db.meta_set("chunk_version", CHUNK_VERSION)?;

    // Vector index (ADR 0019). `vindex = Some` ⇒ mutate the existing usearch sidecar incrementally
    // (add new keys, remove old ones) in lockstep with SQLite + tantivy (G5). `None` ⇒ a full
    // rebuild-from-BLOBs after the loop: triggered by `reindex`, a missing sidecar, a vec-index
    // identity/param change, or a size drift between the sidecar and the embedded-chunk count. The
    // rebuild repacks the authoritative f32 BLOBs with NO re-embed (the embedding identity is
    // unchanged, so CHUNK_VERSION/G4 are untouched).
    let sidecar = cfg.vector_path();
    let vec_meta_ok = !full_reindex
        && db.meta_get("vec_backend")?.as_deref() == Some("usearch")
        && db.meta_get("vec_index_version")?.as_deref() == Some(VEC_INDEX_VERSION)
        && db.meta_get("vec_dims")?.as_deref() == Some(dims.as_str())
        && sidecar.exists();
    let vindex: Option<UsearchIndex> = if vec_meta_ok {
        let idx = UsearchIndex::open_writable(&sidecar, EMBED_DIMS)?;
        let embedded =
            db.count("SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")? as usize;
        // Size drift (e.g. an interrupted prior run) ⇒ fall back to a clean rebuild.
        if idx.len() == embedded {
            Some(idx)
        } else {
            None
        }
    } else {
        None
    };

    // Lazily loaded on the first changed file, so a no-op `index` never loads the model.
    let mut embedder: Option<Embedder> = None;

    let existing = db.existing_files()?;
    let incomplete_files = db.files_with_unembedded_chunks()?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut stats = IndexStats {
        scanned: vault_files.len(),
        full_reindex,
        ..IndexStats::default()
    };

    for file in vault_files {
        let VaultFile { abs, rel, mtime } = file;
        seen.insert(rel.clone());

        // `reindex --since` is normal incremental reconciliation plus a forced refresh set. The
        // mtime is filesystem metadata from the complete pre-write snapshot — never frontmatter.
        // An interrupted run can leave replacement chunk rows with NULL embeddings while the file
        // mtime/hash already look current; treat that as an implicit repair selection so ordinary
        // incremental indexing retries every G5 store instead of blessing the partial state forever.
        let prior = existing.get(&rel);
        let window_selected = mode.force_refresh(mtime);
        let incomplete = prior.is_some() && incomplete_files.contains(&rel);
        let force_refresh = window_selected || incomplete;
        if window_selected {
            stats.selected += 1;
        }
        if !force_refresh
            && let Some((old_mtime, _)) = prior
            && (*old_mtime - mtime).abs() < f64::EPSILON
        {
            stats.unchanged += 1;
            continue; // fast path: mtime unchanged and not explicitly selected
        }

        let bytes = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        let sha = sha256_hex(&bytes);
        if !force_refresh
            && let Some((_, old_sha)) = prior
            && *old_sha == sha
        {
            // Content identical (touch / checkout): just refresh mtime. A selected file deliberately
            // bypasses this shortcut so all three stores are repaired even when hash metadata agrees.
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
        // The OLD chunk ids (pre-replacement) drive incremental vector removal (G5): on a changed file
        // the new chunk set can differ, so we remove every old key then add every new one below.
        let old_ids = db.replace_chunks(&rel, &chunks, Some(created_at), fm.source.as_deref())?;
        if let Some(t) = timings.as_mut() {
            t.replace_chunks_ms += elapsed_ms(t0);
        }
        if let Some(vi) = vindex.as_ref() {
            for id in &old_ids {
                vi.remove(key_for(id))?;
            }
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
            let documents = embedding_documents(&chunks);

            let t0 = Instant::now();
            let vecs = emb.embed_documents(documents)?;
            if let Some(t) = timings.as_mut() {
                t.embed_ms += elapsed_ms(t0);
            }

            let t0 = Instant::now();
            for (c, v) in chunks.iter().zip(&vecs) {
                db.set_embedding(&c.id, v)?;
                // Mirror the vector into the usearch sidecar in lockstep (G5) when mutating
                // incrementally; the full-rebuild path repacks everything after the loop instead.
                if let Some(vi) = vindex.as_ref() {
                    vi.add(key_for(&c.id), v)?;
                }
            }
            if let Some(t) = timings.as_mut() {
                t.insert_embedding_ms += elapsed_ms(t0);
            }
        }
        if prior.is_some() {
            if force_refresh {
                stats.refreshed += 1;
            } else {
                stats.changed += 1;
            }
        } else {
            stats.new += 1;
        }
    }

    // Deletions: indexed files no longer on disk.
    for path in existing.keys() {
        if !seen.contains(path) {
            let removed = db.delete_file(path)?; // chunk ids, for tantivy + vector cleanup (G5)
            if let Some(vi) = vindex.as_ref() {
                for id in &removed {
                    vi.remove(key_for(id))?;
                }
            }
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

    // Persist the vector index after the single tantivy commit (G5: the stores move together). Either
    // save the incrementally-mutated index, or do the one-time full rebuild from the now-current f32
    // BLOBs (no re-embed). A no-op incremental run skips the save (the sidecar is already current).
    let t0 = Instant::now();
    // Forced repairs mutate the in-memory usearch index just like new/changed/deleted files. Omitting
    // `refreshed` here used to discard those mutations at process exit, leaving the sidecar stale even
    // though SQLite/Tantivy were repaired (ADR 0022/G5).
    let changed_any = stats.new + stats.changed + stats.refreshed + stats.removed > 0;
    match &vindex {
        Some(vi) if changed_any => vi.save(&sidecar)?,
        Some(_) => {}
        None => UsearchIndex::rebuild_from_db(&db, EMBED_DIMS)?.save(&sidecar)?,
    }
    db.meta_set("vec_backend", "usearch")?;
    db.meta_set("vec_index_version", VEC_INDEX_VERSION)?;
    db.meta_set("vec_dims", &dims)?;
    if let Some(t) = timings.as_mut() {
        t.vector_ms += elapsed_ms(t0);
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
            vector_ms: 7.0,
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
                "vector_ms",
            ]
        );
        assert_eq!(obj["embed_ms"], serde_json::json!(4.0));
    }

    #[test]
    fn elapsed_ms_is_nonnegative() {
        assert!(elapsed_ms(Instant::now()) >= 0.0);
    }

    #[test]
    fn producer_metadata_is_sent_to_the_semantic_document_path() {
        let chunks = chunk_markdown(
            "transcript.md",
            concat!(
                "---\n",
                "status: inbox\n",
                "corti: {\"models\":{\"asr\":{\"id\":\"nvidia/parakeet-tdt-0.6b-v3\"}}}\n",
                "---\n\n# Transcript\n\nspoken words\n",
            ),
        );
        let documents = embedding_documents(&chunks);
        assert!(documents.iter().any(|body| body.contains("parakeet")));
        assert!(documents.iter().all(|body| !body.contains("status")));
        assert!(documents.iter().all(|body| !body.contains("inbox")));
    }

    fn empty_note_cfg(tag: &str) -> (crate::util::testdir::TempDir, Config) {
        let dir = crate::util::testdir::TempDir::new(tag);
        let cfg = Config {
            vault: dir.path().join("vault"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };
        fs::create_dir_all(&cfg.vault).unwrap();
        (dir, cfg)
    }

    #[test]
    fn since_reindex_force_refreshes_a_matching_file_even_when_mtime_agrees() {
        // Empty Markdown yields no chunks, so this exercises the real three-store index path without
        // loading/downloading the embedding model.
        let (_dir, cfg) = empty_note_cfg("reindex-since-force");
        fs::write(cfg.vault.join("recent.md"), "").unwrap();
        run(&cfg, IndexMode::Full).unwrap();
        {
            let db = Db::open(&cfg.db_path()).unwrap();
            // Simulate stale derived content while the file's cached mtime/hash remain perfectly
            // current. The forced window must bypass both shortcuts and remove this bogus row.
            let bogus = sha256_hex(b"bogus chunk");
            db.conn
                .execute(
                    "INSERT INTO chunks(id,path,ord,kind,heading_path,body,embedding,created_at,source,vec_key)
                     VALUES(?1,'recent.md',0,0,'','stale',NULL,NULL,NULL,?2)",
                    rusqlite::params![bogus, key_for(&bogus) as i64],
                )
                .unwrap();
            let mut vector = vec![0.0; EMBED_DIMS];
            vector[0] = 1.0;
            db.set_embedding(&bogus, &vector).unwrap();
            UsearchIndex::rebuild_from_db(&db, EMBED_DIMS)
                .unwrap()
                .save(&cfg.vector_path())
                .unwrap();
            assert_eq!(
                UsearchIndex::view(&cfg.vector_path(), EMBED_DIMS)
                    .unwrap()
                    .len(),
                1
            );
            db.tick("recent.md").unwrap();
        }

        let stats = run(
            &cfg,
            IndexMode::Since {
                cutoff: now_unix() - 60,
            },
        )
        .unwrap();
        assert_eq!(stats.scanned, 1);
        assert_eq!(stats.selected, 1);
        assert_eq!(stats.refreshed, 1);
        assert_eq!(stats.unchanged, 0);
        assert!(!stats.full_reindex);

        let db = Db::open(&cfg.db_path()).unwrap();
        assert_eq!(db.existing_files().unwrap()["recent.md"].1, sha256_hex(b""));
        assert_eq!(
            db.count("SELECT count(*) FROM chunks").unwrap(),
            0,
            "selected file was really rebuilt rather than mtime/hash-skipped"
        );
        assert_eq!(db.fame(10, true).unwrap()[0].1, 1, "ticks are preserved");
        assert_eq!(
            UsearchIndex::view(&cfg.vector_path(), EMBED_DIMS)
                .unwrap()
                .len(),
            0,
            "forced-refresh vector removals were persisted"
        );
    }

    #[test]
    fn incremental_retries_incomplete_embedding_rows_despite_matching_mtime() {
        // Model-free interrupted-run fixture: an empty note should have no chunks, but the prior run
        // blessed its current file metadata before dying with one replacement row still unembedded.
        let (_dir, cfg) = empty_note_cfg("index-incomplete-repair");
        fs::write(cfg.vault.join("partial.md"), "").unwrap();
        run(&cfg, IndexMode::Full).unwrap();
        {
            let db = Db::open(&cfg.db_path()).unwrap();
            let bogus = sha256_hex(b"partial chunk");
            db.conn
                .execute(
                    "INSERT INTO chunks(id,path,ord,kind,heading_path,body,embedding,created_at,source,vec_key)
                     VALUES(?1,'partial.md',0,0,'','partial',NULL,NULL,NULL,?2)",
                    rusqlite::params![bogus, key_for(&bogus) as i64],
                )
                .unwrap();
        }

        let stats = run(&cfg, IndexMode::Incremental).unwrap();
        assert_eq!(stats.selected, 0, "not a user-selected time window");
        assert_eq!(stats.refreshed, 1, "partial file retried as a repair");
        assert_eq!(stats.unchanged, 0);
        let db = Db::open(&cfg.db_path()).unwrap();
        assert_eq!(db.count("SELECT count(*) FROM chunks").unwrap(), 0);
    }

    #[test]
    fn since_reindex_still_reconciles_new_and_deleted_files_outside_window() {
        let (_dir, cfg) = empty_note_cfg("reindex-since-reconcile");
        fs::write(cfg.vault.join("gone.md"), "").unwrap();
        run(&cfg, IndexMode::Full).unwrap();

        fs::remove_file(cfg.vault.join("gone.md")).unwrap();
        fs::write(cfg.vault.join("new.md"), "").unwrap();
        // A future cutoff selects no file. `new.md` must still be indexed because --since augments
        // normal reconciliation rather than creating an intentionally incomplete local index.
        let stats = run(
            &cfg,
            IndexMode::Since {
                cutoff: now_unix() + 60,
            },
        )
        .unwrap();
        assert_eq!(stats.scanned, 1);
        assert_eq!(stats.selected, 0);
        assert_eq!(stats.new, 1);
        assert_eq!(stats.removed, 1);
        let files = Db::open(&cfg.db_path()).unwrap().existing_files().unwrap();
        assert!(files.contains_key("new.md"));
        assert!(!files.contains_key("gone.md"));
    }

    // `vagus reindex` runs the REAL wipe path (clear_all + tantivy/usearch removal) and must
    // preserve counters, provenance runs, and events — user data, not a derived cache
    // (ADR 0021/G25). An empty vault keeps the embedder unloaded (it is lazy), so this stays a cheap
    // unit test. CHUNK_VERSION auto-reindex calls the same clear_all.
    #[test]
    fn reindex_preserves_ticks() {
        let dir = crate::util::testdir::TempDir::new("reindex-ticks");
        let cfg = Config {
            vault: dir.path().join("vault"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };
        fs::create_dir_all(&cfg.vault).unwrap();
        {
            let db = Db::open(&cfg.db_path()).unwrap();
            db.tick("20-Areas/foo.md").unwrap();
            db.tick("20-Areas/foo.md").unwrap();
            db.conn
                .execute(
                    "INSERT INTO tick_runs(pipeline_id,corpus_sha256,provenance_json,query,ts)
                     VALUES('pipeline','corpus','{}',NULL,1)",
                    [],
                )
                .unwrap();
            let run_id = db.conn.last_insert_rowid();
            db.conn
                .execute(
                    "INSERT INTO tick_events(
                       run_id,path,fusion_rank,rerank_rank,final_rank,rerank_scored
                     ) VALUES(?1,'20-Areas/foo.md',12,2,1,1)",
                    rusqlite::params![run_id],
                )
                .unwrap();
        }

        run(&cfg, IndexMode::Full).unwrap();

        let db = Db::open(&cfg.db_path()).unwrap();
        let rows = db.fame(10, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "20-Areas/foo.md");
        assert_eq!(rows[0].1, 2, "fame unchanged across reindex");
        assert_eq!(db.count("SELECT count(*) FROM tick_runs").unwrap(), 1);
        assert_eq!(db.count("SELECT count(*) FROM tick_events").unwrap(), 1);
    }
}
