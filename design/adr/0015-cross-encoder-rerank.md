# ADR 0015 — In-core cross-encoder reranker

- **Status:** Accepted (2026-05-30); **amended 2026-07-30** to make ordinary `doctor` presence-only,
  add explicit `doctor --fetch-models`, add bounded tokenizer-safe small-to-big rerank context, and
  preserve original-query cosine for ADR 0026 relevance without changing the cap, and expose
  capped-prefix versus unscored-tail state only through ADR 0021's explicit provenance contract.
  Amends [ADR 0003](./0003-search-stack.md) and guardrail G17.

## Context

RRF (k=60) fuses two *rank-based* signals; it can't read a candidate's full text against the query.
qmd's biggest precision lever beyond RRF is a **cross-encoder reranker** that re-scores the fused
top-N. We want that lever in the *shell* (tier-1, [ADR 0012](./0012-three-tier-retrieval.md)) — useful
with no Claude in the loop.

A cross-encoder is a **scoring model, the same category as the embedder** — not a generative LLM. The
decisive finding: `fastembed::reranking::{TextRerank, RerankerModel}` is already in the
`fastembed` 5.14 dependency, running on the **exact same `ort`/onnxruntime stack** vagus already links.
So a reranker adds **zero new heavy dependencies** and preserves the self-contained binary (G13).

Embedding/BM25 chunks intentionally target about 900 tokens (ADR 0013/G20), but an answer's premise,
qualification, or causal conclusion can fall in an adjacent chunk. The reranker can judge a wider
read-only view without changing retrieval chunks. One implementation trap is material: the audited
Jina model config has `max_position_embeddings: 8192`, but its `tokenizer_config.json` says
`model_max_length: 512`; fastembed 5.14 silently clamps every requested 1k–8k limit to 512. Merely
concatenating `previous → center → next` would therefore let right truncation erase the matched center.
A safe small-to-big mode must budget with the **actual tokenizer**, validate model capacity before any
override, and give the center priority at encoded inference time.

We verified that qmd's own reranker, **Qwen3-Reranker-0.6B, does *not* fit:** it's a *decoder* scored
by yes/no-token logprobs, which `fastembed`'s `TextRerank` (a single classifier logit, `logits[.., 0]`)
cannot run — it would force a second runtime (llama.cpp). So we deliberately deviate from qmd here.

## Decision

Add an **in-core** reranker (`src/rerank.rs`, mirroring `src/embed.rs`):

- Model **`jina-reranker-v1-turbo-en`** (`RerankerModel::JINARerankerV1TurboEn`) — a true BERT
  cross-encoder, 37.8M params, ~150MB ONNX, 8192-token context, English-first. Lazily downloaded to
  `~/Library/Caches/vagus/models` (G10); current capped 20-candidate center-only inference is
  about 0.7 s on the measured Apple Silicon corpus (widened costs are recorded below).
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
  - **Amended by ADR 0026:** opt-in semantic relevance carries each hit's finite original-query cosine
    unchanged through prefix reordering and the unscored tail; it never repurposes sigmoid/logit as
    confidence. Unlike `--min-score`, `--min-relevance` does not lift the rerank cap: its semantic
    floor runs after truncation without backfill, and a positive floor drops BM25-only unknowns.
  - **Observed by ADR 0021 without changing behavior:** explicit fixed-pipeline provenance assigns a
    rerank rank only to actually scored prefix candidates. Tail candidates retain fusion/final ranks
    but are marked unscored; they can never be reported as cross-encoder rescues.
- Add **`--rerank-context N`**, bounded to `0..=2`, to `search --rerank`, `search --smart`, and
  `eval --rerank`. It reconstructs up to N ordinal neighbors per side from SQLite only for the
  cross-encoder input. It never changes indexed chunks, retrieval, RRF, filters, note dedup, the
  matched `body`/`snippet`, capped-prefix eligibility, or the unscored RRF tail.
  - **N=0 is compatibility mode.** It sends the center body verbatim, performs no neighbor query,
    keeps fastembed's historical effective 512-token limit and default batching, and reproduces
    pre-flag logits/order exactly. The former 1024 constant was only a request; fastembed clamped it.
  - **N=1/2 are explicit expensive modes.** Their tokenizer limits are 3072/5120. Before overriding
    the stale 512 metadata, vagus reads the exact cached HF revision and requires its model config to
    support the requested positions. Widened inference uses batch size one to bound batch-longest
    padding and quadratic-attention memory; `--smart` drops its no-longer-needed embedder session
    before the rerank forward pass.
  - The planner disables truncation on a tokenizer clone and measures each candidate as the actual
    `(query, document)` pair, including special tokens. Starting from the center, it admits whole
    nearest neighbors in natural ordinal order only while the complete encoded pair fits. It never
    skips an oversized nearest neighbor to take a farther one. Thus neighbor text cannot push out the
    center. If a G20-atomic fenced-code center alone exceeds the limit, it is sent center-only and the
    configured tokenizer must still retain center-sequence tokens; otherwise search fails loudly.
  - Schema-2 eval provenance remains structurally compatible and records radius + tokenizer maximum
    in its existing `rerank_policy` field, so unlike rerank inputs cannot be compared accidentally.
