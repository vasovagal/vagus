# Changelog

All notable, user-noticeable changes to `vagus` are recorded here. Internal refactors and test-only
changes are intentionally omitted (CLAUDE.md → Conventions).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The most recent tagged release is `v0.13.0`;
entries above it accumulate under **Unreleased** until the next `vX.Y.Z` tag.

## [Unreleased]

### Added

- **Privacy-projected local offline tracing.** Official builds include an off-by-default
  `local-tracing` integration shared with Corti. Enable Vagus with global `--trace`, exact
  `VASOVAGAL_TRACE=true`, or strict `~/.config/vasovagal/vagus.yaml`; it writes secure, rotated,
  schema-validated JSONL under local state for offline analysis with no collector/network path and no
  query, note, path, prompt, raw-error, or host-identity fields. Invalid/compiled-out support is a
  silent no-op, and traced/untraced command output stays unchanged. (ADR 0029/G28)

### Fixed

- Local tracing now fails closed before path creation when its fixed state directory resolves inside
  the Markdown vault, including missing-path and symlink-alias spellings. External plugins that exit
  nonzero now retain their exact output/status while Vagus closes the command span and writes the
  graceful trace summary first. (ADR 0029/G1/G28)

### Changed

- **Smaller release binaries.** Release builds are now stripped and compiled with ThinLTO in a single
  codegen unit. The v0.13.0 linux-x86_64 artifact was 55.5 MB unstripped, ~11 MB of which was symbol
  names; the released binary and its download both shrink with no change to behavior or to the
  self-contained linkage (G13). Release-build backtraces no longer carry symbol names.

## [0.13.0] — 2026-08-12

### Added

- `vagus inbox --since <duration>` filters inbox notes by the same
  `created`-frontmatter/filesystem-mtime rule as search. (ADR 0017)
- The bundled search and process-inbox skills recognize requested time windows and apply native
  `--since` filters immediately. Filtered search correctly uses ordinary JSON and counter-only ticks
  because rank provenance intentionally rejects metadata-filtered runs. Reinstall skills after
  upgrading. (ADRs 0012/0021)

### Changed

- **Consistent relative-time windows.** `reindex`, `search`, and `inbox` all validate `--since`
  through one CLI type and accept hours (`10h`), days (`5d`), 30-day months (`3m`), and 365-day
  years (`1y`), while retaining seconds, weeks, bare days, and unambiguous minutes (`30min`). Invalid
  operands fail before configuration or index access. `m` now means months; use `min` for minutes.
  (ADRs 0017/0022)

## [0.12.0] — 2026-08-07

### Added

- **Safe generated-note provenance.** `vagus add-note --frontmatter-json <OBJECT>` adds validated producer
  metadata without accepting raw YAML or allowing overrides of Vagus-owned fields. Integrations can pass the
  same object through the child-only `VAGUS_ADD_NOTE_FRONTMATTER_JSON` compatibility channel, so an older
  Vagus still creates the note during staggered upgrades. Valid non-owned JSON fields are projected into
  dedicated, kind-separated BM25 + semantic chunks, making model/version/config provenance searchable
  without indexing Vagus lifecycle fields or displacing body rerank neighbors. Chunk version 6 performs
  one automatic full re-embed on upgrade. (ADRs 0027/0028; G3/G9g)

## [0.11.0] — 2026-07-31

### Added

- **Atomic cited-note rank provenance.** The bundled search skill's fixed exact+reranked full-body
  path can now emit an explicit `{run,hits}` contract without changing default search JSON or ranking.
  Runs self-verify executable, pipeline, model, corpus, cap/context, scope, and result identity;
  path-bound event IDs reject mismatched rank/path copies, and hits distinguish real scored-prefix rerank ranks from
  the untouched RRF tail. `vagus tick --events`
  validates bounded cited-note payloads and writes the run, all events, and fame counters in one
  transaction; query storage is separately opt-in and bodies/snippets are never stored. `vagus ticks`
  reports path medians grouped by pipeline+corpus with an explicit agent-selection-bias caveat, while
  status/doctor expose counts/orphans. All three local-user-data tables survive every reindex and
  follow `vagus file` moves. Reinstall Claude/pi skills after upgrading. (ADR 0021/G9f/G25)
- **Honest opt-in semantic relevance.** `search --relevance` reports finite original-query
  EmbeddingGemma cosine clamped to `[0,1]` under a model/chunk-named policy, explicitly as a heuristic
  rather than confidence or probability. `--min-relevance 0..=1` applies an order-preserving,
  post-truncation floor with no backfill; positive floors drop unknown/BM25-only hits and disable
  adaptive tidy. Reranking carries cosine unchanged through its capped prefix and RRF tail, while
  BM25-only/`--smart` reject unsupported reporting. `eval --relevance` records the same diagnostic;
  the bundled tier-2 skill deliberately keeps its stronger full-body grade and does not request these
  flags. Default ranking and human/JSON output are unchanged. A 15+/15− development diagnostic
  suggested an exploratory 0.30 floor. On a later frozen 5+/5− holdout, it retained every positive top hit and
  dropped 4/5 plain or 5/5 radius-0 reranked negatives; the plain miss scored 0.300044, exposing the
  boundary rather than justifying calibration. All five existing known answers also survived that
  floor through rerank radii 0–2. (ADR 0026/G9e)
