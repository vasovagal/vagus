# ADR 0005 — Assisted, on-demand PARA filing

- **Status:** Accepted (2026-05-29)

## Context

Notes land in `00-Inbox/` (via `vim` or `/create-note`). PARA wants them filed into
Projects/Areas/Resources/Archive. How much should the tool automate that?

## Options considered

1. **Assisted, on demand** — `/process-inbox`: Claude reads each inbox note, proposes a destination +
   cleaned title + tags; on user approval, `vagus file` moves it and enriches frontmatter.
2. **Automatic on capture** — auto-categorize each new note into PARA immediately.
3. **Manual only** — user `mv`s notes; the tool never moves anything.

## Decision

**Option 1.** Matches PARA's deliberate weekly-review ritual while keeping effort low (Claude does the
proposing). Search works on inbox notes regardless of filing, so nothing is lost while unfiled.

## Consequences

- `vagus file <path> --to <dir>` does the mechanical move + frontmatter normalization + reindex;
  `vagus file <path> --suggest` returns ranked PARA destinations (from hybrid search over existing
  notes) as JSON for the skill to propose.
- The `process-inbox` skill is **`disable-model-invocation: true`** (side-effecting; user-triggered) so
  Claude never silently reorganizes the vault.
- Filing is **never automatic** ([guardrail G15](../guardrails.md)).
