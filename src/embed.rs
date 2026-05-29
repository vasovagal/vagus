//! Local ONNX embeddings via fastembed (bge-small-en-v1.5, 384-dim).
//!
//! Guardrail G10: the cache dir is set EXPLICITLY (fastembed otherwise defaults to
//! `./.fastembed_cache` in the CWD). Guardrail G9: bge was trained with a query-side retrieval
//! instruction — we prepend it to queries only; documents are embedded raw. Vectors are
//! L2-normalized so cosine == dot product (G7).

use std::path::Path;

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// bge-v1.5 query instruction (documents get no prefix).
const BGE_QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let opts = InitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_cache_dir(cache_dir.to_path_buf())
            .with_show_download_progress(true);
        let model = TextEmbedding::try_new(opts)?;
        Ok(Self { model })
    }

    /// Embed document chunk bodies (no prefix), L2-normalized.
    pub fn embed_documents(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let mut out = self.model.embed(texts, None)?;
        for v in out.iter_mut() {
            normalize(v);
        }
        Ok(out)
    }

    /// Embed a query with the bge retrieval instruction, L2-normalized.
    pub fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        let q = format!("{BGE_QUERY_PREFIX}{text}");
        let mut out = self.model.embed(vec![q], None)?;
        let mut v = out.pop().unwrap_or_default();
        normalize(&mut v);
        Ok(v)
    }
}

/// In-place L2 normalization.
pub fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
