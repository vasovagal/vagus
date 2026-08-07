# ADR 0027 — Safe producer metadata at note creation

- **Status:** Accepted (2026-08-07)
- **Amends:** ADR 0005 and G3 (frontmatter written only by explicit capture/filing actions)

## Context

`vagus add-note` owns a note's initial YAML frontmatter (`created`, `status`, and optional `source`).
External producers can supply only the Markdown body. That is insufficient for generated artifacts whose
provenance affects how a user should interpret them. In particular, Corti transcript quality depends on the
Corti release, live-versus-batch path, ASR/diarization models, and effective quality settings.

Letting a producer send raw YAML would make newline/key injection trivial and let it override fields whose
lifecycle Vagus owns. Adding one Corti-specific flag would put another project's schema in Vagus. Requiring a
new command-line flag from Corti would also make release skew destructive: an older Vagus rejects an unknown
flag and no transcript note is created.

## Decision

1. **`add-note` accepts an optional JSON object.** `--frontmatter-json <OBJECT>` maps each top-level key to
   one frontmatter field. Values are emitted as compact JSON; JSON scalars and flow collections are valid YAML,
   and JSON escaping prevents values from injecting additional lines. The top-level key grammar is restricted
   to ASCII letters/digits/`_`/`-`, beginning with a letter or `_`.
2. **Vagus-owned keys cannot be supplied:** `created`, `status`, `source`, `para`, `modified`, and `title` are
   rejected. Producers should put their schema below one namespaced object (for example `{"corti": {...}}`).
   Input is capped at 64 KiB and parsed/validated before Vagus creates a directory or note.
3. **A version-skew-safe integration channel mirrors the flag.** When the flag is absent, `add-note` reads
   `VAGUS_ADD_NOTE_FRONTMATTER_JSON`. A producer spawning Vagus may set this variable only on the child. A
   current Vagus consumes it; an older Vagus ignores it and still creates the note. The explicit CLI flag wins
   if both are present.
4. **This is creation, not later automatic editing.** The caller explicitly invokes `add-note`, and the
   metadata is written in the same initial file creation as the body. G3 still forbids index/search operations
   from editing notes and still permits frontmatter-free Markdown. `vagus file` preserves producer-owned
   fields while enriching only its own filing fields.
5. **Producer metadata is not indexed as note content.** The existing frontmatter stripping remains
   unchanged. A future searchable producer field requires its own explicit indexing/schema decision.

## Consequences

- Generated notes can carry structured, Obsidian-compatible provenance without Vagus knowing producer
  schemas or accepting raw YAML.
- Ordinary `add-note` output is unchanged byte-for-byte when neither metadata input is present.
- Malformed JSON, unsafe keys, reserved keys, and oversized metadata fail before note creation.
- Environment fallback deliberately favors note availability during staggered upgrades: an old Vagus files a
  note without the new metadata rather than rejecting the capture.
- The environment variable is process input, not persistent configuration; integrations should set it on the
  spawned child instead of exporting it globally.
