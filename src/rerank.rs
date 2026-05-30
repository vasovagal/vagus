//! In-core cross-encoder reranking via fastembed (`jina-reranker-v1-turbo-en`).
//!
//! A cross-encoder is a *scoring* model — the same category as the embedder, on the same `ort`/ONNX
//! stack vagus already links (no new heavy deps; G11/G13). It re-scores the fused RRF candidate pool
//! by reading the full chunk body against the query, a precision boost the rank-based RRF floor can't
//! give. This is the tier-1 reranker (offline, no Claude); see ADR 0015. RRF itself (G8) is untouched.
//!
//! Guardrail G6/G10: the model cache dir is set EXPLICITLY (same as the embedder).

use std::path::Path;

use anyhow::Result;
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

/// Cross-encoder input budget (tokens): a query plus a ~900-token chunk body, with headroom. The
/// model itself supports up to 8192.
const MAX_LENGTH: usize = 1024;

pub struct Reranker {
    model: TextRerank,
}

impl Reranker {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let opts = RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
            .with_cache_dir(cache_dir.to_path_buf())
            .with_max_length(MAX_LENGTH)
            .with_show_download_progress(true);
        let model = TextRerank::try_new(opts)?;
        Ok(Self { model })
    }

    /// Score each `(query, doc)` pair; returns `(index_into_docs, raw_score)` best-first.
    ///
    /// The score is the raw cross-encoder logit (no sigmoid) — meaningful for *ordering* only.
    /// Callers map it to a 0–1 display value via [`sigmoid`].
    pub fn rerank(&mut self, query: &str, docs: &[String]) -> Result<Vec<(usize, f32)>> {
        // fastembed unifies the query and document string types; pass matching `&str` slices.
        let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
        // return_documents=false: we already hold the bodies; default batch size.
        let results = self.model.rerank(query, &refs, false, None)?;
        Ok(results.into_iter().map(|r| (r.index, r.score)).collect())
    }
}

/// Map a raw cross-encoder logit into (0, 1) for a stable, human-meaningful display score.
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
