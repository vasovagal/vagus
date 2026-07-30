# ADR 0003 — Search stack: tantivy + fastembed + brute-force cosine + RRF

- **Status:** Accepted (2026-05-29); **amended 2026-07-30** — stable modality-neutral ties;
  ADR 0025 permits only fixed-gate, explicit same-pool fusion experiments while RRF stays default.

## Context

Hybrid retrieval over a personal-scale Markdown vault (tens of thousands of chunks), local and offline.

## Options considered

- **Full-text:** tantivy (BM25, pure-Rust) vs SQLite FTS5 vs Xapian. 
- **Vectors:** brute-force cosine vs sqlite-vec vs ANN crates (hnsw_rs / instant-distance / usearch).
- **Embeddings:** see [ADR 0006](./0006-embeddings-local-no-daemon.md).
- **Fusion:** RRF vs weighted score blend.

## Decision

- **BM25 via `tantivy` 0.26.x.** The piece with an honest single-binary story.
- **Embeddings via `fastembed` (`bge-small-en-v1.5`, 384-dim)** through `ort` (=2.0.0-rc.12).
- **Vectors: brute-force exact cosine in RAM**, stored as BLOBs in `rusqlite` (`bundled`). At this scale
  a full SIMD scan is sub-few-ms; zero extra deps. **No ANN crate yet.**
  > **Superseded for the vector component by [ADR 0019](./0019-usearch-ann-backend.md) (2026-06-07):**
  > semantic/hybrid search now selects exact cosine below 10k chunks and embedded static **usearch
  > HNSW** at/above 10k; `--exact` forces the oracle in every mode. BM25 + RRF below are
  > unchanged. The f32 BLOBs remain the authoritative, rebuildable source of truth.
- **Fusion: Reciprocal Rank Fusion, k=60** (`score = Σ 1/(k + rank)`). Rank-based ⇒ no score
  normalization, **no per-list weighting** (G8 — an earlier "weight the original-query BM25 list ×2"
  note contradicted G8 and is removed; qmd's weighted-RRF/top-rank bonus are rejected). Equal fused
  scores use ascending opaque `chunk_id`: randomized `HashMap` iteration otherwise changed which tied
  chunk/body represented a note across identical processes. This tie-break changes no score and
  favors neither source list.
  > **Amended by [ADR 0025](./0025-evidence-gated-fusion.md):** this remains the only production
  > default. A same-candidate-pool alternate may be developed as explicit opt-in only after the fixed
  > held-out `vagus eval-gate` passes. Raw-score blending remains banned; passing cannot silently
  > replace the default or feed non-RRF scores into ADR 0023.
- **bge prefixing:** query gets `"Represent this sentence for searching relevant passages: "`;
  documents un-prefixed. Don't double-prefix (respect what the lib already applies).

`frankensearch` implements a similar core (tantivy + f16-SIMD brute force + RRF k=60); we **hand-roll**
ours for control + a clean dep tree and use frankensearch/qmd only as references
([ADR 0007](./0007-lean-on-frankensearch.md)).

## Key implementation rules

- **tantivy has no `update_document`:** make `path` a `STRING|STORED` field; per changed file
  `delete_term(path)` → re-`add_document` → single `commit()`. Full rebuild = many adds + one commit.
- **Incremental** keyed on `mtime` then `sha256`; `chunk_id = sha256(path + "#" + ord)`.
- **Consistency:** delete tantivy docs *and* SQLite vector rows together (vector store has no FK).
- **Pin** `embed_model` + `embed_dims` + `tantivy_version` in `meta`; mismatch ⇒ `reindex`.
- **Normalize** vectors at insert (cosine = dot product).

## Consequences

- Brute force is fine now; revisit an ANN backend only if the corpus grows by orders of magnitude.
  > **Update (ADR 0019, 2026-06-07):** with a >500k-chunk trajectory the corpus *is* expected to grow
  > by orders of magnitude, so usearch HNSW was adopted for ≥10k chunks. A 2026-07-30 qrel audit made
  > brute force automatic below 10k; it also remains the explicit all-mode `--exact` oracle.
- tantivy 0.x has no index-format compatibility guarantee across minor bumps → `tantivy_version` gate.
- **Reranking and query-expansion are now tiered** (no longer "deferred"):
  - A cross-encoder reranker (`jina-reranker-v1-turbo-en`) is an **in-core post-fusion stage**
    (`vagus search --rerank`), riding this same `ort` stack — it does **not** touch `rrf()`
    ([ADR 0015](./0015-cross-encoder-rerank.md)).
  - Generative expansion/HyDE runs **locally in core** (tier-1, feature-gated candle,
    [ADR 0016](./0016-local-generative-rewriter.md)) **or** via **Opus in the `/search` skill**
    (tier-2). See the three-tier contract ([ADR 0012](./0012-three-tier-retrieval.md)) and G17/G19.
