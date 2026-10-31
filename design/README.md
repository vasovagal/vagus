# design/ — the vagus design record

Use this design record to find the requirements, binding invariants, and decisions behind Vagus.
Read the relevant contract before changing architecture; use the index below to find its owner.

## How to use it

- Read the binding [`guardrails.md`](./guardrails.md) and the relevant ADR before an architectural change.
- Change a decision with a superseding ADR or a dated amendment; retain the original reasoning.
  Update this index, `guardrails.md`, and the root `CLAUDE.md` summary in the same change.
- Add new significant decisions as `adr/NNNN-title.md`, following the existing format.

## Contents

| File | What |
|---|---|
| [`roadmap.md`](./roadmap.md) | Current capabilities, their owners, milestones, and deferred work. |
| [`requirements.md`](./requirements.md) | Functional and operational requirements, scope, and non-goals. |
| [`guardrails.md`](./guardrails.md) | Binding implementation invariants. |
| [`tradeoffs.md`](./tradeoffs.md) | Comparison tables: build-vs-adopt, embedding backends, the ONNX single-binary reality, vector-store options. |
| [`prior-art.md`](./prior-art.md) | Tools surveyed, with borrow/reject notes and links. |
| [`methodology-para.md`](./methodology-para.md) | Why the vault follows PARA and CODE. |
| [`adr/`](./adr/) | Architecture Decision Records: context, options, decision, and consequences. |

## ADR index

- [`0001-build-vs-adopt.md`](adr/0001-build-vs-adopt.md) — Build the note-management layer; retrieval-engine choice superseded by 0007.
- [`0002-language-rust.md`](adr/0002-language-rust.md) — Choose Rust.
- [`0003-search-stack.md`](adr/0003-search-stack.md) — Use BM25, cosine, and RRF k=60; vector backend superseded by 0019.
- [`0004-icloud-markdown-only.md`](adr/0004-icloud-markdown-only.md) — Keep derived storage outside the vault and make iCloud setup fail closed.
- [`0005-assisted-filing.md`](adr/0005-assisted-filing.md) — Require user approval for PARA filing.
- [`0006-embeddings-local-no-daemon.md`](adr/0006-embeddings-local-no-daemon.md) — Use local embeddings, explicit model fetching, and pinned document recipes.
- [`0007-lean-on-frankensearch.md`](adr/0007-lean-on-frankensearch.md) — Hand-roll retrieval after evaluating `frankensearch`; retain it as a reference.
- [`0008-naming.md`](adr/0008-naming.md) — Name the project `vagus` under `vasovagal`.
- [`0009-cwd-scoped-search.md`](adr/0009-cwd-scoped-search.md) — Inherit search exclusions from the working directory.
- [`0010-plugin-subcommands.md`](adr/0010-plugin-subcommands.md) — Dispatch external `vagus-<name>` commands.
- [`0011-plugin-protocol.md`](adr/0011-plugin-protocol.md) — Define the plugin NDJSON event stream.
- [`0012-three-tier-retrieval.md`](adr/0012-three-tier-retrieval.md) — Select retrieval by channel; bound the search skill and its citation recording.
- [`0013-chunk-budget.md`](adr/0013-chunk-budget.md) — Budget chunks against model context while keeping code fences atomic.
- [`0014-self-contained-universe.md`](adr/0014-self-contained-universe.md) — Require no managed runtime, rather than a single executable.
- [`0015-cross-encoder-rerank.md`](adr/0015-cross-encoder-rerank.md) — Add local reranking with bounded, tokenizer-safe context.
- [`0016-local-generative-rewriter.md`](adr/0016-local-generative-rewriter.md) — Add opt-in local rewriting and HyDE through candle.
- [`0017-indexed-frontmatter-filters.md`](adr/0017-indexed-frontmatter-filters.md) — Define shared note-age semantics and post-rank metadata filters.
- [`0018-multi-agent-guardrails.md`](adr/0018-multi-agent-guardrails.md) — Isolate agents in worktrees and require PRs except for releases.
- [`0019-usearch-ann-backend.md`](adr/0019-usearch-ann-backend.md) — Select exact cosine below 10,000 chunks and embedded usearch above it.
- [`0020-note-level-results.md`](adr/0020-note-level-results.md) — Count distinct notes by default; retain `--chunks` as an opt-out.
- [`0021-usage-ticks.md`](adr/0021-usage-ticks.md) — Retain local counters and validated cited-note provenance through reindexing.
- [`0022-mtime-windowed-reindex.md`](adr/0022-mtime-windowed-reindex.md) — Repair a filesystem-mtime window while reconciling the whole vault.
- [`0023-adaptive-context-tidy-results.md`](adr/0023-adaptive-context-tidy-results.md) — Allow a guarded low-signal suffix cut in plain hybrid note search.
- [`0024-retrieval-eval-harness.md`](adr/0024-retrieval-eval-harness.md) — Evaluate fixed indexes with reproducible labels, metrics, and provenance.
- [`0025-evidence-gated-fusion.md`](adr/0025-evidence-gated-fusion.md) — Gate explicit fusion experiments; keep RRF k=60 as the default.
- [`0026-bounded-semantic-relevance.md`](adr/0026-bounded-semantic-relevance.md) — Expose bounded cosine as a heuristic with an optional drop-only floor.
- [`0027-producer-frontmatter-metadata.md`](adr/0027-producer-frontmatter-metadata.md) — Accept validated producer metadata on explicit note creation.
- [`0028-searchable-producer-metadata.md`](adr/0028-searchable-producer-metadata.md) — Index producer JSON separately from body text and lifecycle fields.
- [`0029-checkpointed-resumable-indexing.md`](adr/0029-checkpointed-resumable-indexing.md) — Checkpoint indexing and resume rebuilds only on explicit commands.
