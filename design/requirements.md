# Requirements

Use these requirements to assess product changes. The functional and operational requirements follow
the purpose below; scope and non-goals bound them. Detailed implementation rules live in
[`guardrails.md`](guardrails.md).

## Problem / intent

Capture thoughts and references as plain Markdown, organize them with
[PARA](./methodology-para.md), and retrieve them by keyword or meaning. Use the CLI directly or
through the Claude Code and pi skills.

## Functional requirements

- **F1 — PARA vault of plain Markdown.** Folders `00-Inbox / 10-Projects / 20-Areas / 30-Resources /
  40-Archive` (+ optional `50-Meta`). One `.md` file per note; whole-folder `mv` for lifecycle moves.
  `vagus init [--icloud]` creates the skeleton explicitly; iCloud setup is fail-closed and never moves
  or recursively deletes an occupied vault.
- **F2 — Capture without required metadata.** `vim ~/brain/00-Inbox/idea.md`, type, save — *no required frontmatter*.
  Also the vagus-create-note skill to capture from a Claude Code or pi session. `inbox --since` can narrow
  the processing list by the same note-creation rule as search without requiring prior indexing.
- **F3 — Hybrid search.** Full-text (BM25) **and** semantic (embeddings) retrieval over the vault,
  fused into one ranked result list, exposed both as a CLI (`--json`) and to coding agents. Plain
  hybrid note search treats `--limit` as a context-conscious ceiling: a robust RRF knee may omit only
  a low-signal suffix and must not cross a top-three BM25/cosine source champion; `--exhaustive`
  restores legacy fill (ADR 0023). Unweighted RRF k=60 remains the only production default; a
  same-candidate-pool alternate is explicit-only and must clear ADR 0025 before it may land. The
  separate cross-encoder may opt into tokenizer-safe small-to-big context (`--rerank-context 1|2`),
  while radius 0 remains the exact center-only default and returned bodies stay unchanged (ADR 0015).
  Valid producer JSON frontmatter is retrievable through dedicated lexical + semantic chunks (for
  example, finding generated notes by model); Vagus lifecycle frontmatter stays out of chunk text
  (ADR 0028).
- **F4 — Incremental indexing.** Re-index changed files (mtime + content hash) and detect deletions.
  Plain `reindex` rebuilds derived stores or resumes an interrupted compatible rebuild.
  `reindex --since <duration>` snapshots the whole vault and
  force-refreshes notes in a recent filesystem-mtime window while preserving older indexed notes;
  forced usearch mutations persist, and incomplete embedding rows trigger implicit per-file repair.
  Every applicable `--since` uses one validated grammar, including hours, days, months, and years.
- **F5 — Assisted filing.** The vagus-process-inbox skill has the agent propose a PARA destination + title +
  tags for each inbox note; on user approval, the note is moved and its frontmatter enriched.
- **F6 — Coding-agent skills.** `vagus-create-note`, `vagus-search`, and `vagus-process-inbox` shell out to the
  `vagus` CLI and install into Claude Code or pi's global skills directory. Search uses a bounded
  10-candidate exact+reranked context, presents only grade ≥2 evidence (max 6), never pads, and
  atomically records only cited-note counters/provenance from its fixed unfiltered primary path.
  Explicit user time windows are applied natively with `--since`; filtered primary citations are
  counter-only. Process-inbox applies the same duration grammar instead of post-filtering a full list.
- **F7 — Obsidian compatibility.** The vault opens in Obsidian unchanged (plain `.md`, optional
  `[[wikilinks]]` and YAML frontmatter); editable on mobile via iCloud.
- **F8 — Reproducible retrieval evaluation.** `vagus eval` scores a fixed current index against
  vault-specific JSONL qrels with standard P@k/R@k/MRR@k/nDCG@k semantics, explicit undefined values,
  complete ranked paths, and schema-versioned label/corpus/index/backend/fusion/cohort provenance;
  rerank provenance identifies its exact context/tokenizer policy. `vagus eval-gate` enforces ADR
  0025's non-configurable held-out fusion thresholds (ADRs 0024/0025, G27).
