# ADR 0012 — Three-tier retrieval (floor / shell-local / Opus-skill)

- **Status:** Accepted (2026-05-30). Supersedes the planned (never-written) "two-tier" ADR; the
  earlier two-tier framing lives in [`plan-advanced-search-three-tier.md`](../plan-advanced-search-three-tier.md).

## Context

We want `tobi/qmd`-class retrieval quality without abandoning vagus's identity (local-first, offline,
no managed runtime — [ADR 0014](./0014-self-contained-universe.md)). qmd's edge over a plain BM25 +
vector + RRF core is three add-ons: **query expansion**, **HyDE**, and **cross-encoder reranking**.

A *two-tier* model was first proposed (CLI = an LLM-free ceiling; the `/search` skill = SOTA via Opus).
But that caps the terminal experience at pure RRF and only delivers smarts when Claude is in the loop.
The author wants the shell to be genuinely good **on its own** — "if you're in the shell, use the
better-than-nothing local models" — *and* the skill to be SOTA when Opus is present. Both tiers should
stand on their own.

## Decision

Adopt a **three-tier** contract. The **channel selects the tier** — no mode flags for "smartness," no
escalation prompts.

| Tier | Channel | Pipeline | Generation |
|---|---|---|---|
| **0 — floor** | `vagus search "q"` | BM25 + cosine + **RRF k=60** | none (deterministic) |
| **1 — shell + local** | `vagus search "q" --smart` (or `--rerank` / `--rewrite`) | local rewrite (`lex:`/`vec:`/`hyde:`) → multi-query retrieve → RRF → **in-core cross-encoder rerank** | local (candle, [ADR 0016](./0016-local-generative-rewriter.md)) |
| **2 — skill + Opus** | the `/search` skill | Opus expansion + HyDE + full-body judge **on top of** `vagus search --json --full --rerank` | Opus |

- **Tiers 1 and 2 are parallel.** They reuse the **same** retrieval + rerank core and the **same**
  typed `lex:/vec:/hyde:` discipline; they differ only in *who generates the rewrite* (a local model
  vs. Opus). The skill literally wraps the CLI, so the tiers can't silently diverge.
- **The reranker is a scoring model, in core** ([ADR 0015](./0015-cross-encoder-rerank.md)) — available
  to both tier 1 and tier 2 (`--rerank`).
- **The generative rewriter is tier-1-local or tier-2-Opus, never tier-0.** The local rewriter is
  opt-in and offline ([ADR 0016](./0016-local-generative-rewriter.md)).
- **RRF is untouched** (G8): `Σ 1/(k+rank)`, k=60, no normalization. Reranking is a *separate
  post-fusion stage*, not an edit to fusion. qmd's weighted-RRF / top-rank bonus / position-blend are
  **rejected** (they would breach G8).

## Consequences

- Recorded as guardrail **G19**. The old "G17 = no LLM in the binary" becomes a *tiered* statement
  (see G17): tier-0 has no generation; tier-1 may compile a local generative model into `vagus`
  (feature-gated, lazily downloaded); tier-2 uses Opus. No cloud, no daemon in any tier (G14).
- `vagus search` gains `--rerank`, `--full`, `--min-score` (shipped) and later `--rewrite`/`--smart`
  (tier-1 generation, [ADR 0016](./0016-local-generative-rewriter.md)).
- The default bare `vagus search` (tier 0) stays byte-identical and < 1s — the fast path is unchanged.
- Advanced search is **not** a plugin: the capture-shaped NDJSON plugin protocol
  ([ADR 0011](./0011-plugin-protocol.md)) doesn't fit a search-time transform, and the reranker/rewriter
  are neither networked nor foreign-runtime, so they belong in core (see ADR 0015/0016 for the
  rejected-plugin rationale).
