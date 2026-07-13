//! Search entry point: BM25 (lexical), vector (semantic), and hybrid (RRF k=60).
//!
//! Human output shows a 0–100 relevance **relative to the top hit** — the raw RRF scalar is
//! rank-based and tiny (≤ 2/(k+1) ≈ 0.033), so printing it directly is misleading. `--json` keeps a
//! stable shape for the Claude Code skill and carries the raw fused `score` plus the per-retriever
//! `cosine` and `bm25` components.

use std::collections::HashMap;
use std::io::IsTerminal;
// Only the `--smart` path (smart_query) prewarms models on threads, so this is generate-gated to keep
// the lean (`--no-default-features`) build warning-free.
#[cfg(feature = "generate")]
use std::thread::JoinHandle;
use std::time::Instant;

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

/// Cap on how many top RRF-ordered candidates the cross-encoder actually *scores*, as a multiple of
/// `limit` (floored at 16). The forward pass dominates `--rerank` wall time, and note-dedup/truncate
/// keep only `limit` notes, so scoring the whole `pool` is wasteful. Retrieval/filter/dedup still run
/// at full `pool` depth; only the reranked prefix is capped — lower-ranked hits keep their RRF order
/// after it (ADR 0015).
const RERANK_CAP_PER_LIMIT: usize = 2;

/// How many top RRF candidates the cross-encoder actually scores, clamped to the pool. Normally
/// `(limit*RERANK_CAP_PER_LIMIT).max(16)`; when a `--min-score` floor is active it lifts to the whole
/// pool. The floor is relative-to-top and the reranked head carries sigmoid scores (~0–1) while an
/// un-scored tail keeps raw RRF scores (~0.01) — comparing the two would floor the whole tail out and
/// silently drop tail-filled slots the full-pool rerank would have kept, so a floor disables the cap.
fn rerank_cap(limit: usize, pool_len: usize, score_floor: bool) -> usize {
    if score_floor {
        pool_len
    } else {
        (limit * RERANK_CAP_PER_LIMIT).max(16).min(pool_len)
    }
}

/// Per-stage wall-clock timings (ms) for an advanced retrieval run, printed to stderr by `--timings`.
/// A diagnostic + regression guard (mirrors `IndexTimings`); it never touches stdout or the `--json`
/// Hit shape (G9a). A field stays 0.0 for any stage the chosen mode/path didn't run, and `print`
/// elides zero rows. Crucially this separates each model's **load** from its **compute** — the load
/// rows fall to ~0 once Fix B prewarms them off the critical path.
#[derive(Default)]
struct SmartTimings {
    rewrite_load_ms: f64,
    rewrite_decode_ms: f64,
    embed_load_ms: f64,
    retrieval_ms: f64,
    fuse_ms: f64,
    rerank_load_ms: f64,
    rerank_ms: f64,
    total_ms: f64,
}

impl SmartTimings {
    fn print(&self, label: &str) {
        eprintln!("vagus --timings ({label}):");
        let rows: [(&str, f64); 7] = [
            ("rewrite_load", self.rewrite_load_ms),
            ("rewrite_decode", self.rewrite_decode_ms),
            ("embed_load", self.embed_load_ms),
            ("retrieval", self.retrieval_ms),
            ("fuse", self.fuse_ms),
            ("rerank_load", self.rerank_load_ms),
            ("rerank", self.rerank_ms),
        ];
        for (name, ms) in rows {
            if ms > 0.0 {
                eprintln!("  {name:<15} {ms:>9.1} ms");
            }
        }
        eprintln!("  {:<15} {:>9.1} ms", "total", self.total_ms);
    }
}

/// Milliseconds elapsed since `t`, as `f64` (mirrors `index::elapsed_ms`).
fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Parse a `--since` duration into seconds (dependency-free, ADR 0017). Accepts a single
/// number+unit token — `30s`, `90m`, `6h`, `10d`, `2w` — or a bare integer interpreted as **days**
/// (`7` == `7d`). Whitespace is trimmed; the unit is case-insensitive. The caller derives the cutoff
/// as `now - parse_duration(..)`. Returns a clear error on anything else (empty, negative, unknown
/// unit, non-numeric, overflow).
pub fn parse_duration(input: &str) -> Result<i64> {
    let s = input.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration (use e.g. 10d, 2w, 6h, 30m, 90s, or a bare number of days)");
    }
    let (num_str, unit_secs): (&str, i64) = match s.chars().last().unwrap() {
        c if c.is_ascii_digit() => (s, 86_400), // bare number -> days
        's' | 'S' => (&s[..s.len() - 1], 1),
        'm' | 'M' => (&s[..s.len() - 1], 60),
        'h' | 'H' => (&s[..s.len() - 1], 3_600),
        'd' | 'D' => (&s[..s.len() - 1], 86_400),
        'w' | 'W' => (&s[..s.len() - 1], 604_800),
        other => anyhow::bail!(
            "invalid duration unit {other:?} in {s:?} (use s, m, h, d, w, or a bare number of days)"
        ),
    };
    // Parse the numeric part as-is (no inner trim) so embedded whitespace like "10 d" is rejected.
    let n: i64 = num_str.parse().map_err(|_| {
        anyhow::anyhow!("invalid duration {s:?} (expected e.g. 10d, 2w, 6h, 30m, 90s, or a number)")
    })?;
    if n < 0 {
        anyhow::bail!("duration must not be negative: {s:?}");
    }
    n.checked_mul(unit_secs)
        .ok_or_else(|| anyhow::anyhow!("duration too large: {s:?}"))
}