- **Tokenizer-safe small-to-big reranking.** `search --rerank-context 1|2` (also `--smart`) lets the
  cross-encoder judge up to one or two adjacent in-note chunks per side while returning only the
  matched chunk. Actual pair-tokenizer budgeting reserves query/special-token space and prevents a
  neighbor from truncating away the center; the audited 8,192-position model config is checked before
  overriding fastembed's stale 512-token metadata. Widened inference is batch-one, and `--smart`
  releases its embedder before that forward pass. The cap, unscored RRF tail, Hit shape/body, and all
  retrieval stages are unchanged. Radius 0 remains the byte-identical 512-token default. Schema-2
  `eval --rerank-context` records the exact policy. On five preselected 483-note-corpus qrels, radius
  0/1/2 kept R@10 at 1.0 and moved MRR@10 .429→.495→.589, but median rerank time rose
  .715s→2.806s→5.723s and one transcript answer regressed, so widening remains selective and opt-in.
  (ADR 0015/G8/G27)
- **Reproducible retrieval evaluation and a fixed fusion gate.** `vagus eval <labels.jsonl>` scores a
  fixed index with P@k/R@k/MRR@k/nDCG@k; schema-2 JSON adds full rankings plus label/corpus/index/model/
  backend/fusion/cohort provenance. `vagus eval-gate BASELINE CANDIDATE` enforces ADR 0025's held-out
  exact-hybrid k=10 sample/cohort floors, ≥.010 nDCG gain, positive paired-bootstrap lower bound, and
  recall/MRR/P/cohort nonregressions, exiting nonzero on rejection. RRF k=60 remains the only default;
  passing permits only an explicit same-pool experiment. (ADRs 0024/0025; G8/G27)
- **Fail-closed first-run setup.** `vagus init` creates the fixed PARA layout; `--icloud` uses the
  standard iCloud Drive `Brain` directory and a friendly vault symlink. It resolves aliases and
  missing paths, preflights equality/overlap/occupancy/traversal before mutation, initializes a direct
  iCloud vault in place, preserves existing iCloud notes, and never moves or recursively deletes an
  occupied local vault. Only an exact empty PARA skeleton is replaceable. (ADR 0004/G1)
- **Explicit model prefetch.** `vagus doctor --fetch-models` downloads both ONNX models, runs an
  embedder and reranker inference, validates finite output/dimensions and complete snapshots, and
  exits nonzero if either fails. Plain `doctor` never performs network-capable model construction.
  (ADRs 0006/0015/G10)
- `vagus vectors export --out DIR [--format npy|f32]`: coherent, streaming dump of the embedding
  matrix for offline analysis (clustering, calibration, eval). Writes `vectors.npy` (NumPy v1.0,
  C-order f32; `--format f32` for raw little-endian f32 instead), row-aligned `meta.jsonl`, and a
  manifest with embedding identity + shape, all from one SQLite snapshot. Fresh staging files and a
  manifest-last publication rule prevent failed exports from blessing mixed generations. `--json`
  emits a stable summary; `--force` replaces existing export artifacts without following symlinks;
  fail-closed path resolution refuses every vault-contained output spelling (G1).

### Changed

- **Personal-scale semantic retrieval is exact by default below 10,000 embedded chunks.** A five-query
  corpus audit found HNSW omitted a transcript answer from 120 vector candidates; exact cosine moved
  it from fused rank 10 to rank 3. Adaptive primary-answer recall improved 4/5→5/5, MRR .800→.867,
  and estimated full-body context fell 18,942→17,923 tokens. At the current 4,023-chunk corpus, exact
  added 42.7 ms median to an approximately one-second command; a committed synthetic 10k×768 release
  fixture measures a 20.5 ms median SQLite load plus 6.1 ms search@120 (~26.6 ms total, ~30 MiB).
  usearch remains automatic at/above 10k and `--exact` forces the oracle in every mode. (ADR 0019/G11)
