# Requirements

## Problem / intent

A personal "second brain": capture thoughts and references with near-zero friction, organize them with
the [PARA](./methodology-para.md) method, and **find them again** later by keyword *or* by meaning —
all local, private, and durable as plain text. Driven from the terminal or an Agent Skills-compatible
coding harness (Claude Code and pi).

## Functional requirements

- **F1 — PARA vault of plain Markdown.** Folders `00-Inbox / 10-Projects / 20-Areas / 30-Resources /
  40-Archive` (+ optional `50-Meta`). One `.md` file per note; whole-folder `mv` for lifecycle moves.
  `vagus init [--icloud]` creates the skeleton explicitly; iCloud setup is fail-closed and never moves
  or recursively deletes an occupied vault.
- **F2 — Frictionless capture.** `vim ~/brain/00-Inbox/idea.md`, type, save — *no required frontmatter*.
  Also the create-note skill to capture from a Claude Code or pi session. `inbox --since` can narrow
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
- **F4 — Incremental indexing.** Re-index only changed files (mtime + content hash); detect deletions;
  `reindex` rebuilds from scratch. `reindex --since <duration>` snapshots the whole vault and
  force-refreshes notes in a recent filesystem-mtime window while preserving older indexed notes;
  forced usearch mutations persist, and incomplete embedding rows trigger implicit per-file repair.
  Every applicable `--since` uses one validated grammar, including hours, days, months, and years.
- **F5 — Assisted filing.** The process-inbox skill has the agent propose a PARA destination + title +
  tags for each inbox note; on user approval, the note is moved and its frontmatter enriched.
- **F6 — Coding-agent skills.** Create-note, search, and process-inbox Agent Skills shell out to the
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

- **N1 — Local-first & private.** Works fully offline after first run; no note text leaves the machine
  by default. Presentation logs omit query text by default and never store bodies/snippets; query
  storage requires separate explicit opt-in. Plain `doctor` is network-incapable; model downloads
  occur only on first model use or explicit `doctor --fetch-models` consent.
- **N2 — No background daemon** in the default path; indexing is on-demand (a watcher is opt-in, later).
- **N3 — Durable & recoverable.** Markdown is the source of truth; the index is a rebuildable cache.
  Local usage counters and presentation runs/events are the named non-rebuildable exception and every
  reindex preserves them. iCloud holds *only* Markdown (ADRs 0004/0021).
- **N4 — Fast enough.** Backend retrieval stays comfortably interactive at personal scale: exact
  cosine is automatic below 10,000 chunks (a 10k×768 fixture is ~30 MiB and measures ~26.6 ms including
  SQLite load), then usearch HNSW supplies growth headroom. One-shot end-to-end search may be about one
  second when local model initialization dominates; explicit `--exact` remains an all-mode oracle.
  Widened cross-encoder context is deliberately opt-in: measured radius 1/2 rerank stages are about
  2.8/5.7 s median and can peak near 3.2/5.6 GiB RSS on the representative Apple Silicon corpus.
- **N5 — Owned in Rust, no versioned runtime.** A self-contained Rust universe — the `vagus` binary
  (plus optional `vagus-<name>` companions/plugins), each statically linking its native deps
  (onnxruntime today; candle/ggml where justified). **No Python/Node/TS/managed runtime to reconcile.**
  Binary size ≠ model footprint (models are a lazily-downloaded cache). The author maintains the code.
  ([ADR 0014](./adr/0014-self-contained-universe.md))
- **N6 — Small surface.** ~500–800 LOC of our own glue over mature crates; no novel algorithms.
- **N7 — Optional local observability.** Explicitly enabled runtime traces use the shared immutable
  schema-v1 catalogue and private local JSONL only: no collector, endpoint, upload, arbitrary path/
  attribute, or user-content/raw-error field. Tracing is off by default; invalid or compiled-out
  integration is a no-op and cannot change command output or failure behavior (ADR 0029/G28).

## Scope (v1)

Indexing + hybrid search + capture + assisted filing + the three skills, on one Mac (Apple Silicon).

## Non-goals

- **Not** a cloud/SaaS service; no server, no account.
- **Not** a multi-device *write* store for the index — the index is per-machine and rebuilt locally;
  only the Markdown syncs (via iCloud).
- **Not** an Obsidian replacement — Obsidian remains an optional GUI over the same files.
- **Not** bound to a *single executable* — but bound to **no managed runtime**. vagus may be several
  self-contained Rust binaries (core + `vagus-<name>`); none requires Python/Node/TS
  ([ADR 0014](./adr/0014-self-contained-universe.md)). (The ONNX path statically links onnxruntime, so
  the binary is in fact self-contained — see [tradeoffs §D](./tradeoffs.md).)
- **No** automatic filing/moving of notes without explicit user approval.
- **No** cloud LLM calls and **no daemon**, in any tier. Generation is *tiered*, not banned: a
  cross-encoder reranker (a scoring model) is in core; generative rewriting/HyDE is an opt-in,
  feature-gated local model (tier-1). Tier-2 uses its host agent for bounded body judgment and allows
  one reformulation fallback only after no useful first-pass hit — never a vagus cloud call or
  background service ([ADR 0012](./adr/0012-three-tier-retrieval.md), G17/G19).
