# ADR 0007 — Retrieval engine: evaluated `frankensearch`, decided to hand-roll

- **Status:** Accepted (2026-05-29). Superseded the "lean on frankensearch" default after evaluation;
  retrieval is hand-rolled per [ADR 0003](./0003-search-stack.md).

## Context

[ADR 0001](./0001-build-vs-adopt.md) decided to build the second-brain layer and *lift* the retrieval
core, defaulting to depending on/vendoring `frankensearch` (MIT) — which implements tantivy BM25 +
vector cosine + RRF k=60 + f16-SIMD brute force + watch + JSON. This ADR records the evaluation.

## Evaluation (2026-05-29)

- **Published, but lagging.** `frankensearch` is on crates.io at **0.3.2** (component crates
  `frankensearch-core/-index/-embed/-lexical` at 0.2.x), while the GitHub repo is at **v1.2.5**.
  Depending on crates.io gets old code; a git-pin tracks a fast-moving **solo (~56★)** project.
- **Architectural mismatch.** It sits on top of the *same* crates we already use directly (`tantivy`,
  `fastembed`) and would impose its own index layout, model-cache location, and embedder abstraction —
  which we'd then have to bend to our guardrails (iCloud split, `meta` pinning, tantivy↔SQLite
  consistency, vectors-as-SQLite-BLOBs). Net coupling cost > savings.
- **Small primitives.** Our retrieval is modest: tantivy BM25 (crate already integrated), brute-force
  cosine over normalized f32 (~20 LOC), RRF k=60 (~15 LOC). ~150 LOC total — less than the glue to
  adapt frankensearch to our layout.
- We deliberately **did not** run a full `cargo add frankensearch` + build, since that pulls the heavy
  `fastembed`→`ort` tree (which we add anyway, directly) only to discard the wrapper; availability +
  fit were decisive.

## Decision

**Hand-roll the retrieval** per [ADR 0003](./0003-search-stack.md) (tantivy + fastembed + brute-force
cosine + RRF), keeping `frankensearch` and `qmd` as **design references, not dependencies**.

## Consequences

- Clean, fully-owned dependency tree; the index layout honors our guardrails directly.
- No bus-factor dependency on a solo project whose release cadence lags its repo.
- We write ~150 LOC of well-understood retrieval glue (already budgeted in [§12 effort](../../)).
- Guardrails updated: G11 now says "retrieval is hand-rolled; frankensearch/qmd are references."
- If retrieval ever outgrows brute force (huge corpus), revisit an ANN crate or re-evaluate adopting an
  engine — and pin/vendor it then.
