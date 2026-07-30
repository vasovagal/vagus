use super::*;
use crate::chunk::Chunk;
use crate::lex::Lex;
use crate::util::testdir::TempDir;

fn ranked(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|s| (*s).to_owned()).collect()
}

fn relevant(entries: &[(&str, u8)]) -> HashMap<String, u8> {
    entries
        .iter()
        .map(|(path, grade)| ((*path).to_owned(), *grade))
        .collect()
}

#[test]
fn precision_at_k_uses_fixed_denominator_and_penalizes_underfill() {
    let rel = relevant(&[("a", 1), ("b", 1), ("c", 1)]);
    assert_eq!(precision_at_k(&ranked(&["a", "x", "b", "y"]), &rel, 4), 0.5);
    assert_eq!(precision_at_k(&ranked(&["a", "x", "b", "y"]), &rel, 2), 0.5);
    assert_eq!(precision_at_k(&ranked(&["a", "b"]), &rel, 5), 0.4);
    assert_eq!(precision_at_k(&[], &rel, 10), 0.0);
}

#[test]
fn recall_and_rr_are_truncated_at_k() {
    let rel = relevant(&[("a", 1), ("b", 1), ("c", 1)]);
    assert!((recall_at_k(&ranked(&["a", "x", "b"]), &rel, 3) - 2.0 / 3.0).abs() < 1e-12);
    assert_eq!(reciprocal_rank_at_k(&ranked(&["x", "a"]), &rel, 1), 0.0);
    assert_eq!(reciprocal_rank_at_k(&ranked(&["x", "a"]), &rel, 2), 0.5);
}

#[test]
fn ndcg_matches_hand_computed_definition() {
    let rel = relevant(&[("a", 3), ("b", 2), ("c", 1)]);
    assert!((ndcg_at_k(&ranked(&["a", "b", "c"]), &rel, 3) - 1.0).abs() < 1e-12);
    let got = ndcg_at_k(&ranked(&["a", "x", "b"]), &rel, 3);
    assert!((got - 0.904949).abs() < 1e-4, "nDCG={got}");
    assert!((dcg(&[3]) - 7.0).abs() < 1e-12);
}

