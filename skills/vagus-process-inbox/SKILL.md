---
name: vagus-process-inbox
description: Manually process the Vagus inbox, including recent items, into PARA folders (Projects/Areas/Resources/Archive). For "organize my notes", "process my inbox", or "file my notes", offer explicit invocation rather than automatic processing. Requires confirmation for each move. Explicit destinations such as repo documentation, release notes, or another notes app override Vagus.
allowed-tools: Bash(vagus *)
disable-model-invocation: true
user-invocable: true
---

<!-- Canonical source: github.com/vasovagal/vagus, skills/vagus-process-inbox/SKILL.md, embedded in the
     vagus binary. `vagus skills install` writes ~/.claude/skills/vagus-process-inbox/ and
     ~/.pi/agent/skills/vagus-process-inbox/; the next install replaces a hand-edited copy and moves
     the edit to SKILL.md.bak. Fix drift with a PR there, then reinstall after the release. -->

# Process the inbox

Help the user empty `~/brain/00-Inbox/` by filing each note into PARA. This **moves files**, so always
confirm each move before acting. "Organize my notes" alone does not authorize automatic invocation
or filing; the user must invoke this skill explicitly.

When invoked:

1. List the inbox: `vagus inbox --json` (each item is `{path, title}`). If the user requests a time
   window, apply it immediately with `vagus inbox --json --since <duration>` rather than listing and
   filtering manually. Use `h` (hours), `d` (days), `m` (30-day months), or `y` (365-day years);
   minutes use `min` (examples: `10h`, `5d`, `3m`, `1y`). For unqualified “recent,” start with `1m`
   and tell the user which window you used. The filter uses note creation time, with filesystem mtime
   as the fallback for bare notes.
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
3. Summarize what was filed and what remains in the inbox. If processing was time-bounded, distinguish
   older items outside the selected window from items still left in that window.

PARA reminder — file by **actionability**: Projects = a goal with an end state; Areas = an ongoing
responsibility/standard; Resources = a reference topic of interest; Archive = inactive items. When in
doubt between two, prefer the more actionable bucket.
