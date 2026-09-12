//! Vault walk + incremental diff (mtime then sha256), keeping SQLite, tantivy, and usearch in step.
//!
//! Paths are stored **vault-relative** so the index is portable and matches the "Brain/ holds only
//! markdown" model.
//!
//! **Durability (ADR 0029).** SQLite autocommits as a run goes, but tantivy persists only on
//! `commit()`. A processed file's row is therefore written `pending` and blessed only by the checkpoint
//! commit (every [`CHECKPOINT_FILES`] indexed files or [`CHECKPOINT_INTERVAL`]) that makes its BM25 docs
//! durable. A killed run loses at most the uncommitted batch's BM25 docs: the next run revisits exactly
//! the `pending` rows, keeping their stored embeddings when a fresh chunking matches them. The usearch
//! sidecar is not saved at checkpoints: the f32 BLOBs are the durable
//! vectors, and a `vec_dirty` flag makes the next run repack the sidecar from them.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tantivy::IndexWriter;
use walkdir::{DirEntry, WalkDir};

use crate::chunk::{Chunk, chunk_markdown, parse_frontmatter};
use crate::config::{CHUNK_VERSION, Config, EMBED_DIMS, EMBED_MODEL, VEC_INDEX_VERSION};
use crate::db::Db;
use crate::embed::{DOC_RECIPE, Embedder, PRE_PINNING_RECIPE};
use crate::lex::Lex;
use crate::util::{key_for, note_created_at_secs, now_unix, sha256_hex};
use crate::vector::{UsearchIndex, VectorIndex};

/// Checkpoint cadence (ADR 0029): commit tantivy and bless the batch's `pending` rows after this many
/// indexed files…
const CHECKPOINT_FILES: usize = 64;
/// …or once this long has passed since the last checkpoint, whichever comes first. Embedding dominates
/// a run, so this bounds the work a kill can throw away.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);

/// `meta` flag: a rebuild from an empty index (`reindex`, a chunk-version auto-rebuild, or a first
/// index) has not finished. Explicit `index`/`reindex` resume it; automatic refreshes defer.
const META_REBUILD_PENDING: &str = "rebuild_pending";
/// `meta` flag: SQLite vectors changed after the usearch sidecar was last saved, so the next run
/// repacks the sidecar from the BLOBs instead of trusting it.
const META_VEC_DIRTY: &str = "vec_dirty";
/// `meta` key: the [`crate::embed::DocRecipe::identity`] the stored vectors were built by (G4).
const META_EMBED_RECIPE: &str = "embed_recipe";

/// How an index run treats the existing derived stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    /// Normal mtime+hash incremental reconciliation, as run by `vagus index`. Resumes an interrupted
    /// rebuild.
    Incremental,
    /// The implicit refresh inside `search`, `add-note`, `file`, and plugin captures. Same as
    /// `Incremental`, except it will not resume an unfinished rebuild (possibly hours of embedding): it
    /// warns, touches nothing, and reports [`IndexStats::deferred`] (ADR 0029).
    AutoRefresh,
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
    /// An `AutoRefresh` found an unfinished rebuild and left the index untouched for `vagus index`.
    pub deferred: bool,
    /// Of `refreshed`: files from an uncommitted batch whose stored chunks and embeddings were reused
    /// instead of re-embedded (ADR 0029).
    pub reused: usize,
}

/// Per-step wall-clock timings (milliseconds) for the index sub-steps, accumulated across every
/// changed/new file in a run. Surfaced by `vagus file --stats` so the embedding bottleneck is
/// visible.
#[derive(Debug, Default, Serialize)]
pub struct IndexTimings {
    /// Markdown chunking (`chunk_markdown`).
    pub chunk_ms: f64,
    /// SQLite chunk-row replacement (`db.replace_chunks`).
    pub replace_chunks_ms: f64,
    /// Building + adding tantivy docs (`lex.replace_file`).
    pub tantivy_add_ms: f64,
    /// Computing embeddings — the usual bottleneck. Includes the one-time model load on the first
    /// changed file.
    pub embed_ms: f64,
    /// Inserting embedding vectors (`db.set_embedding` loop).
    pub insert_embedding_ms: f64,
    /// Every tantivy checkpoint `commit()` plus the final `wait_merging_threads`.
    pub commit_ms: f64,
    /// The single post-loop usearch persist: incremental `save()` or a full rebuild-from-BLOBs (ADR 0019).
    pub vector_ms: f64,
}

/// A run stopped by Ctrl-C. Everything indexed before the interrupt was committed at a checkpoint;
/// deletions, the rebuild-marker clear, and the usearch repack wait for the next run.
#[derive(Debug)]
pub struct Interrupted {
    /// Snapshot files the run never reached.
    pub remaining: usize,
}

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "interrupted with {} vault file(s) not yet checked; everything before that is committed. Run `vagus index` to resume.",
            self.remaining
        )
    }
}

impl std::error::Error for Interrupted {}

/// What an interrupted run left for the next `vagus index`, reported by `vagus doctor` (ADR 0029).
#[derive(Debug)]
pub struct Leftovers {
    /// File rows written by a batch whose tantivy commit never happened.
    pub pending_files: i64,
    /// A rebuild from an empty index has not finished.
    pub rebuild_unfinished: bool,
    /// The usearch sidecar predates the SQLite vectors and awaits a repack.
    pub vectors_stale: bool,
}

pub fn leftovers(db: &Db) -> Result<Leftovers> {
    Ok(Leftovers {
        pending_files: db.count("SELECT count(*) FROM files WHERE pending=1")?,
        rebuild_unfinished: db.meta_get(META_REBUILD_PENDING)?.is_some(),
        vectors_stale: db.meta_get(META_VEC_DIRTY)?.is_some(),
    })
}

/// The recipe the stored vectors were built by, when it differs from this binary's [`DOC_RECIPE`]
/// (G4). An index pinned before `embed_recipe` existed has no key and is compared as
/// [`PRE_PINNING_RECIPE`]; one that never pinned an identity has nothing to differ.
pub fn recipe_change(db: &Db) -> Result<Option<String>> {
    let stored = match db.meta_get(META_EMBED_RECIPE)? {
        Some(recipe) => recipe,
        None if db.meta_get("embed_model")?.is_some() => PRE_PINNING_RECIPE.identity(),
        None => return Ok(None),
    };
    Ok((stored != DOC_RECIPE.identity()).then_some(stored))
}

fn index_lock_path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("index.lock")
}

/// Exclusive advisory lock on `<data_dir>/index.lock`, held for a whole run and taken before any
/// derived-store mutation. The kernel releases it when the process exits or is killed, so an
/// interrupted run can never wedge the next one.
fn lock_index(cfg: &Config) -> Result<fs::File> {
    let path = index_lock_path(cfg);
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => {
            bail!(
                "another vagus index run is in progress ({})",
                path.display()
            )
        }
        Err(fs::TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("locking {}", path.display()))
        }
    }
}