/// Compute the `--since` cutoff in unix seconds: `now - parse_duration(spec)`.
pub fn since_cutoff(spec: &str) -> Result<i64> {
    Ok(crate::util::now_unix() - parse_duration(spec)?)
}

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
    /// Note `created` as unix secs (frontmatter, or file-mtime fallback — G3). Additive optional
    /// field (ADR 0017): `skip_serializing_if = None` so the default `--json` Hit shape stays
    /// byte-identical for pre-v4 rows / callers that don't filter (G9a/G13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    /// Note `source` frontmatter. Additive optional field (ADR 0017); omitted from `--json` when
    /// absent (G9a/G13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Additional ranked chunks from the same note folded into this hit by note-level dedup —
    /// the default mode, where `--limit` counts distinct notes (ADR 0020). Additive optional field:
    /// never set under `--chunks` (and omitted when zero), so chunk-mode `--json` stays
    /// byte-identical (G9a/G13).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub siblings: Option<usize>,
}

/// Ranked id + component scores, before joining SQLite for the display fields.
struct Scored {
    id: String,
    score: f32,
    rrf: Option<f32>,
    cosine: Option<f32>,
    bm25: Option<f32>,
}

/// One `vagus chunk` output element. A stable `--json` contract, additive-only: found elements
/// serialize exactly `chunk_id`/`path`/`heading`/`body` (the `chunk_id` is always the full 64-hex
/// id, even for prefix input); an unresolved arg yields a positional `missing: true` element, so a
/// caller detects staleness deterministically without parsing stderr.
#[derive(Serialize)]
struct ChunkOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    /// The " > "-joined heading_path breadcrumb.
    #[serde(skip_serializing_if = "Option::is_none")]
    heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    /// Serialized only when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    missing: bool,
}

impl ChunkOut {
    fn found(chunk_id: String, path: String, heading: String, body: String) -> Self {
        Self {
            chunk_id: Some(chunk_id),
            path: Some(path),
            heading: Some(heading),
            body: Some(body),
            missing: false,
        }
    }

    fn missing_id(arg: &str) -> Self {
        Self {
            chunk_id: Some(arg.to_string()),
            path: None,
            heading: None,
            body: None,
            missing: true,
        }
    }

    fn missing_path(arg: &str) -> Self {
        Self {
            chunk_id: None,
            path: Some(arg.to_string()),
            heading: None,
            body: None,
            missing: true,
        }
    }
}

/// Minimum hex-prefix length `vagus chunk` accepts for an id lookup.
const CHUNK_PREFIX_MIN: usize = 8;

/// Resolve `vagus chunk` args against the chunks table, in request order — every arg yields ≥1
/// element. An all-hex arg is an id (full 64-hex exact, or a ≥8-char unique prefix); anything else
/// is a vault-relative note path (every chunk of the note, in `ord` order). Ambiguous/short/unknown
/// args become `missing: true` elements with one stderr line each.
fn resolve_chunk_args(db: &Db, args: &[String]) -> Result<Vec<ChunkOut>> {
    let mut out = Vec::new();
    for arg in args {
        let is_hex = !arg.is_empty() && arg.chars().all(|c| c.is_ascii_hexdigit());
        // Ids are stored lowercase; normalize so the exact (`=`) and prefix (LIKE) branches agree.
        let arg = &if is_hex {
            arg.to_ascii_lowercase()
        } else {
            arg.clone()
        };
        if is_hex && arg.len() == 64 {
            match db.chunk_row(arg)? {
                Some((path, heading, body)) => {
                    out.push(ChunkOut::found(arg.clone(), path, heading, body))
                }
                None => {
                    eprintln!(
                        "vagus chunk: no chunk matching {arg} (note edited/renamed? re-run search or Read the note)"
                    );
                    out.push(ChunkOut::missing_id(arg));
                }
            }
        } else if is_hex && arg.len() >= CHUNK_PREFIX_MIN {
            let mut rows = db.chunk_rows_by_prefix(arg)?;
            match rows.len() {
                1 => {
                    let (id, path, heading, body) = rows.remove(0);
                    out.push(ChunkOut::found(id, path, heading, body));
                }
                0 => {
                    eprintln!(
                        "vagus chunk: no chunk matching {arg} (note edited/renamed? re-run search or Read the note)"
                    );
                    out.push(ChunkOut::missing_id(arg));
                }
                _ => {
                    eprintln!("vagus chunk: ambiguous prefix {arg}");
                    out.push(ChunkOut::missing_id(arg));
                }
            }
        } else if is_hex {
            eprintln!("vagus chunk: prefix too short (min {CHUNK_PREFIX_MIN} hex chars): {arg}");
            out.push(ChunkOut::missing_id(arg));
        } else {
            let rows = db.chunks_for_path(arg)?;
            if rows.is_empty() {
                eprintln!(
                    "vagus chunk: no chunk matching {arg} (note edited/renamed? re-run search or Read the note)"
                );
                out.push(ChunkOut::missing_path(arg));
            } else {
                for (id, heading, body) in rows {
                    out.push(ChunkOut::found(id, arg.clone(), heading, body));
                }
            }
        }
    }
    Ok(out)
}

/// `vagus chunk`: print full chunk bodies by id or note path — the /search skill's second pass.
/// Pure derived-cache read (G2); no index refresh by design (keeps the search→fetch id-drift window
/// milliseconds wide); never ticks (retrieval is not usage — ADR 0021).
pub fn chunk_bodies(cfg: &Config, args: &[String], json: bool) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let out = resolve_chunk_args(&db, args)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| "[]".into())
        );
        return Ok(());
    }
    // Human output: `path › heading` header + raw body per resolved chunk, blank-line separated;
    // missing args already reported on stderr above.
    let mut first = true;
    for c in out.iter().filter(|c| !c.missing) {
        if !first {
            println!();
        }
        first = false;
        let path = c.path.as_deref().unwrap_or_default();
        let heading = c.heading.as_deref().unwrap_or_default();
        if heading.is_empty() {
            println!("{path}");
        } else {
            println!("{path} › {heading}");
        }
        println!("{}", c.body.as_deref().unwrap_or_default());
    }
    Ok(())
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

