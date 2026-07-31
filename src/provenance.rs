//! Explicit, versioned search-run provenance for presentation ticks (ADR 0021/G25).
//!
//! Default search JSON stays the stable Hit array. The tier-2 skill opts into a wrapper carrying one
//! shared run identity plus per-hit ranks, then returns only the events it actually presented to
//! `vagus tick --events`. These records are descriptive, selection-biased local user data — never a
//! replacement for ADR 0024 qrels/eval.

use std::collections::HashSet;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::util::sha256_hex;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_EVENTS_PER_TICK: usize = 100;
pub const MAX_PROVENANCE_LIMIT: usize = 1_000;
pub const RESULT_POLICY: &str = "note_level_post_rerank_scope_truncate_v1";
pub const FUSION_CANDIDATE_POOL: &str = "bm25_cosine_union";

/// Shared identity/context for one explicitly instrumented search invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRunProvenance {
    pub schema_version: u32,
    /// Hash over effective pipeline/config and index-shape fields below. Corpus and run outcomes stay
    /// separate because reports group on corpus too and returned/elided counts naturally vary.
    pub pipeline_id: String,
    pub binary_version: String,
    pub binary_sha256: String,
    pub corpus_sha256: String,
    pub indexed_files: usize,
    pub indexed_chunks: usize,
    pub embedded_chunks: usize,
    pub embed_model: String,
    pub embed_dims: usize,
    pub chunk_version: String,
    pub tantivy_version: String,
    pub fusion_policy: String,
    pub fusion_candidate_pool: String,
    pub vector_backend: String,
    pub exact_requested: bool,
    pub automatic_exact_cutoff: usize,
    pub rerank_model: String,
    pub rerank_policy: String,
    pub relevance_policy: String,
    /// Requested per-source depth, requested post-RRF depth, then the actual hydrated fused pool.
    pub source_limit: usize,
    pub fusion_limit: usize,
    pub candidate_pool: usize,
    /// Number of candidates actually cross-encoder scored.
    pub rerank_cap: usize,
    pub limit: usize,
    pub returned: usize,
    pub full_body: bool,
    pub note_level: bool,
    pub metadata_filters: bool,
    pub cwd_scope: bool,
    /// SHA-256 of normalized exclusion words; raw scope content is never stored.
    pub scope_policy: String,
    pub scope_elided: usize,
    pub index_refresh_requested: bool,
    pub index_refresh_succeeded: bool,
    pub result_policy: String,
}

impl SearchRunProvenance {
    fn identity_material(&self) -> String {
        // Length-prefix variable strings so no delimiter choice can alias two identities.
        let mut out = String::new();
        let mut field = |value: &str| {
            out.push_str(&value.len().to_string());
            out.push(':');
            out.push_str(value);
            out.push('|');
        };
        field(&self.schema_version.to_string());
        field(&self.binary_version);
        field(&self.binary_sha256);
        field(&self.indexed_files.to_string());
        field(&self.indexed_chunks.to_string());
        field(&self.embedded_chunks.to_string());
        field(&self.embed_model);
        field(&self.embed_dims.to_string());
        field(&self.chunk_version);
        field(&self.tantivy_version);
        field(&self.fusion_policy);
        field(&self.fusion_candidate_pool);
        field(&self.vector_backend);
        field(&self.exact_requested.to_string());
        field(&self.automatic_exact_cutoff.to_string());
        field(&self.rerank_model);
        field(&self.rerank_policy);
        field(&self.relevance_policy);
        field(&self.source_limit.to_string());
        field(&self.fusion_limit.to_string());
        field(&self.candidate_pool.to_string());
        field(&self.rerank_cap.to_string());
        field(&self.limit.to_string());
        field(&self.full_body.to_string());
        field(&self.note_level.to_string());
        field(&self.metadata_filters.to_string());
        field(&self.cwd_scope.to_string());
        field(&self.scope_policy);
        field(&self.index_refresh_requested.to_string());
        field(&self.index_refresh_succeeded.to_string());
        field(&self.result_policy);
        out
    }

