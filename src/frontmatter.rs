//! Shared frontmatter ownership and searchable producer-metadata projection.
//!
//! Vagus owns lifecycle fields such as `created` and `status`. Every other top-level field accepted
//! through `add-note --frontmatter-json` has a compact JSON value, which lets indexing distinguish
//! producer metadata from arbitrary YAML without adding a YAML parser (ADRs 0027/0028).

use serde_json::Value;

/// Keys whose lifecycle belongs to Vagus rather than an external producer.
pub const VAGUS_OWNED_KEYS: [&str; 6] =
    ["created", "status", "source", "para", "modified", "title"];

/// The deliberately small key grammar accepted at the producer boundary.
pub fn valid_producer_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

pub fn is_vagus_owned(key: &str) -> bool {
    VAGUS_OWNED_KEYS.contains(&key)
}

/// Turn one non-Vagus top-level JSON field into deterministic, model-friendly searchable text.
/// Object keys and scalar values are retained, while JSON punctuation is discarded. Whitespace is
/// normalized so escaped newlines/control whitespace cannot manufacture a derived Markdown shape.
pub fn producer_search_text(key: &str, raw_value: &str) -> Option<String> {
    if !valid_producer_key(key) || is_vagus_owned(key) {
        return None;
    }
    let value: Value = serde_json::from_str(raw_value).ok()?;
    let mut terms = Vec::new();
    push_terms(key, &mut terms);
    flatten_json(&value, &mut terms);
    Some(terms.join(" "))
}

fn flatten_json(value: &Value, terms: &mut Vec<String>) {
    match value {
        Value::Null => terms.push("null".to_owned()),
        Value::Bool(value) => terms.push(value.to_string()),
        Value::Number(value) => terms.push(value.to_string()),
        Value::String(value) => push_terms(value, terms),
        Value::Array(values) => {
            for value in values {
                flatten_json(value, terms);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                push_terms(key, terms);
                flatten_json(value, terms);
            }
        }
    }
}

fn push_terms(text: &str, terms: &mut Vec<String>) {
    terms.extend(text.split_whitespace().map(str::to_owned));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_json_flattens_to_searchable_keys_and_values() {
        let text = producer_search_text(
            "corti",
            r#"{"models":{"asr":{"id":"nvidia/parakeet-tdt-0.6b-v3"}},"mode":"live"}"#,
        )
        .unwrap();
        assert_eq!(
            text,
            "corti mode live models asr id nvidia/parakeet-tdt-0.6b-v3"
        );
    }

    #[test]
    fn lifecycle_invalid_and_non_json_fields_are_not_projected() {
        assert!(producer_search_text("status", r#""inbox""#).is_none());
        assert!(producer_search_text("bad key", r#""value""#).is_none());
        assert!(producer_search_text("custom", "unquoted-yaml").is_none());
    }
}
