//! `vagus tick` / `vagus fame` / `vagus ticks`: local usage counters and explicit presentation
//! provenance (ADR 0021/G25).
//!
//! These tables are non-rebuildable local user data in `meta.db`, never vault/frontmatter content.
//! Bare paths increment counters only. The tier-2 search skill opts into versioned rank provenance and
//! returns events only for notes it actually cites; counter + run + events commit atomically.

use std::collections::HashSet;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail, ensure};
use chrono::{Local, TimeZone};
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::config::Config;
use crate::db::{Db, TickWrite};
use crate::notes::vault_rel;
use crate::provenance::{SCHEMA_VERSION, TickEventBatch};

const MAX_EVENTS_JSON_BYTES: usize = 1_000_000;
const MAX_QUERY_BYTES: usize = 16_384;
const MAX_REPORT_LIMIT: usize = 10_000;
const SELECTION_BIAS_CAVEAT: &str = "presentation events are selection-biased: they describe retrieved candidates the tier-2 judge cited, not recall or ground-truth relevance; path-level tick totals repeat across pipeline/corpus groups";

/// One `vagus fame --json` element. Stable shape (G9a): without `--all`, `missing` is always false
/// and never serialized, so the default element is exactly {path, ticks, first_used, last_used}.
#[derive(Serialize)]
pub struct FameRow {
    pub path: String,
    pub ticks: i64,
    pub first_used: i64,
    pub last_used: i64,
    /// The note has no `files` row (deleted or renamed outside vagus). Only ever true under `--all`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
}

/// One `vagus tick --json` element: exactly {path, ticks}, both always present (G9a).
#[derive(Debug, Serialize)]
struct TickOut {
    path: String,
    ticks: i64,
}

fn normalize_tick_path(cfg: &Config, raw: &str) -> Result<String> {
    let rel = vault_rel(cfg, Path::new(raw));
    ensure!(!rel.is_empty(), "tick path must not be empty");
    ensure!(rel.len() <= 4096, "tick path is too long");
    let path = Path::new(&rel);
    ensure!(
        !path.is_absolute(),
        "tick path must be vault-relative or an absolute path inside the vault: {raw:?}"
    );
    ensure!(
        path.extension().and_then(|ext| ext.to_str()) == Some("md"),
        "tick path must name a Markdown note: {raw:?}"
    );
    for component in path.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "tick path must be normalized without `.` or `..`: {raw:?}"
        );
    }
    Ok(rel)
}

/// Normalize and dedupe one invocation. Provenance events win over duplicate bare paths; duplicate
/// event paths are rejected (including alias spellings that normalize to the same vault key).
fn prepare_records(
    cfg: &Config,
    paths: &[String],
    batch: Option<&TickEventBatch>,
) -> Result<Vec<TickWrite>> {
    if let Some(batch) = batch {
        batch.validate()?;
    }
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    if let Some(batch) = batch {
        for event in &batch.events {
            let path = normalize_tick_path(cfg, &event.path)?;
            ensure!(
                seen.insert(path.clone()),
                "duplicate normalized event path {path:?}"
            );
            records.push(TickWrite {
                path,
                provenance: Some(event.provenance.clone()),
            });
        }
    }
    for raw in paths {
        let path = normalize_tick_path(cfg, raw)?;
        if seen.insert(path.clone()) {
            records.push(TickWrite {
                path,
                provenance: None,
            });
        }
    }
    ensure!(
        !records.is_empty(),
        "nothing to tick: pass note path(s) or --events JSON"
    );
    Ok(records)
}

/// Read cache membership before starting the user-data transaction, then atomically write every
/// counter/event. Unknown paths still record because `files` is derived and can be empty mid-reindex.
fn record(
    db: &mut Db,
    cfg: &Config,
    paths: &[String],
    batch: Option<&TickEventBatch>,
    query: Option<&str>,
) -> Result<Vec<TickOut>> {
    let records = prepare_records(cfg, paths, batch)?;
    let mut known = Vec::with_capacity(records.len());
    for record in &records {
        known.push(
            db.conn
                .query_row(
                    "SELECT 1 FROM files WHERE path=?1",
                    rusqlite::params![record.path],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false),
        );
    }
    let counts = db.record_ticks_atomic(&records, batch.map(|batch| &batch.run), query)?;
    let mut out = Vec::with_capacity(records.len());
    for ((record, ticks), known) in records.into_iter().zip(counts).zip(known) {
        if !known {
            eprintln!("vagus: note not in index: {}", record.path);
        }
        out.push(TickOut {
            path: record.path,
            ticks,
        });
    }
    Ok(out)
}

