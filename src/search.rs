//! Search entry point. BM25 (lexical) is live here; semantic + hybrid RRF arrive with the embed
//! step. Output is a stable shape (`--json`) so the Claude Code skill parses rather than scrapes.

use anyhow::{bail, Result};
use clap::ValueEnum;
use serde::Serialize;

use crate::config::Config;
use crate::db::Db;
use crate::lex::Lex;

#[derive(Clone, Copy, ValueEnum)]
pub enum Mode {
    /// BM25 + semantic, fused with RRF.
    Hybrid,
    /// Full-text (BM25) only.
    Bm25,
    /// Semantic (embeddings) only.
    Vec,
}

#[derive(Serialize)]
pub struct Hit {
    pub chunk_id: String,
    pub path: String,
    pub heading: String,
    pub score: f32,
    pub snippet: String,
}

fn snippet(body: &str, n: usize) -> String {
    let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > n {
        let cut: String = one_line.chars().take(n).collect();
        format!("{cut}…")
    } else {
        one_line
    }
}

/// Resolve ranked chunk ids into displayable hits (joining SQLite for path/heading/body).
/// `score` is a rank-based reciprocal so the JSON shape is stable across modes.
fn hydrate(db: &Db, ids: &[String]) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        if let Some((path, heading, body)) = db.chunk_row(id)? {
            hits.push(Hit {
                chunk_id: id.clone(),
                path,
                heading,
                score: 1.0 / (i as f32 + 1.0),
                snippet: snippet(&body, 200),
            });
        }
    }
    Ok(hits)
}

pub fn run(cfg: &Config, query: &str, mode: Mode, json: bool, limit: usize) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let ids = match mode {
        Mode::Bm25 => {
            let lex = Lex::open(&cfg.tantivy_dir())?;
            lex.search(query, limit)?
        }
        Mode::Vec => bail!("`--mode vec` is not implemented yet (arrives with the embed step)"),
        Mode::Hybrid => {
            bail!("`--mode hybrid` is not implemented yet (arrives with the embed step)")
        }
    };
    let hits = hydrate(&db, &ids)?;
    emit(&hits, json);
    Ok(())
}

fn emit(hits: &[Hit], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(hits).unwrap_or_else(|_| "[]".into()));
        return;
    }
    if hits.is_empty() {
        println!("(no results)");
        return;
    }
    for (i, h) in hits.iter().enumerate() {
        let loc = if h.heading.is_empty() {
            h.path.clone()
        } else {
            format!("{} › {}", h.path, h.heading)
        };
        println!("{:>2}. {}", i + 1, loc);
        println!("    {}", h.snippet);
    }
}
