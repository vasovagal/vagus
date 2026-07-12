---
name: search
description: Search the vagus second-brain vault with hybrid full-text + semantic search. Use when the user wants to find, look up, recall, retrieve, or surface notes, prior research, ideas, snippets, or knowledge from their second brain / vagus vault / knowledge base / personal notes. Not for searching code or the web.
argument-hint: "[query]"
arguments: [query]
allowed-tools: Bash(vagus *), Read
disable-model-invocation: false
user-invocable: true
---

# Search the vault (tier-2 Opus judge)

You are the tier-2 generative reranker. The binary does deterministic retrieval + an in-core
cross-encoder; YOU triage the compact hits, judge full text for a shortlist, drop false positives,
and reorder. Never re-derive ranking or reimplement search — shell out to `vagus` and parse `--json`.

**Hits are chunks; results are notes.** vagus splits each note into heading-aware chunks for
ranking, but by default every note appears **once** — as its best-ranked chunk — and `--limit N`
means **N distinct notes**. `siblings` = how many other ranked chunks of the same note were folded
into the hit (present only when > 0) — a breadth signal. Pass `--chunks` only when the user wants
the exact passage or every occurrence within notes; `--limit` then counts chunks.

## 1. Retrieve 20 candidates (compact)

```bash
vagus search "<query>" --json --rerank --limit 20
```

- Each hit: `{chunk_id, path, heading, score, snippet, rrf?, cosine?, bm25?, rerank?, created?, source?, siblings?}`
  — no bodies yet. Paths are relative to `~/brain`.
- `--rerank` reorders with the in-core cross-encoder: `rerank` is its raw logit, `score` its sigmoid
  (one-time ~150MB model download on first use).
- Optional soft floor: `--min-score 15` drops hits below 15% of the top hit. Keep it low or omit —
  a high floor starves the judge.

## 2. Triage on snippets — shortlist 5–8

Shortlist the candidates worth full-text judging, from snippet + heading + path, using retrieval
rank, the `bm25`/`cosine` split, `rerank`, and `siblings >= 2` as a weak position-aware prior. The
snippet is only the chunk's first ~200 chars — when it is ambiguous but heading or scores suggest
relevance, KEEP it. Drop only obvious false positives here. Shortlist 5–8 (never fewer than 5 when
5+ candidates exist).

## 3. Fetch shortlist bodies

```bash
vagus chunk <chunk_id> <chunk_id> ... --json
```

One call, all ids. Returns `[{chunk_id, path, heading, body}]` in request order. An element with
`"missing": true` means the note changed since indexing — Read `~/brain/<path>` for that hit instead.

## 4. Judge each (query, body) pair — the actual reranking

For every shortlisted hit, read its full `body` and assign a 0–3 relevance grade:

- **3** — directly answers / strongly on-topic.
- **2** — relevant, partial or supporting.
- **1** — tangential; keep only if little else.
- **0** — false positive. **DROP it** (quality floor).

Rules:

- Lean primarily on the **body text** — that is why you fetched it.
- Use retrieval rank + the `bm25`/`cosine` split + the `rerank` score as a **weak prior**
  (position-aware blend): a chunk the corpus signal ranked #1 starts with mild benefit of the
  doubt, but body judgment overrides it.
- `siblings >= 2` means the note matched broadly — another weak positive signal. If the best chunk
  alone is ambiguous, Read the whole note at `~/brain/<path>` **once** (never re-read a note per chunk).
- Do **not** just re-sort by `score`/`rrf`/`rerank` — that's a no-op. Do **not** ignore those
  signals entirely either.
- Reorder survivors by your judged grade (break ties with the weak prior).
- **Escalate before concluding thin results:** if fewer than 3 survivors grade >= 2, fetch bodies
  for the next 4–5 triaged-out hits with one more `vagus chunk` call and judge those too.

## 5. Present the survivors (top 5–8)

For each, in judged order:

- Header: `path › heading`
- The most relevant lines from the body (quote, don't dump the whole chunk).
- A one-line **why this matches**.

## 6. Record usage (tick)

After presenting, record a usage tick for **exactly the notes you presented** — one Bash call, all
paths at once:

```bash
vagus tick '<path1>' '<path2>' ...
```

- **Single-quote every path.** Filenames can contain `$` or backticks, which double quotes would
  expand/execute, silently ticking a mangled path. If a path itself contains a single quote, escape
  it as `'\''`.
- Tick only survivors you actually showed in step 5 — never the full candidate list, never dropped
  (grade-0) chunks, never on the no-results path (step 8).
- Paths are the `path` values from the hits, deduped (under `--chunks` one note may appear in
  several hits — tick it once).
- Run it once, after presenting. Its output is bookkeeping — don't relay it. If it fails, say
  nothing and move on; never retry or block the answer on it.
- Ticks are local usage stats (`vagus fame`); they never touch the note files.

## 7. Drill in on request

If the user wants more from a hit, Read the full note at `~/brain/<path>` and answer from it, citing
the path — once per note, even when it matched multiple chunks. Drilling in needs no extra tick —
presentation already recorded it.

## 8. No results

If nothing survives the floor: say so, offer to broaden the query, or retry with `--mode bm25`
(exact keywords) or `--mode vec` (semantic).

## Directory scoping

Hits may be silently elided by an inherited `.vagus` config found by walking up from the CWD; a
`— N hit(s) elided by inherited config` notice goes to stderr under `--json`. Pass `--all` to
disable scoping.
