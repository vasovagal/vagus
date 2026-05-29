# ADR 0003 — Search stack: tantivy + fastembed + brute-force cosine + RRF

- **Status:** Accepted (2026-05-29)

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
- **Fusion: Reciprocal Rank Fusion, k=60** (`score = Σ 1/(k + rank)`), optionally weighting the
  original-query BM25 list ×2. Rank-based ⇒ no score normalization.
- **bge prefixing:** query gets `"Represent this sentence for searching relevant passages: "`;
  documents un-prefixed. Don't double-prefix (respect what the lib already applies).

`frankensearch` implements exactly this core (tantivy + f16-SIMD brute force + RRF k=60); when we depend
on it we configure rather than re-implement ([ADR 0007](./0007-lean-on-frankensearch.md)).

## Key implementation rules

- **tantivy has no `update_document`:** make `path` a `STRING|STORED` field; per changed file
  `delete_term(path)` → re-`add_document` → single `commit()`. Full rebuild = many adds + one commit.
- **Incremental** keyed on `mtime` then `sha256`; `chunk_id = sha256(path + "#" + ord)`.
- **Consistency:** delete tantivy docs *and* SQLite vector rows together (vector store has no FK).
- **Pin** `embed_model` + `embed_dims` + `tantivy_version` in `meta`; mismatch ⇒ `reindex`.
- **Normalize** vectors at insert (cosine = dot product).

## Consequences

- Brute force is fine now; revisit an ANN backend only if the corpus grows by orders of magnitude.
- tantivy 0.x has no index-format compatibility guarantee across minor bumps → `tantivy_version` gate.
- Reranker (`bge-reranker-base`) and query-expansion are **deferred** to a later optional stage / the
  skill layer, not v1 ([guardrail G17](../guardrails.md)).
