---
name: process-inbox
description: Process the vagus second-brain inbox — triage each captured note into a PARA folder (Projects/Areas/Resources/Archive). Use when the user wants to process, file, organize, triage, sort, or clear out their second-brain / vagus inbox.
allowed-tools: Bash(vagus *)
disable-model-invocation: true
user-invocable: true
---

# Process the inbox

Help the user empty `~/brain/00-Inbox/` by filing each note into PARA. This **moves files**, so always
confirm before acting.

When invoked:

1. List the inbox: `vagus inbox --json` (each item is `{path, title}`).
2. For each inbox note, in turn:
   1. Read it with the Read tool at `~/brain/<path>` to understand it.
   2. Get destination ideas: `vagus file "<path>" --suggest --json` — returns ranked PARA folders
      (similar existing notes first, then the vault's PARA folders) as JSON `[{folder, score}]`.
   3. Propose a destination: pick from the suggestions or propose a sensible PARA folder
      (`10-Projects/<name>`, `20-Areas/<name>`, `30-Resources/<topic>`, or `40-Archive/<name>`), plus a
      cleaned-up title if helpful.
   4. **Ask the user to confirm** (or choose a different folder). Never move without an OK.
   5. On confirmation: `vagus file "<path>" --to "<folder>"`. This moves the note, enriches its
      frontmatter (`status`/`para`/`modified`), and reindexes.
3. Summarize what was filed and what remains in the inbox.

PARA reminder — file by **actionability**: Projects = a goal with an end state; Areas = an ongoing
responsibility/standard; Resources = a reference topic of interest; Archive = inactive items. When in
doubt between two, prefer the more actionable bucket.
