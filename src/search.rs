//! Search entry point: BM25 (lexical), vector (semantic), and hybrid (RRF k=60).
//!
//! Human output shows a 0–100 relevance **relative to the top hit** — the raw RRF scalar is
//! rank-based and tiny (≤ 2/(k+1) ≈ 0.033), so printing it directly is misleading. `--json` keeps a
//! stable shape for the bundled agent skill and carries the raw fused `score` plus the per-retriever
//! `cosine` and `bm25` components.

use std::collections::HashMap;
use std::io::IsTerminal;
// Only the `--smart` path (smart_query) prewarms models on threads, so this is generate-gated to keep
// the lean (`--no-default-features`) build warning-free.
#[cfg(feature = "generate")]
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::Serialize;

use crate::config::Config;
use crate::db::Db;
use crate::embed::Embedder;
use crate::index;
use crate::lex::Lex;
use crate::rerank::{Reranker, sigmoid};
use crate::scope::Scope;
#[cfg(test)]
use crate::util::parse_duration;
use crate::util::since_cutoff;

/// RRF constant (guardrail G8).
const RRF_K: f32 = 60.0;
/// Stable eval provenance for the default fusion policy (ADRs 0003/0025). A future opt-in
/// experiment must report a different policy id without changing this default silently.
pub const FUSION_POLICY: &str = "rrf_k60";

fn fusion_supports_adaptive_tidy(policy: &str) -> bool {
    policy == "rrf_k60"
}

// Adaptive context-tidiness gate (ADR 0023/G9d). The cutoff is deliberately conservative: a score
// cliff must leave a real head and tail, exceed a fixed 10% prominence floor, and be a 3-sigma robust
// outlier relative to this result list's other adjacent log gaps. It only drops a suffix; RRF itself
// and every survivor's order/score remain untouched.
const TIDY_MIN_SIDE: usize = 3;
/// Never let the adaptive suffix drop hide a top-N champion from either source list. This protects a
/// strong exact-term hit when ANN misses its vector (and vice versa) without weighting either list.
const TIDY_PROTECTED_CHANNEL_RANK: usize = 3;
const TIDY_MIN_LOG_DROP: f64 = 0.105_360_515_657_826_3; // ln(10/9): at least a 10% score ratio gap
const TIDY_MAD_SCALE: f64 = 1.4826; // normal-consistent median absolute deviation
const TIDY_OUTLIER_SIGMA: f64 = 3.0;

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// Length of the high-signal RRF prefix to retain. Invalid/short/smooth inputs fail open by returning
/// `scores.len()`. `protected_through` is the one-based position of the last top-channel champion; a
/// score knee before it also fails open. Scale-invariant because adjacent log ratios are compared.
fn tidy_rrf_prefix_len(scores: &[f32], protected_through: usize) -> usize {
    let n = scores.len();
    if n < TIDY_MIN_SIDE * 2 {
        return n;
    }

    let mut gaps = Vec::with_capacity(n - 1);
    for pair in scores.windows(2) {
        let (a, b) = (pair[0] as f64, pair[1] as f64);
        if !a.is_finite() || !b.is_finite() || a <= 0.0 || b <= 0.0 || b > a {
            return n;
        }
        let gap = (a / b).ln();
        if !gap.is_finite() {
            return n;
        }
        gaps.push(gap);
    }

    let mut ordered = gaps.clone();
    let center = median(&mut ordered);
    let mut deviations: Vec<f64> = gaps.iter().map(|g| (g - center).abs()).collect();
    let mad = median(&mut deviations);
    let threshold = TIDY_MIN_LOG_DROP.max(center + TIDY_OUTLIER_SIGMA * TIDY_MAD_SCALE * mad);

    // Largest eligible cliff wins; latest wins an exact tie, preserving more recall.
    let mut best: Option<(usize, f64)> = None;
    for (i, &gap) in gaps.iter().enumerate() {
        let keep = i + 1;
        if keep < TIDY_MIN_SIDE || n - keep < TIDY_MIN_SIDE {
            continue;
        }
        if best.is_none_or(|(_, best_gap)| gap >= best_gap) {
            best = Some((keep, gap));
        }
    }
    match best {
        Some((keep, gap)) if gap > threshold && protected_through <= keep => keep,
        _ => n,
    }
}

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
    /// Internal source-list ranks used only by the adaptive recall guard (ADR 0023). Never serialized:
    /// exposing them would expand the stable Hit contract for implementation-only metadata.
    #[serde(skip)]
    bm25_rank: Option<usize>,
    #[serde(skip)]
    cosine_rank: Option<usize>,
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

