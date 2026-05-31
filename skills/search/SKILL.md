---
name: search
description: Search the vagus second-brain vault with hybrid full-text + semantic search. Use when the user wants to find, look up, recall, retrieve, or surface notes, prior research, ideas, snippets, or knowledge from their second brain / vagus vault / knowledge base / personal notes. Not for searching code or the web.
argument-hint: "[query]"
arguments: [query]
allowed-tools: Bash(vagus *), Read
disable-model-invocation: false
user-invocable: true
---

# Search the vault (tier-2 Opus reranking)

You are the tier-2 generative reranker (G17/G19). The binary does deterministic
retrieval + an in-core cross-encoder; YOU judge relevance on the full body text,
drop false positives, and reorder. Never re-derive RRF or reimplement search —
shell out to `vagus` and parse `--json` (G13).

## 1. Retrieve 20 candidates

```bash
vagus search "<query>" --json --full --rerank --limit 20
```

- `--full` adds `body` (full chunk text) to each hit.
- `--rerank` adds the in-core cross-encoder `rerank` logit and reorders by it.
- Each hit: `{chunk_id, path, heading, score, snippet, rrf?, cosine?, bm25?, rerank?, body?}`.
  Optional fields appear only when their flag is set. Paths are relative to `~/brain`.
- Optional soft floor: add `--min-score 15` (drops hits below 15% of the top hit).
  Keep it low — a high floor starves the judge. Omit if unsure.
- Note: `--full`/`--rerank` trigger a one-time ~150MB reranker model download on first use.

## 2. Judge each (query, chunk) pair — the actual reranking

For every candidate, read its **full `body`** and assign a 0–3 relevance grade:

- **3** — directly answers / strongly on-topic.
- **2** — relevant, partial or supporting.
- **1** — tangential; keep only if little else.
- **0** — false positive. **DROP it** (quality floor).

Rules:

- Lean primarily on the **body text** — this is the whole point of pulling `--full`.
- Use retrieval rank + the `bm25`/`cosine` split + the in-core `rerank` score as a
  **weak prior** (position-aware blend): a chunk the corpus signal ranked #1 starts
  with mild benefit of the doubt, but body judgment overrides it.
- Do **not** just re-sort by `score`/`rrf`/`rerank` — that's a no-op. Do **not**
  ignore those signals entirely either.
- Reorder surviving chunks by your judged grade (break ties with the weak prior).

## 3. Present top 5–10

For each survivor, in judged order:

- Header: `path › heading`
- The most relevant lines from the body (quote, don't dump the whole chunk).
- A one-line **why this matches**.

## 4. Drill in on request

If the user wants more from a hit, Read the full note at `~/brain/<path>` and answer
from it, citing the path.

## 5. No results

If nothing survives the floor: say so, offer to broaden the query, or retry with
`--mode bm25` (exact keywords) or `--mode vec` (semantic).

## Directory scoping

Searches are silently scoped to the current working directory: `vagus` walks up from the CWD for
`.vagus/config.json` files (an "inherited config") and elides hits whose vault path contains an
excluded word (case-insensitive substring; e.g. `"scientist"` hides everything under
`.../scientist/...`). When some are hidden, a `— N hit(s) elided by inherited config (--all to show)`
line is printed — to stderr under `--json`, so the JSON array shape is unchanged. Pass `--all` to
ignore scoping and show every result. These config files live in the user's code dirs, never the
vault.
