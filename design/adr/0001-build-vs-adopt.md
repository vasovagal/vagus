# ADR 0001 — Build the second-brain layer fresh; lean on an engine

- **Status:** Accepted (2026-05-29)

## Context

We want a PARA second brain with hybrid search and Claude Code integration. Could we adopt an existing
tool instead of building? Surveyed `qmd`, `memex`, `basic-memory`, `khoj`, `txtai`, `iwe`, `MALD`,
`papers-cli` (see [prior-art](../prior-art.md)).

## Options considered

1. **Adopt a full app** (qmd / basic-memory / memex / khoj) — fastest, but each imposes its own model
   (TS/Bun + 2 GB models; AGPL + knowledge-graph; Postgres/Django) and none does PARA + our iCloud split.
2. **Adopt a Rust all-in-one** (iwe / MALD) — iwe's search is fuzzy/graph only (no BM25/embeddings) and
   it's link-graph not PARA; MALD is ~4★ solo, no MCP, needs Ollama.
3. **Build fresh in Rust**, lifting the retrieval engine from a maintained crate.

## Decision

**Option 3.** No existing tool hits {hybrid retrieval} ∩ {MCP for Claude} ∩ {pure-Rust} ∩ {PARA}.
Build the second-brain layer (PARA conventions, heading-aware chunking, capture, assisted filing,
skills) ourselves, and **lean on `frankensearch`** for the BM25+vector+RRF retrieval core
([ADR 0007](./0007-lean-on-frankensearch.md)). Bending iwe to PARA+hybrid would be *more* work than
building, and we'd inherit a graph engine we don't want.

## Consequences

- We own a small, single-purpose codebase (~500–800 LOC over mature crates).
- We **lift, don't fork**: `frankensearch` (engine), `codesearch` (MCP shape), `qmd` (chunking/RRF
  spec), `papers-cli` (fastembed/ort wiring), `iwe` (watcher).
- The retrieval engine becomes an external dependency to manage ([ADR 0007](./0007-lean-on-frankensearch.md)).
