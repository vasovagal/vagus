//! Search entry point: BM25 (lexical), vector (semantic), and hybrid (RRF k=60).
//!
//! Human output shows a 0–100 relevance **relative to the top hit** — the raw RRF scalar is
//! rank-based and tiny (≤ 2/(k+1) ≈ 0.033), so printing it directly is misleading. `--json` keeps a
//! stable shape for the Claude Code skill and carries the raw fused `score` plus the per-retriever
//! `cosine` and `bm25` components.

use std::collections::HashMap;
use std::io::IsTerminal;

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

use crate::config::Config;
use crate::db::Db;
use crate::embed::Embedder;
use crate::index;
use crate::lex::Lex;
use crate::rerank::{Reranker, sigmoid};
use crate::scope::Scope;

/// RRF constant (guardrail G8).
const RRF_K: f32 = 60.0;

/// Minimum candidate pool the cross-encoder reranks (the deeper fused set, before truncating to the
/// requested `limit`). Scales with `limit` but never drops below this.
const RERANK_POOL_MIN: usize = 30;

#[derive(Clone, Copy, ValueEnum)]
pub enum Mode {
    /// BM25 + semantic, fused with RRF.
    Hybrid,
    /// Full-text (BM25) only.
    Bm25,
    /// Semantic (embeddings) only.
    Vec,
}

