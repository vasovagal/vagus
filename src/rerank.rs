//! In-core cross-encoder reranking via fastembed (`jina-reranker-v1-turbo-en`).
//!
//! A cross-encoder is a *scoring* model — the same category as the embedder, on the same `ort`/ONNX
//! stack vagus already links (no new heavy deps; G11/G13). It re-scores the fused RRF candidate pool
//! by reading the full chunk body against the query, a precision boost the rank-based RRF floor can't
//! give. This is the tier-1 reranker (offline, no Claude); see ADR 0015. RRF itself (G8) is untouched.
//!
//! Guardrail G10: the model cache dir is set EXPLICITLY (same as the embedder).

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

/// `--rerank-context` is deliberately small: one or two adjacent chunks per side covers section
/// seams without turning each capped candidate into a whole-note cross-encoder pass (ADR 0015).
pub const MAX_CONTEXT_RADIUS: usize = 2;
/// Stable model identity recorded with eval and presentation-tick provenance.
pub const MODEL_ID: &str = "jinaai/jina-reranker-v1-turbo-en";

/// fastembed 5.14 clamps this model to tokenizer_config.model_max_length=512. Keeping radius zero at
/// that *effective* limit (rather than the previously requested-but-ignored 1024) preserves the
/// pre-flag rerank path exactly.
const LEGACY_MAX_LENGTH: usize = 512;
/// A normal indexed chunk targets ~900 estimated tokens. One 1024-token slot per center/neighbor,
/// including query + special-token headroom, gives 3072 at radius 1 and 5120 at radius 2.
const TOKENS_PER_CHUNK_SLOT: usize = 1024;
const MODEL_MAX_LENGTH: usize = 8192;
const MODEL_CACHE_DIR: &str = "models--jinaai--jina-reranker-v1-turbo-en";
const WINDOW_SEPARATOR: &str = "\n\n";

pub fn parse_context_radius(raw: &str) -> std::result::Result<usize, String> {
    let radius: usize = raw
        .parse()
        .map_err(|_| "rerank context must be an integer".to_owned())?;
    if radius <= MAX_CONTEXT_RADIUS {
        Ok(radius)
    } else {
        Err(format!(
            "rerank context must be between 0 and {MAX_CONTEXT_RADIUS}"
        ))
    }
}

fn max_length_for(radius: usize) -> Result<usize> {
    if radius > MAX_CONTEXT_RADIUS {
        bail!("rerank context must be between 0 and {MAX_CONTEXT_RADIUS}");
    }
    if radius == 0 {
        Ok(LEGACY_MAX_LENGTH)
    } else {
        Ok((TOKENS_PER_CHUNK_SLOT * (2 * radius + 1)).min(MODEL_MAX_LENGTH))
    }
}

/// Stable schema-2 eval provenance for the exact rerank input policy (ADR 0024/G27).
pub fn policy_id(radius: usize) -> Result<String> {
    Ok(format!(
        "capped_prefix_context_{radius}_tokenizer_max_{}",
        max_length_for(radius)?
    ))
}

/// Verify the audited ONNX model capacity before overriding fastembed's stale 512-token tokenizer
/// metadata. The HF ref is written by the same `TextRerank::try_new` call immediately before this
/// check; refusing a missing/mismatched config is safer than sending an overlong tensor to an unknown
/// model revision.
fn verify_model_capacity(cache_dir: &Path, required: usize) -> Result<()> {
    let repo = cache_dir.join(MODEL_CACHE_DIR);
    let revision = std::fs::read_to_string(repo.join("refs/main"))
        .with_context(|| "reading cached reranker revision for widened context")?;
    let revision = revision.trim();
    ensure!(
        !revision.is_empty()
            && revision
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid cached reranker revision"
    );
    let config_path = repo.join("snapshots").join(revision).join("config.json");
    let config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&config_path)
            .with_context(|| format!("reading reranker config {}", config_path.display()))?,
    )
    .with_context(|| format!("parsing reranker config {}", config_path.display()))?;
    let capacity = config["max_position_embeddings"]
        .as_u64()
        .context("reranker config has no integer max_position_embeddings")?
        as usize;
    ensure!(
        capacity >= required,
        "reranker model supports {capacity} positions, but widened context requires {required}"
    );
    Ok(())
}