pub fn tick(
    cfg: &Config,
    paths: &[String],
    events_json: Option<&str>,
    store_query: bool,
    query: Option<&str>,
    json: bool,
) -> Result<()> {
    if let Some(raw) = events_json {
        ensure!(
            raw.len() <= MAX_EVENTS_JSON_BYTES,
            "--events JSON exceeds {MAX_EVENTS_JSON_BYTES} bytes"
        );
    }
    let batch: Option<TickEventBatch> = events_json
        .map(|raw| serde_json::from_str(raw).context("parsing --events JSON"))
        .transpose()?;

    let stored_query = match (store_query, query) {
        (false, None) => None,
        (false, Some(_)) => bail!("--query requires --store-query"),
        (true, None) => bail!("--store-query requires --query"),
        (true, Some(query)) => {
            ensure!(batch.is_some(), "--store-query requires --events");
            ensure!(!query.trim().is_empty(), "stored query must not be empty");
            ensure!(
                query.len() <= MAX_QUERY_BYTES,
                "stored query exceeds {MAX_QUERY_BYTES} bytes"
            );
            Some(query)
        }
    };

    let mut db = Db::open(&cfg.db_path())?;
    let out = record(&mut db, cfg, paths, batch.as_ref(), stored_query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for tick in &out {
            println!("ticked {} ({})", tick.path, tick.ticks);
        }
    }
    Ok(())
}

pub fn fame(cfg: &Config, limit: usize, all: bool, json: bool) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let rows: Vec<FameRow> = db
        .fame(limit, all)?
        .into_iter()
        .map(|(path, ticks, first_used, last_used, missing)| FameRow {
            path,
            ticks,
            first_used,
            last_used,
            missing,
        })
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        // Ticks may exist but all be orphaned (vault reorganized outside vagus): don't claim
        // "no ticks yet" when `--all` would show them.
        let orphans = if all { 0 } else { db.orphan_tick_count()? };
        if orphans > 0 {
            println!(
                "no ticks on indexed notes — {orphans} orphaned (moved/deleted outside vagus); try --all"
            );
        } else {
            println!("no ticks yet — the /search skill records usage as it presents notes");
        }
        return Ok(());
    }
    let width = rows.iter().map(|row| row.path.len()).max().unwrap_or(0);
    for row in &rows {
        let marker = if row.missing { "  (missing)" } else { "" };
        println!(
            "{:>4}  {:<width$}  {}{marker}",
            row.ticks,
            row.path,
            day(row.last_used)
        );
    }
    Ok(())
}

/// Stable JSON row for one note+pipeline+corpus group. Grouping prevents medians from silently mixing
/// unlike binaries/models/caps/context windows or corpus generations.
#[derive(Serialize)]
struct RankRow {
    path: String,
    ticks: i64,
    events: i64,
    pipeline_id: Option<String>,
    corpus_sha256: Option<String>,
    median_fusion_rank: Option<f64>,
    median_bm25_rank: Option<f64>,
    median_cosine_rank: Option<f64>,
    median_rerank_rank: Option<f64>,
    median_final_rank: Option<f64>,
    rerank_scored: i64,
    unscored_tail: i64,
    last_event: Option<i64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    missing: bool,
}

#[derive(Serialize)]
struct RankReport {
    schema_version: u32,
    caveat: &'static str,
    rows: Vec<RankRow>,
}