- **RRF is untouched (G8).** Reranking is a separate post-fusion stage; the default (no `--rerank`)
  path and its `--json` shape stay byte-identical (the `rerank`/`body` fields are `skip_serializing_if`
  omitted when unset — G9a).

## Alternatives considered

- **Use `chars/3.5` for rerank windows.** Rejected at this boundary. The heuristic is appropriate in
  the index hot path (G20), but the reranker tokenizer is already loaded and is the authority that
  will truncate the pair. An estimate cannot prove center survival or reserve special/query tokens.
- **Concatenate naturally and rely on model truncation.** Rejected: with the effective 512-token cap,
  a large previous chunk can remove the center. Putting center first would retain it but distort note
  order. Exact preflight fitting preserves natural order and avoids truncation for ordinary centers.
- **Raise the default to the formerly intended 1024.** Rejected: the actual historical behavior was
  512, and changing it moved logits/order. Radius 0 is compatibility; widening must be explicit.
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

### 2026-07-30 small-to-big evidence

On the frozen 483-note / 4,148-chunk personal corpus, five qrels were selected before these runs to
span hybrid technical, semantic paraphrase, entity+semantic, transcript, and causal recall. `vagus
eval --exact --rerank --k 10` produced the same schema-2 corpus fingerprint
`5fd75d935c59612709be2b718ed0eadc5cece941197ebdc27559b8c4a3ece98d` for every run:

| radius | known-answer ranks | R@10 | MRR@10 | mean nDCG@10 | median rerank stage | peak process RSS |
|---:|---|---:|---:|---:|---:|---:|
| 0 | 2, 2, 7, 2, 2 | 1.000 | .429 | .571 | .715 s | 1.37 GiB |
| 1 | 1, 2, 3, 7, 2 | 1.000 | .495 | .619 | 2.806 s | 3.17 GiB |
| 2 | 1, 2, 3, 9, 1 | 1.000 | .589 | .686 | 5.723 s | 5.63 GiB |

The sample supports an **opt-in** context lever, not a default: wider context improved aggregate rank
but hurt the transcript answer, and its quadratic cost is large. Therefore N=0 remains the CLI,
`--smart`, eval, and bounded agent-skill default; the skill's fixed ten-candidate command is unchanged.
N=1 is the practical first try; N=2 is reserved for hard boundary-spanning queries on a machine with
adequate memory. These are development qrels, not ADR 0025 promotion evidence.

Compatibility was checked against pristine `69f219a`: raw full-body exact+reranked JSON was
byte-for-byte identical at N=0 for all five qrels, and a cached `--smart` query was also byte-identical.
An adversarial temporary note put a 9,000-word atomic fenced chunk immediately before a tiny matched
center; radius 1 rejected that neighbor under the real tokenizer, completed inference, and returned
the unchanged center body. Unit tests cover capacity mismatch, edge windows, query/special-token
reserve, natural order, no skipped nearest neighbor, and center-over-budget fallback.

### 2026-07-30 doctor/download amendment

The reranker follows the same explicit-consent rule as the embedder (ADR 0006/G10). Plain `doctor`
checks the exact required local snapshot files and never constructs `TextRerank`, because a partial
cache would otherwise trigger network access. `doctor --fetch-models` unconditionally constructs both
ONNX models, runs a one-pair rerank inference, validates a finite result, and returns nonzero if the
reranker or embedder fails. A directory name containing `jina`/`rerank` is not cache completeness.

## Consequences

- G17 is amended: a deterministic cross-encoder scorer is allowed in core (like the embedder); the
  no-LLM line now governs *generative* models (see G17/G19).
- `--rerank` is the shared rerank lever for tier-1 (shell) and tier-2. The bounded `/search` skill
  pre-reranks 10 exact candidates at context radius zero, then independently judges the matched full
  bodies (ADR 0012 amendment); optional small-to-big model input never enlarges agent context.
- First reranker model → an ADR-gated addition (G11). `doctor` reports whether it's cached without
  forcing the download.
