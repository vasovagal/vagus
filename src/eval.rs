//! Retrieval-quality evaluation (`vagus eval`; ADR 0024 / G27).
//!
//! The harness scores a fixed, already-built local index against vault-specific JSONL qrels. It is
//! deliberately **exhaustive pre-tidy**: it calls [`search::query`] directly, so ADR 0023's adaptive
//! presentation cutoff cannot make a fusion experiment look better by returning fewer notes. Reports
//! pin the labels, corpus-content fingerprint, index/model identities, exact/ANN backend, and search
//! options needed to compare runs. Top-score summaries are diagnostics within one identical config,
//! never calibrated probabilities.
//!
//! This is a batch/offline command. Vector and reranked modes currently load their local model(s) per
//! query through the shared search API; no network or background process is introduced. The sibling
//! `eval-gate` command implements ADR 0025's fixed, paired fusion-promotion contract.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::Db;
use crate::scope::Scope;
use crate::search::{self, Mode};
use crate::util::{corpus_fingerprint, executable_fingerprint, sha256_hex};
use crate::vector;

mod gate;

const REPORT_SCHEMA_VERSION: u32 = 2;
const MAX_EVAL_K: usize = 1_000;
const RESULT_POLICY: &str = "note_level_exhaustive_pre_tidy";

// --- label file --------------------------------------------------------------------------------

/// One parsed qrel line. Positive grades live in `relevant`; `judged_paths` also retains grade-zero
/// entries so a stale/typoed path is never silently treated as a retrieval miss.
struct Label {
    query: String,
    cohort: Option<String>,
    relevant: HashMap<String, u8>,
    judged_paths: Vec<String>,
    /// True when this positive query used the graded-object form; only those lines define nDCG.
    has_grades: bool,
}

impl Label {
    fn is_negative(&self) -> bool {
        self.relevant.is_empty()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLabel {
    query: String,
    /// Optional for exploratory eval; ADR 0025's promotion gate requires a non-empty cohort on every
    /// positive query so one query family cannot hide another's regression.
    cohort: Option<String>,
    // Missing/misspelled `relevant` is a hard error. A negative probe must explicitly use `[]`.
    relevant: Vec<RawRelevant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGraded {
    path: String,
    grade: u8,
}

/// A bare path has grade 1. In the object form, grade 0 means judged non-relevant and 1–3 means
/// relevant. Unknown object keys are rejected by `RawGraded` rather than ignored.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawRelevant {
    Path(String),
    Graded(RawGraded),
}

fn validate_label_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    if path.is_empty()
        || p.is_absolute()
        || p.extension().and_then(|x| x.to_str()) != Some("md")
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("qrel path must be a normalized vault-relative .md path: {path:?}");
    }
    Ok(())
}

fn parse_label(line: &str) -> Result<Label> {
    let raw: RawLabel = serde_json::from_str(line)?;
    let query = raw.query.trim();
    if query.is_empty() {
        bail!("query must not be empty");
    }

    let cohort = raw
        .cohort
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if cohort.as_ref().is_some_and(|value| {
        value.len() > 64
            || !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    }) {
        bail!("cohort must be 1-64 ASCII letters/digits/'-'/ '_' characters");
    }

    let mut relevant = HashMap::new();
    let mut judged_paths = Vec::new();
    let mut seen = HashSet::new();
    let mut used_graded_form = false;
    for entry in raw.relevant {
        let (path, grade) = match entry {
            RawRelevant::Path(path) => (path, 1),
            RawRelevant::Graded(RawGraded { path, grade }) => {
                used_graded_form = true;
                (path, grade)
            }
        };
        validate_label_path(&path)?;
        if grade > 3 {
            bail!("grade {grade} out of range for {path:?} (expected 0-3)");
        }
        if !seen.insert(path.clone()) {
            bail!("duplicate qrel path {path:?}");
        }
        judged_paths.push(path.clone());
        if grade > 0 {
            relevant.insert(path, grade);
        }
    }

    Ok(Label {
        query: query.to_owned(),
        cohort,
        has_grades: used_graded_form && !relevant.is_empty(),
        relevant,
        judged_paths,
    })
}

/// Parse JSONL labels. Blank lines are skipped; errors name the one-based line. An empty file and
/// duplicate query strings are rejected so an accidental sample change cannot fabricate an average.
fn parse_labels(content: &str) -> Result<Vec<Label>> {
    let mut out = Vec::new();
    let mut queries = HashSet::new();
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let label = parse_label(line).with_context(|| format!("labels line {}", i + 1))?;
        if !queries.insert(label.query.clone()) {
            bail!("labels line {}: duplicate query {:?}", i + 1, label.query);
        }
        out.push(label);
    }
    if out.is_empty() {
        bail!("label file contains no queries");
    }
    Ok(out)
}

