//! `vagus tick` / `vagus fame`: local per-note usage counts (ADR 0021/G25).
//!
//! Ticks are user data in `meta.db` — never in the vault or frontmatter, not derivable from the
//! Markdown. The tier-2 `/search` skill records them after presenting results; bare `vagus search`
//! never writes (G19).

use std::path::Path;

use anyhow::Result;
use chrono::{Local, TimeZone};
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::config::Config;
use crate::db::Db;
use crate::notes::vault_rel;

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
#[derive(Serialize)]
struct TickOut {
    path: String,
    ticks: i64,
}

/// Record one tick per unique path, deduped in first-seen order (note-level semantics even when the
/// caller collected paths from `--chunks` hits). Unknown paths still record — the `files` table is a
/// cache — with a stderr notice, keeping `--json` stdout pure (the search elision-notice precedent).
fn record(db: &Db, cfg: &Config, paths: &[String]) -> Result<Vec<TickOut>> {
    let mut unique: Vec<String> = Vec::new();
    for p in paths {
        let rel = vault_rel(cfg, Path::new(p));
        if !unique.contains(&rel) {
            unique.push(rel);
        }
    }
    let mut out = Vec::with_capacity(unique.len());
    for path in unique {
        let ticks = db.tick(&path)?;
        let known: bool = db
            .conn
            .query_row(
                "SELECT 1 FROM files WHERE path=?1",
                rusqlite::params![path],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !known {
            eprintln!("vagus: note not in index: {path}");
        }
        out.push(TickOut { path, ticks });
    }
    Ok(out)
}

pub fn tick(cfg: &Config, paths: &[String], json: bool) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let out = record(&db, cfg, paths)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for t in &out {
            println!("ticked {} ({})", t.path, t.ticks);
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
    let width = rows.iter().map(|r| r.path.len()).max().unwrap_or(0);
    for r in &rows {
        let marker = if r.missing { "  (missing)" } else { "" };
        println!(
            "{:>4}  {:<width$}  {}{marker}",
            r.ticks,
            r.path,
            day(r.last_used)
        );
    }
    Ok(())
}

fn day(secs: i64) -> String {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TempDir;

    fn temp_cfg(tag: &str) -> (TempDir, Config) {
        let dir = TempDir::new(tag);
        let cfg = Config {
            vault: dir.path().join("vault"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };
        (dir, cfg)
    }

    #[test]
    fn fame_json_shape_stable() {
        // Stable shape (G9a): default rows are exactly {path, ticks, first_used, last_used};
        // `missing` appears only when true (the --all orphan case).
        let row = FameRow {
            path: "a.md".into(),
            ticks: 7,
            first_used: 1,
            last_used: 2,
            missing: false,
        };
        let v: serde_json::Value = serde_json::to_value(&row).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["first_used", "last_used", "path", "ticks"]);

        let orphan = FameRow {
            path: "gone.md".into(),
            ticks: 1,
            first_used: 1,
            last_used: 1,
            missing: true,
        };
        let v = serde_json::to_value(&orphan).unwrap();
        assert_eq!(v.as_object().unwrap()["missing"], serde_json::json!(true));
    }

    #[test]
    fn tick_json_shape_stable() {
        let t = TickOut {
            path: "a.md".into(),
            ticks: 7,
        };
        let v: serde_json::Value = serde_json::to_value(&t).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["path", "ticks"]);
    }

    #[test]
    fn tick_dedupes_within_invocation() {
        let (_d, cfg) = temp_cfg("tick-dedupe");
        let db = Db::open(&cfg.db_path()).unwrap();
        let paths = vec!["20-Areas/foo.md".to_string(), "20-Areas/foo.md".to_string()];
        let out = record(&db, &cfg, &paths).unwrap();
        assert_eq!(out.len(), 1, "one output element per unique path");
        assert_eq!(out[0].ticks, 1, "at most +1 per invocation");
        assert_eq!(db.count("SELECT count FROM ticks").unwrap(), 1);
    }

    #[test]
    fn tick_strips_absolute_vault_paths() {
        let (_d, cfg) = temp_cfg("tick-abs");
        let db = Db::open(&cfg.db_path()).unwrap();
        let abs = cfg.vault.join("20-Areas/foo.md").display().to_string();
        let out = record(&db, &cfg, &[abs, "20-Areas/foo.md".into()]).unwrap();
        assert_eq!(out.len(), 1, "absolute-under-vault dedupes with relative");
        assert_eq!(out[0].path, "20-Areas/foo.md");
    }
}