/// Resolve a ranked `(vec_key, cosine)` list from a [`crate::vector::VectorIndex`] back to
/// `(chunk_id, cosine)`, preserving rank order and dropping any key with no surviving chunk row
/// (ADR 0019). usearch returns u64 keys; the reverse map lives in the indexed `chunks.vec_key` column.
fn vec_topk(
    vindex: &dyn crate::vector::VectorIndex,
    db: &Db,
    qv: &[f32],
    limit: usize,
) -> Result<Vec<(String, f32)>> {
    let hits = vindex.search(qv, limit)?;
    let keys: Vec<u64> = hits.iter().map(|(k, _)| *k).collect();
    let map = db.chunk_ids_for_keys(&keys)?;
    Ok(hits
        .into_iter()
        .filter_map(|(k, cos)| map.get(&k).map(|id| (id.clone(), cos)))
        .collect())
}

/// Semantic top-k: embed the query, then rank via the vector index — usearch HNSW, or the exact
/// brute-force backend when `exact`, on a small corpus, or before the sidecar exists (ADR 0019).
/// Returns (chunk_id, cosine) in rank order.
fn vec_search(
    cfg: &Config,
    db: &Db,
    query: &str,
    limit: usize,
    exact: bool,
) -> Result<Vec<(String, f32)>> {
    let mut emb = Embedder::new(&cfg.cache_dir)?;
    let qv = emb.embed_query(query)?; // normalized
    let vindex = crate::vector::open_for_search(cfg, db, exact)?;
    vec_topk(vindex.as_ref(), db, &qv, limit)
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
            // Note-level filter fields (`created`/`source`) live in a sibling read so the hot display
            // join stays unchanged; absent rows (pre-v4) default to None (ADR 0017).
            let (created, source) = db.chunk_filter_fields(&s.id)?.unwrap_or((None, None));
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
                created,
                source,
                siblings: None,
            });
        }
    }
    Ok(hits)
}

/// Reusable: returns ranked hits (used by `run` and by filing `--suggest`). `full` retains the chunk
/// body on each hit; `rerank` re-scores a deeper candidate pool with the cross-encoder (tier-1);
/// `chunks` skips note-level dedup, returning raw chunk hits (ADR 0020).
#[allow(clippy::too_many_arguments)]
pub fn query(
    cfg: &Config,
    q: &str,
    mode: Mode,
    limit: usize,
    scope: &Scope,
    full: bool,
    rerank: bool,
    since: Option<i64>,
    source: Option<&str>,
    exact: bool,
    timings: bool,
    chunks: bool,
    score_floor: bool,
) -> Result<(Vec<Hit>, usize)> {
    let t_total = Instant::now();
    let mut t = SmartTimings::default();
    let db = Db::open(&cfg.db_path())?;
    // Retrieve a deeper pool when reranking (the cross-encoder needs candidates), when filtering
    // (the post-rank `--since`/`--source` stage may drop many top hits, so depth lets it still fill
    // `limit`), or in note mode (dedup compresses chunks → notes — ADR 0020); only `--chunks`
    // without rerank/filters retrieves exactly `limit` (the pre-0.7 behavior).
    let pool = if rerank || since.is_some() || source.is_some() || !chunks {
        (limit * 4).max(RERANK_POOL_MIN)
    } else {
        limit
    };
    let t0 = Instant::now();
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
        Mode::Vec => vec_search(cfg, &db, q, pool, exact)?
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
            let ve = vec_search(cfg, &db, q, cand, exact)?; // (id, cosine), cosine rank order
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
    t.retrieval_ms = ms_since(t0);
    // Bodies are needed for `--full` output and (transiently) to feed the cross-encoder.
    let keep_body = full || rerank;
    let t0 = Instant::now();
    let mut hits = hydrate(&db, ranked, keep_body)?;
    t.fuse_ms = ms_since(t0);

    // Tier-1 rerank: re-score the fused pool against full bodies, then reorder (RRF — G8 — untouched).
    if rerank && !hits.is_empty() {
        let t0 = Instant::now();
        let mut rr = Reranker::new(&cfg.cache_dir)?;
        t.rerank_load_ms = ms_since(t0);
        let cap = rerank_cap(limit, hits.len(), score_floor);
        let docs: Vec<String> = hits[..cap]
            .iter()
            .map(|h| h.body.clone().unwrap_or_default())
            .collect();
        let t0 = Instant::now();
        let order = rr.rerank(q, &docs)?; // (prefix_index, raw_logit), best-first
        hits = apply_rerank_prefix(hits, cap, order);
        t.rerank_ms = ms_since(t0);
    }

    // Post-rank frontmatter filter (ADR 0017): a SEPARATE stage on the already-ranked hits (after
    // fusion and any rerank reordering), exactly like `apply_scope` — it preserves their order and
    // leaves `rrf()` pure (G7/G8). Runs BEFORE truncation so a filtered query still fills `limit`
    // from the deeper pool pulled above.
    let mut hits = apply_filters(hits, since, source);

    // Note-level dedup (ADR 0020): one best-chunk hit per note, so the truncation below makes
    // `--limit` count distinct notes. `--chunks` keeps every ranked chunk.
    if !chunks {
        hits = dedupe_notes(hits);
    }

    // Truncate to the requested limit, then drop any body we only kept transiently for reranking so
    // the default `--json` shape stays byte-identical (G9a).
    hits.truncate(limit);
    if !full {
        for h in &mut hits {
            h.body = None;
        }
    }
    t.total_ms = ms_since(t_total);
    if timings {
        t.print(if rerank { "rerank" } else { "plain" });
    }
    Ok(apply_scope(hits, scope))
}

