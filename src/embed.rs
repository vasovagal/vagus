//! Local ONNX embeddings via fastembed (EmbeddingGemma-300M, 768-dim, 2048-token context).
//!
//! Guardrail G10: the cache dir is set EXPLICITLY (fastembed otherwise defaults to
//! `./.fastembed_cache` in the CWD). Guardrail G9: EmbeddingGemma is prompt-templated — queries and
//! documents each get a *different* instruction prefix (fastembed does NOT apply these itself, so we
//! prepend them here; don't double-prefix). Vectors are L2-normalized so cosine == dot product (G7).
//! Guardrail G4: every input that shapes a stored document vector lives in [`DOC_RECIPE`].

use std::path::Path;

use anyhow::Result;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::config::{EMBED_DIMS, EMBED_MODEL};

/// EmbeddingGemma's retrieval-query prompt (the model was trained with task-typed prefixes). Query
/// vectors are computed per search and never stored, so this is not part of [`DocRecipe`]: changing
/// it needs no reindex.
const GEMMA_QUERY_PREFIX: &str = "task: search result | query: ";

/// Every input that shapes a stored document vector. The embedder reads its settings from
/// [`DOC_RECIPE`], and the index pins [`DocRecipe::identity`] as `meta.embed_recipe` (G4). Editing
/// any of them makes incremental runs refuse until `vagus reindex` instead of mixing old and new
/// vectors. Bump `CHUNK_VERSION` in the same change to make that reindex automatic.
pub struct DocRecipe {
    /// Model id, also pinned on its own as `embed_model`.
    pub model_id: &'static str,
    /// The fastembed variant: which ONNX weights, tokenizer, and pooling run.
    pub model: EmbeddingModel,
    pub dims: usize,
    /// EmbeddingGemma's title-less passage prompt, prepended to every chunk body (G9).
    pub prefix: &'static str,
    /// Token limit per document. fastembed defaults to 512; EmbeddingGemma's context window is
    /// 2048, and the chunker targets well under it (G20).
    pub max_length: usize,
    /// L2-normalize so cosine == dot product (G7).
    pub l2_normalize: bool,
}

pub const DOC_RECIPE: DocRecipe = DocRecipe {
    model_id: EMBED_MODEL,
    model: EmbeddingModel::EmbeddingGemma300M,
    dims: EMBED_DIMS,
    prefix: "title: none | text: ",
    max_length: 2048,
    l2_normalize: true,
};

/// The recipe behind every index pinned before `embed_recipe` existed: vagus 0.13.1 and earlier,
/// whose prefix, length, and normalization are unchanged since EmbeddingGemma landed in 0.2.0.
/// Frozen: an index with no stored recipe is compared as this one, so it still refuses once
/// [`DOC_RECIPE`] moves.
pub const PRE_PINNING_RECIPE: DocRecipe = DocRecipe {
    model_id: "google/embeddinggemma-300m",
    model: EmbeddingModel::EmbeddingGemma300M,
    dims: 768,
    prefix: "title: none | text: ",
    max_length: 2048,
    l2_normalize: true,
};

impl DocRecipe {
    /// The pinned form. Destructured without `..`, so a new field doesn't compile until it is part
    /// of the identity. Reformatting this string changes every pinned value and forces a reindex.
    pub fn identity(&self) -> String {
        let Self {
            model_id,
            model,
            dims,
            prefix,
            max_length,
            l2_normalize,
        } = self;
        format!(
            "{model_id} ({model:?}) dims={dims} max_length={max_length} l2_normalize={l2_normalize} prefix={prefix:?}"
        )
    }
}

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    pub fn new(cache_dir: &Path) -> Result<Self> {
        let opts = TextInitOptions::new(DOC_RECIPE.model)
            .with_cache_dir(cache_dir.to_path_buf())
            .with_max_length(DOC_RECIPE.max_length)
            .with_show_download_progress(true);
        let model = TextEmbedding::try_new(opts)?;
        Ok(Self { model })
    }

    /// Embed document chunk bodies under [`DOC_RECIPE`].
    pub fn embed_documents(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let DocRecipe {
            prefix,
            l2_normalize,
            ..
        } = DOC_RECIPE;
        let prefixed: Vec<String> = texts.into_iter().map(|t| format!("{prefix}{t}")).collect();
        let mut out = self.model.embed(prefixed, None)?;
        if l2_normalize {
            for v in out.iter_mut() {
                normalize(v);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_document_input_changes_the_recipe_identity() {
        let pinned = DOC_RECIPE.identity();
        for changed in [
            DocRecipe {
                model_id: "google/embeddinggemma-300m-v2",
                ..DOC_RECIPE
            },
            DocRecipe {
                model: EmbeddingModel::AllMiniLML6V2,
                ..DOC_RECIPE
            },
            DocRecipe {
                dims: 512,
                ..DOC_RECIPE
            },
            DocRecipe {
                prefix: "title: {title} | text: ",
                ..DOC_RECIPE
            },
            DocRecipe {
                max_length: 1024,
                ..DOC_RECIPE
            },
            DocRecipe {
                l2_normalize: false,
                ..DOC_RECIPE
            },
        ] {
            assert_ne!(changed.identity(), pinned);
        }
    }

    #[test]
    fn changing_only_the_query_prefix_leaves_the_recipe_alone() {
        // The identity is exactly the document-side inputs. The query prompt is not among them, so
        // editing it changes neither this string nor any pinned index.
        assert_eq!(
            DOC_RECIPE.identity(),
            r#"google/embeddinggemma-300m (EmbeddingGemma300M) dims=768 max_length=2048 l2_normalize=true prefix="title: none | text: ""#
        );
        assert!(!DOC_RECIPE.identity().contains(GEMMA_QUERY_PREFIX));
    }
}
