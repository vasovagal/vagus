//! Path resolution for vagus.
//!
//! Guardrail G1/G4: only Markdown lives in the iCloud vault; the index, DB, and model cache live
//! OUTSIDE iCloud and are rebuildable. This module is the single source of truth for those paths.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Default embedding model + its dimensionality. Pinned into the `meta` table at index time so a
/// change forces a `reindex` (guardrail G4). EmbeddingGemma-300M: 768-dim, 2048-token context,
/// ~+8 MTEB over bge-small, 100+ languages. A built-in fastembed variant
/// (`EmbeddingModel::EmbeddingGemma300M`); see [ADR 0006](../design/adr/0006-embeddings-local-no-daemon.md).
pub const EMBED_MODEL: &str = "google/embeddinggemma-300m";
pub const EMBED_DIMS: usize = 768;

/// Bump when the chunker changes shape. A mismatch in the `meta` table forces a one-time reindex so
/// existing vaults self-heal on upgrade (v2 = stop indexing YAML frontmatter; v3 = token-budgeted
/// sub-splitting of oversize sections, sized to the embedder context window — G20).
pub const CHUNK_VERSION: &str = "3";

#[derive(Debug, Clone)]
pub struct Config {
    /// Markdown vault (in iCloud), default `~/brain`. Override: `VAGUS_VAULT`.
    pub vault: PathBuf,
    /// Derived index state (NOT in iCloud), default `~/.local/share/vagus`. Override: `VAGUS_DATA_DIR`.
    pub data_dir: PathBuf,
    /// Cached ONNX model (NOT in iCloud), default `~/Library/Caches/vagus/models`.
    /// Override: `VAGUS_CACHE_DIR` or `FASTEMBED_CACHE_DIR`.
    pub cache_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = dirs::home_dir().context("cannot resolve home directory")?;

        let vault = std::env::var_os("VAGUS_VAULT")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("brain"));

        // Prefer XDG-style ~/.local/share even on macOS (dirs::data_dir() would give
        // ~/Library/Application Support); the guardrails specify ~/.local/share/vagus.
        let data_dir = std::env::var_os("VAGUS_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_DATA_HOME").map(|x| PathBuf::from(x).join("vagus")))
            .unwrap_or_else(|| home.join(".local/share/vagus"));

        // ~/Library/Caches/vagus/models on macOS via dirs::cache_dir().
        let cache_dir = std::env::var_os("VAGUS_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("FASTEMBED_CACHE_DIR").map(PathBuf::from))
            .unwrap_or_else(|| {
                dirs::cache_dir()
                    .unwrap_or_else(|| home.join(".cache"))
                    .join("vagus/models")
            });

        Ok(Self {
            vault,
            data_dir,
            cache_dir,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("meta.db")
    }

    pub fn tantivy_dir(&self) -> PathBuf {
        self.data_dir.join("tantivy")
    }

    /// Create the derived-state directories (NOT the vault — that is the user's iCloud folder).
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("creating data dir {}", self.data_dir.display()))?;
        std::fs::create_dir_all(&self.cache_dir)
            .with_context(|| format!("creating cache dir {}", self.cache_dir.display()))?;
        Ok(())
    }
}