pub struct Reranker {
    model: TextRerank,
    context_radius: usize,
    max_length: usize,
    batch_size: Option<usize>,
}

impl Reranker {
    pub fn new(cache_dir: &Path, context_radius: usize) -> Result<Self> {
        let max_length = max_length_for(context_radius)?;
        let opts = RerankInitOptions::new(RerankerModel::JINARerankerV1TurboEn)
            .with_cache_dir(cache_dir.to_path_buf())
            .with_max_length(max_length)
            .with_show_download_progress(true);
        let mut model = TextRerank::try_new(opts)?;

        // fastembed clamps the requested length to tokenizer_config.model_max_length (512), although
        // this pinned Jina model's config and positional embeddings support 8192. Override only for
        // the explicit widened mode, and only after checking the exact cached revision's capacity.
        let loaded_max = model
            .tokenizer
            .get_truncation()
            .context("reranker tokenizer has no truncation policy")?
            .max_length;
        if max_length > LEGACY_MAX_LENGTH {
            verify_model_capacity(cache_dir, max_length)?;
        }
        if loaded_max != max_length {
            let mut truncation = model
                .tokenizer
                .get_truncation()
                .cloned()
                .context("reranker tokenizer has no truncation policy")?;
            truncation.max_length = max_length;
            model
                .tokenizer
                .with_truncation(Some(truncation))
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }
        ensure!(
            model
                .tokenizer
                .get_truncation()
                .is_some_and(|p| p.max_length == max_length),
            "reranker tokenizer did not apply the {max_length}-token budget"
        );

        Ok(Self {
            model,
            context_radius,
            max_length,
            // Wider attention is the expensive dimension. Avoid batch-longest padding several
            // 3k/5k documents to the same shape and bound peak ONNX memory. Radius zero keeps
            // fastembed's historical default batching for compatibility.
            batch_size: (context_radius > 0).then_some(1),
        })
    }

    pub fn context_radius(&self) -> usize {
        self.context_radius
    }

    /// Build the cross-encoder-only small-to-big document. Whole adjacent chunks are admitted in
    /// ordinal order only while the *actual model tokenizer's untruncated pair encoding* — query,
    /// special tokens, center, and neighbors — fits the configured budget. Thus a neighbor can never
    /// cause the matched center to disappear under encoded truncation. An exceptional atomic chunk
    /// that alone exceeds the model budget is sent center-only; runtime validation still requires
    /// the encoded pair to retain center tokens (G20 permits such over-budget fenced code).
    pub fn prepare_context(
        &self,
        query: &str,
        center: &str,
        before: &[&str],
        after: &[&str],
    ) -> Result<String> {
        if self.context_radius == 0 {
            return Ok(center.to_owned());
        }

        // Clone once per candidate, then disable truncation/padding so every admission decision sees
        // the true pair length. The production tokenizer remains configured for the final inference.
        let mut measuring = self.model.tokenizer.clone();
        measuring
            .with_truncation(None)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        measuring.with_padding(None);
        let fitted = fit_context(center, before, after, self.max_length, |document| {
            measuring
                .encode((query, document), true)
                .map(|encoding| encoding.len())
                .map_err(|e| anyhow::anyhow!(e.to_string()))
        })?;

        // Validate against the exact configured tokenizer that fastembed will use again in rerank().
        // For an ordinary center, equality proves no tokenizer truncation occurred. For an oversized
        // atomic center, require at least one document-sequence token so surrounding text can never
        // erase the matched chunk entirely.
        let encoded = self
            .model
            .tokenizer
            .encode((query, fitted.document.as_str()), true)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let document_tokens = encoded
            .get_sequence_ids()
            .into_iter()
            .filter(|sequence| *sequence == Some(1))
            .count();
        ensure!(
            document_tokens > 0 || center.trim().is_empty(),
            "reranker tokenizer removed the matched center from its encoded input"
        );
        if fitted.center_fits {
            ensure!(
                encoded.len() == fitted.encoded_len && fitted.encoded_len <= self.max_length,
                "reranker tokenizer unexpectedly truncated a fitted context document"
            );
        }
        Ok(fitted.document)
    }