/// Post-rank `--since`/`--source` filter (ADR 0017). Mirrors `apply_scope`: prunes already-ranked
/// hits in order (no backfill, no reordering), keeping `rrf()` pure (G7/G8). A hit is kept iff:
///   - `since` is None OR its `created` is known and `>= cutoff`, AND
///   - `source` is None OR the hit's `source` equals the requested value (ASCII case-insensitive).
///
/// A hit with a NULL `source` never matches a `--source` request.
fn apply_filters(hits: Vec<Hit>, since: Option<i64>, source: Option<&str>) -> Vec<Hit> {
    hits.into_iter()
        .filter(|h| match since {
            Some(cutoff) => h.created.is_some_and(|c| c >= cutoff),
            None => true,
        })
        .filter(|h| match source {
            Some(want) => h
                .source
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(want)),
            None => true,
        })
        .collect()
}

/// Note-level dedup (ADR 0020): keep each note's best-ranked chunk, fold later same-note chunks
/// into its `siblings` count. Mirrors `apply_filters`/`apply_scope`: a SEPARATE post-rank stage on
/// the already-ranked hits — drop-only, order-preserving, `rrf()` pure (G7/G8). Runs after the
/// frontmatter filters (so a note whose best chunk was filtered is represented by its next
/// surviving chunk) and before truncation, where `--limit` then counts distinct notes. `--chunks`
/// skips this stage entirely.
fn dedupe_notes(hits: Vec<Hit>) -> Vec<Hit> {
    let mut kept: Vec<Hit> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    for h in hits {
        match index_of.get(&h.path) {
            Some(&i) => kept[i].siblings = Some(kept[i].siblings.unwrap_or(0) + 1),
            None => {
                index_of.insert(h.path.clone(), kept.len());
                kept.push(h);
            }
        }
    }
    kept
}

/// Apply a cross-encoder ordering to the top `cap` hits, leaving the rest in RRF order *after* the
/// reranked prefix. Caps the reranker's forward-pass workload (~pool → cap) without changing
/// retrieval/filter/dedup depth: the tail still carries its `pool` candidates for dedup to fill
/// `limit` from (ADR 0015). `order` is `(prefix_index, raw_logit)` best-first, as `Reranker::rerank`
/// returns over the passed `hits[..cap]` docs. Split out so the cap invariant is unit-testable
/// without loading the model. `rrf()` untouched (G8).
fn apply_rerank_prefix(hits: Vec<Hit>, cap: usize, order: Vec<(usize, f32)>) -> Vec<Hit> {
    let cap = cap.min(hits.len());
    // `order` must be a permutation of `hits[..cap]` (one entry per doc); a short return would drop
    // those prefix hits silently (they are neither re-emitted nor recovered from the `hits[cap..]`
    // tail). Fail loudly in debug if a future reranker backend ever drops docs.
    debug_assert_eq!(
        order.len(),
        cap,
        "reranker returned {} of {cap} docs",
        order.len()
    );
    let mut reordered = Vec::with_capacity(hits.len());
    for (idx, score) in order {
        let mut h = hits[idx].clone();
        h.rerank = Some(score);
        h.score = sigmoid(score); // display-/floor-friendly primary score for the rerank mode
        reordered.push(h);
    }
    // The un-reranked tail keeps its existing RRF order and scores.
    reordered.extend_from_slice(&hits[cap..]);
    reordered
}