    pub fn expected_pipeline_id(&self) -> String {
        sha256_hex(self.identity_material().as_bytes())
    }

    pub fn set_pipeline_id(&mut self) {
        self.pipeline_id = self.expected_pipeline_id();
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "unsupported tick provenance schema {} (expected {SCHEMA_VERSION})",
            self.schema_version
        );
        ensure!(is_sha256(&self.pipeline_id), "invalid pipeline_id");
        ensure!(
            self.pipeline_id == self.expected_pipeline_id(),
            "pipeline_id does not match the supplied run configuration"
        );
        ensure!(is_sha256(&self.binary_sha256), "invalid binary_sha256");
        ensure!(is_sha256(&self.corpus_sha256), "invalid corpus_sha256");
        ensure!(is_sha256(&self.scope_policy), "invalid scope_policy");
        for (name, value) in [
            ("binary_version", self.binary_version.as_str()),
            ("embed_model", self.embed_model.as_str()),
            ("chunk_version", self.chunk_version.as_str()),
            ("tantivy_version", self.tantivy_version.as_str()),
            ("fusion_policy", self.fusion_policy.as_str()),
            ("fusion_candidate_pool", self.fusion_candidate_pool.as_str()),
            ("vector_backend", self.vector_backend.as_str()),
            ("rerank_model", self.rerank_model.as_str()),
            ("rerank_policy", self.rerank_policy.as_str()),
            ("relevance_policy", self.relevance_policy.as_str()),
            ("result_policy", self.result_policy.as_str()),
        ] {
            ensure!(
                !value.is_empty() && value.len() <= 256,
                "{name} must contain 1..=256 bytes"
            );
        }
        ensure!(
            self.embed_model == crate::config::EMBED_MODEL
                && self.embed_dims == crate::config::EMBED_DIMS
                && self.chunk_version == crate::config::CHUNK_VERSION,
            "tick provenance embed/chunk identity does not match this binary"
        );
        ensure!(
            self.embedded_chunks <= self.indexed_chunks,
            "embedded_chunks exceeds indexed_chunks"
        );
        ensure!(
            (1..=MAX_PROVENANCE_LIMIT).contains(&self.limit),
            "limit must be between 1 and {MAX_PROVENANCE_LIMIT}"
        );
        let expected_fusion_limit = (self.limit * 4).max(crate::search::RERANK_POOL_MIN);
        ensure!(
            self.fusion_limit == expected_fusion_limit,
            "fusion_limit does not match the fixed note-level pool policy"
        );
        ensure!(
            self.fusion_limit <= usize::MAX / 3,
            "fusion_limit is too large for source-depth arithmetic"
        );
        ensure!(
            self.source_limit == (self.fusion_limit * 3).max(30),
            "source_limit does not match the fixed hybrid candidate policy"
        );
        ensure!(
            self.candidate_pool <= self.fusion_limit && self.candidate_pool <= self.indexed_chunks,
            "candidate_pool exceeds the requested/indexed pool"
        );
        ensure!(
            self.returned <= self.limit && self.returned <= self.candidate_pool,
            "returned exceeds limit or candidate pool"
        );
        ensure!(
            self.scope_elided <= self.limit && self.returned + self.scope_elided <= self.limit,
            "returned + scope_elided exceeds limit"
        );
        if self.candidate_pool == 0 {
            ensure!(
                self.rerank_cap == 0 && self.returned == 0,
                "an empty candidate pool must have zero cap and results"
            );
        } else {
            ensure!(
                self.rerank_cap > 0 && self.rerank_cap <= self.candidate_pool,
                "rerank_cap must be within the candidate pool"
            );
        }
        ensure!(
            self.rerank_cap == crate::search::rerank_cap(self.limit, self.candidate_pool, false),
            "rerank_cap does not match the fixed capped-prefix policy"
        );
        ensure!(
            self.index_refresh_requested || !self.index_refresh_succeeded,
            "index refresh cannot succeed when it was not requested"
        );
        ensure!(self.full_body, "tick provenance requires full-body search");
        ensure!(
            self.note_level,
            "tick provenance requires note-level search"
        );
        ensure!(
            !self.metadata_filters,
            "tick provenance schema 1 does not support metadata-filtered runs"
        );
        ensure!(self.exact_requested, "tick provenance requires --exact");
        ensure!(
            self.vector_backend == "exact",
            "tick provenance requires the exact vector backend"
        );
        ensure!(
            self.automatic_exact_cutoff == crate::vector::EXACT_SCAN_CUTOFF,
            "unexpected automatic exact cutoff"
        );
        ensure!(
            self.rerank_model == crate::rerank::MODEL_ID,
            "unexpected reranker identity"
        );
        ensure!(
            (0..=crate::rerank::MAX_CONTEXT_RADIUS).any(|radius| {
                crate::rerank::policy_id(radius).is_ok_and(|policy| policy == self.rerank_policy)
            }),
            "unsupported rerank policy {:?}",
            self.rerank_policy
        );
        ensure!(
            self.relevance_policy == crate::relevance::POLICY,
            "unexpected relevance policy"
        );
        ensure!(
            self.fusion_policy == crate::search::FUSION_POLICY,
            "unsupported fusion policy {:?}",
            self.fusion_policy
        );
        ensure!(
            self.fusion_candidate_pool == FUSION_CANDIDATE_POOL,
            "unsupported fusion candidate pool {:?}",
            self.fusion_candidate_pool
        );
        ensure!(
            self.result_policy == RESULT_POLICY,
            "unsupported result policy {:?}",
            self.result_policy
        );
        ensure!(
            self.rerank_policy != "none",
            "tick provenance requires reranking"
        );
        Ok(())
    }
}

