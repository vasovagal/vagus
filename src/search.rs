//! Search entry point: BM25 (lexical), vector (semantic), and hybrid (RRF k=60).
//!
//! Output is a stable shape (`--json`) so the Claude Code skill parses rather than scrapes.

use std::collections::HashMap;

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

use crate::config::Config;
use crate::db::Db;
use crate::embed::Embedder;
use crate::lex::Lex;

/// RRF constant (guardrail G8).
const RRF_K: f32 = 60.0;

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

fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Brute-force exact cosine over the in-RAM normalized matrix. Returns (chunk_id, cosine) top-k.
fn vec_search(cfg: &Config, db: &Db, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
    let mut emb = Embedder::new(&cfg.cache_dir)?;
    let q = emb.embed_query(query)?; // normalized
    let mut scored: Vec<(String, f32)> = db
        .all_embeddings()?
        .into_iter()
        .map(|(id, v)| (id, dot(&q, &v))) // both normalized -> cosine
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    Ok(scored)
}

/// Reciprocal Rank Fusion over several ranked id-lists (1-based rank). Returns (id, fused_score).
fn rrf(lists: &[Vec<String>], limit: usize) -> Vec<(String, f32)> {
    let mut score: HashMap<String, f32> = HashMap::new();
    for list in lists {
        for (i, id) in list.iter().enumerate() {
            *score.entry(id.clone()).or_insert(0.0) += 1.0 / (RRF_K + (i as f32 + 1.0));
        }
    }
    let mut fused: Vec<(String, f32)> = score.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1));
    fused.truncate(limit);
    fused
}

/// Resolve ranked (chunk_id, score) into displayable hits (joining SQLite for path/heading/body).
fn hydrate(db: &Db, ranked: &[(String, f32)]) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for (id, score) in ranked {
        if let Some((path, heading, body)) = db.chunk_row(id)? {
            hits.push(Hit {
                chunk_id: id.clone(),
                path,
                heading,
                score: *score,
                snippet: snippet(&body, 200),
            });
        }
    }
    Ok(hits)
}

pub fn run(cfg: &Config, query: &str, mode: Mode, json: bool, limit: usize) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let ranked: Vec<(String, f32)> = match mode {
        Mode::Bm25 => {
            let lex = Lex::open(&cfg.tantivy_dir())?;
            lex.search(query, limit)?
                .into_iter()
                .enumerate()
                .map(|(i, id)| (id, 1.0 / (i as f32 + 1.0)))
                .collect()
        }
        Mode::Vec => vec_search(cfg, &db, query, limit)?,
        Mode::Hybrid => {
            // Pull a deeper candidate set from each retriever, then fuse.
            let cand = (limit * 3).max(30);
            let lex = Lex::open(&cfg.tantivy_dir())?;
            let bm: Vec<String> = lex.search(query, cand)?;
            let ve: Vec<String> = vec_search(cfg, &db, query, cand)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            rrf(&[bm, ve], limit)
        }
    };
    let hits = hydrate(&db, &ranked)?;
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
        println!("{:>2}. [{:.3}] {}", i + 1, h.score, loc);
        println!("    {}", h.snippet);
    }
}