- **Agent search uses a measured context budget instead of quota padding.** The bundled Claude Code/pi
  search skill now retrieves 10 exact+cross-encoded full-body candidates (down from 20), presents only
  nonredundant grade≥2 evidence (at most 6 notes), and never fills with tangential hits. It permits one
  BM25-or-vector fallback only when no first-pass candidate is useful and no longer defaults to
  `--min-score`, which forced the cross-encoder to score the whole pool. Across five grounded queries,
  primary-answer recall stayed 5/5, MRR rose 0.829→0.833, candidate-body estimates fell 39,096→20,267
  tokens (−48.16%), aggregate latency fell 12.84→9.44s (−26.45%), and the skill prompt itself shrank
  28.67%. Re-run `vagus skills install --agent <claude|pi>` after upgrading. (ADR 0012/G19)

### Fixed

- Index repair no longer stops at SQLite/Tantivy. Forced `reindex --since` refreshes now persist their
  in-memory usearch additions/removals even when no file was classified new/changed/deleted, fixing a
  reproduced 4,381-embedding/4,361-vector divergence. Ordinary incremental indexing also bypasses
  matching mtime/hash shortcuts for chunk rows left without embeddings by an interrupted prior run,
  retrying the complete replacement path automatically. (ADRs 0019/0022; G5/G26)
- Adaptive context trimming now fails open rather than crossing any note with a top-three BM25 or
  cosine source hit, including when that champion rank belongs to a folded sibling chunk. Exact
  cosine and RRF ties use stable opaque keys instead of randomized map iteration, and `--smart
  --exact` now honors the exact-oracle flag above the automatic cutoff. Scores, `rrf()`, survivor
  order, and serialized Hit fields are unchanged. (ADRs 0003/0023; G8/G9d)
- Derived data and model-cache paths are now checked with shared alias-aware resolution before any
  command can create state, closing relative/missing/`..`/symlink spellings of a G1 violation.
  `doctor` also rejects regular files as vaults and distinguishes complete, partial, and missing
  local model snapshots without turning an interrupted cache into a surprise download.

## [0.10.0] — 2026-07-29

### Added

- **Context-tidy adaptive search results.** Plain tier-0 hybrid note search now treats `--limit` as a
  ceiling rather than a quota: when a guarded robust RRF score knee separates a high-signal prefix
  from a real low-signal tail, vagus drops only that suffix without reordering, backfilling, changing
  scores, or touching `rrf()`. Unsupported/smooth result sets fail open. `--exhaustive` restores the
  legacy fill-up-to-limit behavior. On the motivating query this reduced ten full-body candidates
  from an estimated 4,276 to 2,619 tokens (−38.75%) while retaining all consensus/disputed useful
  evidence; an 18-query development matrix reduced aggregate body characters 30.12%. (ADR 0023/G9d)
- **Mtime-windowed forced reindex.** `vagus reindex --since <duration>` (for example `10d` or `2w`)
  snapshots every Markdown path + filesystem mtime, force-refreshes matching notes across SQLite,
  Tantivy, and usearch even when cached metadata says they are unchanged, and preserves older healthy
  embeddings. New/deleted files are still reconciled across the whole vault; plain `vagus reindex`
  remains the full rebuild. This is intended for recent iCloud synchronization/repair across machines
  without paying for a whole-vault re-embed. (ADR 0022/G26)

## [0.9.0] — 2026-07-25

### Added

- **pi Agent Skills support.** `vagus skills install --agent pi` installs the bundled create-note,
  search, and process-inbox skills into `~/.pi/agent/skills` (or `$PI_CODING_AGENT_DIR/skills`), and
  `vagus skills list --agent pi` reports their status. The existing no-flag commands still target
  Claude Code, and both agents share the same embedded, idempotently installed skill files.

### Changed

- **`vagus search --rerank` is substantially faster** — the cross-encoder now scores only the top
  `(limit*2).max(16)` fused candidates instead of the whole retrieval pool (~60 → ~30 at `--limit
  15`), roughly halving its forward-pass work. Retrieval, `--since`/`--source` filtering, and
  note-dedup still run at full pool depth, so note fill is unchanged; lower-ranked hits keep their
  RRF order after the reranked prefix. Only the top candidates are now eligible to be reranked to the
  front — a deliberate recall-vs-latency tradeoff. `--min-score` still reranks the whole pool, so its
  relative-to-top floor stays meaningful. The default (no `--rerank`) `--json` shape is byte-identical;
  under `--rerank` the un-scored tail hits omit the optional `rerank` field (G9a). (ADR 0015)

### Fixed

- Empty-bodied chunks no longer waste index space with a garbage embedding. A section with no prose
  (an H1 title lead whose content lives under H2s, or an ancestor heading) produced an empty-bodied
  chunk; where the heading's tokens already survive in a descendant chunk's breadcrumb, that empty
  chunk is now dropped. A **bodyless leaf heading** (a placeholder section like `## Open Questions`
  with nothing under it, and a bare `# Foo` stub) is instead kept as a heading-only chunk — its
  heading becomes the body — so the heading text stays searchable (previously it rode along only as
  an empty-bodied chunk's `heading` field; dropping such chunks outright would have removed those
  tokens from full-text search). A truly contentless note (no heading, no prose) now indexes nothing
  instead of injecting an empty vector. A one-time auto-reindex applies this on upgrade.

