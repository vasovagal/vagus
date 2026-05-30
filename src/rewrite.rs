//! Tier-1 local generative query rewriter (ADR 0016): candle + qmd's fine-tuned Qwen3-1.7B GGUF.
//!
//! Feature-gated behind `generate`. Offline, no daemon (G14); the model is lazily downloaded to the
//! model cache **outside iCloud** (G6/G10) on first use. The model emits typed `lex:`/`vec:`/`hyde:`
//! variants (aping qmd's output protocol); `vagus search --smart` routes each to the right retriever
//! and fuses (G19). This is tier-1 generation: the in-skill Opus path (tier-2) is the SOTA sibling.

use std::path::Path;

use anyhow::{Context, Result};
use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};
use candle_transformers::models::quantized_qwen3::ModelWeights;
use candle_transformers::utils::apply_repeat_penalty;
use hf_hub::Repo;
use hf_hub::api::sync::ApiBuilder;
use tokenizers::Tokenizer;

use crate::config::Config;

// qmd's fine-tuned expansion model (the faithful ape) + the upstream tokenizer (the GGUF repo has
// none). Overridable via env so the model is swappable without a rebuild.
const GGUF_REPO: &str = "tobil/qmd-query-expansion-1.7B-gguf";
const GGUF_FILE: &str = "qmd-query-expansion-1.7B-q4_k_m.gguf";
const TOKENIZER_REPO: &str = "Qwen/Qwen3-1.7B";

// Qwen3 non-thinking sampling (qmd's values); greedy (temp 0) causes repetition loops.
const MAX_NEW_TOKENS: usize = 512;
const REPEAT_LAST_N: usize = 64;
const REPEAT_PENALTY: f32 = 1.1;
const TEMPERATURE: f64 = 0.7;
const TOP_K: usize = 20;
const TOP_P: f64 = 0.8;
const SEED: u64 = 299_792_458;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Lex,
    Vec,
    Hyde,
}

impl Kind {
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Lex => "lex",
            Kind::Vec => "vec",
            Kind::Hyde => "hyde",
        }
    }
}

pub struct Variant {
    pub kind: Kind,
    pub text: String,
}

pub struct Rewriter {
    model: ModelWeights,
    tokenizer: Tokenizer,
    device: Device,
    eos: u32,
}

impl Rewriter {
    /// Load the model + tokenizer (downloading both to `cache_dir` on first use).
    pub fn new(cache_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(cache_dir).ok();
        let repo = std::env::var("VAGUS_REWRITE_REPO").unwrap_or_else(|_| GGUF_REPO.to_string());
        let file = std::env::var("VAGUS_REWRITE_FILE").unwrap_or_else(|_| GGUF_FILE.to_string());
        let tok_repo =
            std::env::var("VAGUS_REWRITE_TOKENIZER").unwrap_or_else(|_| TOKENIZER_REPO.to_string());

        let api = ApiBuilder::new()
            .with_cache_dir(cache_dir.to_path_buf())
            .build()
            .context("init hf-hub api")?;
        let gguf_path = api
            .repo(Repo::model(repo.clone()))
            .get(&file)
            .with_context(|| format!("downloading {repo}/{file}"))?;
        let tok_path = api
            .repo(Repo::model(tok_repo.clone()))
            .get("tokenizer.json")
            .with_context(|| format!("downloading {tok_repo}/tokenizer.json"))?;

        let tokenizer = Tokenizer::from_file(&tok_path).map_err(anyhow::Error::msg)?;
        let device = Device::Cpu;
        let mut fh = std::fs::File::open(&gguf_path)
            .with_context(|| format!("open {}", gguf_path.display()))?;
        let content = gguf_file::Content::read(&mut fh).map_err(|e| e.with_path(&gguf_path))?;
        let model = ModelWeights::from_gguf(content, &mut fh, &device)?;

        let eos = tokenizer
            .get_vocab(true)
            .get("<|im_end|>")
            .copied()
            .context("tokenizer missing <|im_end|>")?;
        Ok(Self {
            model,
            tokenizer,
            device,
            eos,
        })
    }

    /// Expand `query` into typed variants; falls back to original-query variants if generation
    /// yields nothing parseable.
    pub fn expand(&mut self, query: &str) -> Result<Vec<Variant>> {
        let raw = self.generate(query)?;
        let mut variants = parse_variants(&raw, query);
        if variants.is_empty() {
            variants = fallback(query);
        }
        Ok(variants)
    }

