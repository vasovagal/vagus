---
name: vagus-create-note
description: Create and index a Markdown note in Vagus when the user asks to capture something — "make a note", "a note of this finding", "save this for later", "jot this down", or "add this to my notes". Generic note capture defaults to Vagus without naming a vault or second brain. Not for finding existing notes or merely asking a question about notes; do not create without capture intent. Explicit destinations such as repo documentation, release notes, or another notes app override this default.
argument-hint: "[title]"
arguments: [title]
allowed-tools: Bash(vagus *)
disable-model-invocation: false
user-invocable: true
---

<!-- Canonical source: github.com/vasovagal/vagus, skills/vagus-create-note/SKILL.md, embedded in the
     vagus binary. `vagus skills install` writes ~/.claude/skills/vagus-create-note/ and
     ~/.pi/agent/skills/vagus-create-note/; the next install replaces a hand-edited copy and moves
     the edit to SKILL.md.bak. Fix drift with a PR there, then reinstall after the release. -->

# Create note

Capture a note from this conversation into the vagus inbox (`~/brain/00-Inbox/`) and index it.

Use `vagus-search` for retrieval instead. A default destination is not permission to write: if the
content to save is unclear, ask before capturing. Respect explicit destinations outside Vagus.

When invoked:

1. Choose a concise **title** — use the invocation argument if given (`$0` in Claude Code; in pi, the
   text appended after the skill block), otherwise infer a short descriptive one.
2. Compose the note **body** in Markdown from the relevant conversation content — the actual
   idea / finding / snippet / links, *not* a summary of the chat. Keep it atomic (one idea per note).
3. Run this with the Bash tool, piping your composed body on stdin (heredoc):

   ```
   vagus add-note "<title>" --source "<url or 'chat session'>" --print-path <<'NOTE'
   <your composed Markdown body>
   NOTE
   ```

   `add-note` writes the note before indexing it, so if the call is killed or times out, check
   `~/brain/00-Inbox/` for the note before retrying — a retry creates a duplicate.

4. Tell the user the path it printed. The note is now in `~/brain/00-Inbox/` and searchable; it can be
   filed into PARA later with the vagus-process-inbox skill (`/vagus-process-inbox` in Claude Code;
   `/skill:vagus-process-inbox` in pi).

Do **not** hand-write YAML frontmatter — `vagus` adds `created` / `status: inbox` / `source`
automatically.