## [0.8.0] — 2026-07-08

### Added

- `vagus tick` and `vagus fame`: local usage counts for notes, recorded by the `/search` skill when
  it presents results; survive reindex, follow `vagus file` moves, never touch the vault (ADR 0021).
  Builtin `tick`/`fame` shadow any same-named plugins. Re-run `vagus skills install` to pick up the
  skill change.

## [0.7.0] — 2026-06-09

### Changed

- **`vagus search --limit N` now returns N distinct notes** instead of N chunks (ADR 0020). Each
  note appears once as its best-ranked chunk, with a `siblings` count of the other ranked chunks
  folded into it (omitted when zero, so single-chunk hits are unchanged). Previously a long note
  matching broadly could fill several of the top slots, so "10 hits" might span only 3–4 notes.
  Ranking itself is untouched — dedup is a post-rank stage like the `--since`/`--source` filters.

### Added

- `vagus search --chunks`: raw chunk-level hits — `--limit` counts chunks, the pre-0.7 behavior.
  Output (including `--json`) is byte-identical to v0.6.1.

## [0.6.1] — 2026-06-07

### Added

- **Embedded vector index: usearch HNSW**, statically linked into the single binary (ADR 0019). Semantic
  and hybrid search now rank via an approximate-nearest-neighbour index instead of a brute-force scan,
  giving real headroom as the vault grows. The `.usearch` sidecar lives outside iCloud and is a
  rebuildable cache of the authoritative f32 vectors — upgrading **backfills it from existing embeddings
  with no re-embed and no reindex**.
- `vagus search --exact`: force exact brute-force semantic search (100% recall) instead of the
  approximate index — the ground-truth escape hatch.
- `vagus doctor` reports the usearch index health (vector count vs embedded chunks, G5 cross-check), and
  `vagus status` shows the sidecar path + size. `vagus file --stats` gains a `vector_ms` timing.

## [0.5.0] — 2026-05-31

### Added

- Multi-agent / worktree guardrails (ADR 0018): worktree-per-agent isolation as a convention
  (`worktree.baseRef = fresh`), a `git-guard` hook that blocks direct commits/pushes to `main`, a
  `worktree-janitor` that lists/prunes merged worktrees, a soft commit-time breadcrumb nudge, and a PR
  template. Documented as guardrails G21–G24.
- This `CHANGELOG.md`, plus a `CLAUDE.md` convention to run `cargo fmt` before pushing and to record
  meaningful work here.
- `vagus search --timings`: print a per-stage wall-clock breakdown (rewrite/embed/rerank load +
  compute, fuse, total) to stderr for `--smart`/`--rerank`. A diagnostic + regression guard; stdout
  and the `--json` Hit shape are unchanged (G9a).

### Changed

- `vagus search --smart` is substantially faster — **~9.5 s → ~5 s on a cold query and ~2.3 s on a
  repeat** on a small vault, with ranking (RRF + rerank) unchanged. Four changes (ADR 0016):
  - The embedder and cross-encoder reranker now load on background threads that overlap the local
    LLM's query-expansion decode, so their cold loads (~2 s embedder + ~0.15 s reranker) no longer sit
    serially on the critical path. Not a daemon — the threads are joined within the one-shot process
    (G14).
  - On macOS the quantized rewriter now decodes on the **Apple GPU via candle's Metal backend** (~2.5×
    faster decode), falling back to CPU if Metal can't initialize. macOS-only; Linux/lean builds are
    unchanged, and the binary stays self-contained (system frameworks only — G13).
  - The deterministic query expansion is **cached** (`meta.db`, keyed on query + model identity +
    sampling params), so a repeat query skips the LLM entirely.
  - The rewriter's token ceiling is capped (512 → 192) to bound a pathological non-terminating
    generation; real output is never clipped.

## [0.4.0] — 2026-05-30

- M3 Opus `/search` skill (tier-2 reranking), `search --since`/`--source` frontmatter filters
  (ADR 0017), and `vagus file --stats` per-step timing. See git history for detail; entries before this
  release predate the changelog.

[Unreleased]: https://github.com/vasovagal/vagus/compare/v0.13.0...HEAD
[0.13.0]: https://github.com/vasovagal/vagus/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/vasovagal/vagus/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/vasovagal/vagus/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/vasovagal/vagus/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/vasovagal/vagus/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/vasovagal/vagus/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/vasovagal/vagus/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/vasovagal/vagus/compare/v0.5.0...v0.6.1
[0.5.0]: https://github.com/vasovagal/vagus/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/vasovagal/vagus/releases/tag/v0.4.0