    fn generate(&mut self, query: &str) -> Result<String> {
        self.model.clear_kv_cache();
        // The expansion model's chat template (no system message); `/no_think` suppresses CoT.
        let prompt = format!(
            "<|im_start|>user\n/no_think Expand this search query: {query}<|im_end|>\n<|im_start|>assistant\n"
        );
        let prompt_ids = self
            .tokenizer
            .encode(prompt, true)
            .map_err(anyhow::Error::msg)?
            .get_ids()
            .to_vec();

        let mut lp = LogitsProcessor::from_sampling(
            SEED,
            Sampling::TopKThenTopP {
                k: TOP_K,
                p: TOP_P,
                temperature: TEMPERATURE,
            },
        );

        // Prefill the whole prompt at offset 0, then decode one token at a time, telling the model the
        // running KV-cache position via `offset` (prompt_len + tokens generated so far).
        let input = Tensor::new(prompt_ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let logits = self.model.forward(&input, 0)?.squeeze(0)?;
        let mut next = lp.sample(&logits)?;

        let mut all = prompt_ids.clone();
        let mut generated: Vec<u32> = Vec::new();
        for index in 0..MAX_NEW_TOKENS {
            if next == self.eos {
                break;
            }
            generated.push(next);
            all.push(next);
            let input = Tensor::new(&[next], &self.device)?.unsqueeze(0)?;
            let logits = self
                .model
                .forward(&input, prompt_ids.len() + index)?
                .squeeze(0)?;
            let logits = if REPEAT_PENALTY == 1.0 {
                logits
            } else {
                let start = all.len().saturating_sub(REPEAT_LAST_N);
                apply_repeat_penalty(&logits, REPEAT_PENALTY, &all[start..])?
            };
            next = lp.sample(&logits)?;
        }
        self.tokenizer
            .decode(&generated, true)
            .map_err(anyhow::Error::msg)
    }
}

/// `vagus rewrite "<query>"`: print the typed expansion lines (for inspection / composition).
pub fn run_cli(cfg: &Config, query: &str) -> Result<()> {
    let mut rw = Rewriter::new(&cfg.cache_dir)?;
    for v in rw.expand(query)? {
        println!("{}: {}", v.kind.tag(), v.text);
    }
    Ok(())
}

/// Parse the model's typed output into variants, dropping chat-template leakage and (for lex/vec)
/// lines that share no token with the query — qmd's anti-drift guard. HyDE passages are exempt.
fn parse_variants(raw: &str, query: &str) -> Vec<Variant> {
    let q_tokens = tokenize_lower(query);
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.contains("<|") {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let kind = match line[..colon].trim().to_ascii_lowercase().as_str() {
            "lex" => Kind::Lex,
            "vec" => Kind::Vec,
            "hyde" => Kind::Hyde,
            _ => continue,
        };
        let text = line[colon + 1..].trim().to_string();
        if text.is_empty() {
            continue;
        }
        if kind != Kind::Hyde && !shares_token(&text, &q_tokens) {
            continue;
        }
        out.push(Variant { kind, text });
    }
    out
}

fn fallback(query: &str) -> Vec<Variant> {
    vec![
        Variant {
            kind: Kind::Hyde,
            text: format!("Information about {query}"),
        },
        Variant {
            kind: Kind::Lex,
            text: query.to_string(),
        },
        Variant {
            kind: Kind::Vec,
            text: query.to_string(),
        },
    ]
}

fn tokenize_lower(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn shares_token(text: &str, q_tokens: &[String]) -> bool {
    if q_tokens.is_empty() {
        return true;
    }
    let t = text.to_ascii_lowercase();
    q_tokens.iter().any(|qt| t.contains(qt.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_lines_and_drops_leakage() {
        let raw = "hyde: A vasovagal response is a reflex causing fainting.\n\
                   lex: vasovagal syncope causes\n\
                   vec: why does the vagus nerve cause fainting\n\
                   <|im_end|>\n\
                   note: should be dropped";
        let v = parse_variants(raw, "vasovagal fainting");
        assert_eq!(v.len(), 3, "expected 3 typed variants");
        assert_eq!(v[0].kind, Kind::Hyde);
        assert!(
            v.iter()
                .any(|x| x.kind == Kind::Lex && x.text.contains("syncope"))
        );
        assert!(v.iter().all(|x| !x.text.contains("<|")));
    }

    #[test]
    fn anti_drift_drops_unrelated_lex_but_keeps_hyde() {
        // lex line shares no token with the query → dropped; hyde is exempt.
        let raw = "lex: completely unrelated terms\nhyde: an unrelated passage\nvec: about rust ownership";
        let v = parse_variants(raw, "rust ownership");
        assert!(v.iter().any(|x| x.kind == Kind::Vec));
        assert!(v.iter().any(|x| x.kind == Kind::Hyde)); // exempt from anti-drift
        assert!(!v.iter().any(|x| x.kind == Kind::Lex)); // dropped (no shared token)
    }

    #[test]
    fn empty_output_falls_back() {
        let v = parse_variants("garbage with no typed lines", "my query");
        assert!(v.is_empty());
        let fb = fallback("my query");
        assert_eq!(fb.len(), 3);
        assert_eq!(fb[0].kind, Kind::Hyde);
    }
}
