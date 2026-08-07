# ADR 0013 — Chunk budget tied to the embedder's context window

- **Status:** Accepted (2026-05-30)

## Context

The v1/v2 chunker split only at H1–H3 headings, so a heading-less or very long section became **one
unbounded chunk**. With bge-small's 512-token context, oversize chunks were silently truncated at
embed time — only the first 512 tokens were vectorized, the rest invisible to semantic search. Adopting
EmbeddingGemma-300M (2048-token context, [ADR 0006](./0006-embeddings-local-no-daemon.md)) widens the
window but doesn't remove the underlying issue: a chunk should be sized for **retrieval precision**,
not left to chance.

## Decision

**Chunk size is budgeted to the embedder's context window.** The *rule* is general; the *value* is
derived from the current embedder:

- **Target ~900 tokens, ~128-token overlap** (aping qmd), comfortably under EmbeddingGemma's 2048.
- A section over budget is **sub-split on paragraph boundaries**, greedily packed, with the overlap
  tail re-prepended (snapped to whitespace) so retrieval context carries across the seam.
- **Token estimate is dep-free:** `chars / 3.5` (conservative for token-dense content) — no tokenizer
  in the chunk hot path (G11).
- **Fenced code blocks are atomic.** Because we now split *within* a section, the chunker tracks
  prose-vs-code segments and never cuts inside a fenced block; an over-budget code block is emitted
  whole as its own chunk (BM25 indexes it fully; the embedding sees its first-window prefix — an
  accepted trade to keep code findable and intact). Without this, the old "code blocks stay whole"
  guarantee would silently become a lie.
- **Producer metadata follows the same budget.** [ADR 0028](./0028-searchable-producer-metadata.md)
  appends valid JSON projections as a separate chunk kind. Whitespace-free values are character-split
  rather than allowed to exceed the embedding window, and kind-aware rerank neighborhoods keep those
  chunks from consuming body-context slots.
- Bump `CHUNK_VERSION` on any change here (force a one-time reindex; G4). v3 = this sub-splitting;
  v6 = kind-separated searchable producer metadata.

## Consequences

- Recorded as guardrail **G20**. If the embedder changes, **re-derive the budget** from its context
  window (the rule is fixed; 900/128 is EmbeddingGemma's instantiation).
- Long notes now yield multiple chunks (per-note chunk counts rise ~20% on long notes); `chunk_id`
  stays `sha256(path + "#" + ord)` with dense `ord`, so ids remain stable for a stable file. ADR 0028
  appends metadata after content, preserving existing content ordinals when metadata is added.
- Overlap introduces intentional duplication across sub-chunks (better recall at a seam); negligible
  at personal scale.
