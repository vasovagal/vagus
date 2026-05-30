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
use crate::scope::Scope;

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
pub fn query(
    cfg: &Config,
    q: &str,
    mode: Mode,
    limit: usize,
    scope: &Scope,
) -> Result<(Vec<Hit>, usize)> {
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
    let hits = hydrate(&db, ranked)?;
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
    let (hits, elided) = query(cfg, q, mode, limit, &scope)?;
    emit(&hits, json, verbose);
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

fn emit(hits: &[Hit], json: bool, verbose: bool) {
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
    let rel = |s: f32| (100.0 * s / top).round().clamp(0.0, 100.0) as i32;

    if verbose {
        // Pre-compaction layout: full path, full breadcrumb, full snippet, no truncation.
        for (i, h) in hits.iter().enumerate() {
            let loc = if h.heading.is_empty() {
                h.path.clone()
            } else {
                format!("{} › {}", h.path, h.heading)
            };
            println!("{:>2}. {:>3}%  {loc}", i + 1, rel(h.score));
            println!("    {}", h.snippet);
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
            let prefix = format!("  {:>3}%  ", rel(h.score));
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
            snippet: String::new(),
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
}
