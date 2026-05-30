# Plan — two-tier search + EmbeddingGemma (to "qmd-class" retrieval)

> **Status:** Planned, not started (2026-05-30). Decisions below are **locked** by the author.
> **Origin:** derived from a verified comparison of `vagus` vs `github.com/tobi/qmd`. Background notes
> live in `10-Projects/vagus/` in the vault (`vagus-vs-tobi-qmd-comparison`,
> `llm-reranking-in-the-search-skill-how-it-works`).
> **Resume here:** start at Phase 0 (docs), then Phase 1. Phases are ordered; 0 and 1 are the core.

## Context

**Goal:** bring `vagus` retrieval "all the way to" tobi/qmd-class quality *without* abandoning vagus's
identity (lean single Rust binary, local-first, offline, no daemon, no LLM in the binary).

Research (two verified workflows) established:
- vagus and qmd share a retrieval skeleton (BM25 + vector + RRF k=60, heading-aware chunking). qmd is
  *only* a query engine; vagus is a second brain (capture + PARA + skills). vagus is already ahead there.
- qmd's quality edge is three add-ons: **query expansion** (fine-tuned Qwen3-1.7B), **HyDE**, and
  **reranking** (qwen3-reranker-0.6b cross-encoder) — plus token-budgeted chunks and a newer embedder
  (**EmbeddingGemma-300M**). qmd ships ~2GB of local GGUF LLMs because it has no Claude in the loop.
- vagus's binary today: real fastembed **bge-small-en-v1.5** (2023, 384-dim, **512-token** ctx),
  heading-aware chunking with **no size cap**, RRF k=60, 200-char snippet, **no rerank/expansion**.

**Two decisions from the user lock the shape of this plan:**
1. **Two-tier search, channel-selected.** The **CLI (`vagus search`) is the ceiling of LLM-free
   smarts**; the **`/search` skill is *always* the full SOTA experience** (Opus does expansion + HyDE +
   rerank on top of `vagus search`). Invoking the skill *is* the opt-in — no mode flags, no escalation
   prompt. The skill literally wraps the CLI, so the tiers never diverge. Opus replaces qmd's two
   generative models (better than a 1.7B/0.6B model, zero extra disk, keeps **G17** intact).
2. **Adopt EmbeddingGemma-300M** as the in-binary embedder (the one model that legitimately lives in
   the binary — a vectorizer, not a generative LLM). Verified to be a **built-in fastembed 5.14.0
   variant** (`EmbeddingModel::EmbeddingGemma300M`) — a ~one-line swap. 768-dim, **2048-token ctx**,
   ~+8 MTEB over bge-small, 100+ languages.

**Outcome:** terminal `vagus search` becomes the best deterministic retriever it can be (modern
embedder, right-sized chunks, optional precision boosts); `/search` becomes a SOTA Opus pipeline; the
binary stays LLM-free, daemonless, and self-contained.

---

## Hard constraints (do not violate)
- **G17 / G14:** no LLM and no daemon in the binary. *All* expansion/HyDE/rerank lives in the skill.
- **G8:** RRF stays `Σ 1/(k+rank)`, k=60, no normalization. Any weighting/bonus requires editing G8 +
  ADR 0003 in the same change → **deferred / not this round.**
- **G4:** embed identity is code-enforced (`index.rs`, `main.rs` bail on mismatch). Changing the model
  (dims 384→768) and chunker (`CHUNK_VERSION` bump) **forces one `vagus reindex`** — do them together.
- **G1/G2:** index/model-cache stay outside iCloud; markdown is source of truth; index is rebuildable.
- **G11:** retrieval stays hand-rolled; no new search-engine dep. (EmbeddingGemma is already in
  fastembed — no new dep.)

---

## Phase 0 — Doc cleanup + the two-tier contract (no code, do first)

Pure docs; resolves a *pre-existing* contradiction and makes the two-tier split binding before code.
- **`design/adr/0003-search-stack.md`:** delete the stray "optionally weighting the original-query
  BM25 list ×2" (it contradicts G8's "no normalization"); change "Reranker + query-expansion are
  *deferred*…" → "…live in the skill layer and are **always-on** there (G17/G19)."
