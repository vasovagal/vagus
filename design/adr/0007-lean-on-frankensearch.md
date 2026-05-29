# ADR 0007 — Lean on `frankensearch` for the retrieval engine

- **Status:** Proposed — confirm via smoke test before committing the dependency (2026-05-29)

## Context

[ADR 0001](./0001-build-vs-adopt.md) decided to build the second-brain layer but lift the retrieval
core. `frankensearch` (MIT, Rust crate + `fsfs` CLI) already implements exactly our core: Tantivy BM25 +
vector cosine + **RRF k=60**, in-process **f16 SIMD brute-force** (optional HNSW), incremental/watch,
JSON output, model cache at `~/.cache/frankensearch/models`, and pluggable embedding backends
(`hash` / `model2vec` / `fastembed`).

## Options considered

1. **Depend on `frankensearch` as a library crate** — least code; configure rather than write §4/§5.
2. **Vendor `frankensearch`** (pin a commit / copy source) — control + insulation from upstream churn.
3. **Hand-roll** tantivy + fastembed + cosine + RRF ourselves — most control, most code; the fallback.

## Decision

**Default to (1) depend, fall back to (2) vendor — to be confirmed by a smoke test** (`cargo add` +
build on darwin-arm64; verify it indexes `.md`, the embedding backend works, and the model cache can be
pointed at `~/Library/Caches/vagus/models`). If the dependency is awkward (the README notes some
sub-crates may not be cleanly publishable yet), **vendor** a pinned snapshot. If neither is clean,
**hand-roll** per [ADR 0003](./0003-search-stack.md).

> **Update this ADR's status with the smoke-test result and the chosen path (depend / vendor / hand-roll).**

## Consequences

- We still build on top: heading-aware Markdown chunking, PARA frontmatter, capture, assisted filing,
  the skills, and (later) the MCP server. `frankensearch` lacks all of these.
- Risk: solo maintainer (~56★). Mitigation: **pin/vendor** ([guardrail G11](../guardrails.md)); the
  hand-roll stack is documented and ready.
- Bonus: `frankensearch`'s `model2vec` backend gives a dylib-free build option for free.
