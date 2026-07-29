# design/ — the vagus design record

This folder is the durable record of **what we built, what we considered, and why**. It exists so
future sessions (human or Claude) inherit the reasoning instead of re-litigating settled decisions or
silently breaking an invariant.

## How to use it

- **Read before any architectural change.** Start with [`guardrails.md`](./guardrails.md) (binding) and
  the relevant ADR.
- **When you change a decision, update the matching ADR** (don't delete history — add a new ADR that
  supersedes the old one, or amend with a dated note). The root `CLAUDE.md` summarizes the invariants;
  keep it in sync with `guardrails.md`.
- **New significant decision?** Add `adr/NNNN-title.md` using the same format.

## Contents

| File | What |
|---|---|
| [`roadmap.md`](./roadmap.md) | **Where we're going**: the three-tier direction, capability×home×status, milestones. |
| [`requirements.md`](./requirements.md) | Functional + non-functional requirements, scope, **non-goals**. |
| [`guardrails.md`](./guardrails.md) | The canonical hard-invariant list (binding). |
| [`tradeoffs.md`](./tradeoffs.md) | Comparison tables: build-vs-adopt, embedding backends, the ONNX single-binary reality, vector-store options. |
| [`prior-art.md`](./prior-art.md) | Tools surveyed, with borrow/reject notes and links. |
| [`methodology-para.md`](./methodology-para.md) | PARA / CODE domain-model reference — the *why* behind the vault shape. |
| [`adr/`](./adr/) | Architecture Decision Records (one per fork): context · options · decision · consequences. |

## ADR index

- `0001-build-vs-adopt.md` — build the second-brain layer fresh; lean on `frankensearch` for retrieval.
- `0002-language-rust.md` — Rust over Python.
- `0003-search-stack.md` — tantivy + fastembed/ort + brute-force cosine + RRF(k=60). *(vector component
  superseded by 0019.)*
- `0004-icloud-markdown-only.md` — iCloud holds Markdown only; index/DB/cache live outside iCloud.
- `0005-assisted-filing.md` — assisted, on-demand PARA filing (never automatic).
- `0006-embeddings-local-no-daemon.md` — local fastembed; no Ollama/cloud by default.
- `0007-lean-on-frankensearch.md` — depend/vendor the retrieval engine (pending smoke test).
- `0008-naming.md` — `vagus` / `vasovagal`.
- `0009-cwd-scoped-search.md` — CWD-inherited `.vagus` exclusion rules for search.
- `0010-plugin-subcommands.md` — plugins via external `vagus-<name>` subcommands on `$PATH`.
- `0011-plugin-protocol.md` — plugin ↔ core NDJSON event stream (logs/progress/notes/result).
- `0012-three-tier-retrieval.md` — floor / shell+local / Opus Agent Skill tiers (Claude Code + pi),
  channel-selected; tier-2 uses a bounded 10-candidate, grade≥2/max-6 context contract.
- `0013-chunk-budget.md` — chunk size tied to the embedder context window; fenced code atomic.
- `0014-self-contained-universe.md` — identity reframe: "no versioned runtime," not "single binary."
- `0015-cross-encoder-rerank.md` — in-core `jina-reranker-v1-turbo-en` (`--rerank`); amends 0003 + G17.
- `0016-local-generative-rewriter.md` — tier-1 local rewriter (candle + qmd GGUF); amends G17 (next).
- `0017-indexed-frontmatter-filters.md` — `search --since`/`--source` via SQLite-denormalized
  `created_at`/`source` (no tantivy schema change); adds G9b.
- `0018-multi-agent-guardrails.md` — worktree-per-agent (convention), no direct commits to `main`,
  worktree janitor, soft breadcrumb nudge; adds G21–G24.
- `0019-usearch-ann-backend.md` — embedded usearch HNSW vector index (statically linked), exact
  brute-force fallback; supersedes 0003's vector component; updates G5/G11/G13.
- `0020-note-level-results.md` — `--limit` counts distinct notes (best chunk + `siblings` count) via
  a post-rank dedup stage; `--chunks` opts out; adds G9c.
- `0021-usage-ticks.md` — usage ticks (`vagus tick`/`fame`): local user data in meta.db, survives
  reindex, re-keyed on `vagus file` moves; adds G25, amends G2/G5.
- `0022-mtime-windowed-reindex.md` — `reindex --since <duration>` snapshots the whole vault and
  force-refreshes the recent filesystem-mtime window without wiping older indexed notes; adds G26.
- `0023-adaptive-context-tidy-results.md` — plain hybrid note search treats `--limit` as a ceiling,
  dropping only a statistically distinct low-signal RRF suffix; `--exhaustive` restores legacy fill;
  adds G9d and amends 0012/0020.