/// True while some process holds the index lock. Diagnostic only (`vagus doctor`).
pub fn run_in_progress(cfg: &Config) -> bool {
    fs::File::open(index_lock_path(cfg))
        .is_ok_and(|file| matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock)))
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
///
/// Ctrl-C during the run finishes the current file, commits a checkpoint, and returns
/// [`Interrupted`]; a second Ctrl-C exits immediately.
pub fn run_timed(
    cfg: &Config,
    mode: IndexMode,
    timings: Option<&mut IndexTimings>,
) -> Result<IndexStats> {
    let interrupt = crate::interrupt::Guard::install();
    // Lazily loaded on the first changed file, so a no-op `index` never loads the model.
    let mut embedder: Option<Embedder> = None;
    let mut embed = |documents: Vec<String>| -> Result<Vec<Vec<f32>>> {
        if embedder.is_none() {
            embedder = Some(Embedder::new(&cfg.cache_dir)?);
        }
        embedder.as_mut().unwrap().embed_documents(documents)
    };
    run_with(
        cfg,
        mode,
        timings,
        Hooks {
            embed: &mut embed,
            interrupted: &|| interrupt.requested(),
            checkpoint_files: CHECKPOINT_FILES,
            checkpoint_interval: CHECKPOINT_INTERVAL,
        },
    )
}

/// The seams [`run_with`] needs from its caller: production wires the real model and the SIGINT
/// flag; tests substitute a model-free embedder, an injected interrupt, and a tight cadence.
struct Hooks<'a> {
    embed: &'a mut dyn FnMut(Vec<String>) -> Result<Vec<Vec<f32>>>,
    interrupted: &'a dyn Fn() -> bool,
    checkpoint_files: usize,
    checkpoint_interval: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOutcome {
    Unchanged,
    /// Unchanged on disk and in SQLite, but its BM25 docs were re-added from the stored chunks.
    Bm25Healed,
    New,
    Changed,
    Refreshed,
    /// Left `pending` by a killed batch with stored rows that match a fresh chunking: search docs
    /// restored, embeddings kept.
    Reused,
}