- **`design/guardrails.md`:** tighten **G17** (drop the tentative "if ever"; state expansion/HyDE/rerank
  run *only* and *always* in the skill); add **G19** — the two-tier contract (CLI = LLM-free ceiling;
  `/search` skill = always-SOTA; channel is the selector; no mode flags).
- **New `design/adr/0012-two-tier-retrieval.md`:** records the CLI-ceiling / skill-SOTA decision and
  that Opus (in-skill) replaces qmd's local expansion+rerank models. Refines the old "deferred" language.
- **`CLAUDE.md`:** mirror G19 into the Hard-invariants summary (guardrails.md wins on divergence).

---

## Phase 1 — EmbeddingGemma swap + chunk right-sizing (ONE reindex)

Both change the index identity, so they ship together and the user reindexes once.

### 1a. Embedder → EmbeddingGemma-300M
- **`src/embed.rs`:** `InitOptions::new(EmbeddingModel::EmbeddingGemma300M)`; add
  `.with_max_length(2048)` (fastembed defaults to 512 — confirmed; the Gemma tokenizer's
  `model_max_length=2048` so it is not clamped). **Prefixes change (this is a real behavior change):**
  - query → `task: search result | query: {text}`
  - **document → `title: none | text: {text}`** (today docs are embedded **raw** — they must now be
    prefixed). Update `embed_documents` accordingly; keep L2-normalize (G7).
- **`src/config.rs`:** `EMBED_MODEL = "google/embeddinggemma-300m"`, `EMBED_DIMS = 768`, bump
  `CHUNK_VERSION` ("2"→"3").
- Keep `with_cache_dir(...)` at `~/Library/Caches/vagus/models` (G6/G10). Default fp32 download is
  **~1.23GB** (vs ~130MB) — acceptable per user; note in docs. (q4/q8 ~197/309MB is a later footprint
  lever via the user-defined-ONNX path — **deferred**, not now.)
- **Vectors widen 384→768** (2× the SQLite BLOB store) — negligible at personal scale; keep full 768
  (no Matryoshka truncation v1).

### 1b. Chunk right-sizing (re-tuned to EmbeddingGemma's 2048 ctx)
Reuse the existing `chunk_markdown` heading pass (`src/chunk.rs:57-126`); add a post-pass that
sub-splits any section over budget.
- **Budget:** target **~900 tokens, ~128 overlap** (ape qmd, now safe under 2048). Estimate tokens
  **dep-free** via `chars/3.5` (conservative for token-dense content). *Note:* the old "512 silent
  truncation" bug is largely **dissolved** by the 2048 window — this phase is now about retrieval
  **precision** (right-sized chunks), not avoiding truncation.
- **Sub-split** oversize *and* heading-less sections on paragraph (`\n`-run) boundaries, greedily
  packing to budget, re-prepending the overlap tail (snapped to whitespace). Each sub-chunk keeps its
  full ` > ` heading breadcrumb.
- **Correctness fix (from critique):** track `in_code` via `Start/End(TagEnd::CodeBlock)` and **never
  split inside a fenced block** — emit an over-budget code block as one atomic chunk. (The current
  "code blocks whole" guarantee would otherwise be a lie.)
- **`chunk_id` unchanged:** still `sha256(path + "#" + ord)` with dense `ord` — sub-splitting just
  pushes more entries; stable for a stable file.
- Update the `chunk.rs` module doc (no longer "no-heading ⇒ one chunk").

### 1c. Rollout / safety
- `src/index.rs` **already** force-reindexes on `CHUNK_VERSION`/identity mismatch (`index.rs:74-100`) —
  reuse it; **no new index code**. Add a one-line **stderr** `reindexing N notes (model/chunk format
  changed)…` when that path fires, so the first post-upgrade search isn't silently slow.
- **`RELEASING.md`:** tell users to run `vagus reindex` once after upgrade.
- **Empirical sanity check** (pooling correctness isn't source-verifiable): after the swap, embed a
  known similar/dissimilar pair and assert cosine ordering is sane (a `#[test]` or a manual
  `vagus search --mode vec`).

### 1d. Docs for Phase 1
- **G7/G9** (prefixes): now include the **document** prefix and the Gemma templates.
- **G4:** dims 768. **New G20:** chunk budget is sized to the embedder's context window (EmbeddingGemma
  2048 → ~900-tok target); never exceed it; roll changes via `CHUNK_VERSION`.
