//! Optional bounded relevance signal for search reporting/filtering (ADR 0026/G9e).
//!
//! RRF and BM25 magnitudes are not query-comparable confidence values. EmbeddingGemma cosine is the
//! one current retrieval component with useful cross-query separation on the held-out vault battery,
//! so relevance v1 is deliberately just finite cosine clamped to `[0, 1]`. It is a heuristic signal,
//! not a probability, and never affects ranking or `rrf()`.

/// Stable identity recorded by `vagus eval --relevance` and tied to ADR 0026's evidence.
pub const POLICY: &str = "embeddinggemma300m_chunk6_cosine_clamped_v1";

/// Convert a finite cosine to the bounded reporting signal. Negative similarities carry no positive
/// evidence and clamp to zero; tiny floating overshoot above one clamps to one. Non-finite derived
/// scores fail open to `None` rather than becoming false confidence or invalid JSON.
pub fn from_cosine(cosine: Option<f32>) -> Option<f32> {
    cosine
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_cosine_is_bounded_without_invented_calibration() {
        assert_eq!(from_cosine(Some(-0.2)), Some(0.0));
        assert_eq!(from_cosine(Some(0.42)), Some(0.42));
        assert_eq!(from_cosine(Some(1.01)), Some(1.0));
        assert_eq!(from_cosine(None), None);
    }

    #[test]
    fn non_finite_cosine_is_unjudged() {
        assert_eq!(from_cosine(Some(f32::NAN)), None);
        assert_eq!(from_cosine(Some(f32::INFINITY)), None);
        assert_eq!(from_cosine(Some(f32::NEG_INFINITY)), None);
    }

    #[test]
    fn policy_name_tracks_the_pinned_model_and_chunk_identity() {
        let model = crate::config::EMBED_MODEL
            .rsplit('/')
            .next()
            .unwrap()
            .replace('-', "");
        assert_eq!(
            POLICY,
            format!(
                "{model}_chunk{}_cosine_clamped_v1",
                crate::config::CHUNK_VERSION
            )
        );
    }
}