    /// Score each `(query, doc)` pair; returns `(index_into_docs, raw_score)` best-first.
    ///
    /// The score is the raw cross-encoder logit (no sigmoid) — meaningful for *ordering* only.
    /// Callers map it to a 0–1 display value via [`sigmoid`].
    pub fn rerank(&mut self, query: &str, docs: &[String]) -> Result<Vec<(usize, f32)>> {
        // fastembed unifies the query and document string types; pass matching `&str` slices.
        let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
        // return_documents=false: we already hold the bodies. Widened inputs use batch size one to
        // bound quadratic-attention memory; radius zero preserves fastembed's historical default.
        let results = self.model.rerank(query, &refs, false, self.batch_size)?;
        Ok(results.into_iter().map(|r| (r.index, r.score)).collect())
    }
}

struct FittedContext {
    document: String,
    encoded_len: usize,
    center_fits: bool,
}

/// Pure planner separated from the model wrapper so adversarial budget/order behavior is unit-tested
/// without downloading an ONNX model. `encoded_len` is production's real tokenizer pair length.
fn fit_context<F>(
    center: &str,
    before: &[&str],
    after: &[&str],
    max_length: usize,
    mut encoded_len: F,
) -> Result<FittedContext>
where
    F: FnMut(&str) -> Result<usize>,
{
    let center_len = encoded_len(center)?;
    if center_len > max_length {
        return Ok(FittedContext {
            document: center.to_owned(),
            encoded_len: center_len,
            center_fits: false,
        });
    }

    let mut kept_before: Vec<&str> = Vec::new(); // chronological order
    let mut kept_after: Vec<&str> = Vec::new(); // chronological order
    let mut document = center.to_owned();
    let mut fitted_len = center_len;
    let mut before_open = true;
    let mut after_open = true;

    // Grow from the center out. If the nearest chunk on one side cannot fit, do not skip over it to
    // admit a non-adjacent farther chunk; the other side may still grow.
    for distance in 0..before.len().max(after.len()) {
        if before_open {
            if let Some(neighbor) = before
                .len()
                .checked_sub(1 + distance)
                .and_then(|index| before.get(index))
            {
                let mut candidate_before = kept_before.clone();
                candidate_before.insert(0, neighbor);
                let candidate = render_context(&candidate_before, center, &kept_after);
                let candidate_len = encoded_len(&candidate)?;
                if candidate_len <= max_length {
                    kept_before = candidate_before;
                    document = candidate;
                    fitted_len = candidate_len;
                } else {
                    before_open = false;
                }
            } else {
                before_open = false;
            }
        }

        if after_open {
            if let Some(neighbor) = after.get(distance) {
                let mut candidate_after = kept_after.clone();
                candidate_after.push(neighbor);
                let candidate = render_context(&kept_before, center, &candidate_after);
                let candidate_len = encoded_len(&candidate)?;
                if candidate_len <= max_length {
                    kept_after = candidate_after;
                    document = candidate;
                    fitted_len = candidate_len;
                } else {
                    after_open = false;
                }
            } else {
                after_open = false;
            }
        }
    }

    Ok(FittedContext {
        document,
        encoded_len: fitted_len,
        center_fits: true,
    })
}