/// Descriptive read path for the local presentation log. It cannot measure unretrieved answers and
/// never claims calibration; ADR 0024 eval remains the acceptance harness.
pub fn ticks_report(cfg: &Config, limit: usize, all: bool, json: bool) -> Result<()> {
    ensure!(
        (1..=MAX_REPORT_LIMIT).contains(&limit),
        "ticks --limit must be between 1 and {MAX_REPORT_LIMIT}"
    );
    let db = Db::open(&cfg.db_path())?;
    let rows: Vec<RankRow> = db
        .rank_report(limit, all)?
        .into_iter()
        .map(|row| RankRow {
            path: row.path,
            ticks: row.ticks,
            events: row.events,
            pipeline_id: row.pipeline_id,
            corpus_sha256: row.corpus_sha256,
            median_fusion_rank: row.median_fusion_rank,
            median_bm25_rank: row.median_bm25_rank,
            median_cosine_rank: row.median_cosine_rank,
            median_rerank_rank: row.median_rerank_rank,
            median_final_rank: row.median_final_rank,
            rerank_scored: row.rerank_scored,
            unscored_tail: row.unscored_tail,
            last_event: row.last_event,
            missing: row.missing,
        })
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&RankReport {
                schema_version: SCHEMA_VERSION,
                caveat: SELECTION_BIAS_CAVEAT,
                rows,
            })?
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("no ticks yet — the /search skill records usage and explicit rank provenance");
        return Ok(());
    }
    println!(
        "{:>4}  {:>4}  {:>3}  {:>3}  {:>5}  {:>5}  {:<12}  {:<12}  path",
        "tick", "evt", "xE", "tail", "fuse", "final", "pipeline", "corpus"
    );
    for row in &rows {
        let pipeline = row
            .pipeline_id
            .as_deref()
            .map(|value| value.get(..value.len().min(12)).unwrap_or(value))
            .unwrap_or("-");
        let corpus = row
            .corpus_sha256
            .as_deref()
            .map(|value| value.get(..value.len().min(12)).unwrap_or(value))
            .unwrap_or("-");
        let marker = if row.missing { "  (missing)" } else { "" };
        println!(
            "{:>4}  {:>4}  {:>3}  {:>3}  {:>5}  {:>5}  {:<12}  {:<12}  {}{marker}",
            row.ticks,
            row.events,
            row.rerank_scored,
            row.unscored_tail,
            rank(row.median_fusion_rank),
            rank(row.median_final_rank),
            pipeline,
            corpus,
            row.path,
        );
    }
    println!("caveat: {SELECTION_BIAS_CAVEAT}");
    Ok(())
}

fn rank(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |rank| format!("{rank:.1}"))
}