- **`design/adr/0006-embeddings-local-no-daemon.md`:** update to EmbeddingGemma; note the **Gemma
  license** (use restrictions; fine for a personal vault, flag before redistribution) and the ~1.23GB
  cache.
- **New `design/adr/0013-chunk-budget.md`:** chunk budget tied to embedder context window; the rule is
  general (re-derive if the model changes), the value (~900/128) is from EmbeddingGemma.
- **`CLAUDE.md`:** mirror dims/model/prefix + chunk-budget notes.

### 1e. Tests
Sub-chunks all under budget; a code block between prose stays intact in one chunk; a long heading-less
note splits into >1 chunk; ids stable across two runs. Update any test/`doctor` assertion that snapshots
chunk counts (overlap raises per-note chunk count ~20% on long notes).

---

## Phase 2 — `--full` flag (the skill enabler)

Near-free: `hydrate()` already fetches the full body and throws it away into `snippet(&body,200)`.
- **`src/search.rs`:** add `pub body: Option<String>` to `Hit` with
  `#[serde(skip_serializing_if = "Option::is_none")]` → default `--json` stays **byte-identical**
  (preserves the G9a contract). Thread a `full: bool` through `run → query → hydrate`; set
  `body = full.then(|| body.clone())`; keep `snippet` as-is. `emit()` prints the full untruncated body
  per hit when `full` (reuse the `--verbose` layout).
- **`src/main.rs`:** add `#[arg(long)] full: bool` to `Command::Search`.
- **`src/notes.rs`** (file `--suggest` calls `query`): pass `full: false` explicitly.
- **Test:** `body` absent without `--full`, present + complete with it.

---

## Phase 3 — `--min-score` + the SOTA `/search` skill

### 3a. `--min-score` (binary)
- **`src/search.rs`:** factor out `emit()`'s `rel = 100*score/top` helper; in `run()`, after `query()`,
  drop hits below the floor (relative-to-top, so mode-dependent in feel — document in `--help`).
  Default `None` = today's behavior, byte-identical.
- **`src/main.rs`:** `#[arg(long)] min_score: Option<f32>`.

### 3b. `skills/search/SKILL.md` — rewrite as the always-SOTA inline pipeline
Replace the thin wrapper. **Opus runs inline in the session** (designed for an Opus session; a
pinned-Opus subagent is a clearly-marked **non-default** later toggle). Flow:
1. **Expand** (Opus): original verbatim + 3 variants — a lexical/synonym paraphrase (for BM25), a more
   *specific* entity/jargon rewrite, a *broader* conceptual rewrite (for the embedder). Skip variants
   for trivial/exact lookups (run 1–2 queries, not 4).
2. **HyDE** (Opus, conditional): only for conceptual/answer-seeking queries — one 2–4 sentence
   hypothetical passage, run as a 5th query via `--mode vec`. Skip for keyword/name/path lookups.
3. **Retrieve:** one `vagus search "<q>" --json --full --limit 8 --min-score <low>` per query (HyDE adds
   `--mode vec`); issue them together. Parse stdout as pure `Hit[]` (scope notice is on stderr).
4. **Merge + dedup:** union; dedup by `chunk_id`, then collapse near-dup chunks of the same `path`
   (keep best). Retain "how many variants surfaced this" as a weak agreement signal. Pool ≈ 15–30
   full-body chunks.
5. **Rerank/judge** (Opus, on **full bodies**, never the snippet) with a verbatim 0–3 rubric: judge
   primarily on text vs intent; RRF/bm25/cosine + multi-query agreement are a **weak prior** only.
6. **Quality floor:** drop < 2 (false positives); if none survive, say so and offer
   `--mode bm25`/`vec` — don't present junk.
7. **Reorder** by judged score; weak prior breaks ties.
8. **Present + cite** `path > heading`; `Read ~/brain/<path>` when a chunk is insufficient to answer.
- **Frontmatter:** add `Read(/Users/xlange/brain/**)` and the iCloud-canonical
  `Read(/Users/xlange/Library/Mobile Documents/com~apple~CloudDocs/Brain/**)` to `allowed-tools`.
