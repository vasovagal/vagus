use super::*;
use crate::eval::{
    Aggregate, EvalConfig, IndexSnapshot, Provenance, QueryReport, REPORT_SCHEMA_VERSION,
    RESULT_POLICY,
};

fn report(policy: &str, binary: &str, ndcg_delta: f64, recall_delta: f64) -> EvalReport {
    let mut queries = Vec::new();
    for i in 0..20 {
        let baseline_ndcg = 0.50 + (i % 3) as f64 * 0.01;
        queries.push(QueryReport {
            query: format!("query-{i}"),
            cohort: Some(format!("cohort-{}", i % 4)),
            is_negative: false,
            precision_at_k: Some(0.10),
            recall_at_k: Some(1.0 + recall_delta),
            reciprocal_rank_at_k: Some(0.80),
            ndcg_at_k: Some(baseline_ndcg + ndcg_delta),
            top_score: Some(0.03),
            returned: 10,
            ranked: vec![format!("note-{i}.md")],
            found: vec![format!("note-{i}.md")],
            missing: vec![],
        });
    }
    EvalReport {
        schema_version: REPORT_SCHEMA_VERSION,
        config: EvalConfig {
            k: 10,
            mode: "hybrid".to_owned(),
            fusion_policy: policy.to_owned(),
            fusion_candidate_pool: "bm25_cosine_union".to_owned(),
            rerank: false,
            rerank_policy: "none".to_owned(),
            score_floor: false,
            exact_requested: true,
            vector_backend: "exact".to_owned(),
            automatic_exact_cutoff: 10_000,
            score_kind: "rrf".to_owned(),
            result_policy: RESULT_POLICY.to_owned(),
            note_level: true,
            adaptive_cutoff: false,
            index_refresh: false,
            cwd_scope: false,
        },
        provenance: Provenance {
            vagus_version: "test".to_owned(),
            binary_sha256: binary.repeat(64),
            labels_sha256: "a".repeat(64),
            index: IndexSnapshot {
                corpus_sha256: "b".repeat(64),
                indexed_files: 20,
                indexed_chunks: 20,
                embedded_chunks: 20,
                embed_model: Some("model".to_owned()),
                embed_dims: Some("768".to_owned()),
                chunk_version: Some("5".to_owned()),
                tantivy_version: Some("0.26".to_owned()),
                vec_backend: Some("usearch".to_owned()),
                vec_index_version: Some("1".to_owned()),
            },
        },
        aggregate: Aggregate {
            mean_precision_at_k: Some(0.10),
            mean_recall_at_k: Some(1.0 + recall_delta),
            mrr_at_k: Some(0.80),
            mean_ndcg_at_k: Some(0.51 + ndcg_delta),
            n_positive: 20,
            n_negative: 0,
            n_graded: 20,
            n_positive_with_top_score: 20,
            n_negative_with_top_score: 0,
            mean_top_score_positive: Some(0.03),
            mean_top_score_negative: None,
            top_score_delta: None,
        },
        queries,
    }
}

#[test]
fn qualifying_paired_improvement_passes_every_check() {
    let baseline = report("rrf_k60", "1", 0.0, 0.0);
    let candidate = report("weighted_rrf_v1", "2", 0.02, 0.0);
    let result = compare(&baseline, &candidate).unwrap();
    assert!(result.accepted);
    assert!(result.checks.iter().all(|check| check.passed));
    assert_eq!(result.positive_queries, 20);
    assert_eq!(result.cohorts.len(), 4);
    assert!(result.metrics.paired_ndcg_bootstrap_lower_95.unwrap() > 0.0);
    let json = serde_json::to_value(&result).unwrap();
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "accepted",
            "baseline_binary_sha256",
            "baseline_fusion_policy",
            "candidate_binary_sha256",
            "candidate_fusion_policy",
            "checks",
            "cohorts",
            "contract",
            "metrics",
            "positive_queries",
            "schema_version",
            "thresholds",
        ]
    );
    let mut metric_keys: Vec<&str> = json["metrics"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    metric_keys.sort_unstable();
    assert_eq!(
        metric_keys,
        [
            "mean_ndcg_at_10",
            "mean_precision_at_10",
            "mean_recall_at_10",
            "mrr_at_10",
            "paired_ndcg_bootstrap_lower_95",
            "worst_cohort_ndcg_delta",
        ]
    );
    let mut check_keys: Vec<&str> = json["checks"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    check_keys.sort_unstable();
    assert_eq!(check_keys, ["detail", "name", "passed"]);
}

#[test]
fn recall_loss_and_ungraded_or_small_samples_are_rejected() {
    let baseline = report("rrf_k60", "1", 0.0, 0.0);
    let mut candidate = report("weighted_rrf_v1", "2", 0.02, -0.1);
    candidate.queries.truncate(8);
    let mut baseline_small = baseline;
    baseline_small.queries.truncate(8);
    baseline_small.queries[0].ndcg_at_k = None;
    candidate.queries[0].ndcg_at_k = None;
    let result = compare(&baseline_small, &candidate).unwrap();
    assert!(!result.accepted);
    assert!(
        result
            .checks
            .iter()
            .any(|check| check.name == "no_per_query_recall_loss" && !check.passed)
    );
    assert!(
        result
            .checks
            .iter()
            .any(|check| check.name == "held_out_sample_size" && !check.passed)
    );
    assert!(
        result
            .checks
            .iter()
            .any(|check| check.name == "graded_diverse_cohorts" && !check.passed)
    );
}

#[test]
fn mismatched_corpus_or_nonfusion_config_is_not_comparable() {
    let same_baseline = report("rrf_k60", "1", 0.0, 0.0);
    let same_policy = report("rrf_k60", "2", 0.02, 0.0);
    let result = compare(&same_baseline, &same_policy).unwrap();
    assert!(!result.accepted);
    assert!(
        result
            .checks
            .iter()
            .any(|check| { check.name == "candidate_is_distinct_fusion_policy" && !check.passed })
    );

    let baseline = report("rrf_k60", "1", 0.0, 0.0);
    let mut candidate = report("weighted_rrf_v1", "2", 0.02, 0.0);
    candidate.provenance.index.indexed_chunks += 1;
    assert!(compare(&baseline, &candidate).is_err());

    let baseline = report("rrf_k60", "1", 0.0, 0.0);
    let mut candidate = report("weighted_rrf_v1", "2", 0.02, 0.0);
    candidate.config.k = 20;
    assert!(compare(&baseline, &candidate).is_err());
}
