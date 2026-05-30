---
name: search
description: Search the vagus second-brain vault with hybrid full-text + semantic search. Use when the user wants to find, look up, recall, retrieve, or surface notes, prior research, ideas, snippets, or knowledge from their second brain / vagus vault / knowledge base / personal notes. Not for searching code or the web.
argument-hint: "[query]"
arguments: [query]
allowed-tools: Bash(vagus *)
disable-model-invocation: false
user-invocable: true
---

# Search the second brain

Query the vagus vault and present the most relevant notes.

When invoked:

1. Run with the Bash tool, using `$0` / the user's phrasing as the query:

   ```
   vagus search "<query>" --json --limit 10
   ```

2. Parse the JSON array of hits — each is `{chunk_id, path, heading, score, snippet}` (hybrid hits
   also carry `rrf` + `cosine` + `bm25` components; `score` is the fused rank score, not a similarity).
3. Present a short ranked list: for each, show the location (`path › heading`) and the snippet.
   Paths are relative to `~/brain`.
4. If the user needs more than a snippet, read the full note with the Read tool at `~/brain/<path>`,
   then answer from it (cite the path).
5. If there are no results, say so and offer to broaden the query or try `--mode bm25` (exact keywords)
   or `--mode vec` (semantic).

Default mode is hybrid (BM25 + semantic, RRF-fused). Only report what `vagus` returns — don't invent notes.

## Directory scoping

Searches are silently scoped to the current working directory: `vagus` walks up from the CWD for
`.vagus/config.json` files (an "inherited config") and elides hits whose vault path contains an
excluded word (case-insensitive substring; e.g. `"scientist"` hides everything under
`.../scientist/...`). When some are hidden, a `— N hit(s) elided by inherited config (--all to show)`
line is printed — to stderr under `--json`, so the JSON array shape is unchanged. Pass `--all` to
ignore scoping and show every result. These config files live in the user's code dirs, never the
vault.
