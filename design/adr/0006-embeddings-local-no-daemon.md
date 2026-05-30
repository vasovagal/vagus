# ADR 0006 — Embeddings: local, no daemon (fastembed EmbeddingGemma-300M)

- **Status:** Accepted (2026-05-29); **amended 2026-05-30** to adopt EmbeddingGemma-300M (was
  bge-small-en-v1.5). The "local, no daemon, fastembed/ort, swappable backend" decision is unchanged —
  only the model identity changed.

## Context

Semantic search needs embeddings. The user chose "local, no daemon" (privacy + no background service).
Ollama is not installed; the user is on Apple Silicon.

## Options considered

| Option | Offline | Daemon | Notes |
|---|---|---|---|
| **fastembed (ONNX, EmbeddingGemma-300M 768d)** | ✅ | no | **chosen (2026-05-30)**; ~1.23 GB, 2048-ctx, +8 MTEB, multilingual; in-process via `ort` |
| fastembed (ONNX, bge-small 384d) | ✅ | no | original default (2026-05-29); ~130 MB, 512-ctx, English |
| Ollama (nomic-embed-text 768d) | ✅ | **yes** | better quality, but a daemon to install/run |
| Cloud (Voyage / OpenAI) | ❌ | no | best quality, but note text leaves the device |
| candle (bge-small safetensors) | ✅ | no | same quality, **no ONNX dylib**, hand-rolled tokenize |
| model2vec (potion, static) | ✅ | no | pure-Rust single binary, ~11 MTEB pts worse |

## Decision

**fastembed with `EmbeddingGemma-300M` (768-dim, 2048-token context)** as the default
(`EmbeddingModel::EmbeddingGemma300M`, a built-in fastembed 5.14 variant — a one-line swap from
bge-small). Local, offline after first download, no daemon. ~+8 MTEB over bge-small, 100+ languages,
and a 4× wider context window that dissolves the old 512-token silent-truncation problem. It's a
*vectorizer*, not a generative LLM — legitimately in core (G17). The backend stays swappable.

**Prefixes (G9):** EmbeddingGemma is prompt-templated and fastembed does **not** apply the template, so
we prepend it ourselves — query `task: search result | query: {text}`, document
`title: none | text: {text}` (documents were *un*-prefixed under bge — this is a real behavior change).
Vectors are L2-normalized (G7). Changing model + dims (384→768) forces one `vagus reindex` (G4); we bump
`CHUNK_VERSION` in the same change so the reindex is automatic.

## Consequences

- First **run** downloads the model (**~1.23 GB** fp32, vs ~130 MB for bge-small) to
  `~/Library/Caches/vagus/models`, outside iCloud. **Set the cache dir explicitly** — fastembed
  defaults to `./.fastembed_cache` in the CWD ([guardrail G10](../guardrails.md)). (q4/q8 ~197/309 MB
  is a later footprint lever via the user-defined-ONNX path — deferred.)
- **Gemma license:** EmbeddingGemma carries Google's Gemma terms (use restrictions). Fine for a
  personal vault; **flag before any redistribution** of the model or a bundle that includes it.
- Vectors widen 384→768 (2× the SQLite BLOB store) — negligible at personal scale; no Matryoshka
  truncation for now.
- **Verified:** the default build **statically links** onnxruntime (`libonnxruntime.a`); the installed
  binary references only system dylibs — effectively **self-contained** (see [tradeoffs §D](../tradeoffs.md)).
- **Escape hatch:** `model2vec` (dylib-free single binary, lower quality — lean on BM25) or `candle`
  (same bge quality, no ONNX). Switch via the embedding-backend seam if the dylib ever bites.
- `ort` is version-locked by fastembed (`=2.0.0-rc.12`) — don't bump it independently.
