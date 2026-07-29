---
name: search
description: Search the vagus second-brain vault with hybrid full-text + semantic search. Use when the user wants to find, look up, recall, retrieve, or surface notes, prior research, ideas, snippets, or knowledge from their second brain / vagus vault / knowledge base / personal notes. Not for searching code or the web.
argument-hint: "[query]"
arguments: [query]
allowed-tools: Bash(vagus *), Read
disable-model-invocation: false
user-invocable: true
---

# Search the vault (tier-2 agent judge)

The binary retrieves and cross-encodes; **you** make the final relevance decision from full chunk
text. Never recreate RRF or rank from numbers alone. Shell out to `vagus`, parse JSON, answer from the
best evidence, and keep weak candidates out of the user's context.

## 1. Retrieve a bounded candidate set

```bash
vagus search '<query>' --json --full --rerank --exact --limit 10
```

Shell-quote the query as one literal. `--exact` makes semantic candidates reproducible even on a
large vault; `--rerank` is only a prior, not the final verdict. `--full` supplies the matching chunk
body. Results are distinct notes by default (`path` is unique); use `--chunks` only when the user asks
for exact passages or every occurrence.

Each Hit may contain `{chunk_id, path, heading, score, snippet, rrf, cosine, bm25, rerank, body,
siblings}`. Optional fields can be absent. `siblings > 0` means other chunks in that note also matched.
Do not add `--min-score`: your judgment is the quality floor, and that flag makes the local reranker
score a much deeper pool.

## 2. Grade every candidate

Read each `body` and assign:

- **3 — direct:** answers the question or is strongly on-topic.
- **2 — useful:** partial answer, corroboration, or necessary context.
- **1 — tangential:** related vocabulary/entity but not useful evidence.
- **0 — false positive.**

Only grades **2–3 survive**. Never pad with grade 1 just to hit a count.

Use retrieval order, `bm25`/`cosine`/`rerank`, and `siblings` only as weak priors. Body evidence wins.
If one promising chunk is ambiguous, Read `~/brain/<path>` once; do not reread a note per chunk.
Prefer the smallest nonredundant set: several notes repeating one fact are not several useful results.

## 3. Answer concisely; show at most 6 notes

Start with a direct synthesis when the evidence supports one. Then give **1–6** evidence bullets,
ordered by your grades (retrieval order breaks ties):

- `path › heading`
- quote only the 1–3 most relevant lines
- one short sentence explaining the match

One excellent note is a complete result. Never dump full bodies, expose numeric grading, or announce
that you omitted quota-filling candidates.

## 4. One fallback only when nothing scores 2+

Choose exactly one retry based on the query; do not fan out routinely:

- rare literal, identifier, error text, filename, or proper noun → `--mode bm25`
- conceptual paraphrase with few shared words → `--mode vec --exact`

Keep `--json --full --rerank --limit 10`, grade again, and stop. If nothing reaches 2, say no confident
match was found and offer to broaden or ask for another clue. Do not tick on this path.

## 5. Record only presented notes

After answering, tick exactly the unique paths you actually cited, once:

```bash
vagus tick '<path1>' '<path2>'
```

Single-quote every path; escape an embedded single quote as `'\''`. Never tick retrieved-but-dropped
notes. Tick output is bookkeeping: do not relay it, retry it, or let failure block the answer.

## Scope

Search inherits `.vagus/config.json` exclusions from the current code directory. JSON stays on stdout;
an elision notice goes to stderr. Use `--all` only when the user asks for the whole vault or the likely
answer is hidden by that scope. Paths are relative to `~/brain`.
