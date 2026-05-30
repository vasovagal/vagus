//! Local ONNX embeddings via fastembed (EmbeddingGemma-300M, 768-dim, 2048-token context).
//!
//! Guardrail G10: the cache dir is set EXPLICITLY (fastembed otherwise defaults to
//! `./.fastembed_cache` in the CWD). Guardrail G9: EmbeddingGemma is prompt-templated — queries and
//! documents each get a *different* instruction prefix (fastembed does NOT apply these itself, so we
//! prepend them here; don't double-prefix). Vectors are L2-normalized so cosine == dot product (G7).

use std::path::Path;

use anyhow::Result;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

/// EmbeddingGemma prompt templates (the model was trained with task-typed prefixes). The retrieval
/// query carries the search instruction; documents carry the (title-less) passage instruction.
const GEMMA_QUERY_PREFIX: &str = "task: search result | query: ";
const GEMMA_DOC_PREFIX: &str = "title: none | text: ";

/// EmbeddingGemma's context window (tokens). fastembed defaults to 512; raise it so longer chunks
/// aren't silently truncated (the chunker targets well under this — G20).
const MAX_LENGTH: usize = 2048;

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let opts = TextInitOptions::new(EmbeddingModel::EmbeddingGemma300M)
            .with_cache_dir(cache_dir.to_path_buf())
            .with_max_length(MAX_LENGTH)
            .with_show_download_progress(true);
        let model = TextEmbedding::try_new(opts)?;
        Ok(Self { model })
    }

    /// Embed document chunk bodies with the passage prefix, L2-normalized.
    pub fn embed_documents(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts
            .into_iter()
            .map(|t| format!("{GEMMA_DOC_PREFIX}{t}"))
            .collect();
        let mut out = self.model.embed(prefixed, None)?;
        for v in out.iter_mut() {
            normalize(v);
        }
        Ok(out)
    }

    /// Embed a query with the retrieval prefix, L2-normalized.
    pub fn embed_query(&mut self, text: &str) -> Result<Vec<f32>> {
        let q = format!("{GEMMA_QUERY_PREFIX}{text}");
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
