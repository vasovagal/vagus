# ADR 0028 — Searchable producer metadata as dedicated chunks

- **Status:** Accepted (2026-08-07)
- **Amends:** ADRs 0013, 0017, 0020, and 0027; adds G9g

## Context

ADR 0027 lets generated notes carry validated, namespaced JSON in YAML frontmatter, but deliberately
left it outside the search index pending an explicit decision. That makes Corti provenance readable on
the note yet impossible to recall: a user cannot ask for transcripts produced by Parakeet, a particular
Corti release, live mode, or a quality setting unless those words also happen to occur in the transcript.

Indexing the complete YAML block as ordinary note text would make noisy lifecycle fields (`created`,
`status`, `source`, `para`, `modified`, `title`) affect BM25 and embeddings. Repeating one note-level
metadata object on every content chunk would over-weight long notes and crowd the candidate pool. A
Tantivy-only metadata field would omit semantic retrieval and cross-encoder evidence, as well as require
an on-disk Tantivy schema migration.

## Decision

1. **Project only producer-shaped fields.** Within a complete leading `---` block, Vagus considers a
   top-level field searchable when its key satisfies ADR 0027's safe grammar, is not Vagus-owned, and its
   one-line value parses as JSON. Object keys and scalar values are flattened deterministically into
   whitespace-normalized search text. Arbitrary YAML and Vagus lifecycle fields remain excluded. This
   recognizes both `add-note --frontmatter-json` output and equivalent Markdown without adding a YAML
   dependency.
2. **Use dedicated first-class chunks.** Each qualifying top-level field becomes one or more
   `ProducerMetadata` chunks headed `Frontmatter > <key>`. They are appended after ordinary content so
   existing content ordinals/chunk IDs stay stable. The normal ~900-token budget and ~128-token overlap
   apply; even an unbroken JSON string is hard-split within the budget.
3. **Send the same text through every retrieval store.** Metadata chunks are persisted in SQLite,
   indexed by Tantivy BM25, embedded by EmbeddingGemma, mirrored into usearch, hydrated, displayed, and
   eligible for the cross-encoder exactly like other chunks. A provenance query therefore returns a
   transparent metadata hit rather than an unrelated transcript snippet. Note-level dedup still returns
   at most one best chunk per note.
4. **Do not pollute body context windows.** `chunks.kind` distinguishes content (`0`) from producer
   metadata (`1`). `--rerank-context` selects adjacent chunks of the center's own kind only, so an
   appended metadata chunk cannot displace actual transcript neighbors; a large metadata field may still
   supply adjacent metadata context to a metadata hit.
5. **Roll as chunk version 6.** The shape and embedding corpus changed, so `CHUNK_VERSION` forces one
   automatic full reindex/re-embed. The index remains a derived cache (G2) rebuilt entirely from the
   Markdown source of truth.

## Consequences

- Queries such as `vagus search parakeet` can retrieve Corti notes through lexical and semantic paths;
  `--full` shows the normalized producer metadata that matched, and the heading identifies its namespace.
- Generated notes add at least one chunk per producer field, intentionally changing chunk counts and
  potentially retrieval rankings. Metadata is not repeated per transcript/body chunk.
- Vagus-owned lifecycle frontmatter keeps its existing behavior: `created`/`source` are SQLite filter
  fields under ADR 0017, and none of the owned fields become BM25/embedding text.
- Upgrade cost is one full-vault re-embedding. Subsequent edits remain ordinary mtime+hash incremental
  reconciliation across all G5 stores.
- JSON-compatible custom frontmatter written outside `add-note` is also searchable when it follows the
  same key/value shape. Block-style YAML is not; supporting general YAML would require a separate parser
  and decision.
