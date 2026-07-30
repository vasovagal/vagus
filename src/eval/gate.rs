//! Executable ADR 0025 fusion-promotion gate over two `vagus eval --json` reports.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{EvalReport, QueryReport};

const GATE_SCHEMA_VERSION: u32 = 1;
const CONTRACT: &str = "adr0025-fusion-v1";
const REQUIRED_K: usize = 10;
const MIN_POSITIVE_QUERIES: usize = 20;
const MIN_COHORTS: usize = 4;
const MIN_QUERIES_PER_COHORT: usize = 3;
const MIN_MEAN_NDCG_GAIN: f64 = 0.01;
const MAX_MEAN_MRR_LOSS: f64 = 0.005;
const MAX_MEAN_PRECISION_LOSS: f64 = 0.005;
const MAX_COHORT_NDCG_LOSS: f64 = 0.01;
const BOOTSTRAP_SAMPLES: usize = 10_000;
const BOOTSTRAP_LOWER_QUANTILE: f64 = 0.025;
const EPSILON: f64 = 1e-12;

#[derive(Debug, Serialize)]
struct Thresholds {
    required_k: usize,
    min_positive_queries: usize,
    min_cohorts: usize,
    min_queries_per_cohort: usize,
    min_mean_ndcg_gain: f64,
    bootstrap_samples: usize,
    bootstrap_lower_quantile: f64,
    min_bootstrap_lower_bound: f64,
    max_mean_mrr_loss: f64,
    max_mean_precision_loss: f64,
    max_mean_recall_loss: f64,
    max_per_query_recall_loss: f64,
    max_cohort_ndcg_loss: f64,
}

#[derive(Debug, Serialize)]
struct MetricDelta {
    baseline: Option<f64>,
    candidate: Option<f64>,
    delta: Option<f64>,
}

impl MetricDelta {
    fn new(baseline: Option<f64>, candidate: Option<f64>) -> Self {
        Self {
            baseline,
            candidate,
            delta: baseline.zip(candidate).map(|(b, c)| c - b),
        }
    }
}