fn validate_qrel_paths(
    labels: &[Label],
    files: &HashMap<String, (f64, String)>,
    db: &Db,
) -> Result<()> {
    for label in labels {
        for path in &label.judged_paths {
            if !files.contains_key(path) {
                bail!(
                    "qrel path {path:?} for query {:?} is not present in the current index",
                    label.query
                );
            }
            if db.chunk_ids_for(path)?.is_empty() {
                bail!(
                    "qrel path {path:?} for query {:?} has no retrievable chunks in the current index",
                    label.query
                );
            }
        }
    }
    Ok(())
}

// --- metrics (model-free CI regression lock) ---------------------------------------------------
//
// The sole runner uses note-level results (one path per result). A future chunk-level caller must
// deduplicate paths first or precision/nDCG can be overstated.

/// Standard precision@k: relevant notes in the first k positions divided by the requested k. An
/// under-filled result set is therefore penalized rather than rewarded.
fn precision_at_k(ranked: &[String], relevant: &HashMap<String, u8>, k: usize) -> f64 {
    debug_assert!(k > 0);
    let hits = ranked
        .iter()
        .take(k)
        .filter(|p| relevant.contains_key(*p))
        .count();
    hits as f64 / k as f64
}

fn recall_at_k(ranked: &[String], relevant: &HashMap<String, u8>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let hits = ranked
        .iter()
        .take(k)
        .filter(|p| relevant.contains_key(*p))
        .count();
    hits as f64 / relevant.len() as f64
}

/// Reciprocal rank truncated at k (RR@k): zero when the first k notes contain no relevant path.
fn reciprocal_rank_at_k(ranked: &[String], relevant: &HashMap<String, u8>, k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .position(|p| relevant.contains_key(p))
        .map_or(0.0, |i| 1.0 / (i as f64 + 1.0))
}

/// DCG over rank-ordered grades: Σ (2^grade − 1) / log2(rank+1), rank one-based.
fn dcg(grades: &[u8]) -> f64 {
    grades
        .iter()
        .enumerate()
        .map(|(i, &g)| (2f64.powi(g as i32) - 1.0) / ((i as f64 + 2.0).log2()))
        .sum()
}

fn ndcg_at_k(ranked: &[String], relevant: &HashMap<String, u8>, k: usize) -> f64 {
    let ranked_grades: Vec<u8> = ranked
        .iter()
        .take(k)
        .map(|p| relevant.get(p).copied().unwrap_or(0))
        .collect();
    let mut ideal: Vec<u8> = relevant.values().copied().collect();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    ideal.truncate(k);
    let idcg = dcg(&ideal);
    if idcg == 0.0 {
        0.0
    } else {
        dcg(&ranked_grades) / idcg
    }
}

