# ADR 0006 — Embeddings: local, no daemon (fastembed bge-small)

- **Status:** Accepted (2026-05-29)

## Context

Semantic search needs embeddings. The user chose "local, no daemon" (privacy + no background service).
Ollama is not installed; the user is on Apple Silicon.

## Options considered

| Option | Offline | Daemon | Notes |
|---|---|---|---|
| **fastembed (ONNX, bge-small 384d)** | ✅ | no | model ~130 MB; runs in-process via `ort`; needs `libonnxruntime.dylib` |
| Ollama (nomic-embed-text 768d) | ✅ | **yes** | better quality, but a daemon to install/run |
| Cloud (Voyage / OpenAI) | ❌ | no | best quality, but note text leaves the device |
| candle (bge-small safetensors) | ✅ | no | same quality, **no ONNX dylib**, hand-rolled tokenize |
| model2vec (potion, static) | ✅ | no | pure-Rust single binary, ~11 MTEB pts worse |

## Decision

**fastembed with `bge-small-en-v1.5` (384-dim)** as the default. Local, offline after first download,
no daemon, good quality at personal scale. The backend stays swappable.

## Consequences

- First **run** downloads the model (~130 MB) to `~/Library/Caches/vagus/models`. **Set the cache dir
  explicitly** — fastembed defaults to `./.fastembed_cache` in the CWD ([guardrail G10](../guardrails.md)).
- The default build is **binary + `libonnxruntime.dylib`** (see [tradeoffs §D](../tradeoffs.md)).
- **Escape hatch:** `model2vec` (dylib-free single binary, lower quality — lean on BM25) or `candle`
  (same bge quality, no ONNX). Switch via the embedding-backend seam if the dylib ever bites.
- `ort` is version-locked by fastembed (`=2.0.0-rc.12`) — don't bump it independently.