- **F9 — Honest opt-in semantic relevance.** Search may report only finite original-query
  EmbeddingGemma cosine clamped to `[0,1]` under a model/chunk-named policy, explicitly as a heuristic
  rather than probability. An explicit floor filters the already-truncated prefix without reorder or
  backfill; positive floors drop unknown/BM25-only hits. Ranking and default human/JSON output remain
  unchanged, and eval exposes the same policy for private-corpus evidence (ADR 0026, G9e).
- **F10 — Honest local presentation provenance.** An explicit fixed search path may return a
  self-verifying binary/pipeline/corpus wrapper plus path-bound source/fusion/rerank/final ranks
  without changing ranking or default JSON. Capped-tail hits are unscored, never assigned rerank ranks. The
  skill writes only cited paths, one run, events, and counters atomically. Reports group by pipeline
  and corpus and label agent-selection bias; they never substitute for qrel evaluation (ADR 0021,
  G9f/G25).

## Non-functional requirements

- **N1 — Local-first & private.** Core retrieval works offline once its models are cached and sends
  no note text to a cloud service. A host agent's data handling is outside that guarantee.
  Presentation logs omit query text by default and never store bodies/snippets; query storage
  requires separate opt-in. Plain `doctor` is network-incapable; missing models download on first
  use or explicit `doctor --fetch-models` consent.
- **N2 — No background daemon** in the default path; indexing is on demand; a watcher is deferred.
- **N3 — Durable & recoverable.** Markdown is the source of truth; the index is a rebuildable cache.
  Local usage counters and presentation runs/events are the named non-rebuildable exception and every
  reindex preserves them. iCloud holds *only* Markdown (ADRs 0004/0021).
- **N4 — Interactive retrieval at personal scale.** Exact cosine is automatic below 10,000 chunks;
  usearch HNSW handles larger indexes. `--exact` remains available in every search mode. Model startup
  can dominate end-to-end latency, and wider rerank context costs more time and memory, so it stays
  opt-in. See the scoped measurements in [ADR 0019](adr/0019-usearch-ann-backend.md#2026-07-30-corpus-and-cutoff-evidence) and
  [ADR 0015](adr/0015-cross-encoder-rerank.md#2026-07-30-small-to-big-evidence); they are observations, not latency or memory guarantees.
- **N5 — Rust without a managed runtime.** The Vagus binary and its optional companions/plugins statically link native
  dependencies and require no Python/Node/TypeScript runtime. Model caches are separate from binary
  size and download on demand. See [ADR 0014](adr/0014-self-contained-universe.md).
  **Policy conflict:** the [plugin contract](../docs/plugin-contract.md) is language-independent,
  while ADR 0014 includes plugins in this no-managed-runtime requirement. Resolving that scope
  requires an owner decision and ADR update; this documentation change does not exempt plugins.
- **N6 — Small surface.** Use focused glue over mature crates; no novel algorithms. The original
  500–800-line budget is a design target, not a claim about the current source size.

## Scope (v1)

Indexing + hybrid search + capture + assisted filing + the three skills, on one Mac (Apple Silicon).

## Non-goals

- **Not** a cloud/SaaS service; no server, no account.
- **Not** a multi-device *write* store for the index — the index is per-machine and rebuilt locally;
  only the Markdown syncs (via iCloud).
- **Not** an Obsidian replacement — Obsidian remains an optional GUI over the same files.
- No requirement for a single executable. The runtime boundary is N5 above; see the
  [linkage tradeoffs](tradeoffs.md#d-the-onnx-single-binary-reality-verified-on-this-build).
- **No** automatic filing/moving of notes without explicit user approval.
- **No** cloud LLM calls and **no daemon**, in any tier. Generation is *tiered*, not banned: a
  cross-encoder reranker (a scoring model) is in core; generative rewriting/HyDE is an opt-in,
  feature-gated local model (tier-1). Tier-2 uses its host agent for bounded body judgment and allows
  one reformulation fallback only after no useful first-pass hit — never a vagus cloud call or
  background service ([ADR 0012](./adr/0012-three-tier-retrieval.md), G17/G19).
