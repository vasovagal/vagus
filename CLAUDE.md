# CLAUDE.md — vagus

`vagus` is a local-first **PARA second brain**: a Rust CLI providing hybrid full-text + semantic
search over a plain-Markdown vault in iCloud, plus Claude Code skills for capture and retrieval.

**Before any architectural change, read [`design/`](./design/).** It holds the requirements, the ADRs
(what we considered and why), the tradeoff study, and the prior-art survey. When you change a decision,
**update the matching ADR** in `design/adr/`. [`design/guardrails.md`](./design/guardrails.md) is the
canonical invariant list and is **binding** — the summary below must stay in sync with it.

## Hard invariants (do not violate without an ADR change)

1. **Only plain Markdown goes in the iCloud vault.** The search index (`tantivy/`), the SQLite
   `meta.db`, and the model cache live **outside** iCloud — `~/.local/share/vagus/` and
   `~/Library/Caches/vagus/`. Never write a database or index into the iCloud vault: iCloud syncs
   `.db`/`-wal`/`-shm` independently and will corrupt it (`database disk image is malformed`).
2. **The index is a derived cache, never the source of truth.** It must be fully rebuildable from the
   Markdown via `vagus reindex`. The Markdown files are authoritative.
3. **Never auto-edit a note the user is writing.** Frontmatter is *optional*; a bare `vim
   ~/brain/00-Inbox/x.md` with no frontmatter must index fine (title falls back to first `# heading`
   or filename). Frontmatter is only added/enriched during an explicit, approved filing step.
4. **Pin the embedding identity.** Store `embed_model` + `embed_dims` + `tantivy_version` in the
   `meta` table. On any mismatch, refuse incremental indexing and require `vagus reindex`. Never mix
   vectors from different models/dims — it silently corrupts ranking.
5. **Keep the two stores consistent off one hash-diff.** On a changed/deleted file: delete its tantivy
   docs (`writer.delete_term(path)` → `commit()`) **and** delete its SQLite vector rows (the vector
   store has no foreign keys/triggers). Same `chunk_id`/`path` keys drive both.
6. **Set the fastembed cache dir explicitly.** fastembed defaults to `./.fastembed_cache` in the CWD —
   always override to `~/Library/Caches/vagus/models` (`with_cache_dir(...)` or `FASTEMBED_CACHE_DIR`).
7. **Hybrid search = RRF (k=60).** Fuse BM25 ranks and cosine ranks with `score = Σ 1/(k + rank)`.
   Handle bge query/document prefixes correctly (query gets the retrieval instruction; documents do
   not) and **don't double-prefix**.
8. **Retrieval is hand-rolled** (tantivy BM25 + brute-force cosine + RRF; see
   `design/adr/0003-search-stack.md`). `frankensearch`/`qmd` are design references, **not
   dependencies** (see `design/adr/0007-lean-on-frankensearch.md`). Don't add a heavyweight
   search-engine dependency without an ADR.
9. **Local-first, offline by default.** No cloud calls and no background daemon in the default path.
10. **PARA layout is fixed** (`00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive`).
    Filing inbox → PARA is **assisted and user-approved, never automatic.**
11. **Stay Obsidian-compatible** (plain `.md`, optional `[[wikilinks]]`/frontmatter). Do **not** claim
    a "single static binary" — the default ONNX build is *binary + `libonnxruntime.dylib`* (use the
    `model2vec` backend for a truly dylib-free build, at a quality cost).

## Layout

```
~/code/vasovagal/vagus/     # this repo (org dir ~/code/vasovagal/)
  src/                      # the vagus crate
  design/                   # requirements, ADRs, tradeoffs, prior-art, guardrails  <- READ FIRST
~/brain -> ~/Library/Mobile Documents/com~apple~CloudDocs/Brain   # the vault (markdown only, in iCloud)
~/.local/share/vagus/       # index: tantivy/ + meta.db + config.toml   (OUTSIDE iCloud)
~/Library/Caches/vagus/models/   # cached ONNX embedding model         (OUTSIDE iCloud)
~/.claude/skills/{create-note,search,process-inbox}/   # the three skills (shell out to `vagus`)
```

## Build / test / run

```sh
cargo build              # first build fetches prebuilt ONNX Runtime (network, one-time)
cargo test
cargo clippy --all-targets
cargo install --path .   # installs `vagus` on PATH
vagus doctor             # verify symlink, model cache, dylib, dims, index health
vagus status
```

## Conventions

- Match the surrounding Rust style; keep modules small and single-purpose (`index`, `chunk`, `embed`,
  `search`, `notes`, `db`, `config`, `cli`).
- All data-producing commands support a stable `--json` shape so the skills parse rather than scrape.
- Commit `Cargo.lock` (this is a binary crate).
- Personal repo under the **`vasovagal`** GitHub org — **not** `scientist-hq` (that's work).