fn source_champion_through(hits: &[Hit]) -> usize {
    hits.iter()
        .rposition(|h| {
            h.bm25_rank
                .is_some_and(|r| r <= TIDY_PROTECTED_CHANNEL_RANK)
                || h.cosine_rank
                    .is_some_and(|r| r <= TIDY_PROTECTED_CHANNEL_RANK)
        })
        .map_or(0, |i| i + 1)
}

/// Ranked id + component scores, before joining SQLite for the display fields.
struct Scored {
    id: String,
    score: f32,
    rrf: Option<f32>,
    cosine: Option<f32>,
    bm25: Option<f32>,
    bm25_rank: Option<usize>,
    cosine_rank: Option<usize>,
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
    // HashMap iteration is randomized. Equal RRF sums are common for mirrored rank pairs, so use the
    // opaque stable chunk id as a modality-neutral final tie-break instead of leaking process entropy
    // into note selection and full-body context. Scores/formula remain exactly G8 RRF k=60.
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
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
                bm25_rank: s.bm25_rank,
                cosine_rank: s.cosine_rank,
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

/// Build exactly the documents scored by the capped reranker prefix. Radius zero is intentionally a
/// byte-for-byte body clone with no extra DB lookup. An opt-in radius reconstructs only each hit's
/// adjacent in-note chunks, then delegates exact tokenizer-budget fitting to the loaded reranker
/// (ADR 0015). Returned Hit bodies/snippets remain the matched center chunk.
fn rerank_documents(
    db: &Db,
    reranker: &Reranker,
    query: &str,
    hits: &[Hit],
) -> Result<Vec<String>> {
    if reranker.context_radius() == 0 {
        return Ok(hits
            .iter()
            .map(|hit| hit.body.clone().unwrap_or_default())
            .collect());
    }

    let mut documents = Vec::with_capacity(hits.len());
    for hit in hits {
        let center_body = hit.body.as_deref().unwrap_or_default();
        let window = db.chunk_window(&hit.path, &hit.chunk_id, reranker.context_radius())?;
        let Some(center_index) = window.iter().position(|(_, id, _)| id == &hit.chunk_id) else {
            // A second process can replace this derived row between hydration and the neighbor read.
            // Preserve a useful center-only rerank rather than letting optional context erase a hit.
            documents.push(reranker.prepare_context(query, center_body, &[], &[])?);
            continue;
        };
        let before: Vec<&str> = window[..center_index]
            .iter()
            .map(|(_, _, body)| body.as_str())
            .collect();
        let after: Vec<&str> = window[center_index + 1..]
            .iter()
            .map(|(_, _, body)| body.as_str())
            .collect();
        documents.push(reranker.prepare_context(
            query,
            &window[center_index].2,
            &before,
            &after,
        )?);
    }
    Ok(documents)
}

/// Reusable: returns ranked hits (used by `run` and by filing `--suggest`). `full` retains the chunk
/// body on each hit; `rerank` re-scores a deeper candidate pool with the cross-encoder (tier-1),
/// optionally reading `rerank_context` adjacent chunks without changing the returned Hit;
/// `chunks` skips note-level dedup, returning raw chunk hits (ADRs 0015/0020).
#[allow(clippy::too_many_arguments)]
pub fn query(
    cfg: &Config,
    q: &str,
    mode: Mode,
    limit: usize,
    scope: &Scope,
    full: bool,
    rerank: bool,
    rerank_context: usize,
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
                .enumerate()
                .map(|(i, (id, bm25))| Scored {
                    id,
                    score: bm25,
                    rrf: None,
                    cosine: None,
                    bm25: Some(bm25),
                    bm25_rank: Some(i + 1),
                    cosine_rank: None,
                })
                .collect()
        }
        Mode::Vec => vec_search(cfg, &db, q, pool, exact)?
            .into_iter()
            .enumerate()
            .map(|(i, (id, cosine))| Scored {
                id,
                score: cosine,
                rrf: None,
                cosine: Some(cosine),
                bm25: None,
                bm25_rank: None,
                cosine_rank: Some(i + 1),
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
            let bm_rank_of: HashMap<&str, usize> = bm
                .iter()
                .enumerate()
                .map(|(i, (id, _))| (id.as_str(), i + 1))
                .collect();
            let cos_rank_of: HashMap<&str, usize> = ve
                .iter()
                .enumerate()
                .map(|(i, (id, _))| (id.as_str(), i + 1))
                .collect();
            let bm_ids: Vec<String> = bm.iter().map(|(id, _)| id.clone()).collect();
            let ve_ids: Vec<String> = ve.iter().map(|(id, _)| id.clone()).collect();
            rrf(&[bm_ids, ve_ids], pool)
                .into_iter()
                .map(|(id, r)| Scored {
                    cosine: cos_of.get(id.as_str()).copied(),
                    bm25: bm25_of.get(id.as_str()).copied(),
                    bm25_rank: bm_rank_of.get(id.as_str()).copied(),
                    cosine_rank: cos_rank_of.get(id.as_str()).copied(),
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
        let mut rr = Reranker::new(&cfg.cache_dir, rerank_context)?;
        t.rerank_load_ms = ms_since(t0);
        let cap = rerank_cap(limit, hits.len(), score_floor);
        let t0 = Instant::now();
        let docs = rerank_documents(&db, &rr, q, &hits[..cap])?;
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
    fn best_rank(a: Option<usize>, b: Option<usize>) -> Option<usize> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    let mut kept: Vec<Hit> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    for h in hits {
        match index_of.get(&h.path) {
            Some(&i) => {
                // The selected chunk remains the note's display/ranking representative (G9c), but a
                // folded sibling can be the note's top lexical/vector source champion. Preserve each
                // note's best private source ranks so the later G9d cutoff veto cannot lose that
                // evidence during dedup. Scores, selected body, and order remain unchanged.
                kept[i].siblings = Some(kept[i].siblings.unwrap_or(0) + 1);
                kept[i].bm25_rank = best_rank(kept[i].bm25_rank, h.bm25_rank);
                kept[i].cosine_rank = best_rank(kept[i].cosine_rank, h.cosine_rank);
            }
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
/// Offline, no coding agent — the local sibling of the Opus search skill.
#[cfg(feature = "generate")]
#[allow(clippy::too_many_arguments)]
fn smart_query(
    cfg: &Config,
    q: &str,
    limit: usize,
    scope: &Scope,
    full: bool,
    rerank_context: usize,
    since: Option<i64>,
    source: Option<&str>,
    exact: bool,
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
        std::thread::spawn(move || Reranker::new(&cache, rerank_context))
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
                // Forward the explicit oracle flag in every tier. Automatic exact selection still
                // handles personal-scale corpora; `--smart --exact` must also force it above cutoff.
                vindex = Some(crate::vector::open_for_search(cfg, &db, exact)?);
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
    // Retrieval is complete. Release the embedder before widened quadratic-attention inference;
    // keeping two ONNX sessions resident buys no latency now and materially raises `--smart
    // --rerank-context` peak memory. The vector sidecar is likewise no longer needed.
    drop(vindex);
    drop(emb);

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
            bm25_rank: None,
            cosine_rank: None,
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
            Err(_) => Reranker::new(&cfg.cache_dir, rerank_context)?,
        };
        t.rerank_load_ms = ms_since(t0);
        let cap = rerank_cap(limit, hits.len(), score_floor);
        let t0 = Instant::now();
        let docs = rerank_documents(&db, &rr, q, &hits[..cap])?;
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
    rerank_context: usize,
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
            rerank_context,
            since,
            source,
            exact,
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
        rerank_context,
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
    rerank_context: usize,
    min_score: Option<f32>,
    smart: bool,
    since: Option<&str>,
    source: Option<&str>,
    exact: bool,
    timings: bool,
    chunks: bool,
    exhaustive: bool,
) -> Result<()> {
    if rerank_context > 0 && !rerank && !smart {
        bail!("--rerank-context requires --rerank or --smart");
    }
    // Parse the `--since` duration up front so a bad spec errors clearly before any indexing/search.
    let since_cut = match since {
        Some(spec) => Some(since_cutoff(spec)?),
        None => None,
    };
    // Keep results fresh: an incremental refresh before searching so a just-edited or just-dropped
    // note is findable. Cheap when nothing changed (mtime fast-path; the model only loads if a file
    // actually changed). `--no-index` skips it.
    if !no_index && let Err(e) = index::run(cfg, index::IndexMode::Incremental) {
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
        rerank_context,
        smart,
        since_cut,
        source,
        exact,
        timings,
        chunks,
        score_floor,
    )?;
    // Explicit quality floor: when supplied, the caller owns tail selection and the adaptive gate
    // below stays out of the way.
    if let Some(floor) = min_score {
        let top = hits.first().map(|h| h.score).unwrap_or(1.0);
        hits.retain(|h| rel(h.score, top) as f32 >= floor);
    }

    // Context tidiness (ADR 0023/G9d): for the plain tier-0 hybrid note path, `--limit` is a ceiling,
    // not a quota. If a robust RRF score knee separates a high-signal prefix from a real tail, drop
    // only that suffix. Unsupported/mixed-score modes fail open; --exhaustive restores the old fill.
    let tidy_omitted = if !exhaustive
        && fusion_supports_adaptive_tidy(FUSION_POLICY)
        && min_score.is_none()
        && matches!(mode, Mode::Hybrid)
        && !rerank
        && !smart
        && !chunks
        && hits.len() == limit
    {
        let scores: Vec<f32> = hits.iter().filter_map(|h| h.rrf).collect();
        let protected_through = source_champion_through(&hits);
        let keep = if scores.len() == hits.len() {
            tidy_rrf_prefix_len(&scores, protected_through)
        } else {
            hits.len()
        };
        let omitted = hits.len() - keep;
        hits.truncate(keep);
        omitted
    } else {
        0
    };

    emit(&hits, json, verbose, full);
    if tidy_omitted > 0 {
        let msg = format!(
            "{tidy_omitted} low-signal tail hit(s) omitted by adaptive cutoff (--exhaustive to show)"
        );
        if json {
            // Preserve stdout as a pure Hit array for skills and scripts (G9a).
            eprintln!("vagus: {msg}");
        } else {
            println!("{}", Style::detect().dim(&format!("— {msg}")));
        }
    }
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
            bm25_rank: None,
            cosine_rank: None,
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
        h2.bm25_rank = Some(1);
        h2.cosine_rank = Some(2);
        let j2 = serde_json::to_string(&h2).unwrap();
        assert!(j2.contains("\"rerank\":1.5"));
        assert!(j2.contains("\"body\":\"full text\""));
        assert!(j2.contains("\"siblings\":2"));
        assert!(!j2.contains("bm25_rank"));
        assert!(!j2.contains("cosine_rank"));
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

    #[test]
    fn rrf_equal_scores_have_a_deterministic_modality_neutral_tie_break() {
        // Mirrored ranks produce exactly equal G8 RRF sums. HashMap iteration is process-random, so
        // the opaque chunk id is the final tie-break; no source list receives extra weight.
        let lists = vec![
            vec!["b".to_string(), "a".to_string()],
            vec!["a".to_string(), "b".to_string()],
        ];
        let got = rrf(&lists, 10);
        assert_eq!(got[0].0, "a");
        assert_eq!(got[1].0, "b");
        assert_eq!(got[0].1, got[1].1);
        let expected = 1.0 / (RRF_K + 1.0) + 1.0 / (RRF_K + 2.0);
        assert_eq!(got[0].1, expected);
        assert_eq!(rrf(&lists, 10), got, "same input must be repeatable");
    }

    // --- adaptive context tidiness (ADR 0023) -----------------------------------------------------

    #[test]
    fn alternate_fusion_cannot_reuse_the_rrf_score_knee() {
        assert!(fusion_supports_adaptive_tidy("rrf_k60"));
        assert!(!fusion_supports_adaptive_tidy("weighted_rrf_v1"));
    }

    #[test]
    fn source_champion_guard_tracks_only_top_three_channel_ranks() {
        let mut hits = vec![hit("a"), hit("b"), hit("c"), hit("d"), hit("e")];
        hits[1].bm25_rank = Some(3);
        hits[3].cosine_rank = Some(4); // not protected
        assert_eq!(source_champion_through(&hits), 2);
        hits[4].cosine_rank = Some(1);
        assert_eq!(source_champion_through(&hits), 5);
    }

    #[test]
    fn tidy_hunter_fixture_keeps_signal_prefix_and_cuts_context() {
        // Frozen v0.9 exhaustive result scores/body sizes for:
        //   Hunter was downtrodden at the end
        // Four independent judges agreed rank 1 alone answers; the conservative cutoff keeps through
        // rank 7 (including every disputed/supporting hit) and drops only the unanimous trash tail.
        let scores = [
            0.032786883,
            0.03175403,
            0.030309988,
            0.027912386,
            0.025816994,
            0.02444842,
            0.022785103,
            0.019655071,
            0.018332252,
            0.017220989,
        ];
        assert_eq!(tidy_rrf_prefix_len(&scores, 0), 7);
        assert_eq!(
            tidy_rrf_prefix_len(&scores[..7], 0),
            7,
            "cutoff is idempotent"
        );

        let body_chars = [125, 272, 264, 2842, 944, 3124, 2905, 1027, 2814, 2786];
        let body_words = [22, 42, 35, 564, 182, 581, 577, 150, 416, 550];
        let old_chars: usize = body_chars.iter().sum();
        let new_chars: usize = body_chars[..7].iter().sum();
        assert_eq!((old_chars, new_chars), (17_103, 10_476));
        assert_eq!(
            (old_chars.div_ceil(4), new_chars.div_ceil(4)),
            (4_276, 2_619)
        );
        assert_eq!(
            (
                body_words.iter().sum::<usize>(),
                body_words[..7].iter().sum::<usize>()
            ),
            (3_119, 2_003)
        );
    }

    #[test]
    fn tidy_detects_only_a_guarded_internal_cliff() {
        let clear_head = [1.0, 0.99, 0.98, 0.50, 0.49, 0.48, 0.47, 0.46];
        assert_eq!(tidy_rrf_prefix_len(&clear_head, 0), 3);
        // A top-three source-list champion beyond the knee makes the cutoff fail open rather than
        // hiding evidence that RRF can rank low when only one retriever found it.
        assert_eq!(tidy_rrf_prefix_len(&clear_head, 7), clear_head.len());

        // A smooth geometric decay has no outlier relative to its own adjacent gaps.
        let smooth = [1.0, 0.9, 0.81, 0.729, 0.6561, 0.59049, 0.531441, 0.4782969];
        assert_eq!(tidy_rrf_prefix_len(&smooth, 0), smooth.len());

        // Dramatic endpoint cliffs are not enough: both retained head and omitted tail need 3 hits.
        let endpoint_only = [1.0, 0.50, 0.49, 0.48, 0.47, 0.46, 0.45, 0.44, 0.43, 0.01];
        assert_eq!(tidy_rrf_prefix_len(&endpoint_only, 0), endpoint_only.len());
    }

    #[test]
    fn tidy_fails_open_on_invalid_short_or_unordered_scores() {
        assert_eq!(tidy_rrf_prefix_len(&[1.0, 0.5, 0.25], 0), 3);
        for invalid in [
            vec![1.0, 0.9, f32::NAN, 0.7, 0.6, 0.5],
            vec![1.0, 0.9, 0.0, 0.7, 0.6, 0.5],
            vec![1.0, 0.9, 0.8, 0.85, 0.7, 0.6],
        ] {
            assert_eq!(tidy_rrf_prefix_len(&invalid, 0), invalid.len());
        }
    }

    #[test]
    fn tidy_cutoff_is_scale_invariant() {
        let scores = [1.0, 0.99, 0.98, 0.50, 0.49, 0.48, 0.47, 0.46];
        let scaled: Vec<f32> = scores.iter().map(|s| s * 0.03125).collect();
        assert_eq!(
            tidy_rrf_prefix_len(&scores, 0),
            tidy_rrf_prefix_len(&scaled, 0)
        );
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
    fn folded_source_champion_survives_dedup_and_vetoes_knee() {
        // The robust knee is after rank 3. The displayed chunk for f.md is rank 6 and is not itself a
        // source champion; a later sibling is BM25 rank 1. Dedup must fold that private rank into the
        // keeper so the composed G9c→G9d pipeline fails open instead of dropping the note.
        let specs = [
            ("a.md", 0, 1.00),
            ("b.md", 0, 0.99),
            ("c.md", 0, 0.98),
            ("d.md", 0, 0.50),
            ("e.md", 0, 0.49),
            ("f.md", 0, 0.48),
            ("g.md", 0, 0.47),
            ("f.md", 1, 0.46),
        ];
        let mut hits: Vec<Hit> = specs
            .into_iter()
            .map(|(path, ord, score)| Hit {
                score,
                rrf: Some(score),
                ..hit_chunk(path, ord)
            })
            .collect();
        hits.last_mut().unwrap().bm25_rank = Some(1);

        let kept = dedupe_notes(hits);
        assert_eq!(kept.len(), 7);
        assert_eq!(kept[5].path, "f.md");
        assert_eq!(kept[5].bm25_rank, Some(1));
        assert_eq!(kept[5].siblings, Some(1));
        let scores: Vec<f32> = kept.iter().map(|hit| hit.rrf.unwrap()).collect();
        assert_eq!(
            tidy_rrf_prefix_len(&scores, 0),
            3,
            "fixture has a real knee"
        );
        let protected_through = source_champion_through(&kept);
        assert_eq!(protected_through, 6);
        assert_eq!(
            tidy_rrf_prefix_len(&scores, protected_through),
            scores.len(),
            "folded champion vetoes the lossy cutoff"
        );
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
