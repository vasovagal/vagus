# ADR 0012 — Three-tier retrieval (floor / shell-local / Opus-skill)

- **Status:** Accepted (2026-05-30); **amended 2026-07-25** — the tier-2 Agent Skill supports both
  Claude Code and pi; **amended 2026-07-29** — tier-0 `--limit` is an adaptive context ceiling via
  [ADR 0023](./0023-adaptive-context-tidy-results.md), tier-2 uses a bounded 10-candidate exact+
  reranked judge with a grade-2 presentation floor, and ADR 0026 adds orthogonal opt-in semantic
  reporting without changing any tier pipeline; **amended 2026-07-30** — ADR 0021 adds strict,
  query-free-by-default presentation provenance to the fixed tier-2 primary path. Supersedes the planned (never-written)
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
| **1 — shell + local** | `vagus search "q" --smart` (or `--rerank` / `--rewrite`) | local rewrite (`lex:`/`vec:`/`hyde:`) → multi-query retrieve → RRF → **in-core cross-encoder rerank**; optional tokenizer-safe radius 1/2 context | local (candle, [ADR 0016](./0016-local-generative-rewriter.md)) |
| **2 — skill + Opus** | bundled search Agent Skill (`/search` in Claude Code; `/skill:search` in pi) | 10 exact+reranked full-body candidates at context radius 0 + strict run/rank provenance → agent 0–3 judge → grade ≥2, max 6 + atomic cited-note tick; one counter-only fallback if none survive | Opus |

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
  to both tier 1 and tier 2 (`--rerank`). Small-to-big context is an expensive tier-1 opt-in; tier 2's
  fixed command keeps radius 0 so model-input tuning cannot silently change the agent-context contract.
- **The generative rewriter is tier-1-local; tier-2 reformulation is a bounded fallback, never
  tier-0.** The local rewriter is opt-in and offline ([ADR 0016](./0016-local-generative-rewriter.md)).
  The skill does not routinely fan out expansion/HyDE because every extra full-body retrieval consumes
  agent context; its one retry is selected by query shape only after the initial judge finds nothing.
- **Production RRF is untouched** (G8): `Σ 1/(60+rank)`, no normalization; reranking remains a
  separate stage. ADR 0025 permits a same-pool alternate only as explicit, fixed-gate experiment—not
  in any default tier—and default promotion needs another ADR. Likewise, ADR 0023's tier-0 context
  gate consumes only standard RRF scores and never edits fusion.
- **ADR 0026 relevance is orthogonal to tier selection.** Plain/ordinary-reranked search may opt into
  a bounded original-query cosine heuristic and post-truncation floor; `--smart` rejects it because
  typed multi-query fusion does not retain that signal. The tier-2 skill keeps its stronger 0–3
  full-body judgment and does not reinterpret local cosine as agent confidence.
- **ADR 0021 provenance observes tier 2; it does not rank.** The fixed primary command emits a
  self-verifying run plus honest capped-prefix/tail rank states without changing any result. The skill
  atomically records only cited notes; query text is off, and fallback searches remain counter-only.
  Result reports are selection-biased diagnostics, never ADR 0024 evaluation evidence.

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

The bounded-skill change itself shrank text 5,316 → 3,790 characters (estimated 1,329 → 948 tokens,
**−28.67%**). ADR 0026 guidance and ADR 0021's strict run/event copying bring the current skill to
4,344 characters (~1,086 tokens): still **−18.28%** from the old prompt. The compact, path-bound
run/rank wrapper adds 11,584 characters (~2,896 tokens) across the same five queries on the current
496-note/4,381-chunk generation (`corpus_sha256=46269684…`). Applying that schema-dominated overhead
to the frozen candidate-body budget gives ~28,593 estimated tokens (**−37.49%** versus 45,741), before
the output saving from “grade ≥2, max 6, never pad.” Default JSON syntax is excluded from both sides.

`--min-score` was rejected for the default skill command: with rerank it intentionally lifts the
cross-encoder cap and scores the whole pool (ADR 0015), adding latency while duplicating the agent's
own relevance floor. Routine BM25+vector fan-out was likewise rejected; one fallback is allowed only
when the initial ten contain no useful evidence.

## Consequences

- Recorded as guardrail **G19**. The old "G17 = no LLM in the binary" becomes a *tiered* statement
  (see G17): tier-0 has no generation; tier-1 may compile a local generative model into `vagus`
  (feature-gated, lazily downloaded); tier-2 uses Opus. No cloud, no daemon in any tier (G14).
- `vagus search` gains `--rerank`, `--full`, `--min-score`, opt-in `--relevance`/
  `--min-relevance` (shipped), and later `--rewrite`/`--smart` (tier-1 generation,
  [ADR 0016](./0016-local-generative-rewriter.md)); relevance remains unavailable to smart fusion.
- The default bare `vagus search` (tier 0) keeps the same retrieval/ranking and <1s model path, but
  its result **count** may now be lower than `--limit` when ADR 0023 finds a guarded RRF knee.
  `--exhaustive` restores the legacy fill/count/content; the default Hit JSON schema remains unchanged
  unless ADR 0026 reporting is explicit.
- Tier-2 retrieval is deliberately bounded: ten candidate bodies, only grade 2–3 evidence presented,
  at most six nonredundant notes, and no quota padding. A single shape-selected retry is the only
  expansion path when the first pass has no useful evidence.
- Skill installation is harness-selectable without duplicating skill bodies; the existing no-flag
  command continues to target Claude Code, while `--agent pi` targets pi.
- Advanced search is **not** a plugin: the capture-shaped NDJSON plugin protocol
  ([ADR 0011](./0011-plugin-protocol.md)) doesn't fit a search-time transform, and the reranker/rewriter
  are neither networked nor foreign-runtime, so they belong in core (see ADR 0015/0016 for the
  rejected-plugin rationale).
