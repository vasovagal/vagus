---
name: search
description: Search the vagus second-brain vault with hybrid full-text + semantic search, including recent or explicitly time-bounded notes. Use when the user wants to find, look up, recall, retrieve, or surface notes, prior research, ideas, snippets, or knowledge from their second brain / vagus vault / knowledge base / personal notes. Not for searching code or the web.
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
vagus search '<query>' --json --full --rerank --exact --limit 10 --tick-provenance
```

Shell-quote the query as one literal. `--exact` makes semantic candidates reproducible even on a
large vault; `--rerank` is only a prior, not the final verdict. `--full` supplies the matching chunk
body.

When the request includes a time window, apply it during retrieval instead of fetching all time and
filtering afterward:

```bash
vagus search '<topic query>' --since <duration> --json --full --rerank --exact --limit 10
```

Use the user's exact window. Units are `h` (hours), `d` (days), `m` (30-day months), and `y`
(365-day years); minutes use `min`. Examples: `10h`, `5d`, `3m`, `1y`. For unqualified “recent” or
“latest,” start with `1m` and state that window in the answer. Strip the temporal phrase from the
query when a meaningful topic remains. `--since` filters note creation time (`created` frontmatter,
then filesystem mtime), not dates mentioned inside note bodies.

The unfiltered command responds with `{run,hits}`. Grade `hits`; retain `run` and each cited hit's
`provenance` for step 5. A `--since` search must omit `--tick-provenance` (filtered runs cannot use
that fixed provenance contract), so it returns the ordinary Hit array and uses counter-only ticking.
For an explicit request for every passage, likewise omit `--tick-provenance`, add `--chunks`, parse
the Hit array, and use counter-only ticking.

Hits may contain `{path, heading, score, snippet, rrf, cosine, bm25, rerank, body, siblings,
provenance}`. Fields may be absent; `siblings > 0` means more chunks matched. Provenance is bookkeeping,
never a relevance verdict.
Do not add `--min-score`: your judgment is the quality floor, and that flag makes the local reranker
score a much deeper pool. Do not add `--relevance` or `--min-relevance`: bounded cosine is a local
heuristic, while your grade from the full body is this tier's final relevance decision.

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

Keep `--json --full --rerank --limit 10`, preserve the exact `--since` window when one was requested,
grade again, and stop. If nothing reaches 2, say no confident match was found and offer to broaden
the time window or ask for another clue. Do not tick on this path.

## 5. Record only presented notes

After an unfiltered primary search, copy `run` verbatim and only cited `{path,provenance}` pairs into
one compact object:

```bash
vagus tick --events '{"run":<complete run>,"events":[{"path":<cited path>,"provenance":<complete provenance>}]}'
```

Do not include the query. Escape an embedded single quote as `'\''`; never include dropped notes.
For a primary `--since` or explicit `--chunks` search, use one counter-only
`vagus tick '<path1>' '<path2>'`. The step-4 retry remains unticked. Never relay/retry tick output or
let failure block the answer.

## Scope

Search inherits `.vagus/config.json` exclusions from the current code directory. JSON stays on stdout;
an elision notice goes to stderr. Use `--all` only when the user asks for the whole vault or the likely
answer is hidden by that scope. Paths are relative to `~/brain`.
