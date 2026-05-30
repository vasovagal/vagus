//! Search entry point: BM25 (lexical), vector (semantic), and hybrid (RRF k=60).
//!
//! Human output shows a 0–100 relevance **relative to the top hit** — the raw RRF scalar is
//! rank-based and tiny (≤ 2/(k+1) ≈ 0.033), so printing it directly is misleading. `--json` keeps a
//! stable shape for the Claude Code skill and carries the raw fused `score` plus the per-retriever
//! `cosine` and `bm25` components.

use std::collections::HashMap;

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

use crate::config::Config;
use crate::db::Db;
use crate::embed::Embedder;
use crate::index;
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
    /// Primary ranking score for the chosen mode (RRF for hybrid, cosine for vec, BM25 for bm25).
    pub score: f32,
    /// RRF fused score (hybrid mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrf: Option<f32>,
    /// Cosine similarity from the vector retriever, when computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cosine: Option<f32>,
    /// Tantivy BM25 score from the lexical retriever, when computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25: Option<f32>,
    pub snippet: String,
}

/// Ranked id + component scores, before joining SQLite for the display fields.
struct Scored {
    id: String,
    score: f32,
    rrf: Option<f32>,
    cosine: Option<f32>,
    bm25: Option<f32>,
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

/// Resolve ranked `Scored` into displayable hits (joining SQLite for path/heading/body).
fn hydrate(db: &Db, ranked: Vec<Scored>) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for s in ranked {
        if let Some((path, heading, body)) = db.chunk_row(&s.id)? {
            hits.push(Hit {
                chunk_id: s.id,
                path,
                heading,
                score: s.score,
                rrf: s.rrf,
                cosine: s.cosine,
                bm25: s.bm25,
                snippet: snippet(&body, 200),
            });
        }
    }
    Ok(hits)
}

/// Reusable: returns ranked hits (used by `run` and by filing `--suggest`).
pub fn query(cfg: &Config, q: &str, mode: Mode, limit: usize) -> Result<Vec<Hit>> {
    let db = Db::open(&cfg.db_path())?;
    let ranked: Vec<Scored> = match mode {
        Mode::Bm25 => {
            let lex = Lex::open(&cfg.tantivy_dir())?;
            lex.search(q, limit)?
                .into_iter()
                .map(|(id, bm25)| Scored {
                    id,
                    score: bm25,
                    rrf: None,
                    cosine: None,
                    bm25: Some(bm25),
                })
                .collect()
        }
        Mode::Vec => vec_search(cfg, &db, q, limit)?
            .into_iter()
            .map(|(id, cosine)| Scored {
                id,
                score: cosine,
                rrf: None,
                cosine: Some(cosine),
                bm25: None,
            })
            .collect(),
        Mode::Hybrid => {
            // Pull a deeper candidate set from each retriever, then fuse — keeping each retriever's
            // raw score so the fused hit can report its cosine + BM25 components.
            let cand = (limit * 3).max(30);
            let lex = Lex::open(&cfg.tantivy_dir())?;
            let bm = lex.search(q, cand)?; // (id, bm25), BM25 rank order
            let ve = vec_search(cfg, &db, q, cand)?; // (id, cosine), cosine rank order
            let bm25_of: HashMap<&str, f32> = bm.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            let cos_of: HashMap<&str, f32> = ve.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            let bm_ids: Vec<String> = bm.iter().map(|(id, _)| id.clone()).collect();
            let ve_ids: Vec<String> = ve.iter().map(|(id, _)| id.clone()).collect();
            rrf(&[bm_ids, ve_ids], limit)
                .into_iter()
                .map(|(id, r)| Scored {
                    cosine: cos_of.get(id.as_str()).copied(),
                    bm25: bm25_of.get(id.as_str()).copied(),
                    rrf: Some(r),
                    score: r,
                    id,
                })
                .collect()
        }
    };
    hydrate(&db, ranked)
}

pub fn run(
    cfg: &Config,
    q: &str,
    mode: Mode,
    json: bool,
    limit: usize,
    no_index: bool,
) -> Result<()> {
    // Keep results fresh: an incremental refresh before searching so a just-edited or just-dropped
    // note is findable. Cheap when nothing changed (mtime fast-path; the model only loads if a file
    // actually changed). `--no-index` skips it.
    if !no_index && let Err(e) = index::run(cfg, false) {
        eprintln!("vagus: index refresh skipped ({e})");
    }
    let hits = query(cfg, q, mode, limit)?;
    emit(&hits, json);
    Ok(())
}

fn emit(hits: &[Hit], json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(hits).unwrap_or_else(|_| "[]".into())
        );
        return;
    }
    if hits.is_empty() {
        println!("(no results)");
        return;
    }
    // Relevance relative to the top hit — the raw RRF/cosine scalar isn't human-meaningful.
    let top = hits
        .first()
        .map(|h| h.score)
        .unwrap_or(1.0)
        .max(f32::EPSILON);
    for (i, h) in hits.iter().enumerate() {
        let rel = (100.0 * h.score / top).round().clamp(0.0, 100.0) as i32;
        let loc = if h.heading.is_empty() {
            h.path.clone()
        } else {
            format!("{} › {}", h.path, h.heading)
        };
        println!("{:>2}. {rel:>3}%  {loc}", i + 1);
        println!("    {}", h.snippet);
    }
}