#[derive(Debug, Serialize)]
struct Metrics {
    mean_ndcg_at_10: MetricDelta,
    mrr_at_10: MetricDelta,
    mean_precision_at_10: MetricDelta,
    mean_recall_at_10: MetricDelta,
    paired_ndcg_bootstrap_lower_95: Option<f64>,
    worst_cohort_ndcg_delta: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
struct GateReport {
    schema_version: u32,
    contract: &'static str,
    accepted: bool,
    baseline_binary_sha256: String,
    candidate_binary_sha256: String,
    baseline_fusion_policy: String,
    candidate_fusion_policy: String,
    positive_queries: usize,
    cohorts: BTreeMap<String, usize>,
    thresholds: Thresholds,
    metrics: Metrics,
    checks: Vec<Check>,
}

fn load(path: &Path) -> Result<EvalReport> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading eval report {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing eval report {}", path.display()))
}

fn validate_comparable(baseline: &EvalReport, candidate: &EvalReport) -> Result<()> {
    if baseline.schema_version != super::REPORT_SCHEMA_VERSION
        || candidate.schema_version != super::REPORT_SCHEMA_VERSION
    {
        bail!(
            "fusion gate requires eval schema {}; got baseline {} and candidate {}",
            super::REPORT_SCHEMA_VERSION,
            baseline.schema_version,
            candidate.schema_version
        );
    }
    if baseline.provenance.labels_sha256 != candidate.provenance.labels_sha256 {
        bail!("reports use different label files (labels_sha256 mismatch)");
    }
    if baseline.provenance.index != candidate.provenance.index {
        bail!("reports use different corpus/index snapshots");
    }

    let mut candidate_config = candidate.config.clone();
    candidate_config.fusion_policy = baseline.config.fusion_policy.clone();
    candidate_config.score_kind = baseline.config.score_kind.clone();
    if baseline.config != candidate_config {
        bail!("eval configs differ in fields other than fusion_policy/score_kind");
    }
    if baseline.queries.len() != candidate.queries.len() {
        bail!("reports contain different query counts");
    }
    for (index, (before, after)) in baseline.queries.iter().zip(&candidate.queries).enumerate() {
        if before.query != after.query
            || before.cohort != after.cohort
            || before.is_negative != after.is_negative
        {
            bail!("query identity mismatch at report position {}", index + 1);
        }
    }
    Ok(())
}

fn check(name: &'static str, passed: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        passed,
        detail: detail.into(),
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn metric_delta(
    baseline: &[&QueryReport],
    candidate: &[&QueryReport],
    field: impl Fn(&QueryReport) -> Option<f64>,
) -> MetricDelta {
    let before: Vec<f64> = baseline.iter().filter_map(|query| field(query)).collect();
    let after: Vec<f64> = candidate.iter().filter_map(|query| field(query)).collect();
    let baseline_mean = (before.len() == baseline.len())
        .then(|| mean(&before))
        .flatten();
    let candidate_mean = (after.len() == candidate.len())
        .then(|| mean(&after))
        .flatten();
    MetricDelta::new(baseline_mean, candidate_mean)
}

fn xorshift64(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn bootstrap_lower_bound(differences: &[f64], labels_sha256: &str) -> Option<f64> {
    if differences.is_empty() || differences.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let seed_hex = labels_sha256.get(..16)?;
    let mut state = u64::from_str_radix(seed_hex, 16).ok()?.max(1);
    let mut samples = Vec::with_capacity(BOOTSTRAP_SAMPLES);
    for _ in 0..BOOTSTRAP_SAMPLES {
        let mut total = 0.0;
        for _ in 0..differences.len() {
            let index = (xorshift64(&mut state) as usize) % differences.len();
            total += differences[index];
        }
        samples.push(total / differences.len() as f64);
    }
    samples.sort_by(f64::total_cmp);
    let index = ((BOOTSTRAP_SAMPLES as f64 * BOOTSTRAP_LOWER_QUANTILE).floor() as usize)
        .min(samples.len() - 1);
    Some(samples[index])
}

fn compare(baseline: &EvalReport, candidate: &EvalReport) -> Result<GateReport> {
    validate_comparable(baseline, candidate)?;

    let before: Vec<&QueryReport> = baseline.queries.iter().filter(|q| !q.is_negative).collect();
    let after: Vec<&QueryReport> = candidate
        .queries
        .iter()
        .filter(|q| !q.is_negative)
        .collect();
    let ndcg = metric_delta(&before, &after, |q| q.ndcg_at_k);
    let mrr = metric_delta(&before, &after, |q| q.reciprocal_rank_at_k);
    let precision = metric_delta(&before, &after, |q| q.precision_at_k);
    let recall = metric_delta(&before, &after, |q| q.recall_at_k);

    let paired_ndcg: Option<Vec<f64>> = before
        .iter()
        .zip(&after)
        .map(|(b, c)| Some(c.ndcg_at_k? - b.ndcg_at_k?))
        .collect();
    let bootstrap = paired_ndcg.as_deref().and_then(|differences| {
        bootstrap_lower_bound(differences, &baseline.provenance.labels_sha256)
    });

    let mut cohorts: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut all_have_cohorts = true;
    for (b, c) in before.iter().zip(&after) {
        match (&b.cohort, b.ndcg_at_k, c.ndcg_at_k) {
            (Some(cohort), Some(b), Some(c)) => {
                cohorts.entry(cohort.clone()).or_default().push(c - b)
            }
            _ => all_have_cohorts = false,
        }
    }
    let cohort_counts: BTreeMap<String, usize> = cohorts
        .iter()
        .map(|(name, values)| (name.clone(), values.len()))
        .collect();
    let cohort_deltas: Vec<f64> = cohorts.values().filter_map(|values| mean(values)).collect();
    let worst_cohort = cohort_deltas.iter().copied().min_by(f64::total_cmp);

    let same_exact_gate_config = baseline.config.k == REQUIRED_K
        && baseline.config.mode == "hybrid"
        && !baseline.config.rerank
        && baseline.config.exact_requested
        && baseline.config.vector_backend == "exact"
        && baseline.config.fusion_candidate_pool == "bm25_cosine_union"
        && !baseline.config.adaptive_cutoff
        && baseline.config.result_policy == super::RESULT_POLICY;
    let every_cohort_large_enough = cohort_counts
        .values()
        .all(|count| *count >= MIN_QUERIES_PER_COHORT);
    let recall_losses: Vec<f64> = before
        .iter()
        .zip(&after)
        .filter_map(|(b, c)| Some(c.recall_at_k? - b.recall_at_k?))
        .collect();
    let no_query_recall_loss =
        recall_losses.len() == before.len() && recall_losses.iter().all(|delta| *delta >= -EPSILON);

    let checks = vec![
        check(
            "fixed_exact_hybrid_config",
            same_exact_gate_config,
            format!(
                "requires k=10, hybrid, no rerank, explicit exact, exact backend, same source union, pre-tidy; got k={}, mode={}, rerank={}, exact={}, backend={}, pool={}, policy={}",
                baseline.config.k,
                baseline.config.mode,
                baseline.config.rerank,
                baseline.config.exact_requested,
                baseline.config.vector_backend,
                baseline.config.fusion_candidate_pool,
                baseline.config.result_policy
            ),
        ),
        check(
            "baseline_is_rrf_k60",
            baseline.config.fusion_policy == "rrf_k60",
            format!("baseline fusion_policy={}", baseline.config.fusion_policy),
        ),
        check(
            "candidate_is_distinct_fusion_policy",
            baseline.config.fusion_policy != candidate.config.fusion_policy,
            format!(
                "binary differs={} policy {} -> {} (one binary with explicit policies is allowed)",
                baseline.provenance.binary_sha256 != candidate.provenance.binary_sha256,
                baseline.config.fusion_policy,
                candidate.config.fusion_policy
            ),
        ),
        check(
            "held_out_sample_size",
            before.len() >= MIN_POSITIVE_QUERIES,
            format!(
                "{} positive queries (minimum {})",
                before.len(),
                MIN_POSITIVE_QUERIES
            ),
        ),
        check(
            "graded_diverse_cohorts",
            all_have_cohorts && cohort_counts.len() >= MIN_COHORTS && every_cohort_large_enough,
            format!(
                "{} cohorts, counts {:?}; require >= {} cohorts and >= {} queries each",
                cohort_counts.len(),
                cohort_counts,
                MIN_COHORTS,
                MIN_QUERIES_PER_COHORT
            ),
        ),
        check(
            "mean_ndcg_gain",
            ndcg.delta
                .is_some_and(|delta| delta + EPSILON >= MIN_MEAN_NDCG_GAIN),
            format!(
                "nDCG@10 delta {:?} (minimum +{MIN_MEAN_NDCG_GAIN:.3})",
                ndcg.delta
            ),
        ),
        check(
            "paired_ndcg_bootstrap",
            bootstrap.is_some_and(|lower| lower > 0.0),
            format!("paired bootstrap 95% lower bound {bootstrap:?} (must be > 0)"),
        ),
        check(
            "no_mean_recall_loss",
            recall.delta.is_some_and(|delta| delta >= -EPSILON),
            format!("mean R@10 delta {:?} (must be >= 0)", recall.delta),
        ),
        check(
            "no_per_query_recall_loss",
            no_query_recall_loss,
            "every positive query must preserve R@10",
        ),
        check(
            "bounded_mrr_loss",
            mrr.delta
                .is_some_and(|delta| delta + EPSILON >= -MAX_MEAN_MRR_LOSS),
            format!(
                "MRR@10 delta {:?} (floor -{MAX_MEAN_MRR_LOSS:.3})",
                mrr.delta
            ),
        ),
        check(
            "bounded_precision_loss",
            precision
                .delta
                .is_some_and(|delta| delta + EPSILON >= -MAX_MEAN_PRECISION_LOSS),
            format!(
                "mean P@10 delta {:?} (floor -{MAX_MEAN_PRECISION_LOSS:.3})",
                precision.delta
            ),
        ),
        check(
            "bounded_cohort_ndcg_loss",
            worst_cohort.is_some_and(|delta| delta + EPSILON >= -MAX_COHORT_NDCG_LOSS),
            format!(
                "worst cohort nDCG@10 delta {worst_cohort:?} (floor -{MAX_COHORT_NDCG_LOSS:.3})"
            ),
        ),
    ];
    let accepted = checks.iter().all(|check| check.passed);

    Ok(GateReport {
        schema_version: GATE_SCHEMA_VERSION,
        contract: CONTRACT,
        accepted,
        baseline_binary_sha256: baseline.provenance.binary_sha256.clone(),
        candidate_binary_sha256: candidate.provenance.binary_sha256.clone(),
        baseline_fusion_policy: baseline.config.fusion_policy.clone(),
        candidate_fusion_policy: candidate.config.fusion_policy.clone(),
        positive_queries: before.len(),
        cohorts: cohort_counts,
        thresholds: Thresholds {
            required_k: REQUIRED_K,
            min_positive_queries: MIN_POSITIVE_QUERIES,
            min_cohorts: MIN_COHORTS,
            min_queries_per_cohort: MIN_QUERIES_PER_COHORT,
            min_mean_ndcg_gain: MIN_MEAN_NDCG_GAIN,
            bootstrap_samples: BOOTSTRAP_SAMPLES,
            bootstrap_lower_quantile: BOOTSTRAP_LOWER_QUANTILE,
            min_bootstrap_lower_bound: 0.0,
            max_mean_mrr_loss: MAX_MEAN_MRR_LOSS,
            max_mean_precision_loss: MAX_MEAN_PRECISION_LOSS,
            max_mean_recall_loss: 0.0,
            max_per_query_recall_loss: 0.0,
            max_cohort_ndcg_loss: MAX_COHORT_NDCG_LOSS,
        },
        metrics: Metrics {
            mean_ndcg_at_10: ndcg,
            mrr_at_10: mrr,
            mean_precision_at_10: precision,
            mean_recall_at_10: recall,
            paired_ndcg_bootstrap_lower_95: bootstrap,
            worst_cohort_ndcg_delta: worst_cohort,
        },
        checks,
    })
}

fn print_human(report: &GateReport) {
    println!(
        "vagus eval-gate — {} ({})",
        if report.accepted { "ACCEPT" } else { "REJECT" },
        report.contract
    );
    println!(
        "fusion: {} -> {}; {} positive queries across {} cohorts",
        report.baseline_fusion_policy,
        report.candidate_fusion_policy,
        report.positive_queries,
        report.cohorts.len()
    );
    for check in &report.checks {
        println!(
            "  [{}] {:<36} {}",
            if check.passed { "ok" } else { "!!" },
            check.name,
            check.detail
        );
    }
}

pub(super) fn run(baseline_path: &Path, candidate_path: &Path, json: bool) -> Result<()> {
    let baseline = load(baseline_path)?;
    let candidate = load(candidate_path)?;
    let report = compare(&baseline, &candidate)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    if !report.accepted {
        bail!("fusion evidence gate rejected the candidate");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
