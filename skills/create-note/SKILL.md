---
name: create-note
description: Create and index a new Markdown note in the vagus second-brain inbox. Use when the user wants to save, capture, jot, record, note down, or add a thought, idea, finding, link, snippet, or reference into their second brain / vagus vault / knowledge base / personal notes.
argument-hint: "[title]"
arguments: [title]
allowed-tools: Bash(vagus *)
disable-model-invocation: false
user-invocable: true
---

# Create note

Capture a note from this conversation into the vagus inbox (`~/brain/00-Inbox/`) and index it.

When invoked:

1. Choose a concise **title** — use the `$0` argument if given, otherwise infer a short descriptive one.
2. Compose the note **body** in Markdown from the relevant conversation content — the actual
   idea / finding / snippet / links, *not* a summary of the chat. Keep it atomic (one idea per note).
3. Run this with the Bash tool, piping your composed body on stdin (heredoc):

   ```
   vagus add-note "<title>" --source "<url or 'chat session'>" --print-path <<'NOTE'
   <your composed Markdown body>
   NOTE
   ```

4. Tell the user the path it printed. The note is now in `~/brain/00-Inbox/` and searchable; it can be
   filed into PARA later with `/process-inbox`.

Do **not** hand-write YAML frontmatter — `vagus` adds `created` / `status: inbox` / `source`
automatically.