/// Tier-1 "smart" retrieval (ADR 0016, G19): a local model expands the query into typed lex/vec/hyde
/// variants; each (plus the original, as both BM25 and vector) is retrieved, all lists are RRF-fused
/// (k=60, unchanged — G8), and the fused pool is reranked against the *original* query on full bodies.
/// Offline, no Claude — the local sibling of the Opus `/search` skill.
#[cfg(feature = "generate")]
#[allow(clippy::too_many_arguments)]
fn smart_query(
    cfg: &Config,
    q: &str,
    limit: usize,
    scope: &Scope,
    full: bool,
    since: Option<i64>,
    source: Option<&str>,
    timings: bool,
    chunks: bool,
    score_floor: bool,
) -> Result<(Vec<Hit>, usize)> {
    use crate::rewrite::{Kind, Rewriter, Variant};

    let t_total = Instant::now();
    let mut t = SmartTimings::default();
    let pool = (limit * 4).max(RERANK_POOL_MIN);
    let db = Db::open(&cfg.db_path())?;

    // Fix B: warm the two ONNX models (embedder ~2s, reranker ~0.15s) on background threads so their
    // cold load overlaps the multi-second LLM decode below instead of running serially after it. The
    // rewrite→embed→rerank *data* dependency is unchanged; only the independent *model loads* move off
    // the critical path. NOT a daemon — the threads are joined within this one-shot call and die with
    // the process (G14). Trades a little peak RAM (all three models briefly resident) for latency. On a
    // worker panic we rebuild on the main thread, so run_query's "smart unavailable → --rerank"
    // graceful fallback still holds.
    let mut emb_warm: Option<JoinHandle<Result<Embedder>>> = Some({
        let cache = cfg.cache_dir.clone();
        std::thread::spawn(move || Embedder::new(&cache))
    });
    let rr_warm: JoinHandle<Result<Reranker>> = {
        let cache = cfg.cache_dir.clone();
        std::thread::spawn(move || Reranker::new(&cache))
    };

    // 1) Expand the query into typed variants. The rewriter is deterministic (fixed seed), so consult
    //    the cache first (Fix C); a hit skips the LLM entirely — load + decode — which is the big win
    //    for iterative re-querying. On a miss, run the local LLM (the long pole; the prewarm threads
    //    load meanwhile), then store the result. The Rewriter is dropped after expanding, freeing RAM.
    let cache_key = crate::rewrite::expansion_cache_key(q);
    let cached: Option<Vec<Variant>> = db
        .expansion_cache_get(&cache_key)?
        .and_then(|json| serde_json::from_str(&json).ok());
    let variants = match cached {
        // Cache hit: rewrite_load_ms / rewrite_decode_ms stay 0.0 — no model was touched.
        Some(v) => v,
        None => {
            let t0 = Instant::now();
            let mut rw = Rewriter::new(&cfg.cache_dir)?;
            t.rewrite_load_ms = ms_since(t0);
            let t0 = Instant::now();
            let v = rw.expand(q)?;
            t.rewrite_decode_ms = ms_since(t0);
            // Best-effort cache write; a failure here just means the next run regenerates.
            if let Ok(json) = serde_json::to_string(&v) {
                let _ = db.expansion_cache_put(&cache_key, &json);
            }
            v
        }
    };

    // 2) One ranked id-list per plan: the original as BM25 + vector, each lex variant via BM25, each
    //    vec/hyde variant via vector. Load the embedder + open the vector index once (lazily).
    let mut plans: Vec<(bool, &str)> = vec![(false, q), (true, q)];
    for v in &variants {
        plans.push((!matches!(v.kind, Kind::Lex), v.text.as_str()));
    }

    let lex = Lex::open(&cfg.tantivy_dir())?;
    let mut emb: Option<Embedder> = None;
    let mut vindex: Option<Box<dyn crate::vector::VectorIndex>> = None;
    let mut lists: Vec<Vec<String>> = Vec::new();
    for (is_vec, text) in plans {
        if is_vec {
            if emb.is_none() {
                let t0 = Instant::now();
                // Collect the prewarmed embedder (Fix B); join is instant if the thread finished
                // during the LLM decode. A worker panic falls back to a foreground build.
                let e = match emb_warm.take() {
                    Some(h) => match h.join() {
                        Ok(r) => r?,
                        Err(_) => Embedder::new(&cfg.cache_dir)?,
                    },
                    None => Embedder::new(&cfg.cache_dir)?,
                };
                emb = Some(e);
                // `--smart` is a tier-1 quality path; honor `--exact` only via the plain path. Here the
                // approximate HNSW backend is fine (the rerank stage re-scores the fused pool anyway).
                vindex = Some(crate::vector::open_for_search(cfg, &db, false)?);
                t.embed_load_ms += ms_since(t0);
            }
            let t0 = Instant::now();
            let qv = emb.as_mut().unwrap().embed_query(text)?;
            let vi = vindex.as_deref().expect("vector index opened above");
            lists.push(
                vec_topk(vi, &db, &qv, pool)?
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect(),
            );
            t.retrieval_ms += ms_since(t0);
        } else {
            let t0 = Instant::now();
            lists.push(
                lex.search(text, pool)?
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect(),
            );
            t.retrieval_ms += ms_since(t0);
        }
    }

    // 3) Fuse all lists, hydrate with bodies.
    let t0 = Instant::now();
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
    t.fuse_ms = ms_since(t0);

    // 4) Rerank against the ORIGINAL query on full bodies, then reorder.
    if !hits.is_empty() {
        let t0 = Instant::now();
        // Collect the prewarmed reranker (Fix B); a worker panic falls back to a foreground build.
        let mut rr = match rr_warm.join() {
            Ok(r) => r?,
            Err(_) => Reranker::new(&cfg.cache_dir)?,
        };
        t.rerank_load_ms = ms_since(t0);
        let cap = rerank_cap(limit, hits.len(), score_floor);
        let docs: Vec<String> = hits[..cap]
            .iter()
            .map(|h| h.body.clone().unwrap_or_default())
            .collect();
        let t0 = Instant::now();
        let order = rr.rerank(q, &docs)?; // (prefix_index, raw_logit), best-first
        hits = apply_rerank_prefix(hits, cap, order);
        t.rerank_ms = ms_since(t0);
    }

    // Post-rank `--since`/`--source` filter (ADR 0017) — the same separate stage as the plain path,
    // before truncation so the deeper smart pool can still fill `limit`. RRF/rerank order untouched.
    let mut hits = apply_filters(hits, since, source);
    // Note-level dedup (ADR 0020), same stage order as the plain path (pool is already 4x here).
    if !chunks {
        hits = dedupe_notes(hits);
    }
    hits.truncate(limit);
    if !full {
        for h in &mut hits {
            h.body = None;
        }
    }
    t.total_ms = ms_since(t_total);
    if timings {
        t.print("smart");
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
    since: Option<i64>,
    source: Option<&str>,
    exact: bool,
    timings: bool,
    chunks: bool,
    score_floor: bool,
) -> Result<(Vec<Hit>, usize)> {
    #[cfg(feature = "generate")]
    if smart {
        match smart_query(
            cfg,
            q,
            limit,
            scope,
            full,
            since,
            source,
            timings,
            chunks,
            score_floor,
        ) {
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
    query(
        cfg,
        q,
        mode,
        limit,
        scope,
        full,
        rerank || smart,
        since,
        source,
        exact,
        timings,
        chunks,
        score_floor,
    )
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
    since: Option<&str>,
    source: Option<&str>,
    exact: bool,
    timings: bool,
    chunks: bool,
) -> Result<()> {
    // Parse the `--since` duration up front so a bad spec errors clearly before any indexing/search.
    let since_cut = match since {
        Some(spec) => Some(since_cutoff(spec)?),
        None => None,
    };
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
    // A `--min-score` floor that can actually drop hits lifts the rerank cap (rerank the whole pool)
    // so the tail shares the head's sigmoid scale — otherwise the relative-to-top floor would drop
    // every tail-filled slot (raw RRF score vs sigmoid top). A zero/absent floor drops nothing, so it
    // keeps the fast capped path.
    let score_floor = min_score.is_some_and(|f| f > 0.0);
    let (mut hits, elided) = run_query(
        cfg,
        q,
        mode,
        limit,
        &scope,
        full,
        rerank,
        smart,
        since_cut,
        source,
        exact,
        timings,
        chunks,
        score_floor,
    )?;
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
        // Overflow line, one expression for both modes: chunk mode counts hits beyond the display
        // cap (`siblings` is never set there); note mode counts the chunks dedup folded (ADR 0020).
        let more = group.len().saturating_sub(PER_FILE_CAP)
            + group.iter().map(|h| h.siblings.unwrap_or(0)).sum::<usize>();
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
            created: None,
            source: None,
            siblings: None,
        }
    }

    /// `hit` plus the note-level filter fields the `--since`/`--source` stage reads (ADR 0017).
    fn hit_meta(path: &str, created: Option<i64>, source: Option<&str>) -> Hit {
        Hit {
            created,
            source: source.map(str::to_string),
            ..hit(path)
        }
    }

    /// `hit` with a per-chunk id, for the note-level dedup tests where one path yields several
    /// ranked chunks (ADR 0020).
    fn hit_chunk(path: &str, ord: usize) -> Hit {
        Hit {
            chunk_id: format!("id:{path}#{ord}"),
            ..hit(path)
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
        // `siblings` is never set under `--chunks`, keeping that mode's JSON byte-identical to
        // pre-0.7 output (ADR 0020/G9a).
        assert!(
            !j.contains("\"siblings\""),
            "siblings leaked into default JSON: {j}"
        );
        // …but they serialize when populated (the `--rerank` / `--full` / note-mode paths).
        let mut h2 = hit("30-Resources/rust/d.md");
        h2.rerank = Some(1.5);
        h2.body = Some("full text".into());
        h2.siblings = Some(2);
        let j2 = serde_json::to_string(&h2).unwrap();
        assert!(j2.contains("\"rerank\":1.5"));
        assert!(j2.contains("\"body\":\"full text\""));
        assert!(j2.contains("\"siblings\":2"));
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

    // --- --since / --source filters (ADR 0017) ----------------------------------------------------

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("30m").unwrap(), 30 * 60);
        assert_eq!(parse_duration("6h").unwrap(), 6 * 3600);
        assert_eq!(parse_duration("10d").unwrap(), 10 * 86_400);
        assert_eq!(parse_duration("2w").unwrap(), 2 * 604_800);
        // A bare number means days; the unit is case-insensitive; surrounding whitespace is trimmed.
        assert_eq!(parse_duration("7").unwrap(), 7 * 86_400);
        assert_eq!(parse_duration("3D").unwrap(), 3 * 86_400);
        assert_eq!(parse_duration("  5h ").unwrap(), 5 * 3600);
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        for bad in ["", "abc", "10x", "1.5d", "10 d", "-3d"] {
            assert!(parse_duration(bad).is_err(), "expected error for {bad:?}");
        }
    }

    #[test]
    fn since_keeps_in_window_drops_older() {
        // cutoff = 1000; a chunk exactly at the cutoff survives, one second older drops, NULL drops.
        let hits = vec![
            hit_meta("a.md", Some(1001), None),
            hit_meta("b.md", Some(1000), None),
            hit_meta("c.md", Some(999), None),
            hit_meta("d.md", None, None),
        ];
        let kept = apply_filters(hits, Some(1000), None);
        let paths: Vec<&str> = kept.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "b.md"]);
    }

    #[test]
    fn source_matches_case_insensitively_and_excludes_null() {
        let hits = vec![
            hit_meta("a.md", None, Some("Corti")),
            hit_meta("b.md", None, Some("slack")),
            hit_meta("c.md", None, None),
        ];
        let kept = apply_filters(hits, None, Some("corti"));
        let paths: Vec<&str> = kept.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["a.md"]);
    }

    #[test]
    fn filters_preserve_rank_order_of_survivors() {
        // apply_filters prunes in place (no reordering, no backfill) — RRF order is untouched (G7/G8).
        let hits = vec![
            hit_meta("first.md", Some(2000), Some("corti")),
            hit_meta("second.md", Some(500), Some("corti")), // dropped by --since
            hit_meta("third.md", Some(3000), Some("corti")),
        ];
        let kept = apply_filters(hits, Some(1000), Some("corti"));
        let paths: Vec<&str> = kept.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["first.md", "third.md"]);
    }

    #[test]
    fn no_filters_is_passthrough() {
        let hits = vec![
            hit_meta("a.md", None, None),
            hit_meta("b.md", Some(1), None),
        ];
        let kept = apply_filters(hits, None, None);
        assert_eq!(kept.len(), 2);
    }

    // --- note-level dedup (ADR 0020) ---------------------------------------------------------------

    #[test]
    fn note_dedup_keeps_best_chunk_per_note_in_rank_order() {
        // RRF interleaves chunks from different notes; dedup keeps each note's first-seen
        // (best-ranked) chunk and preserves the note order — no reordering (G7/G8).
        let hits = vec![
            hit_chunk("a.md", 1),
            hit_chunk("b.md", 1),
            hit_chunk("a.md", 2),
            hit_chunk("c.md", 1),
            hit_chunk("b.md", 2),
        ];
        let kept = dedupe_notes(hits);
        let paths: Vec<&str> = kept.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "b.md", "c.md"]);
        let ids: Vec<&str> = kept.iter().map(|h| h.chunk_id.as_str()).collect();
        assert_eq!(ids, ["id:a.md#1", "id:b.md#1", "id:c.md#1"]);
    }

    #[test]
    fn note_dedup_counts_folded_siblings() {
        let hits = vec![
            hit_chunk("a.md", 1),
            hit_chunk("b.md", 1),
            hit_chunk("a.md", 2),
            hit_chunk("c.md", 1),
            hit_chunk("b.md", 2),
        ];
        let kept = dedupe_notes(hits);
        let siblings: Vec<Option<usize>> = kept.iter().map(|h| h.siblings).collect();
        // Folded chunks are counted on the keeper; a single-chunk note stays None so its JSON is
        // indistinguishable from chunk-mode output (G9a).
        assert_eq!(siblings, [Some(1), Some(1), None]);
    }

    #[test]
    fn note_dedup_after_filters_promotes_surviving_chunk() {
        // Stage order is load-bearing: filters run first, so a note whose best chunk was dropped by
        // `--since` is represented by its next surviving chunk, not lost.
        let hits = vec![
            Hit {
                created: Some(500),
                ..hit_chunk("a.md", 1)
            },
            Hit {
                created: Some(2000),
                ..hit_chunk("a.md", 2)
            },
            Hit {
                created: Some(2000),
                ..hit_chunk("b.md", 1)
            },
        ];
        let kept = dedupe_notes(apply_filters(hits, Some(1000), None));
        let ids: Vec<&str> = kept.iter().map(|h| h.chunk_id.as_str()).collect();
        assert_eq!(ids, ["id:a.md#2", "id:b.md#1"]);
        assert_eq!(kept[0].siblings, None); // the filtered chunk was never folded, just dropped
    }

    #[test]
    fn note_dedup_then_truncate_fills_limit() {
        // 8 chunks over 5 notes: dedup first, then truncate — `--limit` counts distinct notes.
        let hits = vec![
            hit_chunk("a.md", 1),
            hit_chunk("a.md", 2),
            hit_chunk("b.md", 1),
            hit_chunk("a.md", 3),
            hit_chunk("c.md", 1),
            hit_chunk("d.md", 1),
            hit_chunk("b.md", 2),
            hit_chunk("e.md", 1),
        ];
        let mut kept = dedupe_notes(hits);
        kept.truncate(3);
        let paths: Vec<&str> = kept.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "b.md", "c.md"]);
        assert_eq!(kept[0].siblings, Some(2));
    }

    // --- rerank compute cap (ADR 0015) -------------------------------------------------------------

    #[test]
    fn rerank_cap_formula_and_floor() {
        // Pins the ~60→~30 shrink so a silent revert of the cap is caught: (limit*2).max(16), clamped
        // to the pool.
        assert_eq!(rerank_cap(15, 60, false), 30);
        assert_eq!(rerank_cap(8, 60, false), 16); // .max(16) floor
        assert_eq!(rerank_cap(4, 60, false), 16);
        assert_eq!(rerank_cap(15, 10, false), 10); // clamped to a shallow pool
        // A `--min-score` floor lifts the cap to the whole pool so the un-scored tail can't be floored
        // out against the sigmoid-scaled head (the recall-fill regression this guards).
        assert_eq!(rerank_cap(15, 60, true), 60);
        assert_eq!(rerank_cap(15, 10, true), 10);
    }

    #[test]
    fn rerank_cap_scores_prefix_only_tail_keeps_rrf() {
        // Only the top `cap` candidates are scored/reordered by the cross-encoder; the tail keeps its
        // RRF order and score untouched. This is the compute cap that halves the forward passes.
        let mk = |path: &str, rrf: f32| Hit {
            score: rrf,
            rrf: Some(rrf),
            ..hit(path)
        };
        let hits = vec![
            mk("a.md", 0.05),
            mk("b.md", 0.04),
            mk("c.md", 0.03),
            mk("d.md", 0.02),
            mk("e.md", 0.01),
        ];
        // Reranker scores the top 2 and flips them (b above a); indices are into the prefix.
        let order = vec![(1, 3.0), (0, -1.0)];
        let out = apply_rerank_prefix(hits, 2, order);
        let paths: Vec<&str> = out.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["b.md", "a.md", "c.md", "d.md", "e.md"]);
        // Prefix carries a rerank logit (+ sigmoid score); the tail has neither.
        assert_eq!(out[0].rerank, Some(3.0));
        assert_eq!(out[1].rerank, Some(-1.0));
        assert!(out[2..].iter().all(|h| h.rerank.is_none()));
        assert_eq!(out[2].score, 0.03); // tail RRF score untouched
        assert_eq!(out[4].score, 0.01);
    }

    #[test]
    fn rerank_cap_beyond_len_reranks_all() {
        // With fewer hits than the cap, everything is reranked and there is no tail.
        let hits = vec![hit("a.md"), hit("b.md")];
        let order = vec![(1, 2.0), (0, 1.0)];
        let out = apply_rerank_prefix(hits, 16, order); // cap floored to len
        let paths: Vec<&str> = out.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, ["b.md", "a.md"]);
        assert!(out.iter().all(|h| h.rerank.is_some()));
    }

    #[test]
    fn rerank_cap_then_dedup_fills_limit_from_tail() {
        // Capping the reranked prefix must not starve note-dedup: the un-reranked tail still carries
        // enough distinct notes to fill `limit` after dedup — only the compute is capped, not depth.
        let hits = vec![
            hit_chunk("a.md", 1),
            hit_chunk("a.md", 2), // same note as the top hit — dedup folds it
            hit_chunk("b.md", 1),
            // tail, beyond cap=3:
            hit_chunk("c.md", 1),
            hit_chunk("d.md", 1),
        ];
        let order = vec![(0, 2.0), (1, 1.0), (2, 0.5)]; // rerank the top 3, keep their order
        let reranked = apply_rerank_prefix(hits, 3, order);
        let mut deduped = dedupe_notes(reranked);
        deduped.truncate(3);
        let paths: Vec<&str> = deduped.iter().map(|h| h.path.as_str()).collect();
        // Prefix had only 2 distinct notes (a, b); c from the tail fills the 3rd slot.
        assert_eq!(paths, ["a.md", "b.md", "c.md"]);
        assert_eq!(deduped[0].siblings, Some(1)); // a#2 folded into a
    }
}