/// Per-presented-hit ranks. `rerank_rank` exists only when the cross-encoder actually scored that
/// candidate; the unscored RRF tail still has an honest `final_rank` but never a fake rerank rank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitRankProvenance {
    /// Self-verifying binding of this rank tuple to one run, corpus, and returned note path.
    pub event_id: String,
    pub fusion_rank: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bm25_rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cosine_rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_rank: Option<usize>,
    pub final_rank: usize,
    pub rerank_scored: bool,
}

impl HitRankProvenance {
    pub fn expected_event_id(&self, run: &SearchRunProvenance, path: &str) -> String {
        let mut bytes = Vec::new();
        for value in [run.pipeline_id.as_str(), run.corpus_sha256.as_str(), path] {
            bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        for value in [
            run.returned,
            run.scope_elided,
            self.fusion_rank,
            self.final_rank,
        ] {
            bytes.extend_from_slice(&(value as u64).to_le_bytes());
        }
        for value in [self.bm25_rank, self.cosine_rank, self.rerank_rank] {
            match value {
                Some(rank) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&(rank as u64).to_le_bytes());
                }
                None => bytes.push(0),
            }
        }
        bytes.push(u8::from(self.rerank_scored));
        sha256_hex(&bytes)
    }

    pub fn bind_to(&mut self, run: &SearchRunProvenance, path: &str) {
        self.event_id = self.expected_event_id(run, path);
    }

    pub fn validate_ranks(&self, run: &SearchRunProvenance) -> Result<()> {
        ensure!(
            (1..=run.candidate_pool).contains(&self.fusion_rank),
            "fusion_rank {} is outside candidate pool 1..={}",
            self.fusion_rank,
            run.candidate_pool
        );
        for (name, rank) in [
            ("bm25_rank", self.bm25_rank),
            ("cosine_rank", self.cosine_rank),
        ] {
            ensure!(
                rank.is_none_or(|value| (1..=run.source_limit).contains(&value)),
                "{name} must be within the requested source depth"
            );
        }
        ensure!(
            (1..=run.returned).contains(&self.final_rank),
            "final_rank {} is outside returned results 1..={}",
            self.final_rank,
            run.returned
        );
        if self.rerank_scored {
            ensure!(
                self.fusion_rank <= run.rerank_cap,
                "scored event came from beyond the rerank cap"
            );
            ensure!(
                self.rerank_rank
                    .is_some_and(|rank| (1..=run.rerank_cap).contains(&rank)),
                "scored event requires a rerank_rank within the cap"
            );
        } else {
            ensure!(
                self.fusion_rank > run.rerank_cap,
                "unscored event came from inside the rerank cap"
            );
            ensure!(
                self.rerank_rank.is_none(),
                "unscored event cannot have rerank_rank"
            );
        }
        Ok(())
    }

    pub fn validate_for_path(&self, run: &SearchRunProvenance, path: &str) -> Result<()> {
        self.validate_ranks(run)?;
        ensure!(is_sha256(&self.event_id), "invalid event_id");
        ensure!(
            self.event_id == self.expected_event_id(run, path),
            "event_id does not bind these ranks to path {path:?} and the supplied run"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentedEvent {
    pub path: String,
    pub provenance: HitRankProvenance,
}

/// Payload accepted by `vagus tick --events`: copy `run` from the instrumented search response and
/// include only the event objects for notes the agent actually cited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TickEventBatch {
    pub run: SearchRunProvenance,
    pub events: Vec<PresentedEvent>,
}

impl TickEventBatch {
    pub fn validate(&self) -> Result<()> {
        self.run.validate()?;
        ensure!(
            !self.events.is_empty() && self.events.len() <= MAX_EVENTS_PER_TICK,
            "events must contain 1..={MAX_EVENTS_PER_TICK} presented notes"
        );
        ensure!(
            self.events.len() <= self.run.returned,
            "more events than returned search hits"
        );
        let mut paths = HashSet::new();
        let mut fusion_ranks = HashSet::new();
        let mut bm25_ranks = HashSet::new();
        let mut cosine_ranks = HashSet::new();
        let mut rerank_ranks = HashSet::new();
        let mut final_ranks = HashSet::new();
        for event in &self.events {
            ensure!(
                paths.insert(event.path.as_str()),
                "duplicate event path {:?}",
                event.path
            );
            ensure!(
                fusion_ranks.insert(event.provenance.fusion_rank),
                "duplicate fusion_rank {}",
                event.provenance.fusion_rank
            );
            for (name, rank, ranks) in [
                ("bm25_rank", event.provenance.bm25_rank, &mut bm25_ranks),
                (
                    "cosine_rank",
                    event.provenance.cosine_rank,
                    &mut cosine_ranks,
                ),
                (
                    "rerank_rank",
                    event.provenance.rerank_rank,
                    &mut rerank_ranks,
                ),
            ] {
                ensure!(
                    rank.is_none_or(|rank| ranks.insert(rank)),
                    "duplicate {name}"
                );
            }
            ensure!(
                final_ranks.insert(event.provenance.final_rank),
                "duplicate final_rank {}",
                event.provenance.final_rank
            );
            event.provenance.validate_for_path(&self.run, &event.path)?;
        }
        Ok(())
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> SearchRunProvenance {
        let mut run = SearchRunProvenance {
            schema_version: SCHEMA_VERSION,
            pipeline_id: String::new(),
            binary_version: "0.10.0".into(),
            binary_sha256: "a".repeat(64),
            corpus_sha256: "b".repeat(64),
            indexed_files: 10,
            indexed_chunks: 30,
            embedded_chunks: 30,
            embed_model: "google/embeddinggemma-300m".into(),
            embed_dims: 768,
            chunk_version: "5".into(),
            tantivy_version: "0.26".into(),
            fusion_policy: crate::search::FUSION_POLICY.into(),
            fusion_candidate_pool: FUSION_CANDIDATE_POOL.into(),
            vector_backend: "exact".into(),
            exact_requested: true,
            automatic_exact_cutoff: 10_000,
            rerank_model: "jinaai/jina-reranker-v1-turbo-en".into(),
            rerank_policy: "capped_prefix_context_0_tokenizer_max_512".into(),
            relevance_policy: crate::relevance::POLICY.into(),
            source_limit: 120,
            fusion_limit: 40,
            candidate_pool: 30,
            rerank_cap: 20,
            limit: 10,
            returned: 10,
            full_body: true,
            note_level: true,
            metadata_filters: false,
            cwd_scope: true,
            scope_policy: "c".repeat(64),
            scope_elided: 0,
            index_refresh_requested: true,
            index_refresh_succeeded: true,
            result_policy: RESULT_POLICY.into(),
        };
        run.set_pipeline_id();
        run
    }

    #[test]
    fn pipeline_identity_is_self_verifying_and_config_sensitive() {
        let run = run();
        run.validate().unwrap();
        let mut changed = run.clone();
        changed.rerank_cap += 1;
        assert_ne!(changed.expected_pipeline_id(), run.pipeline_id);
        assert!(changed.validate().is_err());
        changed.set_pipeline_id();
        assert!(changed.validate().is_err(), "invalid cap cannot self-bless");

        let mut invalid_depth = run.clone();
        invalid_depth.source_limit += 1;
        invalid_depth.set_pipeline_id();
        assert!(invalid_depth.validate().is_err());

        let mut new_version = run.clone();
        new_version.binary_version = "different".into();
        assert_ne!(new_version.expected_pipeline_id(), run.pipeline_id);

        let mut new_corpus = run.clone();
        new_corpus.corpus_sha256 = "d".repeat(64);
        assert_eq!(new_corpus.expected_pipeline_id(), run.pipeline_id);
        new_corpus.validate().unwrap();

        let mut new_scope = run.clone();
        new_scope.scope_policy = "e".repeat(64);
        assert_ne!(new_scope.expected_pipeline_id(), run.pipeline_id);
    }

    #[test]
    fn scored_and_unscored_rank_states_are_strict_and_path_bound() {
        let run = run();
        let mut scored = HitRankProvenance {
            event_id: String::new(),
            fusion_rank: 12,
            bm25_rank: Some(8),
            cosine_rank: Some(14),
            rerank_rank: Some(2),
            final_rank: 1,
            rerank_scored: true,
        };
        scored.bind_to(&run, "a.md");
        scored.validate_for_path(&run, "a.md").unwrap();
        assert!(scored.validate_for_path(&run, "other.md").is_err());
        let mut altered_outcome = run.clone();
        altered_outcome.returned -= 1;
        altered_outcome.validate().unwrap();
        assert!(scored.validate_for_path(&altered_outcome, "a.md").is_err());

        let mut tail = HitRankProvenance {
            event_id: String::new(),
            fusion_rank: 24,
            bm25_rank: None,
            cosine_rank: Some(24),
            rerank_rank: None,
            final_rank: 8,
            rerank_scored: false,
        };
        tail.bind_to(&run, "tail.md");
        tail.validate_for_path(&run, "tail.md").unwrap();

        let mut impossible = tail.clone();
        impossible.rerank_rank = Some(1);
        assert!(impossible.validate_ranks(&run).is_err());
        let mut impossible = scored;
        impossible.fusion_rank = 21;
        assert!(impossible.validate_ranks(&run).is_err());
    }

    #[test]
    fn event_batch_rejects_duplicate_paths_and_ranks() {
        let run = run();
        let mut provenance = HitRankProvenance {
            event_id: String::new(),
            fusion_rank: 1,
            bm25_rank: Some(1),
            cosine_rank: Some(2),
            rerank_rank: Some(1),
            final_rank: 1,
            rerank_scored: true,
        };
        provenance.bind_to(&run, "a.md");
        let event = PresentedEvent {
            path: "a.md".into(),
            provenance,
        };
        let batch = TickEventBatch {
            run,
            events: vec![event.clone()],
        };
        batch.validate().unwrap();
        let duplicate = TickEventBatch {
            run: batch.run.clone(),
            events: vec![event.clone(), event.clone()],
        };
        assert!(duplicate.validate().is_err());

        let mut duplicate_rank = event;
        duplicate_rank.path = "b.md".into();
        duplicate_rank
            .provenance
            .bind_to(&batch.run, &duplicate_rank.path);
        let duplicate_rank = TickEventBatch {
            run: batch.run,
            events: vec![duplicate.events[0].clone(), duplicate_rank],
        };
        assert!(duplicate_rank.validate().is_err());
    }
}