- **Keep** the CWD-scoping + `--mode` sections; note **all expanded variants inherit the same CWD
  scope** (and the skill must **not** pass `--all`), and that under scoping each per-query pool can be
  thinner than `--limit` (no backfill — G9a) — the multi-variant union softens this.

---

## Phase 4 — Optional, only if a real-vault query proves the need
- **Query-aware snippet:** center the snippet window on the first matched term. **JSON snippet stays
  plain text** (no ANSI — the skill parses it); highlight is human-output-only. *Cut* the per-token
  bold sub-feature (UTF-8 boundary risk for sugar).
- **BM25 precision:** heading-field boost + exact-phrase bonus — built via **`QueryParser` quoted-phrase
  syntax** (NOT a hand-built `PhraseQuery`, which mismatches the `en_stem` analyzer and silently
  no-ops). Must **not** touch `rrf()`. Validate it doesn't crowd out genuine body-only hits.

## Not this round (explicit "do not build")
- RRF weighting / top-rank bonus (would breach G8; nothing to dilute with 2 lists) — revisit only if
  in-skill expansion later proves it needs binary-side help (then edit G8 + ADR 0003 together).
- True cosine-MMR / ranked-set per-note cap (`PER_FILE_CAP=3` already handles display dominance).
- Pinned-Opus-subagent plumbing (inline pool fits Opus context; keep as documented toggle).
- A real tokenizer in the chunk hot path (the char heuristic suffices; G11).
- Quantized-Gemma via custom-ONNX (footprint lever for later).

---

## Critical files
- `src/embed.rs` (model + prefixes + max_length) · `src/config.rs` (EMBED_MODEL/EMBED_DIMS/CHUNK_VERSION)
- `src/chunk.rs` (`chunk_markdown` + new sub-split/`estimate_tokens`, code-block atomicity)
- `src/search.rs` (`Hit.body`, `hydrate`, `query`/`run` signatures, `emit`, `--min-score` filter,
  factored `rel`) · `src/main.rs` (`--full`, `--min-score` args; doctor disk line)
- `src/notes.rs` (pass `full:false` at the `--suggest` call) · `src/index.rs` (reindex stderr line only)
- `skills/search/SKILL.md` (full rewrite) · `RELEASING.md`
- Docs: `design/guardrails.md` (G17/G19/G20, G7/G9, G4), `design/adr/0003`, new `0012`, new `0013`,
  `0006`, `CLAUDE.md`

## Reuse (don't reinvent)
- `index::run` auto-reindex on identity/`CHUNK_VERSION` mismatch — the rollout mechanism (no new code).
- `chunk_markdown` heading pass + `strip_frontmatter`; `hydrate`/`snippet`/`emit`/`rel`; `Embedder`
  API + L2 `normalize`; `Scope`/`apply_scope`; `skills::install` `include_str!` embedding.

## Verification (end-to-end)
1. `cargo build && cargo test && cargo clippy --all-targets` (incl. new chunk/`--full`/min-score tests).
2. `vagus reindex` (forced) — time it; `vagus doctor` shows embed identity
   `google/embeddinggemma-300m / 768`, `files/chunks/embedded` consistent, index outside vault, and the
   model cache (~1.23GB) under `index size`.
3. **Embedding sanity:** `vagus search --mode vec "<known topic>"` returns sensible ordering (cosine
   pooling check).
4. **`--full`:** `vagus search "q" --json --full` → `body` present + complete; without `--full` → no
   `body` key (default byte-identical). `--min-score 30` trims tail; default unchanged.
5. **Chunking:** a long heading-less note now yields multiple chunks; a note with a fenced code block
   keeps the block intact in one chunk (`vagus search --json --full` to inspect bodies).
6. **`/search` skill** in an Opus session on a conceptual query: confirm expansion (+HyDE when
   conceptual), full-body rerank, quality-floor drop, cited results; confirm CWD scoping still elides a
   foreign-employer path and `--all` is not used.
7. Confirm the **terminal fast path** is unchanged in feel: `vagus search "q"` returns < 1s with the
   new model on a personal-scale vault.