fn day(secs: i64) -> String {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{
        FUSION_CANDIDATE_POOL, HitRankProvenance, PresentedEvent, RESULT_POLICY,
        SearchRunProvenance,
    };
    use crate::util::testdir::TempDir;

    fn temp_cfg(tag: &str) -> (TempDir, Config) {
        let dir = TempDir::new(tag);
        let cfg = Config {
            vault: dir.path().join("vault"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };
        std::fs::create_dir_all(&cfg.vault).unwrap();
        (dir, cfg)
    }

    fn run() -> SearchRunProvenance {
        let mut run = SearchRunProvenance {
            schema_version: SCHEMA_VERSION,
            pipeline_id: String::new(),
            binary_version: "0.10.0".into(),
            binary_sha256: "a".repeat(64),
            corpus_sha256: "b".repeat(64),
            indexed_files: 1,
            indexed_chunks: 30,
            embedded_chunks: 30,
            embed_model: crate::config::EMBED_MODEL.into(),
            embed_dims: crate::config::EMBED_DIMS,
            chunk_version: crate::config::CHUNK_VERSION.into(),
            tantivy_version: "0.26".into(),
            fusion_policy: crate::search::FUSION_POLICY.into(),
            fusion_candidate_pool: FUSION_CANDIDATE_POOL.into(),
            vector_backend: "exact".into(),
            exact_requested: true,
            automatic_exact_cutoff: crate::vector::EXACT_SCAN_CUTOFF,
            rerank_model: crate::rerank::MODEL_ID.into(),
            rerank_policy: crate::rerank::policy_id(0).unwrap(),
            relevance_policy: crate::relevance::POLICY.into(),
            source_limit: 120,
            fusion_limit: 40,
            candidate_pool: 30,
            rerank_cap: 20,
            limit: 10,
            returned: 10,
            full_body: true,
            note_level: true,
            metadata_filters: false,
            cwd_scope: true,
            scope_policy: "c".repeat(64),
            scope_elided: 0,
            index_refresh_requested: true,
            index_refresh_succeeded: true,
            result_policy: RESULT_POLICY.into(),
        };
        run.set_pipeline_id();
        run
    }

    fn batch(path: &str) -> TickEventBatch {
        let run = run();
        let mut provenance = HitRankProvenance {
            event_id: String::new(),
            fusion_rank: 12,
            bm25_rank: Some(8),
            cosine_rank: Some(14),
            rerank_rank: Some(2),
            final_rank: 1,
            rerank_scored: true,
        };
        provenance.bind_to(&run, path);
        TickEventBatch {
            run,
            events: vec![PresentedEvent {
                path: path.into(),
                provenance,
            }],
        }
    }

    #[test]
    fn fame_json_shape_stable() {
        let row = FameRow {
            path: "a.md".into(),
            ticks: 7,
            first_used: 1,
            last_used: 2,
            missing: false,
        };
        let value: serde_json::Value = serde_json::to_value(&row).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["first_used", "last_used", "path", "ticks"]);

        let orphan = FameRow {
            path: "gone.md".into(),
            ticks: 1,
            first_used: 1,
            last_used: 1,
            missing: true,
        };
        let value = serde_json::to_value(&orphan).unwrap();
        assert_eq!(value["missing"], true);
    }

    #[test]
    fn tick_json_shape_stable() {
        let tick = TickOut {
            path: "a.md".into(),
            ticks: 7,
        };
        let value: serde_json::Value = serde_json::to_value(&tick).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["path", "ticks"]);
    }

    #[test]
    fn tick_dedupes_and_normalizes_within_invocation() {
        let (_dir, cfg) = temp_cfg("tick-dedupe");
        let mut db = Db::open(&cfg.db_path()).unwrap();
        let abs = cfg.vault.join("20-Areas/foo.md").display().to_string();
        let paths = vec![abs, "20-Areas/foo.md".into(), "20-Areas/foo.md".into()];
        let out = record(&mut db, &cfg, &paths, None, None).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "20-Areas/foo.md");
        assert_eq!(out[0].ticks, 1);
    }

    #[test]
    fn tick_rejects_paths_outside_the_vault_key_space() {
        let (_dir, cfg) = temp_cfg("tick-paths");
        for path in [
            "../outside.md",
            "/tmp/outside.md",
            "not-markdown.txt",
            "./a.md",
        ] {
            assert!(normalize_tick_path(&cfg, path).is_err(), "accepted {path}");
        }
        assert_eq!(
            normalize_tick_path(&cfg, "00-Inbox/a.md").unwrap(),
            "00-Inbox/a.md"
        );
    }

    #[test]
    fn event_and_duplicate_bare_path_become_one_atomic_tick() {
        let (_dir, cfg) = temp_cfg("tick-event");
        let mut db = Db::open(&cfg.db_path()).unwrap();
        let batch = batch("20-Areas/foo.md");
        let out = record(
            &mut db,
            &cfg,
            &["20-Areas/foo.md".into()],
            Some(&batch),
            None,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ticks, 1);
        assert_eq!(db.count("SELECT count(*) FROM tick_runs").unwrap(), 1);
        assert_eq!(db.count("SELECT count(*) FROM tick_events").unwrap(), 1);
        assert_eq!(
            db.count("SELECT count(*) FROM tick_runs WHERE query IS NOT NULL")
                .unwrap(),
            0
        );
    }

    #[test]
    fn payload_and_query_bounds_fail_before_opening_the_database() {
        let (_dir, cfg) = temp_cfg("tick-input-bounds");
        let oversized_payload = "x".repeat(MAX_EVENTS_JSON_BYTES + 1);
        assert!(tick(&cfg, &[], Some(&oversized_payload), false, None, false).is_err());
        let payload = serde_json::to_string(&batch("a.md")).unwrap();
        let oversized_query = "x".repeat(MAX_QUERY_BYTES + 1);
        assert!(
            tick(
                &cfg,
                &[],
                Some(&payload),
                true,
                Some(&oversized_query),
                false
            )
            .is_err()
        );
        assert!(!cfg.db_path().exists());
    }

    #[test]
    fn query_content_is_stored_only_under_explicit_opt_in() {
        let (_dir, cfg) = temp_cfg("tick-query");
        let payload = serde_json::to_string(&batch("a.md")).unwrap();
        assert!(tick(&cfg, &[], Some(&payload), false, Some("secret"), false).is_err());
        tick(&cfg, &[], Some(&payload), true, Some("secret"), false).unwrap();
        let db = Db::open(&cfg.db_path()).unwrap();
        let query: Option<String> = db
            .conn
            .query_row("SELECT query FROM tick_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(query.as_deref(), Some("secret"));
    }

    #[test]
    fn report_limit_is_bounded_before_db_work() {
        let (_dir, cfg) = temp_cfg("tick-report-limit");
        assert!(ticks_report(&cfg, 0, false, true).is_err());
        assert!(ticks_report(&cfg, MAX_REPORT_LIMIT + 1, false, true).is_err());
    }

    #[test]
    fn rank_report_json_contract_names_selection_bias() {
        let report = RankReport {
            schema_version: SCHEMA_VERSION,
            caveat: SELECTION_BIAS_CAVEAT,
            rows: vec![],
        };
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert!(
            value["caveat"]
                .as_str()
                .unwrap()
                .contains("selection-biased")
        );
        assert!(value["rows"].is_array());
    }

    #[test]
    fn event_failure_rolls_back_counter_run_and_every_event() {
        let (_dir, cfg) = temp_cfg("tick-rollback");
        let mut db = Db::open(&cfg.db_path()).unwrap();
        db.conn
            .execute_batch(
                "CREATE TRIGGER reject_tick_event BEFORE INSERT ON tick_events
                 BEGIN SELECT RAISE(ABORT, 'injected event failure'); END;",
            )
            .unwrap();
        let error = record(&mut db, &cfg, &[], Some(&batch("20-Areas/foo.md")), None).unwrap_err();
        assert!(error.to_string().contains("injected event failure"));
        assert_eq!(db.count("SELECT count(*) FROM ticks").unwrap(), 0);
        assert_eq!(db.count("SELECT count(*) FROM tick_runs").unwrap(), 0);
        assert_eq!(db.count("SELECT count(*) FROM tick_events").unwrap(), 0);
    }

    #[test]
    fn event_paths_follow_moves_and_destination_conflicts_preserve_history() {
        let (_dir, cfg) = temp_cfg("tick-event-move");
        let mut db = Db::open(&cfg.db_path()).unwrap();
        let mut batch = batch("00-Inbox/foo.md");
        let mut destination = HitRankProvenance {
            event_id: String::new(),
            fusion_rank: 13,
            bm25_rank: Some(9),
            cosine_rank: Some(15),
            rerank_rank: Some(3),
            final_rank: 2,
            rerank_scored: true,
        };
        destination.bind_to(&batch.run, "30-Resources/foo.md");
        batch.events.push(PresentedEvent {
            path: "30-Resources/foo.md".into(),
            provenance: destination,
        });
        record(&mut db, &cfg, &[], Some(&batch), None).unwrap();
        db.tick_rename("00-Inbox/foo.md", "30-Resources/foo.md")
            .unwrap();
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT count FROM ticks WHERE path='30-Resources/foo.md'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            db.count("SELECT count(*) FROM tick_events WHERE path='30-Resources/foo.md'")
                .unwrap(),
            2
        );
        assert_eq!(
            db.count("SELECT count(*) FROM tick_events WHERE path='00-Inbox/foo.md'")
                .unwrap(),
            0
        );
    }

    #[test]
    fn report_never_mixes_corpus_generations_and_names_unscored_tail() {
        let (_dir, cfg) = temp_cfg("tick-report-groups");
        let mut db = Db::open(&cfg.db_path()).unwrap();
        let first = batch("a.md");
        record(&mut db, &cfg, &[], Some(&first), None).unwrap();

        let mut second = batch("a.md");
        second.run.corpus_sha256 = "c".repeat(64);
        second.events[0].provenance = HitRankProvenance {
            event_id: String::new(),
            fusion_rank: 24,
            bm25_rank: None,
            cosine_rank: Some(24),
            rerank_rank: None,
            final_rank: 8,
            rerank_scored: false,
        };
        let second_path = second.events[0].path.clone();
        second.events[0]
            .provenance
            .bind_to(&second.run, &second_path);
        // Corpus is intentionally outside pipeline identity; reports group on both columns.
        second.run.validate().unwrap();
        record(&mut db, &cfg, &[], Some(&second), None).unwrap();

        let rows = db.rank_report(10, true).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.events == 1 && row.ticks == 2));
        assert_eq!(rows.iter().map(|row| row.rerank_scored).sum::<i64>(), 1);
        assert_eq!(rows.iter().map(|row| row.unscored_tail).sum::<i64>(), 1);
        assert_ne!(rows[0].corpus_sha256, rows[1].corpus_sha256);
    }
}