#[derive(Serialize, Clone)]
pub struct Hit {
    pub chunk_id: String,
    pub path: String,
    pub heading: String,
    /// Primary ranking score for the chosen mode (RRF for hybrid, cosine for vec, BM25 for bm25).
    /// When `--rerank` is on, this is the cross-encoder score (sigmoid of the raw logit).
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
    /// Raw cross-encoder rerank logit, when `--rerank` reordered this hit (ordering signal only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<f32>,
    pub snippet: String,
    /// Full chunk body, only when `--full` is requested (skill path); omitted otherwise so the
    /// default `--json` shape stays byte-identical (G9a).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
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

/// Relevance of a hit relative to the top hit, as a 0–100 integer. The raw RRF/cosine scalar isn't
/// human-meaningful, and it's also the basis for the `--min-score` floor. Shared by `emit` and `run`.
fn rel(score: f32, top: f32) -> i32 {
    (100.0 * score / top.max(f32::EPSILON))
        .round()
        .clamp(0.0, 100.0) as i32
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Cosine top-k of a (normalized) query vector against the preloaded (normalized) matrix. Shared by
/// the single-query path and the `--smart` multi-query path (which preloads the matrix once).
fn cosine_topk(qv: &[f32], all: &[(String, Vec<f32>)], limit: usize) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = all
        .iter()
        .map(|(id, v)| (id.clone(), dot(qv, v))) // both normalized -> cosine
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    scored
}

/// Brute-force exact cosine over the in-RAM normalized matrix. Returns (chunk_id, cosine) top-k.
fn vec_search(cfg: &Config, db: &Db, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
    let mut emb = Embedder::new(&cfg.cache_dir)?;
    let qv = emb.embed_query(query)?; // normalized
    Ok(cosine_topk(&qv, &db.all_embeddings()?, limit))
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

/// Resolve ranked `Scored` into displayable hits (joining SQLite for path/heading/body). `keep_body`
/// retains the full chunk body on the hit (for `--full` output and for cross-encoder reranking).
fn hydrate(db: &Db, ranked: Vec<Scored>, keep_body: bool) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for s in ranked {
        if let Some((path, heading, body)) = db.chunk_row(&s.id)? {
            let snippet = snippet(&body, 200);
            hits.push(Hit {
                chunk_id: s.id,
                path,
                heading,
                score: s.score,
                rrf: s.rrf,
                cosine: s.cosine,
                bm25: s.bm25,
                rerank: None,
                snippet,
                body: keep_body.then_some(body),
            });
        }
    }
    Ok(hits)
}

/// Reusable: returns ranked hits (used by `run` and by filing `--suggest`). `full` retains the chunk
/// body on each hit; `rerank` re-scores a deeper candidate pool with the cross-encoder (tier-1).
pub fn query(
    cfg: &Config,
    q: &str,
    mode: Mode,
    limit: usize,
    scope: &Scope,
    full: bool,
    rerank: bool,
) -> Result<(Vec<Hit>, usize)> {
    let db = Db::open(&cfg.db_path())?;
    // When reranking, retrieve a deeper pool so the cross-encoder has real candidates to reorder
    // before we truncate to `limit`; otherwise retrieve exactly `limit` (today's behavior).
    let pool = if rerank {
        (limit * 4).max(RERANK_POOL_MIN)
    } else {
        limit
    };
    let ranked: Vec<Scored> = match mode {
        Mode::Bm25 => {
            let lex = Lex::open(&cfg.tantivy_dir())?;
            lex.search(q, pool)?
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
        Mode::Vec => vec_search(cfg, &db, q, pool)?
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
            let cand = (pool * 3).max(30);
            let lex = Lex::open(&cfg.tantivy_dir())?;
            let bm = lex.search(q, cand)?; // (id, bm25), BM25 rank order
            let ve = vec_search(cfg, &db, q, cand)?; // (id, cosine), cosine rank order
            let bm25_of: HashMap<&str, f32> = bm.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            let cos_of: HashMap<&str, f32> = ve.iter().map(|(id, s)| (id.as_str(), *s)).collect();
            let bm_ids: Vec<String> = bm.iter().map(|(id, _)| id.clone()).collect();
            let ve_ids: Vec<String> = ve.iter().map(|(id, _)| id.clone()).collect();
            rrf(&[bm_ids, ve_ids], pool)
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
    // Bodies are needed for `--full` output and (transiently) to feed the cross-encoder.
    let keep_body = full || rerank;
    let mut hits = hydrate(&db, ranked, keep_body)?;

    // Tier-1 rerank: re-score the fused pool against full bodies, then reorder (RRF — G8 — untouched).
    if rerank && !hits.is_empty() {
        let mut rr = Reranker::new(&cfg.cache_dir)?;
        let docs: Vec<String> = hits
            .iter()
            .map(|h| h.body.clone().unwrap_or_default())
            .collect();
        let order = rr.rerank(q, &docs)?; // (index, raw_logit), best-first
        let mut reordered = Vec::with_capacity(order.len());
        for (idx, score) in order {
            let mut h = hits[idx].clone();
            h.rerank = Some(score);
            h.score = sigmoid(score); // display-/floor-friendly primary score for the rerank mode
            reordered.push(h);
        }
        hits = reordered;
    }

    // The rerank path pulled a deeper pool — truncate to the requested limit, then drop any body we
    // only kept transiently for reranking so the default `--json` shape stays byte-identical (G9a).
    hits.truncate(limit);
    if !full {
        for h in &mut hits {
            h.body = None;
        }
    }
    Ok(apply_scope(hits, scope))
}

/// Tier-1 "smart" retrieval (ADR 0016, G19): a local model expands the query into typed lex/vec/hyde
/// variants; each (plus the original, as both BM25 and vector) is retrieved, all lists are RRF-fused
/// (k=60, unchanged — G8), and the fused pool is reranked against the *original* query on full bodies.
/// Offline, no Claude — the local sibling of the Opus `/search` skill.
#[cfg(feature = "generate")]
fn smart_query(
    cfg: &Config,
    q: &str,
    limit: usize,
    scope: &Scope,
    full: bool,
) -> Result<(Vec<Hit>, usize)> {
    use crate::rewrite::{Kind, Rewriter};

    let pool = (limit * 4).max(RERANK_POOL_MIN);
    let db = Db::open(&cfg.db_path())?;

    // 1) Expand, then drop the LLM to free RAM before the embedder/reranker load.
    let variants = {
        let mut rw = Rewriter::new(&cfg.cache_dir)?;
        rw.expand(q)?
    };

    // 2) One ranked id-list per plan: the original as BM25 + vector, each lex variant via BM25, each
    //    vec/hyde variant via vector. Load the embedder + the vector matrix once (lazily).
    let mut plans: Vec<(bool, &str)> = vec![(false, q), (true, q)];
    for v in &variants {
        plans.push((!matches!(v.kind, Kind::Lex), v.text.as_str()));
    }

    let lex = Lex::open(&cfg.tantivy_dir())?;
    let mut emb: Option<Embedder> = None;
    let mut matrix: Vec<(String, Vec<f32>)> = Vec::new();
    let mut lists: Vec<Vec<String>> = Vec::new();
    for (is_vec, text) in plans {
        if is_vec {
            if emb.is_none() {
                emb = Some(Embedder::new(&cfg.cache_dir)?);
                matrix = db.all_embeddings()?;
            }
            let qv = emb.as_mut().unwrap().embed_query(text)?;
            lists.push(
                cosine_topk(&qv, &matrix, pool)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect(),
            );
        } else {
            lists.push(
                lex.search(text, pool)?
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect(),
            );
        }
    }

    // 3) Fuse all lists, hydrate with bodies.
    let ranked: Vec<Scored> = rrf(&lists, pool)
        .into_iter()
        .map(|(id, r)| Scored {
            id,
            score: r,
            rrf: Some(r),
            cosine: None,
            bm25: None,
        })
        .collect();
    let mut hits = hydrate(&db, ranked, true)?;

    // 4) Rerank against the ORIGINAL query on full bodies, then reorder.
    if !hits.is_empty() {
        let mut rr = Reranker::new(&cfg.cache_dir)?;
        let docs: Vec<String> = hits
            .iter()
            .map(|h| h.body.clone().unwrap_or_default())
            .collect();
        for (idx, score) in rr.rerank(q, &docs)? {
            hits[idx].rerank = Some(score);
            hits[idx].score = sigmoid(score);
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    }

    hits.truncate(limit);
    if !full {
        for h in &mut hits {
            h.body = None;
        }
    }
    Ok(apply_scope(hits, scope))
}

/// Drop hits whose path matches the active scope, returning the kept hits and the number elided.
/// "Remove + notice" semantics: filters the already-ranked top results in place (no backfill).
fn apply_scope(hits: Vec<Hit>, scope: &Scope) -> (Vec<Hit>, usize) {
    if scope.is_empty() {
        return (hits, 0);
    }
    let before = hits.len();
    let kept: Vec<Hit> = hits
        .into_iter()
        .filter(|h| !scope.is_excluded(&h.path))
        .collect();
    let elided = before - kept.len();
    (kept, elided)
}

/// Dispatch the retrieval: tier-1 `--smart` (local expand → multi-query fuse → rerank) when the
/// `generate` feature is built and requested, else the plain (optionally `--rerank`ed) query. A smart
/// run that can't load its model degrades to `--rerank` with a warning.
#[allow(clippy::too_many_arguments)]
fn run_query(
    cfg: &Config,
    q: &str,
    mode: Mode,
    limit: usize,
    scope: &Scope,
    full: bool,
    rerank: bool,
    smart: bool,
) -> Result<(Vec<Hit>, usize)> {
    #[cfg(feature = "generate")]
    if smart {
        match smart_query(cfg, q, limit, scope, full) {
            Ok(r) => return Ok(r),
            Err(e) => {
                eprintln!("vagus: local rewriter unavailable ({e}); falling back to --rerank")
            }
        }
    }
    #[cfg(not(feature = "generate"))]
    if smart {
        eprintln!(
            "vagus: built without the local rewriter (`generate` feature); --smart falls back to --rerank"
        );
    }
    query(cfg, q, mode, limit, scope, full, rerank || smart)
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    cfg: &Config,
    q: &str,
    mode: Mode,
    json: bool,
    limit: usize,
    no_index: bool,
    verbose: bool,
    all: bool,
    full: bool,
    rerank: bool,
    min_score: Option<f32>,
    smart: bool,
) -> Result<()> {
    // Keep results fresh: an incremental refresh before searching so a just-edited or just-dropped
    // note is findable. Cheap when nothing changed (mtime fast-path; the model only loads if a file
    // actually changed). `--no-index` skips it.
    if !no_index && let Err(e) = index::run(cfg, false) {
        eprintln!("vagus: index refresh skipped ({e})");
    }
    // Discover directory-scoped exclusions by walking up from the CWD, unless `--all` bypasses scoping.
    let scope = if all {
        Scope::none()
    } else {
        Scope::discover()?
    };
    let (mut hits, elided) = run_query(cfg, q, mode, limit, &scope, full, rerank, smart)?;
    // Quality floor: drop hits below `min_score`% of the top hit (relative-to-top, so its feel is
    // mode-dependent). Default `None` keeps every ranked hit (today's behavior).
    if let Some(floor) = min_score {
        let top = hits.first().map(|h| h.score).unwrap_or(1.0);
        hits.retain(|h| rel(h.score, top) as f32 >= floor);
    }
    emit(&hits, json, verbose, full);
    if elided > 0 {
        let msg = format!("{elided} hit(s) elided by inherited config (--all to show)");
        if json {
            // `--json` stdout stays a pure array of Hit; the notice goes to stderr.
            eprintln!("vagus: {msg}");
        } else {
            // Trailing in-results line, dimmed with the same NO_COLOR/TTY gate emit() uses (Style).
            let st = Style::detect();
            println!("{}", st.dim(&format!("— {msg}")));
            // Under --verbose, name the inherited config that did the eliding.
            if verbose && let Some(src) = scope.source.as_deref() {
                println!("{}", st.dim(&format!("  (scope: {})", src.display())));
            }
        }
    }
    Ok(())
}

// --- human-readable rendering ------------------------------------------------------------------

/// ANSI styling, gated once: color only on a real TTY with NO_COLOR unset (https://no-color.org),
/// so piped output (and the `--json` skill path) stays plain.
struct Style {
    on: bool,
}
impl Style {
    fn detect() -> Self {
        Self {
            on: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }
    fn dim(&self, s: &str) -> String {
        if self.on {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn bold(&self, s: &str) -> String {
        if self.on {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

/// Display width for human output: real TTY columns, then `$COLUMNS`, then 100. Clamped so neither a
/// narrow nor an ultrawide terminal produces silly line lengths.
fn term_width() -> usize {
    if let Some((terminal_size::Width(w), _)) = terminal_size::terminal_size() {
        return (w as usize).clamp(40, 140);
    }
    if let Ok(n) = std::env::var("COLUMNS")
        .unwrap_or_default()
        .parse::<usize>()
    {
        return n.clamp(40, 140);
    }
    100
}

/// Top-level PARA bucket of a vault-relative path (e.g. "10-Projects"); "" if none.
fn para_bucket(path: &str) -> &str {
    path.split('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("")
}

/// Strip a leading `YYYYMMDD-HHMMSS-` stamp (8 digits, '-', 6 digits, '-') if present.
fn strip_timestamp(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() > 16
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'-'
    {
        &s[16..]
    } else {
        s
    }
}

/// Short display title from a vault path: basename minus `.md` and any leading timestamp stamp.
fn short_title(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let base = base.strip_suffix(".md").unwrap_or(base);
    strip_timestamp(base).to_string()
}

/// Last segment of a `" > "`-joined heading breadcrumb (the deepest, most specific heading).
fn leaf_heading(heading_path: &str) -> &str {
    heading_path
        .rsplit(" > ")
        .next()
        .unwrap_or(heading_path)
        .trim()
}

/// Truncate to at most `w` display columns (char count), adding '…' when cut.
fn truncate_cols(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    if s.chars().count() <= w {
        return s.to_string();
    }
    let cut: String = s.chars().take(w.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Max hits shown per note before collapsing to a "+N more" line.
const PER_FILE_CAP: usize = 3;

fn emit(hits: &[Hit], json: bool, verbose: bool, full: bool) {
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
    let top = hits.first().map(|h| h.score).unwrap_or(1.0);

    if verbose || full {
        // Pre-compaction layout: full path, full breadcrumb, no width truncation. With `--full`,
        // print the entire chunk body; otherwise the (≤200-char) snippet.
        for (i, h) in hits.iter().enumerate() {
            let loc = if h.heading.is_empty() {
                h.path.clone()
            } else {
                format!("{} › {}", h.path, h.heading)
            };
            println!("{:>2}. {:>3}%  {loc}", i + 1, rel(h.score, top));
            let text = if full {
                h.body.as_deref().unwrap_or(&h.snippet)
            } else {
                &h.snippet
            };
            println!("    {text}");
        }
        return;
    }

    let st = Style::detect();
    let width = term_width();

    // Group hits by note, preserving best-rank order. RRF interleaves chunks from different notes,
    // so a note's chunks are NOT contiguous in the ranked list — group explicitly, ordering each
    // note by its best (first-seen) hit.
    let mut order: Vec<&str> = Vec::new();
    let mut groups: HashMap<&str, Vec<&Hit>> = HashMap::new();
    for h in hits {
        groups
            .entry(h.path.as_str())
            .or_insert_with(|| {
                order.push(h.path.as_str());
                Vec::new()
            })
            .push(h);
    }

    for path in order {
        let group = &groups[path];

        // Header (once per note): "▸ <title>  ·  <bucket>", title bold, marker+bucket dim.
        let title = short_title(path);
        let bucket = para_bucket(path);
        let sep = "  ·  ";
        let reserved = 2 + if bucket.is_empty() {
            0
        } else {
            sep.chars().count() + bucket.chars().count()
        };
        let title = truncate_cols(&title, width.saturating_sub(reserved));
        if bucket.is_empty() {
            println!("{} {}", st.dim("▸"), st.bold(&title));
        } else {
            println!(
                "{} {}{}",
                st.dim("▸"),
                st.bold(&title),
                st.dim(&format!("{sep}{bucket}"))
            );
        }

        // Hit lines: "  <rel>%  <leaf>  — <snippet>", whole line hard-truncated to one terminal row.
        for h in group.iter().take(PER_FILE_CAP) {
            let prefix = format!("  {:>3}%  ", rel(h.score, top));
            let leaf = leaf_heading(&h.heading);
            let body = if leaf.is_empty() {
                h.snippet.clone()
            } else {
                format!("{leaf}  — {}", h.snippet)
            };
            let body = truncate_cols(&body, width.saturating_sub(prefix.chars().count()));
            // Bold the leaf heading if it survived truncation intact.
            let body = if !leaf.is_empty() && body.starts_with(leaf) {
                format!("{}{}", st.bold(leaf), &body[leaf.len()..])
            } else {
                body
            };
            println!("{}{}", st.dim(&prefix), body);
        }
        let more = group.len().saturating_sub(PER_FILE_CAP);
        if more > 0 {
            println!("{}", st.dim(&format!("    …   +{more} more in this note")));
        }
    }
}

#[cfg(test)]
mod scope_filter_tests {
    use super::*;
    use crate::scope::Scope;

    fn hit(path: &str) -> Hit {
        Hit {
            chunk_id: format!("id:{path}"),
            path: path.to_string(),
            heading: String::new(),
            score: 0.0,
            rrf: None,
            cosine: None,
            bm25: None,
            rerank: None,
            snippet: String::new(),
            body: None,
        }
    }

    #[test]
    fn removes_excluded_and_counts() {
        let hits = vec![
            hit("10-Projects/scientist/a.md"),
            hit("10-Projects/viasat/b.md"),
            hit("10-Projects/scientist/c.md"),
            hit("30-Resources/rust/d.md"),
        ];
        let scope = Scope::from_words(["scientist".to_string()], None);
        let (kept, elided) = apply_scope(hits, &scope);
        assert_eq!(elided, 2);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|h| !h.path.contains("scientist")));
    }

    #[test]
    fn none_is_passthrough() {
        let hits = vec![hit("10-Projects/scientist/a.md"), hit("x/b.md")];
        let (kept, elided) = apply_scope(hits, &Scope::none());
        assert_eq!(elided, 0);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn default_json_shape_omits_rerank_and_body() {
        // The optional `rerank`/`body` fields must not appear when unset, so the default `--json`
        // shape the skill parses stays byte-identical (G9a).
        let h = hit("30-Resources/rust/d.md");
        let j = serde_json::to_string(&h).unwrap();
        assert!(
            !j.contains("\"rerank\""),
            "rerank leaked into default JSON: {j}"
        );
        assert!(
            !j.contains("\"body\""),
            "body leaked into default JSON: {j}"
        );
        // …but they serialize when populated (the `--rerank` / `--full` paths).
        let mut h2 = hit("30-Resources/rust/d.md");
        h2.rerank = Some(1.5);
        h2.body = Some("full text".into());
        let j2 = serde_json::to_string(&h2).unwrap();
        assert!(j2.contains("\"rerank\":1.5"));
        assert!(j2.contains("\"body\":\"full text\""));
    }

    #[test]
    fn rel_is_relative_to_top() {
        assert_eq!(rel(1.0, 1.0), 100);
        assert_eq!(rel(0.5, 1.0), 50);
        assert_eq!(rel(2.0, 1.0), 100); // clamped
        assert_eq!(rel(1.0, 0.0), 100); // top==0 guarded, doesn't divide-by-zero
    }

    #[test]
    fn sigmoid_is_monotonic_in_unit_interval() {
        assert!(sigmoid(0.0) > 0.49 && sigmoid(0.0) < 0.51);
        assert!(sigmoid(5.0) > sigmoid(-5.0));
        assert!((0.0..=1.0).contains(&sigmoid(10.0)));
    }
}
