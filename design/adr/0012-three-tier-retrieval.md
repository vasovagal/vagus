# ADR 0012 — Three-tier retrieval (floor / shell-local / Opus-skill)

- **Status:** Accepted (2026-05-30); **amended 2026-07-25** — the tier-2 Agent Skill supports both
  Claude Code and pi; **amended 2026-07-29** — tier-0 `--limit` is an adaptive context ceiling via
  [ADR 0023](./0023-adaptive-context-tidy-results.md), and tier-2 uses a bounded 10-candidate exact+
  reranked judge with a grade-2 presentation floor. Supersedes the planned (never-written)
  "two-tier" ADR; the earlier framing lives in
  [`plan-advanced-search-three-tier.md`](../plan-advanced-search-three-tier.md).

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
| **0 — floor** | `vagus search "q"` | BM25 + cosine + **RRF k=60**; optional post-rank low-signal suffix drop | none (deterministic) |
| **1 — shell + local** | `vagus search "q" --smart` (or `--rerank` / `--rewrite`) | local rewrite (`lex:`/`vec:`/`hyde:`) → multi-query retrieve → RRF → **in-core cross-encoder rerank** | local (candle, [ADR 0016](./0016-local-generative-rewriter.md)) |
| **2 — skill + Opus** | bundled search Agent Skill (`/search` in Claude Code; `/skill:search` in pi) | 10 exact+reranked full-body candidates → agent 0–3 judge → grade ≥2, max 6; one modality-selected fallback only if none survive | Opus |

The tier-2 channel is the **Agent Skill**, not a Claude Code-specific command surface. The same
standards-compatible `SKILL.md` is embedded once and installed by
`vagus skills install --agent <claude|pi>` into the harness's global discovery directory. Claude Code
remains the backward-compatible default target; pi honors `PI_CODING_AGENT_DIR` and loads the skill
under its `/skill:search` command. Opus remains the intended tier-2 model regardless of harness.

- **Tiers 1 and 2 share the retrieval + rerank core but have different budgets.** Tier 1 owns the
  typed `lex:/vec:/hyde:` generative rewrite. Tier 2 normally spends its stronger host-model reasoning
  on full-body judgment, not routine query fan-out; only when zero candidates grade ≥2 may it make one
  modality-selected retry (BM25 for rare literals, exact vector for conceptual paraphrase). The skill
  literally wraps the CLI, so retrieval itself cannot silently diverge.
- **The reranker is a scoring model, in core** ([ADR 0015](./0015-cross-encoder-rerank.md)) — available
  to both tier 1 and tier 2 (`--rerank`).
- **The generative rewriter is tier-1-local; tier-2 reformulation is a bounded fallback, never
  tier-0.** The local rewriter is opt-in and offline ([ADR 0016](./0016-local-generative-rewriter.md)).
  The skill does not routinely fan out expansion/HyDE because every extra full-body retrieval consumes
  agent context; its one retry is selected by query shape only after the initial judge finds nothing.
- **RRF is untouched** (G8): `Σ 1/(k+rank)`, k=60, no normalization. Reranking is a *separate
  post-fusion stage*, not an edit to fusion. qmd's weighted-RRF / top-rank bonus / position-blend are
  **rejected** (they would breach G8). Likewise, ADR 0023's tier-0 context gate is a separate
  order-preserving suffix drop over the finished RRF list; it never edits fusion.

## 2026-07-29 bounded-skill evidence

A five-query corpus-grounded comparison used the same primary qrels as the ADR 0019 recall audit.
Body-token estimates are `ceil(chars/4)` and exclude JSON syntax; latency is one-shot wall time:

| Skill retrieval preset | Primary recall | MRR | Full-body est. tokens | Aggregate latency |
|---|---:|---:|---:|---:|
| old: ANN + rerank, `--limit 20` | 5/5 | 0.829 | 39,096 | 12.84 s |
| chosen: `--exact --rerank --limit 10` | 5/5 | 0.833 | 20,267 (**−48.16%**) | 9.44 s (**−26.45%**) |
| considered: `--exact --rerank --limit 8` | 5/5 | 0.840 | 17,389 (**−55.52%**) | 8.78 s |

The eight-candidate preset won slightly on this tiny primary-qrel set, but was rejected: after
false-positive and redundancy drops it leaves too little room for corroborating notes. Ten aligns with
maximum and keeps a modest judging buffer. Twenty was pure over-retrieval because the old skill then
presented only 5–10.

The skill text itself shrank 5,316 → 3,790 characters (estimated 1,329 → 948 tokens, **−28.67%**) while
retaining JSON semantics, note/chunk guidance, safe ticking, fallback, and scope behavior. Combining
skill text plus candidate bodies over five invocations gives 45,741 → 25,007 estimated tokens
(**−45.33%**), before the additional output saving from the new “grade ≥2, max 6, never pad” rule.

`--min-score` was rejected for the default skill command: with rerank it intentionally lifts the
cross-encoder cap and scores the whole pool (ADR 0015), adding latency while duplicating the agent's
own relevance floor. Routine BM25+vector fan-out was likewise rejected; one fallback is allowed only
when the initial ten contain no useful evidence.

## Consequences

- Recorded as guardrail **G19**. The old "G17 = no LLM in the binary" becomes a *tiered* statement
  (see G17): tier-0 has no generation; tier-1 may compile a local generative model into `vagus`
  (feature-gated, lazily downloaded); tier-2 uses Opus. No cloud, no daemon in any tier (G14).
- `vagus search` gains `--rerank`, `--full`, `--min-score` (shipped) and later `--rewrite`/`--smart`
  (tier-1 generation, [ADR 0016](./0016-local-generative-rewriter.md)).
- The default bare `vagus search` (tier 0) keeps the same retrieval/ranking and <1s model path, but
  its result **count** may now be lower than `--limit` when ADR 0023 finds a guarded RRF knee.
  `--exhaustive` restores the legacy fill/count/content; the Hit JSON schema remains unchanged.
- Tier-2 retrieval is deliberately bounded: ten candidate bodies, only grade 2–3 evidence presented,
  at most six nonredundant notes, and no quota padding. A single shape-selected retry is the only
  expansion path when the first pass has no useful evidence.
- Skill installation is harness-selectable without duplicating skill bodies; the existing no-flag
  command continues to target Claude Code, while `--agent pi` targets pi.
- Advanced search is **not** a plugin: the capture-shaped NDJSON plugin protocol
  ([ADR 0011](./0011-plugin-protocol.md)) doesn't fit a search-time transform, and the reranker/rewriter
  are neither networked nor foreign-runtime, so they belong in core (see ADR 0015/0016 for the
  rejected-plugin rationale).