fn render_context(before: &[&str], center: &str, after: &[&str]) -> String {
    let mut parts = Vec::with_capacity(before.len() + 1 + after.len());
    parts.extend_from_slice(before);
    parts.push(center);
    parts.extend_from_slice(after);
    parts.join(WINDOW_SEPARATOR)
}

/// Map a raw cross-encoder logit into (0, 1) for a stable, human-meaningful display score.
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TempDir;

    fn words(document: &str) -> Result<usize> {
        Ok(document.split_whitespace().count())
    }

    #[test]
    fn context_radius_parser_and_budgets_are_bounded() {
        assert_eq!(parse_context_radius("0").unwrap(), 0);
        assert_eq!(parse_context_radius("2").unwrap(), 2);
        assert!(parse_context_radius("3").is_err());
        assert!(parse_context_radius("nope").is_err());
        assert_eq!(max_length_for(0).unwrap(), 512);
        assert_eq!(max_length_for(1).unwrap(), 3072);
        assert_eq!(max_length_for(2).unwrap(), 5120);
        assert_eq!(
            policy_id(1).unwrap(),
            "capped_prefix_context_1_tokenizer_max_3072"
        );
        assert!(max_length_for(3).is_err());
    }

    #[test]
    fn widened_capacity_check_is_revision_specific_and_fail_closed() {
        let temp = TempDir::new("rerank-capacity");
        let repo = temp.path().join(MODEL_CACHE_DIR);
        let revision = "abc123";
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::create_dir_all(repo.join("snapshots").join(revision)).unwrap();
        std::fs::write(repo.join("refs/main"), revision).unwrap();
        let config = repo.join("snapshots").join(revision).join("config.json");
        std::fs::write(&config, r#"{"max_position_embeddings":8192}"#).unwrap();
        assert!(verify_model_capacity(temp.path(), 5120).is_ok());
        std::fs::write(&config, r#"{"max_position_embeddings":2048}"#).unwrap();
        assert!(verify_model_capacity(temp.path(), 3072).is_err());
        std::fs::write(repo.join("refs/main"), "../escape").unwrap();
        assert!(verify_model_capacity(temp.path(), 512).is_err());
    }

    #[test]
    fn planner_keeps_natural_order_while_growing_center_out() {
        let fitted = fit_context(
            "center",
            &["previous two", "previous one"],
            &["next one", "next two"],
            20,
            words,
        )
        .unwrap();
        assert_eq!(
            fitted.document,
            "previous two\n\nprevious one\n\ncenter\n\nnext one\n\nnext two"
        );
        assert!(fitted.center_fits);
    }

    #[test]
    fn oversized_neighbor_cannot_push_center_out_of_encoded_budget() {
        let huge = "previous ".repeat(100);
        // Account for a 3-token query/special-token reserve in the mock pair encoder. The previous
        // neighbor is rejected; the next neighbor still fits, and the center remains verbatim.
        let fitted = fit_context(
            "CENTER_MARKER answer",
            &[huge.as_str()],
            &["next context"],
            8,
            |document| Ok(3 + document.split_whitespace().count()),
        )
        .unwrap();
        assert_eq!(fitted.document, "CENTER_MARKER answer\n\nnext context");
        assert!(fitted.encoded_len <= 8);
        assert!(fitted.document.contains("CENTER_MARKER answer"));
    }

    #[test]
    fn planner_never_skips_an_oversized_nearest_neighbor() {
        let huge = "too-large ".repeat(20);
        let fitted =
            fit_context("center", &["far previous", huge.as_str()], &[], 5, words).unwrap();
        assert_eq!(fitted.document, "center");
    }

    #[test]
    fn center_over_budget_is_returned_alone() {
        let center = "center ".repeat(20);
        let fitted = fit_context(&center, &["before"], &["after"], 5, words).unwrap();
        assert_eq!(fitted.document, center);
        assert!(!fitted.center_fits);
        assert!(fitted.encoded_len > 5);
    }
}
