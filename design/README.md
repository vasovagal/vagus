# design/ — the vagus design record

This folder is the durable record of **what we built, what we considered, and why**. It exists so
future sessions (human or Claude) inherit the reasoning instead of re-litigating settled decisions or
silently breaking an invariant.

## How to use it

- **Read before any architectural change.** Start with [`guardrails.md`](./guardrails.md) (binding) and
  the relevant ADR.
- **When you change a decision, update the matching ADR** (don't delete history — add a new ADR that
  supersedes the old one, or amend with a dated note). The root `CLAUDE.md` summarizes the invariants;
  keep it in sync with `guardrails.md`.
- **New significant decision?** Add `adr/NNNN-title.md` using the same format.

## Contents

| File | What |
|---|---|
| [`requirements.md`](./requirements.md) | Functional + non-functional requirements, scope, **non-goals**. |
| [`guardrails.md`](./guardrails.md) | The canonical hard-invariant list (binding). |
| [`tradeoffs.md`](./tradeoffs.md) | Comparison tables: build-vs-adopt, embedding backends, the ONNX single-binary reality, vector-store options. |
| [`prior-art.md`](./prior-art.md) | Tools surveyed, with borrow/reject notes and links. |
| [`methodology-para.md`](./methodology-para.md) | PARA / CODE domain-model reference — the *why* behind the vault shape. |
| [`adr/`](./adr/) | Architecture Decision Records (one per fork): context · options · decision · consequences. |

## ADR index

- `0001-build-vs-adopt.md` — build the second-brain layer fresh; lean on `frankensearch` for retrieval.
- `0002-language-rust.md` — Rust over Python.
- `0003-search-stack.md` — tantivy + fastembed/ort + brute-force cosine + RRF(k=60).
- `0004-icloud-markdown-only.md` — iCloud holds Markdown only; index/DB/cache live outside iCloud.
- `0005-assisted-filing.md` — assisted, on-demand PARA filing (never automatic).
- `0006-embeddings-local-no-daemon.md` — local fastembed; no Ollama/cloud by default.
- `0007-lean-on-frankensearch.md` — depend/vendor the retrieval engine (pending smoke test).
- `0008-naming.md` — `vagus` / `vasovagal`.