// --- stable report -----------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryReport {
    query: String,
    cohort: Option<String>,
    is_negative: bool,
    /// `null` on a negative probe: no positive relevant set exists, so P/R/RR/nDCG are undefined.
    precision_at_k: Option<f64>,
    recall_at_k: Option<f64>,
    reciprocal_rank_at_k: Option<f64>,
    ndcg_at_k: Option<f64>,
    /// Mode-specific diagnostic, `null` when retrieval returned no hit. Not a calibrated probability.
    top_score: Option<f64>,
    returned: usize,
    ranked: Vec<String>,
    found: Vec<String>,
    missing: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Aggregate {
    mean_precision_at_k: Option<f64>,
    mean_recall_at_k: Option<f64>,
    mrr_at_k: Option<f64>,
    mean_ndcg_at_k: Option<f64>,
    n_positive: usize,
    n_negative: usize,
    n_graded: usize,
    n_positive_with_top_score: usize,
    n_negative_with_top_score: usize,
    /// Diagnostics are defined only when every query in that non-empty cohort returned a finite hit.
    mean_top_score_positive: Option<f64>,
    mean_top_score_negative: Option<f64>,
    top_score_delta: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalConfig {
    k: usize,
    mode: String,
    fusion_policy: String,
    fusion_candidate_pool: String,
    rerank: bool,
    rerank_policy: String,
    score_floor: bool,
    exact_requested: bool,
    vector_backend: String,
    automatic_exact_cutoff: usize,
    score_kind: String,
    result_policy: String,
    note_level: bool,
    adaptive_cutoff: bool,
    index_refresh: bool,
    cwd_scope: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexSnapshot {
    corpus_sha256: String,
    indexed_files: usize,
    indexed_chunks: usize,
    embedded_chunks: usize,
    embed_model: Option<String>,
    embed_dims: Option<String>,
    chunk_version: Option<String>,
    tantivy_version: Option<String>,
    vec_backend: Option<String>,
    vec_index_version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    vagus_version: String,
    binary_sha256: String,
    labels_sha256: String,
    index: IndexSnapshot,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalReport {
    schema_version: u32,
    config: EvalConfig,
    provenance: Provenance,
    queries: Vec<QueryReport>,
    aggregate: Aggregate,
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Hybrid => "hybrid",
        Mode::Bm25 => "bm25",
        Mode::Vec => "vec",
    }
}

fn score_kind(mode: Mode, rerank: bool, relevance: bool) -> &'static str {
    if relevance {
        crate::relevance::POLICY
    } else if rerank {
        "rerank_sigmoid"
    } else {
        match mode {
            Mode::Hybrid => "rrf",
            Mode::Bm25 => "bm25",
            Mode::Vec => "cosine",
        }
    }
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

fn complete_mean(xs: &[Option<f64>]) -> Option<f64> {
    if xs.is_empty() || xs.iter().any(Option::is_none) {
        None
    } else {
        mean(&xs.iter().flatten().copied().collect::<Vec<_>>())
    }
}

fn score_query(
    label: &Label,
    ranked: Vec<String>,
    top_score: Option<f64>,
    k: usize,
) -> QueryReport {
    let topk: HashSet<&str> = ranked.iter().take(k).map(String::as_str).collect();
    let mut found: Vec<String> = label
        .relevant
        .keys()
        .filter(|p| topk.contains(p.as_str()))
        .cloned()
        .collect();
    let mut missing: Vec<String> = label
        .relevant
        .keys()
        .filter(|p| !topk.contains(p.as_str()))
        .cloned()
        .collect();
    found.sort();
    missing.sort();

    let positive = !label.is_negative();
    QueryReport {
        query: label.query.clone(),
        cohort: label.cohort.clone(),
        is_negative: !positive,
        precision_at_k: positive.then(|| precision_at_k(&ranked, &label.relevant, k)),
        recall_at_k: positive.then(|| recall_at_k(&ranked, &label.relevant, k)),
        reciprocal_rank_at_k: positive.then(|| reciprocal_rank_at_k(&ranked, &label.relevant, k)),
        ndcg_at_k: (positive && label.has_grades).then(|| ndcg_at_k(&ranked, &label.relevant, k)),
        top_score,
        returned: ranked.len(),
        ranked,
        found,
        missing,
    }
}

fn aggregate(reports: &[QueryReport]) -> Aggregate {
    let positives: Vec<&QueryReport> = reports.iter().filter(|q| !q.is_negative).collect();
    let negatives: Vec<&QueryReport> = reports.iter().filter(|q| q.is_negative).collect();
    let graded: Vec<f64> = positives.iter().filter_map(|q| q.ndcg_at_k).collect();
    let precision: Vec<f64> = positives.iter().filter_map(|q| q.precision_at_k).collect();
    let recall: Vec<f64> = positives.iter().filter_map(|q| q.recall_at_k).collect();
    let rr: Vec<f64> = positives
        .iter()
        .filter_map(|q| q.reciprocal_rank_at_k)
        .collect();
    let pos_top: Vec<Option<f64>> = positives.iter().map(|q| q.top_score).collect();
    let neg_top: Vec<Option<f64>> = negatives.iter().map(|q| q.top_score).collect();
    let mean_top_score_positive = complete_mean(&pos_top);
    let mean_top_score_negative = complete_mean(&neg_top);
    let top_score_delta = mean_top_score_positive
        .zip(mean_top_score_negative)
        .map(|(positive, negative)| positive - negative);

    Aggregate {
        mean_precision_at_k: mean(&precision),
        mean_recall_at_k: mean(&recall),
        mrr_at_k: mean(&rr),
        mean_ndcg_at_k: mean(&graded),
        n_positive: positives.len(),
        n_negative: negatives.len(),
        n_graded: graded.len(),
        n_positive_with_top_score: pos_top.iter().flatten().count(),
        n_negative_with_top_score: neg_top.iter().flatten().count(),
        mean_top_score_positive,
        mean_top_score_negative,
        top_score_delta,
    }
}

fn ensure_index_unchanged(initial: &IndexSnapshot, final_snapshot: &IndexSnapshot) -> Result<()> {
    if initial != final_snapshot {
        bail!("index changed during eval; discard the run and retry against a fixed index");
    }
    Ok(())
}

fn index_snapshot(db: &Db, files: &HashMap<String, (f64, String)>) -> Result<IndexSnapshot> {
    Ok(IndexSnapshot {
        corpus_sha256: corpus_fingerprint(files),
        indexed_files: files.len(),
        indexed_chunks: db.count("SELECT count(*) FROM chunks")? as usize,
        embedded_chunks: db.count("SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")?
            as usize,
        embed_model: db.meta_get("embed_model")?,
        embed_dims: db.meta_get("embed_dims")?,
        chunk_version: db.meta_get("chunk_version")?,
        tantivy_version: db.meta_get("tantivy_version")?,
        vec_backend: db.meta_get("vec_backend")?,
        vec_index_version: db.meta_get("vec_index_version")?,
    })
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    cfg: &Config,
    label_content: &str,
    k: usize,
    mode: Mode,
    rerank: bool,
    rerank_context: usize,
    exact: bool,
    relevance: bool,
) -> Result<EvalReport> {
    if !(1..=MAX_EVAL_K).contains(&k) {
        bail!("--k must be between 1 and {MAX_EVAL_K}");
    }
    if exact && matches!(mode, Mode::Bm25) {
        bail!("--exact requires --mode hybrid or --mode vec");
    }
    if rerank_context > 0 && !rerank {
        bail!("--rerank-context requires --rerank");
    }
    if relevance && matches!(mode, Mode::Bm25) {
        bail!("--relevance requires --mode hybrid or --mode vec");
    }

    let labels = parse_labels(label_content)?;
    let db = Db::open(&cfg.db_path())?;
    let files = db.existing_files()?;
    validate_qrel_paths(&labels, &files, &db)?;

    let vector_backend = if matches!(mode, Mode::Bm25) {
        "none"
    } else {
        vector::backend_name_for_search(cfg, &db, exact)?
    };
    let config = EvalConfig {
        k,
        mode: mode_label(mode).to_owned(),
        fusion_policy: if matches!(mode, Mode::Hybrid) {
            search::FUSION_POLICY
        } else {
            "none"
        }
        .to_owned(),
        fusion_candidate_pool: if matches!(mode, Mode::Hybrid) {
            "bm25_cosine_union"
        } else {
            "single_retriever"
        }
        .to_owned(),
        rerank,
        // Keep schema 2 while making the exact input policy self-describing in its existing field.
        // The fusion gate compares this value, so unlike contexts cannot be mistaken as comparable.
        rerank_policy: if rerank {
            crate::rerank::policy_id(rerank_context)?
        } else {
            "none".to_owned()
        },
        score_floor: false,
        exact_requested: exact,
        vector_backend: vector_backend.to_owned(),
        automatic_exact_cutoff: vector::EXACT_SCAN_CUTOFF,
        score_kind: score_kind(mode, rerank, relevance).to_owned(),
        result_policy: RESULT_POLICY.to_owned(),
        note_level: true,
        adaptive_cutoff: false,
        index_refresh: false,
        cwd_scope: false,
    };
    let initial_snapshot = index_snapshot(&db, &files)?;
    let provenance = Provenance {
        vagus_version: env!("CARGO_PKG_VERSION").to_owned(),
        binary_sha256: executable_fingerprint()?,
        labels_sha256: sha256_hex(label_content.as_bytes()),
        index: initial_snapshot.clone(),
    };

    let mut reports = Vec::with_capacity(labels.len());
    for label in &labels {
        let (hits, _elided, _query_meta) = search::query(
            cfg,
            &label.query,
            mode,
            k,
            &Scope::none(),
            false, // full bodies are not part of the report
            rerank,
            rerank_context,
            None, // no frontmatter time filter
            None, // no source filter
            exact,
            false, // timings
            false, // note-level results, not chunks
            false, // no score floor; preserve the complete requested prefix
        )?;
        let ranked: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();
        let unique: HashSet<&str> = ranked.iter().map(String::as_str).collect();
        if unique.len() != ranked.len() {
            bail!("internal eval error: note-level search returned duplicate paths");
        }
        let top_score = if relevance {
            hits.first().and_then(|hit| hit.relevance).map(f64::from)
        } else {
            hits.first().map(|hit| f64::from(hit.score))
        };
        if top_score.is_some_and(|score| !score.is_finite()) {
            bail!(
                "search returned a non-finite top score for query {:?}",
                label.query
            );
        }
        reports.push(score_query(label, ranked, top_score, k));
    }

    // There is intentionally no daemon, but another shell could run `vagus index` during a long
    // model-backed eval. Re-fingerprint after all queries and refuse a mixed-generation report.
    let final_db = Db::open(&cfg.db_path())?;
    let final_files = final_db.existing_files()?;
    let final_snapshot = index_snapshot(&final_db, &final_files)?;
    ensure_index_unchanged(&initial_snapshot, &final_snapshot)?;

    let aggregate = aggregate(&reports);
    Ok(EvalReport {
        schema_version: REPORT_SCHEMA_VERSION,
        config,
        provenance,
        queries: reports,
        aggregate,
    })
}

/// Score the current index and print either the stable JSON report or a human table. This command
/// never refreshes the index; run `vagus index` explicitly before taking a baseline.
#[allow(clippy::too_many_arguments)]
pub fn run(
    cfg: &Config,
    labels: &Path,
    k: usize,
    mode: Mode,
    rerank: bool,
    rerank_context: usize,
    exact: bool,
    relevance: bool,
    json: bool,
) -> Result<()> {
    let content = std::fs::read_to_string(labels)
        .with_context(|| format!("reading label file {}", labels.display()))?;
    let report = evaluate(
        cfg,
        &content,
        k,
        mode,
        rerank,
        rerank_context,
        exact,
        relevance,
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_table(&report, labels);
    }
    Ok(())
}

/// Compare two schema-compatible `vagus eval --json` reports under ADR 0025's fixed promotion gate.
pub fn run_gate(baseline: &Path, candidate: &Path, json: bool) -> Result<()> {
    gate::run(baseline, candidate, json)
}

pub(crate) fn parse_positive_k(raw: &str) -> std::result::Result<usize, String> {
    let k: usize = raw.parse().map_err(|_| "k must be an integer".to_owned())?;
    if !(1..=MAX_EVAL_K).contains(&k) {
        Err(format!("k must be between 1 and {MAX_EVAL_K}"))
    } else {
        Ok(k)
    }
}

fn display_metric(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| format!("{v:.3}"))
}

fn print_table(report: &EvalReport, labels: &Path) {
    let c = &report.config;
    let a = &report.aggregate;
    println!(
        "vagus eval — k={} mode={} rerank={} ({}) exact={} backend={} policy={}",
        c.k,
        c.mode,
        c.rerank,
        c.rerank_policy,
        c.exact_requested,
        c.vector_backend,
        c.result_policy
    );
    println!(
        "labels: {}  ({} queries: {} positive, {} negative)\n",
        labels.display(),
        report.queries.len(),
        a.n_positive,
        a.n_negative
    );
    println!(
        "{:<44} {:>5} {:>5} {:>6} {:>6} {:>7} {:>5}",
        "QUERY", "P@k", "R@k", "RR@k", "nDCG", "top", "miss"
    );
    println!("{}", "-".repeat(82));
    for q in &report.queries {
        println!(
            "{:<44} {:>5} {:>5} {:>6} {:>6} {:>7} {:>5}",
            truncate(&q.query, 44),
            display_metric(q.precision_at_k),
            display_metric(q.recall_at_k),
            display_metric(q.reciprocal_rank_at_k),
            display_metric(q.ndcg_at_k),
            display_metric(q.top_score),
            if q.is_negative {
                "-".to_owned()
            } else {
                q.missing.len().to_string()
            }
        );
    }

    println!("\nAggregate");
    println!(
        "  mean P@{:<15}: {}",
        c.k,
        display_metric(a.mean_precision_at_k)
    );
    println!(
        "  mean R@{:<15}: {}",
        c.k,
        display_metric(a.mean_recall_at_k)
    );
    println!("  MRR@{:<19}: {}", c.k, display_metric(a.mrr_at_k));
    println!(
        "  mean nDCG@{:<12}: {}  ({} graded positives)",
        c.k,
        display_metric(a.mean_ndcg_at_k),
        a.n_graded
    );
    println!(
        "  top-score diagnostic: positive {}  negative {}  delta {}  ({})",
        display_metric(a.mean_top_score_positive),
        display_metric(a.mean_top_score_negative),
        display_metric(a.top_score_delta),
        c.score_kind
    );
    if c.score_kind == crate::relevance::POLICY {
        println!("  relevance is a bounded cosine heuristic, not a calibrated probability");
    } else {
        println!("  top scores are mode-specific diagnostics, not calibrated probabilities");
    }
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_owned();
    }
    let cut: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests;
