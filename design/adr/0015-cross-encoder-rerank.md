# ADR 0015 — In-core cross-encoder reranker

- **Status:** Accepted (2026-05-30). Amends [ADR 0003](./0003-search-stack.md) and guardrail G17.

## Context

RRF (k=60) fuses two *rank-based* signals; it can't read a candidate's full text against the query.
qmd's biggest precision lever beyond RRF is a **cross-encoder reranker** that re-scores the fused
top-N. We want that lever in the *shell* (tier-1, [ADR 0012](./0012-three-tier-retrieval.md)) — useful
with no Claude in the loop.

A cross-encoder is a **scoring model, the same category as the embedder** — not a generative LLM. The
decisive finding: `fastembed::reranking::{TextRerank, RerankerModel}` is already in the
`fastembed` 5.14 dependency, running on the **exact same `ort`/onnxruntime stack** vagus already links.
So a reranker adds **zero new heavy dependencies** and preserves the self-contained binary (G13).

We verified that qmd's own reranker, **Qwen3-Reranker-0.6B, does *not* fit:** it's a *decoder* scored
by yes/no-token logprobs, which `fastembed`'s `TextRerank` (a single classifier logit, `logits[.., 0]`)
cannot run — it would force a second runtime (llama.cpp). So we deliberately deviate from qmd here.

## Decision

Add an **in-core** reranker (`src/rerank.rs`, mirroring `src/embed.rs`):

- Model **`jina-reranker-v1-turbo-en`** (`RerankerModel::JINARerankerV1TurboEn`) — a true BERT
  cross-encoder, 37.8M params, ~150MB ONNX, 8192-token context, English-first. Lazily downloaded to
  `~/Library/Caches/vagus/models` (G6/G10); tens of ms for 20–30 candidates on Apple Silicon CPU.
- Exposed via **`vagus search --rerank`** (opt-in). It re-scores the **fused RRF candidate pool**
  (a deeper set, `(limit*4).max(30)`) against **full chunk bodies**, reorders, then truncates to
  `--limit`. The raw cross-encoder logit is carried as `Hit.rerank`; the displayed/`score` value is its
  sigmoid (ordering signal → 0–1).
  - **Amended 2026-07-08:** the reranker scores only the top `(limit*2).max(16)` of that pool (the
    forward pass is ~75% of `--rerank` wall time, and dedup/truncate keep only `limit` notes anyway).
    Retrieval/filter/dedup still run at full pool depth; the un-scored tail keeps its RRF order (and
    its raw RRF `score`) after the reranked prefix, so note fill is unchanged. Two cap consequences are
    deliberate: (a) only the top `cap` RRF candidates are eligible to be reranked into the results — a
    recall-vs-latency tradeoff (a strong cross-encoder can no longer lift a note from beyond `cap`);
    (b) head and tail live on different score scales (sigmoid vs raw RRF), so a `--rerank` hit's
    `score` is **not** comparable across the head/tail boundary (a `--json` consumer must not re-sort
    by `score`). Because `--min-score` is a *relative-to-top* floor, comparing a raw-RRF tail against
    the sigmoid head top would floor the whole tail out and drop tail-filled slots the full-pool rerank
    would have kept; so when a `--min-score` floor is active the cap is lifted (the whole pool is
    reranked), restoring the pre-cap fill exactly for that combination.
- **RRF is untouched (G8).** Reranking is a separate post-fusion stage; the default (no `--rerank`)
  path and its `--json` shape stay byte-identical (the `rerank`/`body` fields are `skip_serializing_if`
  omitted when unset — G9a).

## Alternatives considered

- **Ape Qwen3-Reranker-0.6B** — rejected: decoder, not fastembed-compatible; forces llama.cpp + ~640MB
  + a generative model in core. The English cross-encoder is the right tool and rides the stack.
- **A `vagus-rerank` plugin** — rejected: the capture-shaped NDJSON protocol
  ([ADR 0011](./0011-plugin-protocol.md)) doesn't fit a query+candidates→reordered transform (stdin is
  inherited, the stream is one-way note→index), and a reranker is neither networked nor a foreign
  runtime, so the plugin boundary (G18) buys nothing — only per-search process-spawn + model-reload
  cost. It belongs in core.
- **`jina-reranker-v2-base-multilingual`** — the stack-native upgrade if the vault ever needs
  multilingual reranking (still a fastembed cross-encoder, in-core); not the default (heavier, English
  vault).

## Consequences

- G17 is amended: a deterministic cross-encoder scorer is allowed in core (like the embedder); the
  no-LLM line now governs *generative* models (see G17/G19).
- `--rerank` is the shared rerank lever for tier-1 (shell) and tier-2 (the `/search` skill judges full
  bodies *and* can pre-rerank with this).
- First reranker model → an ADR-gated addition (G11). `doctor` reports whether it's cached without
  forcing the download.