#[cfg(test)]
mod chunk_bodies_tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::util::testdir::TempDir;

    fn temp_db(tag: &str) -> (TempDir, Db) {
        let dir = TempDir::new(tag);
        let db = Db::open(&dir.path().join("meta.db")).unwrap();
        (dir, db)
    }

    /// A synthetic 64-hex id: `lead` right-padded with '0'. Lets tests craft colliding prefixes.
    fn hexid(lead: &str) -> String {
        format!("{lead:0<64}")
    }

    /// Seed a note with chunks as (id, ord, heading_path, body).
    fn seed(db: &Db, path: &str, chunks: &[(String, usize, &str, &str)]) {
        db.upsert_file(path, 1.0, "sha", 1).unwrap();
        let cs: Vec<Chunk> = chunks
            .iter()
            .map(|(id, ord, heading, body)| Chunk {
                id: id.clone(),
                ord: *ord,
                heading_path: heading.to_string(),
                body: body.to_string(),
            })
            .collect();
        db.replace_chunks(path, &cs, None, None).unwrap();
    }

    #[test]
    fn exact_id_resolves_in_request_order() {
        let (_d, db) = temp_db("chunk-exact");
        let (a, b) = (hexid("aa11"), hexid("bb22"));
        seed(
            &db,
            "10-Projects/a.md",
            &[(a.clone(), 0, "A > H", "body a")],
        );
        seed(&db, "30-Resources/b.md", &[(b.clone(), 0, "", "body b")]);
        // Request order (b first) is preserved, not DB order.
        let out = resolve_chunk_args(&db, &[b.clone(), a.clone()]).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].chunk_id.as_deref(), Some(b.as_str()));
        assert_eq!(out[0].path.as_deref(), Some("30-Resources/b.md"));
        assert_eq!(out[0].body.as_deref(), Some("body b"));
        assert_eq!(out[1].chunk_id.as_deref(), Some(a.as_str()));
        assert_eq!(out[1].heading.as_deref(), Some("A > H"));
        assert!(out.iter().all(|c| !c.missing));
    }

    #[test]
    fn unknown_id_yields_positional_missing() {
        let (_d, db) = temp_db("chunk-missing");
        let a = hexid("aa11");
        seed(&db, "a.md", &[(a.clone(), 0, "", "body a")]);
        let ghost = hexid("dddd");
        let out = resolve_chunk_args(&db, &[a.clone(), ghost.clone()]).unwrap();
        assert_eq!(out.len(), 2);
        assert!(!out[0].missing);
        assert!(out[1].missing);
        assert_eq!(out[1].chunk_id.as_deref(), Some(ghost.as_str()));
        assert!(out[1].path.is_none() && out[1].body.is_none());
    }

    #[test]
    fn prefix_resolves_to_full_id_ambiguous_is_missing() {
        let (_d, db) = temp_db("chunk-prefix");
        // Two ids sharing their first 8 hex chars, one unique.
        let (amb1, amb2, uniq) = (hexid("deadbeefaa"), hexid("deadbeefbb"), hexid("cafe1234"));
        seed(
            &db,
            "a.md",
            &[
                (amb1, 0, "", "x"),
                (amb2, 1, "", "y"),
                (uniq.clone(), 2, "H", "body u"),
            ],
        );
        let out = resolve_chunk_args(
            &db,
            &[
                "cafe1234".to_string(),
                "deadbeef".to_string(),
                "cafe".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(out.len(), 3);
        // Unique prefix: found, and the emitted chunk_id is the FULL 64-hex id.
        assert!(!out[0].missing);
        assert_eq!(out[0].chunk_id.as_deref(), Some(uniq.as_str()));
        assert_eq!(out[0].body.as_deref(), Some("body u"));
        // Ambiguous prefix: treated as missing.
        assert!(out[1].missing);
        assert_eq!(out[1].chunk_id.as_deref(), Some("deadbeef"));
        // Below the 8-char minimum: missing.
        assert!(out[2].missing);
        assert_eq!(out[2].chunk_id.as_deref(), Some("cafe"));
    }

    #[test]
    fn uppercase_hex_args_resolve() {
        let (_d, db) = temp_db("chunk-case");
        let a = hexid("abc1234def");
        seed(&db, "a.md", &[(a.clone(), 0, "", "body a")]);
        let out = resolve_chunk_args(&db, &[a.to_ascii_uppercase(), a[..12].to_ascii_uppercase()])
            .unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|c| !c.missing));
        assert!(
            out.iter()
                .all(|c| c.chunk_id.as_deref() == Some(a.as_str()))
        );
    }

    #[test]
    fn path_emits_all_chunks_in_ord_order() {
        let (_d, db) = temp_db("chunk-path");
        let (c0, c1) = (hexid("aa"), hexid("bb"));
        // Insert out of ord order; output must be ord order.
        seed(
            &db,
            "20-Areas/note.md",
            &[
                (c1.clone(), 1, "H1 > H2", "second"),
                (c0.clone(), 0, "H1", "first"),
            ],
        );
        let out = resolve_chunk_args(&db, &["20-Areas/note.md".to_string()]).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].chunk_id.as_deref(), Some(c0.as_str()));
        assert_eq!(out[0].body.as_deref(), Some("first"));
        assert_eq!(out[1].body.as_deref(), Some("second"));
        assert!(
            out.iter()
                .all(|c| c.path.as_deref() == Some("20-Areas/note.md"))
        );

        let out = resolve_chunk_args(&db, &["40-Archive/gone.md".to_string()]).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].missing);
        assert_eq!(out[0].path.as_deref(), Some("40-Archive/gone.md"));
        assert!(out[0].chunk_id.is_none());
    }

    #[test]
    fn json_shape_exact_keys() {
        // The `vagus chunk --json` contract is additive-only from day one: found elements carry
        // exactly {chunk_id, path, heading, body}; missing elements exactly {chunk_id|path, missing}.
        let keys = |c: &ChunkOut| -> Vec<String> {
            let v = serde_json::to_value(c).unwrap();
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let found = ChunkOut::found(hexid("aa"), "a.md".into(), "H".into(), "b".into());
        assert_eq!(keys(&found), ["body", "chunk_id", "heading", "path"]);
        assert_eq!(keys(&ChunkOut::missing_id("dead")), ["chunk_id", "missing"]);
        assert_eq!(keys(&ChunkOut::missing_path("x.md")), ["missing", "path"]);
    }
}