fn run_with(
    cfg: &Config,
    mode: IndexMode,
    mut timings: Option<&mut IndexTimings>,
    hooks: Hooks<'_>,
) -> Result<IndexStats> {
    let Hooks {
        embed,
        interrupted,
        checkpoint_files,
        checkpoint_interval,
    } = hooks;
    if !cfg.vault.exists() {
        bail!(
            "vault not found: {} (set VAGUS_VAULT or create the vault + ~/brain symlink)",
            cfg.vault.display()
        );
    }
    cfg.ensure_dirs()?;
    let _lock = lock_index(cfg)?;
    let db = Db::open(&cfg.db_path())?;

    // Resuming an interrupted rebuild can mean hours of embedding. That is the explicit commands' job,
    // never a side effect of searching or capturing a note (ADR 0029).
    let rebuild_unfinished = db.meta_get(META_REBUILD_PENDING)?.is_some();
    if mode == IndexMode::AutoRefresh && rebuild_unfinished {
        eprintln!(
            "vagus: skipping the automatic index refresh: an interrupted rebuild is unfinished ({} notes indexed so far). Run `vagus index` to resume it.",
            committed_files(&db)?
        );
        return Ok(IndexStats {
            deferred: true,
            ..IndexStats::default()
        });
    }

    // Snapshot every vault path + mtime before any derived-store mutation. `--since` selects from
    // this list; all modes use it for complete deletion detection (ADR 0022/G26).
    let vault_files = snapshot_vault(&cfg.vault)?;

    // A chunker change reshapes every chunk; force a one-time rebuild so old indexes self-heal.
    let dims = EMBED_DIMS.to_string();
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
    // An interrupted rebuild under the current identity resumes instead of restarting, even from
    // `vagus reindex`: the files it already committed are exactly what a restart would re-embed.
    let resume = rebuild_unfinished
        && db.meta_get("embed_model")?.as_deref() == Some(EMBED_MODEL)
        && db.meta_get("embed_dims")?.as_deref() == Some(dims.as_str())
        && recipe_change(&db)?.is_none()
        && db.meta_get("chunk_version")?.as_deref() == Some(CHUNK_VERSION)
        && (!mode.is_full() || Lex::open(&cfg.tantivy_dir()).is_ok());
    if resume {
        mode = match mode {
            IndexMode::Full => IndexMode::Incremental,
            other => other,
        };
        eprintln!(
            "vagus: resuming an interrupted rebuild ({} notes already indexed)…",
            committed_files(&db)?
        );
    } else if auto_reindex {
        // The first run after an upgrade re-embeds the whole vault — say so, so a `vagus search`
        // (which calls this incrementally) isn't silently slow on its first post-upgrade invocation.
        eprintln!("vagus: embedding/chunk format changed — reindexing the whole vault (one-time)…");
    }
    let full_reindex = mode.is_full();
    if full_reindex {
        // Mark before wiping, so a kill at any later point leaves an index that auto-refresh won't
        // resume and whose sidecar is known stale.
        db.meta_set(META_REBUILD_PENDING, "1")?;
        db.meta_set(META_VEC_DIRTY, "1")?;
        db.clear_all()?;
        let _ = fs::remove_dir_all(cfg.tantivy_dir());
        // The usearch sidecar is a derived cache; `clear_all` doesn't touch it, so drop it explicitly
        // or a stale index would survive the rebuild (ADR 0019/G5).
        let _ = fs::remove_file(cfg.vector_path());
    }
    let lex = Lex::open(&cfg.tantivy_dir())?;
    let mut writer = lex.writer()?;

    // Guardrail G4: pin / validate the embedding identity: model, dims, and the document recipe. A
    // pre-pinning index whose recipe still matches gets the key backfilled here, with no rebuild.
    if !full_reindex
        && let (Some(m), Some(d)) = (db.meta_get("embed_model")?, db.meta_get("embed_dims")?)
        && (m != EMBED_MODEL || d != dims)
    {
        bail!("embedding identity changed ({m} {d} -> {EMBED_MODEL} {dims}); run `vagus reindex`");
    }
    let recipe = DOC_RECIPE.identity();
    if !full_reindex && let Some(stored) = recipe_change(&db)? {
        bail!("embedding recipe changed ({stored} -> {recipe}); run `vagus reindex`");
    }
    db.meta_set("embed_model", EMBED_MODEL)?;
    db.meta_set("embed_dims", &dims)?;
    db.meta_set(META_EMBED_RECIPE, &recipe)?;
    db.meta_set("tantivy_version", "0.26")?;
    db.meta_set("chunk_version", CHUNK_VERSION)?;

    let existing = db.existing_files()?;
    // Files that must be redone despite matching mtime/hash: `pending` rows from a batch whose
    // checkpoint never committed (ADR 0029), and rows with NULL embeddings (ADR 0022).
    let repair = db.files_needing_repair()?;
    let rebuilding =
        full_reindex || rebuild_unfinished || (existing.is_empty() && !vault_files.is_empty());
    if rebuilding {
        db.meta_set(META_REBUILD_PENDING, "1")?;
    }

    // Vector index (ADR 0019). `vindex = Some` ⇒ mutate the existing usearch sidecar incrementally
    // (add new keys, remove old ones) in lockstep with SQLite + tantivy (G5). `None` ⇒ a full
    // rebuild-from-BLOBs after the loop: triggered by `reindex`, a missing sidecar, a vec-index
    // identity/param change, a sidecar left stale by an interrupted run, or a size drift between the
    // sidecar and the embedded-chunk count. The rebuild repacks the authoritative f32 BLOBs with NO
    // re-embed (the embedding identity is unchanged, so CHUNK_VERSION/G4 are untouched).
    let sidecar = cfg.vector_path();
    let vec_dirty_at_start = db.meta_get(META_VEC_DIRTY)?.is_some();
    let vec_meta_ok = !full_reindex
        && !vec_dirty_at_start
        && db.meta_get("vec_backend")?.as_deref() == Some("usearch")
        && db.meta_get("vec_index_version")?.as_deref() == Some(VEC_INDEX_VERSION)
        && db.meta_get("vec_dims")?.as_deref() == Some(dims.as_str())
        && sidecar.exists();
    let vindex: Option<UsearchIndex> = if vec_meta_ok {
        let idx = UsearchIndex::open_writable(&sidecar, EMBED_DIMS)?;
        let embedded =
            db.count("SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")? as usize;
        // Size drift (e.g. external damage) ⇒ fall back to a clean rebuild.
        if idx.len() == embedded {
            Some(idx)
        } else {
            None
        }
    } else {
        None
    };

    // BM25 self-heal (ADR 0029). Before checkpoints, a run killed after its SQLite writes but before
    // the single tantivy commit left files blessed in SQLite with no BM25 docs, forever. The totals are
    // cheap; only a disagreement pays for the per-path census. Files already slated for a full redo
    // are left to that path.
    let mut bm25_repair: HashSet<String> = HashSet::new();
    let mut bm25_stale = 0usize;
    if !full_reindex {
        let chunks_total = db.count("SELECT count(*) FROM chunks")? as u64;
        if lex.num_docs()? != chunks_total {
            let in_lex = lex.doc_counts_by_path()?;
            let in_db = db.chunk_counts_by_path()?;
            for path in in_lex.keys() {
                if !existing.contains_key(path) {
                    // BM25 docs for a note SQLite no longer holds (e.g. a deletion killed before commit).
                    lex.delete_file(&writer, path);
                    bm25_stale += 1;
                }
            }
            for path in existing.keys() {
                let stored = in_db.get(path).copied().unwrap_or(0);
                let searchable = in_lex.get(path).copied().unwrap_or(0);
                if stored != searchable && !repair.contains(path) {
                    bm25_repair.insert(path.clone());
                }
            }
        }
    }

    let total = vault_files.len();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stats = IndexStats {
        scanned: total,
        full_reindex,
        ..IndexStats::default()
    };
    let run_started = Instant::now();
    let mut last_checkpoint = Instant::now();
    // Paths indexed since the last checkpoint: exactly the rows that checkpoint may bless.
    let mut batch: Vec<String> = Vec::new();
    let mut embedded_chunks = 0usize;
    let mut bm25_healed = 0usize;
    let mut vec_marked = false;

    for (position, file) in vault_files.into_iter().enumerate() {
        if interrupted() {
            // Graceful stop: make the work so far durable, but skip deletions (`seen` is incomplete),
            // the rebuild-marker clear, and the vector repack. The next run owns those.
            checkpoint(&db, &mut writer, &mut batch, timings.as_deref_mut())?;
            return Err(Interrupted {
                remaining: total - position,
            }
            .into());
        }
        let VaultFile { abs, rel, mtime } = file;
        seen.insert(rel.clone());

        // `reindex --since` is normal incremental reconciliation plus a forced refresh set. The
        // mtime is filesystem metadata from the complete pre-write snapshot — never frontmatter.
        // A file in `repair` looks current by mtime/hash but some store never durably received it;
        // treat it as an implicit repair selection so every G5 store is retried instead of blessing
        // the partial state forever.
        let prior = existing.get(&rel);
        let window_selected = mode.force_refresh(mtime);
        let incomplete = prior.is_some() && repair.contains(&rel);
        let force_refresh = window_selected || incomplete;
        if window_selected {
            stats.selected += 1;
        }

        let outcome = 'file: {
            if !force_refresh
                && let Some((old_mtime, _)) = prior
                && (*old_mtime - mtime).abs() < f64::EPSILON
            {
                // fast path: mtime unchanged and not explicitly selected
                break 'file heal_bm25(&db, &lex, &writer, &rel, &bm25_repair)?;
            }

            let bytes = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
            let sha = sha256_hex(&bytes);
            if !force_refresh
                && let Some((_, old_sha)) = prior
                && *old_sha == sha
            {
                // Content identical (touch / checkout): just refresh mtime. A selected file
                // deliberately bypasses this shortcut so all three stores are repaired even when hash
                // metadata agrees.
                db.upsert_file(&rel, mtime, &sha, now_unix())?;
                break 'file heal_bm25(&db, &lex, &writer, &rel, &bm25_repair)?;
            }

            // New or changed content. From here until the final save the sidecar on disk disagrees
            // with SQLite, so flag it before the first vector write.
            mark_vectors_dirty(&db, &mut vec_marked)?;
            // Persist the file row first (chunks FK-reference it), as `pending`: it is not current
            // until the next checkpoint commit makes its tantivy docs durable (ADR 0029).
            db.upsert_file_pending(&rel, mtime, &sha, now_unix())?;
            let text = String::from_utf8_lossy(&bytes);
            // Note-level indexed filters (ADR 0017): `created_at` (frontmatter `created`, else mtime —
            // G3) and `source` (frontmatter `source`, else NULL), attached to every chunk of this note.
            let fm = parse_frontmatter(&text);
            let created_at = note_created_at_secs(fm.created.as_deref(), mtime);

            let t0 = Instant::now();
            let chunks = chunk_markdown(&rel, &text);
            if let Some(t) = timings.as_mut() {
                t.chunk_ms += elapsed_ms(t0);
            }

            // A file left `pending` by a killed batch usually already holds exactly what this redo
            // would write; only its tantivy docs never became durable. Keep the embeddings when the
            // stored rows provably match (see `reusable_embeddings`). An explicit `--since` selection
            // asks for a forced rebuild, and NULL-embedding repairs fail the match, so both re-embed.
            if incomplete
                && !window_selected
                && let Some(stored) = reusable_embeddings(&db, &rel, &chunks)?
            {
                db.set_note_filters(&rel, Some(created_at), fm.source.as_deref())?;
                // Pending rows imply `vec_dirty`, so the sidecar is normally repacked from the BLOBs
                // after the loop. Mirror the vectors anyway in case it is being mutated in place.
                if let Some(vi) = vindex.as_ref() {
                    for (id, vector) in &stored {
                        vi.remove(key_for(id))?;
                        vi.add(key_for(id), vector)?;
                    }
                }
                let t0 = Instant::now();
                lex.replace_file(&writer, &rel, &chunks)?;
                if let Some(t) = timings.as_mut() {
                    t.tantivy_add_ms += elapsed_ms(t0);
                }
                break 'file FileOutcome::Reused;
            }

            let t0 = Instant::now();
            // The OLD chunk ids (pre-replacement) drive incremental vector removal (G5): on a changed
            // file the new chunk set can differ, so we remove every old key then add every new one.
            let old_ids =
                db.replace_chunks(&rel, &chunks, Some(created_at), fm.source.as_deref())?;
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
                let documents = embedding_documents(&chunks);

                let t0 = Instant::now();
                let vecs = embed(documents)?;
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
                embedded_chunks += chunks.len();
            }
            match (prior.is_some(), force_refresh) {
                (false, _) => FileOutcome::New,
                (true, true) => FileOutcome::Refreshed,
                (true, false) => FileOutcome::Changed,
            }
        };

        match outcome {
            FileOutcome::Unchanged => stats.unchanged += 1,
            FileOutcome::Bm25Healed => {
                stats.refreshed += 1;
                bm25_healed += 1;
            }
            FileOutcome::New => stats.new += 1,
            FileOutcome::Changed => stats.changed += 1,
            FileOutcome::Refreshed => stats.refreshed += 1,
            FileOutcome::Reused => {
                stats.refreshed += 1;
                stats.reused += 1;
            }
        }
        if outcome == FileOutcome::Unchanged {
            continue;
        }
        batch.push(rel);
        if batch.len() >= checkpoint_files || last_checkpoint.elapsed() >= checkpoint_interval {
            checkpoint(&db, &mut writer, &mut batch, timings.as_deref_mut())?;
            last_checkpoint = Instant::now();
            let reused = if stats.reused > 0 {
                format!(" ({} reused)", stats.reused)
            } else {
                String::new()
            };
            eprintln!(
                "vagus: {}/{total} files checked, {} indexed{reused}, {embedded_chunks} chunks embedded ({}); progress committed",
                position + 1,
                stats.new + stats.changed + stats.refreshed,
                human_elapsed(run_started.elapsed())
            );
        }
    }

    // Deletions: indexed files no longer on disk.
    for path in existing.keys() {
        if !seen.contains(path) {
            mark_vectors_dirty(&db, &mut vec_marked)?;
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

    checkpoint(&db, &mut writer, &mut batch, timings.as_deref_mut())?;
    let t0 = Instant::now();
    // Let tantivy's merge policy finish any scheduled merges so segments stay bounded instead of
    // accumulating across checkpoint commits (the writer would otherwise drop before they run).
    writer.wait_merging_threads()?;
    if let Some(t) = timings.as_mut() {
        t.commit_ms += elapsed_ms(t0);
    }
    // Every snapshot file is now durable in SQLite and tantivy.
    if rebuilding {
        db.meta_delete(META_REBUILD_PENDING)?;
    }
    if bm25_healed > 0 {
        eprintln!(
            "vagus: restored full-text docs for {bm25_healed} note(s) from stored chunks (no re-embedding)"
        );
    }
    if stats.reused > 0 {
        eprintln!(
            "vagus: reused stored chunks and embeddings for {} note(s) from an interrupted batch (no re-embedding)",
            stats.reused
        );
    }
    if bm25_stale > 0 {
        eprintln!("vagus: dropped full-text docs for {bm25_stale} note(s) no longer indexed");
    }

    // Persist the vector index after the tantivy commit (G5: the stores move together). Either save
    // the incrementally-mutated index, or do the one-time full rebuild from the now-current f32 BLOBs
    // (no re-embed). A run with no vector mutation skips the save (the sidecar is already current).
    // Forced repairs count: `vec_marked` is set by every new/changed/refreshed/deleted file, so their
    // in-memory usearch mutations are never discarded at process exit (ADR 0022/G5).
    let t0 = Instant::now();
    match &vindex {
        Some(vi) if vec_marked => vi.save(&sidecar)?,
        Some(_) => {}
        None => UsearchIndex::rebuild_from_db(&db, EMBED_DIMS)?.save(&sidecar)?,
    }
    db.meta_set("vec_backend", "usearch")?;
    db.meta_set("vec_index_version", VEC_INDEX_VERSION)?;
    db.meta_set("vec_dims", &dims)?;
    if vec_marked || vec_dirty_at_start {
        db.meta_delete(META_VEC_DIRTY)?;
    }
    if let Some(t) = timings.as_mut() {
        t.vector_ms += elapsed_ms(t0);
    }
    Ok(stats)
}

/// Make the work so far durable: commit tantivy, then bless the `pending` rows of the files this batch
/// indexed. The order is the invariant — a row never looks current before its BM25 docs are on disk
/// (ADR 0029). Rows a killed run left pending are not in `batch` until this run redoes them.
fn checkpoint(
    db: &Db,
    writer: &mut IndexWriter,
    batch: &mut Vec<String>,
    timings: Option<&mut IndexTimings>,
) -> Result<()> {
    let t0 = Instant::now();
    writer.commit()?;
    db.finalize_pending_files(batch)?;
    batch.clear();
    if let Some(t) = timings {
        t.commit_ms += elapsed_ms(t0);
    }
    Ok(())
}

/// The stored embeddings of `rel`, if its rows provably hold what a full redo of `chunks` would write
/// (ADR 0029): the same chunk set (id, ord, kind, heading, body) with every embedding present at full
/// dimension.
///
/// Why that is enough: an embedding is a function of the chunk body alone under the pinned identity
/// (G4 refuses incremental runs across a model or recipe change; chunk-version changes wipe), and a
/// non-NULL embedding is only ever written for the body stored beside it — `replace_chunks` inserts
/// rows NULL in one transaction, and `set_embedding` fills them from that same chunk list. So equal
/// bodies mean equal vectors, whichever run or crash left the rows. A matching sha proves nothing:
/// `upsert_file_pending` stamps the new hash before `replace_chunks`, so a crash between them leaves
/// the new hash over the OLD, fully embedded rows. Comparing bodies catches exactly that.
#[allow(clippy::type_complexity)]
fn reusable_embeddings(
    db: &Db,
    rel: &str,
    chunks: &[Chunk],
) -> Result<Option<Vec<(String, Vec<f32>)>>> {
    let stored = db.chunk_rows_with_embeddings(rel)?;
    if stored.len() != chunks.len() {
        return Ok(None);
    }
    let mut vectors = Vec::with_capacity(stored.len());
    // `chunk_markdown` assigns ords sequentially and rows come back ORDER BY ord: compare by position.
    for ((row, embedding), fresh) in stored.into_iter().zip(chunks) {
        let same = row.id == fresh.id
            && row.ord == fresh.ord
            && row.kind == fresh.kind
            && row.heading_path == fresh.heading_path
            && row.body == fresh.body;
        match embedding {
            Some(vector) if same && vector.len() == EMBED_DIMS => vectors.push((row.id, vector)),
            _ => return Ok(None),
        }
    }
    Ok(Some(vectors))
}

/// Record, once per run and before the first SQLite vector change, that the sidecar on disk is
/// about to go stale. Cleared only after the post-loop save.
fn mark_vectors_dirty(db: &Db, marked: &mut bool) -> Result<()> {
    if !*marked {
        db.meta_set(META_VEC_DIRTY, "1")?;
        *marked = true;
    }
    Ok(())
}

/// Exit for a file whose disk content and SQLite rows are current. If the census found its BM25 docs
/// out of step, re-add them from the stored chunk rows — the exact text tantivy indexes — so the
/// repair needs no read, chunking, or embedding.
fn heal_bm25(
    db: &Db,
    lex: &Lex,
    writer: &IndexWriter,
    rel: &str,
    bm25_repair: &HashSet<String>,
) -> Result<FileOutcome> {
    if !bm25_repair.contains(rel) {
        return Ok(FileOutcome::Unchanged);
    }
    lex.replace_file(writer, rel, &db.chunks_for(rel)?)?;
    Ok(FileOutcome::Bm25Healed)
}

fn committed_files(db: &Db) -> Result<i64> {
    db.count("SELECT count(*) FROM files WHERE pending=0")
}

fn human_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Milliseconds since `start`, as `f64`.
fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::chunk::ChunkKind;
    use crate::embed::DocRecipe;

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

    // --- checkpoints, resume, and BM25 self-heal (ADR 0029) --------------------------------------

    /// A vault of content notes, one distinctive word each, so BM25 can name every note.
    fn content_vault(tag: &str, words: &[&str]) -> (crate::util::testdir::TempDir, Config) {
        let (dir, cfg) = empty_note_cfg(tag);
        for word in words {
            fs::write(
                cfg.vault.join(format!("{word}.md")),
                format!("# {word}\n\nThis note is about {word}.\n"),
            )
            .unwrap();
        }
        (dir, cfg)
    }

    /// Model-free stand-in for the embedder. Counts calls (one per file with chunks), records every
    /// embedded document, and can crash on a given call or raise the interrupt after N calls.
    #[derive(Default)]
    struct Fake {
        calls: Cell<usize>,
        documents: RefCell<Vec<String>>,
        crash_on_call: Option<usize>,
        interrupt_after_calls: Option<usize>,
    }

    impl Fake {
        fn run(
            &self,
            cfg: &Config,
            mode: IndexMode,
            checkpoint_files: usize,
        ) -> Result<IndexStats> {
            let mut embed = |documents: Vec<String>| -> Result<Vec<Vec<f32>>> {
                let call = self.calls.get() + 1;
                self.calls.set(call);
                if self.crash_on_call == Some(call) {
                    bail!("injected crash while embedding");
                }
                let vectors = documents
                    .iter()
                    .map(|body| {
                        let mut vector = vec![0.0; EMBED_DIMS];
                        vector[body.len() % EMBED_DIMS] = 1.0;
                        vector
                    })
                    .collect();
                self.documents.borrow_mut().extend(documents);
                Ok(vectors)
            };
            let interrupted = || {
                self.interrupt_after_calls
                    .is_some_and(|after| self.calls.get() >= after)
            };
            run_with(
                cfg,
                mode,
                None,
                Hooks {
                    embed: &mut embed,
                    interrupted: &interrupted,
                    checkpoint_files,
                    checkpoint_interval: Duration::MAX,
                },
            )
        }

        fn embedded(&self, word: &str) -> bool {
            self.documents
                .borrow()
                .iter()
                .any(|body| body.contains(word))
        }
    }

    fn bm25_finds(cfg: &Config, word: &str) -> bool {
        !Lex::open(&cfg.tantivy_dir())
            .unwrap()
            .search(word, 10)
            .unwrap()
            .is_empty()
    }

    fn bm25_docs(cfg: &Config) -> u64 {
        Lex::open(&cfg.tantivy_dir()).unwrap().num_docs().unwrap()
    }

    /// Every store agrees and nothing is left for a later run.
    fn assert_stores_agree(cfg: &Config) {
        let db = Db::open(&cfg.db_path()).unwrap();
        let chunks = db.count("SELECT count(*) FROM chunks").unwrap();
        assert!(chunks > 0, "fixture produced chunks");
        assert_eq!(
            db.count("SELECT count(*) FROM chunks WHERE embedding IS NULL")
                .unwrap(),
            0
        );
        assert_eq!(bm25_docs(cfg), chunks as u64, "BM25 docs == chunks");
        assert_eq!(
            UsearchIndex::view(&cfg.vector_path(), EMBED_DIMS)
                .unwrap()
                .len() as i64,
            chunks,
            "usearch vectors == embedded chunks"
        );
        let left = leftovers(&db).unwrap();
        assert_eq!(left.pending_files, 0);
        assert!(!left.rebuild_unfinished);
        assert!(!left.vectors_stale);
    }

    #[test]
    fn a_killed_run_never_blesses_uncommitted_files_and_the_next_index_heals_bm25() {
        // The 2026-09-10 shape: a reindex dies after SQLite holds fully embedded files but before
        // tantivy ever committed them. One batch for the whole run mirrors the old single commit.
        let words = ["alphaword", "bravoword", "charlieword"];
        let (_dir, cfg) = content_vault("index-killed-reindex", &words);
        let crash = Fake {
            crash_on_call: Some(3),
            ..Fake::default()
        };
        assert!(crash.run(&cfg, IndexMode::Full, usize::MAX).is_err());
        {
            let db = Db::open(&cfg.db_path()).unwrap();
            assert!(
                db.count(
                    "SELECT count(*) FROM chunks WHERE embedding IS NOT NULL
                     AND path IN ('alphaword.md','bravoword.md')"
                )
                .unwrap()
                    > 0,
                "two files were fully embedded in SQLite"
            );
            assert_eq!(bm25_docs(&cfg), 0, "but tantivy never committed them");
            assert_eq!(
                committed_files(&db).unwrap(),
                0,
                "so no file row may look current"
            );
            assert!(leftovers(&db).unwrap().rebuild_unfinished);
        }

        // The refresh inside `vagus search` must not pick up the rebuild.
        let auto = Fake::default();
        let stats = auto.run(&cfg, IndexMode::AutoRefresh, usize::MAX).unwrap();
        assert!(stats.deferred);
        assert_eq!(auto.calls.get(), 0);

        let resume = Fake::default();
        let stats = resume
            .run(&cfg, IndexMode::Incremental, usize::MAX)
            .unwrap();
        assert_eq!(stats.refreshed, 3, "all three uncommitted files redone");
        assert_eq!(
            stats.reused, 2,
            "the two fully embedded notes keep their vectors"
        );
        assert_eq!(
            resume.calls.get(),
            1,
            "only the note whose embedding never finished re-embeds (ADR 0022)"
        );
        assert!(resume.embedded("charlieword"));
        assert_eq!(stats.unchanged, 0);
        for word in words {
            assert!(bm25_finds(&cfg, word), "{word} is searchable by BM25");
        }
        assert_stores_agree(&cfg);
    }

    #[test]
    fn checkpointed_reindex_resumes_without_re_embedding_committed_files() {
        let words = ["alphaword", "bravoword", "charlieword", "deltaword"];
        let (_dir, cfg) = content_vault("index-checkpoint-resume", &words);
        let crash = Fake {
            crash_on_call: Some(3),
            ..Fake::default()
        };
        assert!(crash.run(&cfg, IndexMode::Full, 1).is_err());
        {
            let db = Db::open(&cfg.db_path()).unwrap();
            assert_eq!(committed_files(&db).unwrap(), 2, "two checkpoints landed");
            assert_eq!(
                leftovers(&db).unwrap().pending_files,
                1,
                "the in-flight file"
            );
            assert_eq!(
                bm25_docs(&cfg),
                db.count(
                    "SELECT count(*) FROM chunks WHERE path IN ('alphaword.md','bravoword.md')"
                )
                .unwrap() as u64
            );
        }

        // `vagus reindex` again resumes rather than wiping the committed half.
        let resume = Fake::default();
        let stats = resume.run(&cfg, IndexMode::Full, 1).unwrap();
        assert_eq!(resume.calls.get(), 2, "only the two unfinished files embed");
        assert!(!resume.embedded("alphaword") && !resume.embedded("bravoword"));
        assert!(resume.embedded("charlieword") && resume.embedded("deltaword"));
        assert!(!stats.full_reindex, "resumed, not restarted");
        assert_eq!(
            (stats.unchanged, stats.refreshed, stats.new),
            (2, 1, 1),
            "committed files skipped, pending file redone, unreached file added"
        );
        for word in words {
            assert!(bm25_finds(&cfg, word));
        }
        assert_stores_agree(&cfg);
    }

    #[test]
    fn interrupt_commits_finished_files_and_the_next_index_picks_up_the_rest() {
        let words = ["alphaword", "bravoword", "charlieword", "deltaword"];
        let (_dir, cfg) = content_vault("index-interrupt", &words);
        let stop = Fake {
            interrupt_after_calls: Some(2),
            ..Fake::default()
        };
        let error = stop.run(&cfg, IndexMode::Full, usize::MAX).unwrap_err();
        assert_eq!(error.downcast_ref::<Interrupted>().unwrap().remaining, 2);
        {
            let db = Db::open(&cfg.db_path()).unwrap();
            assert_eq!(committed_files(&db).unwrap(), 2);
            let left = leftovers(&db).unwrap();
            assert_eq!(
                left.pending_files, 0,
                "the stop checkpoint blessed both files"
            );
            assert!(left.rebuild_unfinished && left.vectors_stale);
            assert_eq!(
                bm25_docs(&cfg),
                db.count("SELECT count(*) FROM chunks").unwrap() as u64
            );
        }

        let resume = Fake::default();
        let stats = resume
            .run(&cfg, IndexMode::Incremental, usize::MAX)
            .unwrap();
        assert_eq!(resume.calls.get(), 2);
        assert_eq!((stats.unchanged, stats.new), (2, 2));
        assert_stores_agree(&cfg);
    }

    #[test]
    fn incremental_index_restores_bm25_docs_stranded_by_an_older_binary() {
        let words = ["alphaword", "bravoword", "charlieword"];
        let (_dir, cfg) = content_vault("index-bm25-self-heal", &words);
        Fake::default().run(&cfg, IndexMode::Full, 64).unwrap();

        // An index already in the stranded state: alphaword.md is blessed and embedded in SQLite but
        // has no BM25 docs, and tantivy holds docs for a path SQLite never had.
        {
            let lex = Lex::open(&cfg.tantivy_dir()).unwrap();
            let mut writer = lex.writer().unwrap();
            lex.delete_file(&writer, "alphaword.md");
            let ghost: Vec<Chunk> = (0..5)
                .map(|ord| Chunk {
                    id: sha256_hex(format!("ghost.md#{ord}").as_bytes()),
                    ord,
                    kind: ChunkKind::Content,
                    heading_path: String::new(),
                    body: "ghostword".into(),
                })
                .collect();
            lex.replace_file(&writer, "ghost.md", &ghost).unwrap();
            writer.commit().unwrap();
            writer.wait_merging_threads().unwrap();
        }
        let chunks = Db::open(&cfg.db_path())
            .unwrap()
            .count("SELECT count(*) FROM chunks")
            .unwrap() as u64;
        assert_ne!(bm25_docs(&cfg), chunks, "fixture totals disagree");
        assert!(!bm25_finds(&cfg, "alphaword"));

        let heal = Fake {
            crash_on_call: Some(1), // any embedding would be a wasted re-embed
            ..Fake::default()
        };
        let stats = heal.run(&cfg, IndexMode::Incremental, 64).unwrap();
        assert_eq!(heal.calls.get(), 0, "healed from stored chunks");
        assert_eq!((stats.refreshed, stats.unchanged), (1, 2));
        assert!(bm25_finds(&cfg, "alphaword"));
        assert!(!bm25_finds(&cfg, "ghostword"));
        assert_stores_agree(&cfg);
    }

    #[test]
    fn a_second_run_is_refused_while_another_holds_the_index_lock() {
        let (_dir, cfg) = empty_note_cfg("index-lock");
        cfg.ensure_dirs().unwrap();
        let held = lock_index(&cfg).unwrap();
        assert!(run_in_progress(&cfg));
        let error = Fake::default()
            .run(&cfg, IndexMode::Incremental, 64)
            .unwrap_err();
        assert!(error.to_string().contains("in progress"), "{error}");
        drop(held);
        assert!(!run_in_progress(&cfg));
    }

    #[test]
    fn a_checkpoint_blesses_only_the_files_it_committed() {
        // A killed run leaves bravoword.md embedded and charlieword.md half-done, both pending.
        let (_dir, cfg) = content_vault("index-checkpoint-scope", &["bravoword", "charlieword"]);
        let crash = Fake {
            crash_on_call: Some(2),
            ..Fake::default()
        };
        assert!(crash.run(&cfg, IndexMode::Full, usize::MAX).is_err());

        // The next run checkpoints a new note that sorts first, then stops before reaching the
        // leftovers. Its checkpoints must not bless rows it never redid.
        fs::write(
            cfg.vault.join("alphaword.md"),
            "# alphaword\n\nThis note is about alphaword.\n",
        )
        .unwrap();
        let stop = Fake {
            interrupt_after_calls: Some(1),
            ..Fake::default()
        };
        assert!(
            stop.run(&cfg, IndexMode::Incremental, 1)
                .unwrap_err()
                .is::<Interrupted>()
        );
        let db = Db::open(&cfg.db_path()).unwrap();
        assert_eq!(leftovers(&db).unwrap().pending_files, 2);
        drop(db);

        let resume = Fake::default();
        resume.run(&cfg, IndexMode::Incremental, 1).unwrap();
        assert!(!resume.embedded("alphaword"));
        assert!(
            !resume.embedded("bravoword"),
            "fully embedded leftover is reused"
        );
        assert!(
            resume.embedded("charlieword"),
            "NULL-embedding leftover re-embeds"
        );
        assert_stores_agree(&cfg);
    }

    /// Every stored vector of `path` is in the usearch sidecar and finds itself at cosine ~1.
    fn vectors_findable(cfg: &Config, path: &str) -> bool {
        let db = Db::open(&cfg.db_path()).unwrap();
        let index = UsearchIndex::view(&cfg.vector_path(), EMBED_DIMS).unwrap();
        let rows = db.chunk_rows_with_embeddings(path).unwrap();
        !rows.is_empty()
            && rows.iter().all(|(chunk, vector)| {
                let hits = index.search(vector.as_ref().unwrap(), 64).unwrap();
                hits.iter()
                    .any(|(key, cosine)| *key == key_for(&chunk.id) && *cosine > 0.99)
            })
    }

    /// What a crash right after `upsert_file_pending` leaves: the file's current hash and mtime,
    /// flagged pending, over whatever chunk rows were already stored.
    fn stamp_pending(cfg: &Config, rel: &str) {
        let note = cfg.vault.join(rel);
        let db = Db::open(&cfg.db_path()).unwrap();
        db.upsert_file_pending(
            rel,
            mtime_secs(&note).unwrap(),
            &sha256_hex(&fs::read(&note).unwrap()),
            now_unix(),
        )
        .unwrap();
    }

    #[test]
    fn a_killed_batch_with_unchanged_content_resumes_without_re_embedding() {
        let words = ["alphaword", "bravoword", "charlieword", "deltaword"];
        let (_dir, cfg) = content_vault("index-reuse-batch", &words);
        // One batch; die embedding the fourth note, so three notes sit fully embedded but uncommitted.
        let crash = Fake {
            crash_on_call: Some(4),
            ..Fake::default()
        };
        assert!(crash.run(&cfg, IndexMode::Full, usize::MAX).is_err());
        // Drop the half-done note, leaving nothing the model is needed for.
        fs::remove_file(cfg.vault.join("deltaword.md")).unwrap();

        let resume = Fake::default();
        let stats = resume
            .run(&cfg, IndexMode::Incremental, usize::MAX)
            .unwrap();
        assert_eq!(resume.calls.get(), 0, "stored embeddings reused");
        assert_eq!((stats.reused, stats.refreshed, stats.removed), (3, 3, 1));
        for word in &words[..3] {
            assert!(bm25_finds(&cfg, word), "{word} is searchable by BM25");
            assert!(
                vectors_findable(&cfg, &format!("{word}.md")),
                "{word} is searchable by vector"
            );
        }
        assert_stores_agree(&cfg);
    }

    #[test]
    fn a_crash_between_the_pending_upsert_and_replace_chunks_re_embeds() {
        let (_dir, cfg) = content_vault("index-reuse-trap", &["alphaword", "bravoword"]);
        Fake::default().run(&cfg, IndexMode::Full, 64).unwrap();
        fs::write(
            cfg.vault.join("alphaword.md"),
            "# alphaword\n\nRevised after indexing, now about zuluword.\n",
        )
        .unwrap();
        // The new hash lands over the OLD rows, whose embeddings are complete.
        stamp_pending(&cfg, "alphaword.md");
        Db::open(&cfg.db_path())
            .unwrap()
            .meta_set(META_VEC_DIRTY, "1")
            .unwrap();

        let resume = Fake::default();
        let stats = resume.run(&cfg, IndexMode::Incremental, 64).unwrap();
        assert_eq!(resume.calls.get(), 1, "stale rows are not reused");
        assert!(resume.embedded("zuluword"));
        assert_eq!((stats.reused, stats.refreshed, stats.unchanged), (0, 1, 1));
        assert!(bm25_finds(&cfg, "zuluword"));
        assert_stores_agree(&cfg);
    }

    #[test]
    fn content_edited_after_the_kill_re_embeds_instead_of_reusing() {
        let words = ["alphaword", "bravoword", "charlieword", "deltaword"];
        let (_dir, cfg) = content_vault("index-reuse-edited", &words);
        let crash = Fake {
            crash_on_call: Some(4),
            ..Fake::default()
        };
        assert!(crash.run(&cfg, IndexMode::Full, usize::MAX).is_err());
        fs::remove_file(cfg.vault.join("deltaword.md")).unwrap();
        fs::write(
            cfg.vault.join("bravoword.md"),
            "# bravoword\n\nEdited after the crash, now about yankeeword.\n",
        )
        .unwrap();

        let resume = Fake::default();
        let stats = resume
            .run(&cfg, IndexMode::Incremental, usize::MAX)
            .unwrap();
        assert_eq!(resume.calls.get(), 1);
        assert!(resume.embedded("yankeeword"));
        assert!(!resume.embedded("alphaword") && !resume.embedded("charlieword"));
        assert_eq!(stats.reused, 2);
        assert!(bm25_finds(&cfg, "yankeeword"));
        assert_stores_agree(&cfg);
    }

    #[test]
    fn reuse_rewrites_note_level_created_at_and_source() {
        let (_dir, cfg) = content_vault("index-reuse-filters", &["alphaword"]);
        Fake::default().run(&cfg, IndexMode::Full, 64).unwrap();
        // Same body, new lifecycle frontmatter: identical chunks, different filter columns.
        fs::write(
            cfg.vault.join("alphaword.md"),
            "---\ncreated: 2020-01-02T03:04\nsource: slack\n---\n# alphaword\n\nThis note is about alphaword.\n",
        )
        .unwrap();
        stamp_pending(&cfg, "alphaword.md");
        // Deliberately no `vec_dirty`: exercises the in-place sidecar path as well as the repack.

        let resume = Fake::default();
        let stats = resume.run(&cfg, IndexMode::Incremental, 64).unwrap();
        assert_eq!(resume.calls.get(), 0);
        assert_eq!(stats.reused, 1);
        let db = Db::open(&cfg.db_path()).unwrap();
        let (created_at, source): (Option<i64>, Option<String>) = db
            .conn
            .query_row(
                "SELECT created_at, source FROM chunks WHERE path='alphaword.md' LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            created_at,
            Some(note_created_at_secs(Some("2020-01-02T03:04"), 0.0))
        );
        assert_eq!(source.as_deref(), Some("slack"));
        assert_eq!(
            db.count(
                "SELECT count(DISTINCT coalesce(created_at,-1) || '|' || coalesce(source,'')) FROM chunks WHERE path='alphaword.md'"
            )
            .unwrap(),
            1,
            "every chunk of the note carries the same filters"
        );
        drop(db);
        assert!(vectors_findable(&cfg, "alphaword.md"));
        assert_stores_agree(&cfg);
    }

    /// Stamps the index as built by another document recipe: scenario A's prefix change.
    fn pin_another_recipe(cfg: &Config) {
        let recipe = DocRecipe {
            prefix: "title: {title} | text: ",
            ..DOC_RECIPE
        };
        Db::open(&cfg.db_path())
            .unwrap()
            .meta_set(META_EMBED_RECIPE, &recipe.identity())
            .unwrap();
    }

    fn pinned_recipe(cfg: &Config) -> Option<String> {
        Db::open(&cfg.db_path())
            .unwrap()
            .meta_get(META_EMBED_RECIPE)
            .unwrap()
    }

    #[test]
    fn a_changed_embedding_recipe_refuses_incremental_runs_and_reindex_rebuilds() {
        let (_dir, cfg) = content_vault("index-recipe-changed", &["alphaword", "bravoword"]);
        Fake::default().run(&cfg, IndexMode::Full, 64).unwrap();
        pin_another_recipe(&cfg);
        // Its vectors would land next to the old recipe's.
        fs::write(
            cfg.vault.join("charlieword.md"),
            "# charlieword\n\nThis note is about charlieword.\n",
        )
        .unwrap();

        for mode in [
            IndexMode::Incremental,
            IndexMode::AutoRefresh,
            IndexMode::Since { cutoff: 0 },
        ] {
            let refused = Fake::default();
            let error = refused.run(&cfg, mode, 64).unwrap_err().to_string();
            assert!(
                error.contains("embedding recipe changed") && error.contains("run `vagus reindex`"),
                "{mode:?}: {error}"
            );
            assert_eq!(refused.calls.get(), 0, "{mode:?} embeds nothing");
        }
        assert!(
            !bm25_finds(&cfg, "charlieword"),
            "refused runs index nothing"
        );

        let rebuild = Fake::default();
        let stats = rebuild.run(&cfg, IndexMode::Full, 64).unwrap();
        assert!(stats.full_reindex);
        assert_eq!(
            rebuild.calls.get(),
            3,
            "every note re-embeds under this recipe"
        );
        assert_eq!(pinned_recipe(&cfg), Some(DOC_RECIPE.identity()));
        assert_stores_agree(&cfg);
    }

    #[test]
    fn an_interrupted_rebuild_neither_resumes_nor_reuses_vectors_across_a_recipe_change() {
        // Scenario B: a reindex dies with two notes fully embedded but uncommitted, then the recipe
        // changes. Without the change both would resume on reuse alone (see the backfill test).
        let words = ["alphaword", "bravoword", "charlieword"];
        let (_dir, cfg) = content_vault("index-recipe-resume", &words);
        let crash = Fake {
            crash_on_call: Some(3),
            ..Fake::default()
        };
        assert!(crash.run(&cfg, IndexMode::Full, usize::MAX).is_err());
        fs::remove_file(cfg.vault.join("charlieword.md")).unwrap();
        pin_another_recipe(&cfg);

        let refused = Fake::default();
        let error = refused
            .run(&cfg, IndexMode::Incremental, usize::MAX)
            .unwrap_err();
        assert!(error.to_string().contains("run `vagus reindex`"), "{error}");
        assert_eq!(refused.calls.get(), 0);
        let db = Db::open(&cfg.db_path()).unwrap();
        assert_eq!(leftovers(&db).unwrap().pending_files, 3, "nothing blessed");
        drop(db);

        let rebuild = Fake::default();
        let stats = rebuild.run(&cfg, IndexMode::Full, usize::MAX).unwrap();
        assert!(
            stats.full_reindex,
            "`vagus reindex` restarts instead of resuming"
        );
        assert_eq!(rebuild.calls.get(), 2, "no stored vector is reused");
        assert_eq!(pinned_recipe(&cfg), Some(DOC_RECIPE.identity()));
        assert_stores_agree(&cfg);
    }

    #[test]
    fn an_index_from_before_the_recipe_was_pinned_backfills_it_without_re_embedding() {
        // vagus 0.13.1 and earlier pinned no `embed_recipe`; their vectors came from
        // PRE_PINNING_RECIPE. While that equals DOC_RECIPE the upgrade is free. Once DOC_RECIPE
        // moves, these runs must refuse instead, and this test flips with it.
        let words = ["alphaword", "bravoword", "charlieword"];
        let forget_recipe = |cfg: &Config| {
            Db::open(&cfg.db_path())
                .unwrap()
                .meta_delete(META_EMBED_RECIPE)
                .unwrap();
        };

        // A complete index, refreshed by the first search after the upgrade.
        let (_dir, cfg) = content_vault("index-recipe-backfill", &words);
        Fake::default().run(&cfg, IndexMode::Full, 64).unwrap();
        forget_recipe(&cfg);
        let upgraded = Fake::default();
        let stats = upgraded.run(&cfg, IndexMode::AutoRefresh, 64).unwrap();
        assert_eq!(upgraded.calls.get(), 0, "no embed calls");
        assert!(!stats.full_reindex);
        assert_eq!(stats.unchanged, 3);
        assert_eq!(
            pinned_recipe(&cfg),
            Some(DOC_RECIPE.identity()),
            "backfilled"
        );
        assert_stores_agree(&cfg);

        // An interrupted rebuild from before pinning resumes and reuses its vectors.
        let (_resume_dir, cfg) = content_vault("index-recipe-backfill-resume", &words);
        let crash = Fake {
            crash_on_call: Some(3),
            ..Fake::default()
        };
        assert!(crash.run(&cfg, IndexMode::Full, usize::MAX).is_err());
        fs::remove_file(cfg.vault.join("charlieword.md")).unwrap();
        forget_recipe(&cfg);
        let resume = Fake::default();
        let stats = resume.run(&cfg, IndexMode::Full, usize::MAX).unwrap();
        assert!(!stats.full_reindex, "resumed, not restarted");
        assert_eq!((stats.reused, resume.calls.get()), (2, 0));
        assert_eq!(pinned_recipe(&cfg), Some(DOC_RECIPE.identity()));
        assert_stores_agree(&cfg);
    }
}