#[test]
fn parser_accepts_binary_graded_and_explicit_negative_forms() {
    let binary = parse_label(r#"{"query":"q","relevant":["a.md","b.md"]}"#).unwrap();
    assert_eq!(binary.relevant.get("a.md"), Some(&1));
    assert!(!binary.has_grades);

    let graded = parse_label(
        r#"{"query":"q","relevant":[{"path":"a.md","grade":3},{"path":"b.md","grade":0}]}"#,
    )
    .unwrap();
    assert_eq!(graded.relevant.get("a.md"), Some(&3));
    assert!(!graded.relevant.contains_key("b.md"));
    assert!(graded.has_grades);
    assert_eq!(graded.judged_paths, ["a.md", "b.md"]);

    let negative = parse_label(r#"{"query":"none","relevant":[]}"#).unwrap();
    assert!(negative.is_negative());
}

#[test]
fn parser_rejects_ambiguous_or_invalid_labels() {
    for bad in [
        r#"{"query":"","relevant":[]}"#,
        r#"{"query":"q"}"#,
        r#"{"query":"q","relevent":[]}"#,
        r#"{"query":"q","relevant":["../a.md"]}"#,
        r#"{"query":"q","relevant":["a.txt"]}"#,
        r#"{"query":"q","relevant":["a.md","a.md"]}"#,
        r#"{"query":"q","relevant":[{"path":"a.md","grade":4}]}"#,
        r#"{"query":"q","relevant":[{"path":"a.md","grade":1,"typo":2}]}"#,
    ] {
        assert!(parse_label(bad).is_err(), "unexpectedly accepted {bad}");
    }
    assert!(parse_labels("\n").is_err());
    assert!(
        parse_labels("{\"query\":\"q\",\"relevant\":[]}\n{\"query\":\"q\",\"relevant\":[]}")
            .is_err()
    );
    assert!(parse_positive_k("0").is_err());
    assert!(parse_positive_k("1001").is_err());
    assert_eq!(parse_positive_k("7").unwrap(), 7);
}

#[test]
fn negative_metrics_and_missing_cohorts_are_null_not_fabricated_zeroes() {
    let label = parse_label(r#"{"query":"none","relevant":[]}"#).unwrap();
    let query = score_query(&label, vec![], None, 10);
    assert_eq!(query.precision_at_k, None);
    assert_eq!(query.reciprocal_rank_at_k, None);
    let a = aggregate(&[query]);
    assert_eq!(a.mean_precision_at_k, None);
    assert_eq!(a.mean_top_score_negative, None);
    assert_eq!(a.top_score_delta, None);
}

fn fixture() -> (TempDir, Config) {
    let root = TempDir::new("eval-runner");
    let cfg = Config {
        vault: root.path().join("vault"),
        data_dir: root.path().join("data"),
        cache_dir: root.path().join("cache"),
    };
    std::fs::create_dir_all(&cfg.vault).unwrap();
    let db = Db::open(&cfg.db_path()).unwrap();
    let lex = Lex::open(&cfg.tantivy_dir()).unwrap();
    let mut writer = lex.writer().unwrap();
    for (path, word) in [("a.md", "alpha"), ("b.md", "beta")] {
        let id = sha256_hex(format!("{path}#0").as_bytes());
        let chunk = Chunk {
            id,
            ord: 0,
            heading_path: word.to_owned(),
            body: format!("{word} answer"),
        };
        db.upsert_file(path, 1.0, &sha256_hex(word.as_bytes()), 1)
            .unwrap();
        db.replace_chunks(path, std::slice::from_ref(&chunk), Some(1), None)
            .unwrap();
        lex.replace_file(&writer, path, &[chunk]).unwrap();
    }
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    (root, cfg)
}

#[test]
fn bm25_runner_uses_current_index_and_emits_reproducible_contract() {
    let (_root, cfg) = fixture();
    let labels = concat!(
        "{\"query\":\"alpha\",\"relevant\":[\"a.md\"]}\n",
        "{\"query\":\"beta\",\"relevant\":[]}\n"
    );
    let report = evaluate(&cfg, labels, 2, Mode::Bm25, false, false).unwrap();
    assert_eq!(report.schema_version, 1);
    assert_eq!(report.config.result_policy, RESULT_POLICY);
    assert_eq!(report.config.vector_backend, "none");
    assert_eq!(report.config.rerank_policy, "none");
    assert!(!report.config.score_floor);
    assert!(!report.config.adaptive_cutoff);
    assert!(!report.config.index_refresh);
    assert_eq!(report.provenance.index.indexed_files, 2);
    assert_eq!(report.provenance.binary_sha256.len(), 64);
    assert_eq!(
        report.provenance.labels_sha256,
        sha256_hex(labels.as_bytes())
    );
    assert_eq!(
        report.queries[0].ranked.first().map(String::as_str),
        Some("a.md")
    );
    assert_eq!(report.queries[0].precision_at_k, Some(0.5));
    assert_eq!(report.queries[0].reciprocal_rank_at_k, Some(1.0));
    assert_eq!(report.aggregate.mrr_at_k, Some(1.0));
    assert_eq!(report.aggregate.n_positive, 1);
    assert_eq!(report.aggregate.n_negative, 1);

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(
        json["queries"][1]["precision_at_k"],
        serde_json::Value::Null
    );
    assert_eq!(json["config"]["score_kind"], "bm25");
    assert!(json["provenance"]["index"]["corpus_sha256"].is_string());

    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }
    assert_eq!(
        keys(&json),
        [
            "aggregate",
            "config",
            "provenance",
            "queries",
            "schema_version"
        ]
    );
    assert_eq!(
        keys(&json["config"]),
        [
            "adaptive_cutoff",
            "automatic_exact_cutoff",
            "cwd_scope",
            "exact_requested",
            "index_refresh",
            "k",
            "mode",
            "note_level",
            "rerank",
            "rerank_policy",
            "result_policy",
            "score_floor",
            "score_kind",
            "vector_backend",
        ]
    );
    assert_eq!(
        keys(&json["provenance"]),
        ["binary_sha256", "index", "labels_sha256", "vagus_version"]
    );
    assert_eq!(
        keys(&json["provenance"]["index"]),
        [
            "chunk_version",
            "corpus_sha256",
            "embed_dims",
            "embed_model",
            "embedded_chunks",
            "indexed_chunks",
            "indexed_files",
            "tantivy_version",
            "vec_backend",
            "vec_index_version",
        ]
    );
    assert_eq!(
        keys(&json["aggregate"]),
        [
            "mean_ndcg_at_k",
            "mean_precision_at_k",
            "mean_recall_at_k",
            "mean_top_score_negative",
            "mean_top_score_positive",
            "mrr_at_k",
            "n_graded",
            "n_negative",
            "n_negative_with_top_score",
            "n_positive",
            "n_positive_with_top_score",
            "top_score_delta",
        ]
    );
    assert_eq!(
        keys(&json["queries"][0]),
        [
            "found",
            "is_negative",
            "missing",
            "ndcg_at_k",
            "precision_at_k",
            "query",
            "ranked",
            "recall_at_k",
            "reciprocal_rank_at_k",
            "returned",
            "top_score",
        ]
    );
}

#[test]
fn changed_index_snapshot_is_rejected() {
    let (_root, cfg) = fixture();
    let db = Db::open(&cfg.db_path()).unwrap();
    let files = db.existing_files().unwrap();
    let initial = index_snapshot(&db, &files).unwrap();
    let mut changed = initial.clone();
    changed.indexed_chunks += 1;
    assert!(ensure_index_unchanged(&initial, &changed).is_err());
    assert!(ensure_index_unchanged(&initial, &initial).is_ok());
}

#[test]
fn runner_rejects_unknown_qrel_path_and_zero_k() {
    let (_root, cfg) = fixture();
    let unknown = "{\"query\":\"alpha\",\"relevant\":[\"missing.md\"]}\n";
    assert!(evaluate(&cfg, unknown, 10, Mode::Bm25, false, false).is_err());
    let db = Db::open(&cfg.db_path()).unwrap();
    db.upsert_file("empty.md", 1.0, "empty", 1).unwrap();
    let empty = "{\"query\":\"empty\",\"relevant\":[\"empty.md\"]}\n";
    assert!(evaluate(&cfg, empty, 10, Mode::Bm25, false, false).is_err());
    let known = "{\"query\":\"alpha\",\"relevant\":[\"a.md\"]}\n";
    assert!(evaluate(&cfg, known, 0, Mode::Bm25, false, false).is_err());
    assert!(evaluate(&cfg, known, 10, Mode::Bm25, false, true).is_err());
}
