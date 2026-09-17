---
name: create-note
description: Create and index a new Markdown note in the vagus second-brain inbox. Use when the user wants to save, capture, jot, record, note down, or add a thought, idea, finding, link, snippet, or reference into their second brain / vagus vault / knowledge base / personal notes.
argument-hint: "[title]"
arguments: [title]
allowed-tools: Bash(vagus *)
disable-model-invocation: false
user-invocable: true
---

<!-- Canonical source: github.com/vasovagal/vagus, skills/create-note/SKILL.md, embedded in the
     vagus binary. `vagus skills install` writes ~/.claude/skills/create-note/ and
     ~/.pi/agent/skills/create-note/; the next install replaces a hand-edited copy and moves
     the edit to SKILL.md.bak. Fix drift with a PR there, then reinstall after the release. -->

# Create note

Capture a note from this conversation into the vagus inbox (`~/brain/00-Inbox/`) and index it.

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
   filed into PARA later with the process-inbox skill (`/process-inbox` in Claude Code;
   `/skill:process-inbox` in pi).

Do **not** hand-write YAML frontmatter — `vagus` adds `created` / `status: inbox` / `source`
automatically.
