# Plan — Embed `usearch` (static HNSW) as the vagus vector index (issue #5)

## Context

Issue [#5](https://github.com/vasovagal/vagus/issues/5) asks vagus to embed a **real, statically-linked
vector/ANN database** into the single `vagus` binary, replacing today's brute-force O(n) cosine scan.
Today embeddings are 768-dim L2-normalized f32 LE BLOBs in `chunks.embedding` (`db.rs:32`), and every
semantic/hybrid query calls `all_embeddings()` (`db.rs:251`) to reload + re-allocate **every** vector
into a `Vec`-of-`Vec`s, then `cosine_topk` (`search.rs:197`) clones every id and does a full
`O(N log N)` sort. That's fine at tens of thousands of chunks but is the wrong shape as the corpus grows.

A 20-agent audit (popular / technical excellence / speed, under the hard static-single-binary
constraint) scored every serious Rust-compatible candidate. **`usearch`** (Unum's embedded HNSW) was the
runner-up overall but the **best true-ANN** option that satisfies the constraint, and the maintainer
chose it explicitly with a **>500k-chunk growth trajectory** that justifies adopting ANN now (rather
than the "optimize brute-force, defer ANN" path that wins at purely personal scale).

**Disqualified** on the hard requirement (documented so we don't revisit): `faiss-rs` (static feature
still emits dynamic `gomp/blas/lapack` with no macOS branch → Homebrew dylibs), `lancedb` (needs system
`protoc` or cmake+C++ via `protobuf-src`), `hora` (cosine NaN-panics, abandoned). `hnsw_rs`
(no delete API → breaks G5), `instant-distance` (rebuild-only, frozen), `arroy` (deprecated by
Meilisearch for hannoy) lost on maturity/fit. `sqlite-vec` is the cleanest *storage* fit (vec0 inside
meta.db) but its stable release is still brute-force — not the ANN the maintainer wants now.

**Why usearch fits the static-binary identity (ADR 0014/G13):** pure `cxx-build` (no cmake, no bindgen,
no prebuilt download), and with `openmp` OFF the binary links only the platform C++ runtime + OS libs —
exactly the verified `ort`/onnxruntime precedent. A static C++ inference/index lib is in-character.

**Outcome:** `--mode vec` / `--mode hybrid` query an embedded HNSW index instead of a brute-force scan,
with equal-or-better results; the f32 BLOBs remain the authoritative, rebuildable source of truth; the
binary stays self-contained and offline; `vagus doctor` reports the new index healthy.

## Decision (scored)

| Candidate | Popular | Technical | Speed | Static-link | SQLite-fit | Total |
|---|:-:|:-:|:-:|:-:|:-:|:-:|
| **usearch** ✅ chosen | 4 | 5 | 5 | 4 | 1 | 19 |
| tuned brute-force (kept as exact fallback/oracle) | 4 | 4 | 4 | 5 | 5 | 22 |
| sqlite-vec (storage tidy-up, not chosen) | 5 | 4 | 3 | 5 | 5 | 22 |

usearch wins on the maintainer's stated priorities **for the >500k trajectory**: true HNSW headroom,
f16/i8 quantization, mmap `view()` for instant cold-start, incremental add+remove. The tuned exact
brute-force is **retained** as the test oracle, the small-corpus / missing-sidecar fallback, and the
`--exact` escape hatch — so we keep exact recall available and never regress.

## Architecture

A narrow **`VectorIndex` trait** is the swappable seam so the backend never leaks into `search.rs`'s
fusion/rerank (G7/G8 stay untouched — the index only feeds the cosine-rank *source* list).

```rust
// src/vector.rs (new)
pub trait VectorIndex {
    fn search(&self, q: &[f32], k: usize) -> Result<Vec<(u64, f32)>>; // (vec_key, cosine)
    fn add(&self, key: u64, v: &[f32]) -> Result<()>;
    fn remove(&self, key: u64) -> Result<()>;
    fn save(&self) -> Result<()>;
    fn len(&self) -> usize;
}
pub fn key_for(id: &str) -> u64 { u64::from_str_radix(&id[..16], 16).unwrap() } // sha256 hex → u64
pub fn open(cfg: &Config, db: &Db, exact: bool) -> Result<Box<dyn VectorIndex>>; // picks impl + rebuild
```

- **`UsearchIndex`** (primary): HNSW over the `.usearch` sidecar at `cfg.data_dir.join("vectors.usearch")`
  (OUTSIDE iCloud — G1). Metric `MetricKind::IP`, `ScalarKind::F32`, `connectivity=16`,
  `expansion_add=128`, `expansion_search=64` (sane for <1M; bump search to 128 if recall test fails).
  usearch `Index` methods take `&self` (interior mutability) and are single-threaded-safe — fine for our
  indexer. **`cosine = 1.0 - distance`** (IP distance is `1 - dot`; vectors are L2-normalized so
  `dot == cosine` — G7). Getting this sign right is load-bearing.
- **`BruteForceIndex`** (exact fallback/oracle): contiguous f32 matrix from `all_embeddings()` +
  bounded top-k via `select_nth_unstable_by` over the existing `dot` (`search.rs:188`). Used when the
  sidecar is missing, the corpus is tiny (~<2,000 embedded chunks), or `--exact` is passed.

**Key mapping (u64 ↔ chunk_id):** `chunk.id` is `sha256(path + '#' + ord)` hex (`chunk.rs:235`,
`util.rs:9`). `vec_key = u64::from_str_radix(&id[..16], 16)` — deterministic, recomputable from any id we
already hold (so removals need no lookup), collision prob ≈ 1.4e-8 at 1M. A persisted **indexed
`vec_key INTEGER` column** provides the reverse `vec_key → id` lookup at search time. (rowid rejected:
`clear_all`/VACUUM reassign it.)

## Implementation steps

1. **`Cargo.toml`** — add `usearch` (PIN the exact version), `default-features = false`,
   `features = ["simsimd"]`, **`openmp` must stay OFF** (its build.rs emits `rustc-link-lib=dylib=omp`,
   which breaks the single binary). Verify the actual feature names against the pinned crate's
   `Cargo.toml` during impl (audit and docs disagreed on the SIMD feature name `simsimd` vs `numkong`).
   Commit `Cargo.lock` (pins the young transitive SIMD crate too).

2. **`src/vector.rs` (new)** — the trait, `key_for`, `UsearchIndex`, `BruteForceIndex`, and
   `open(cfg, db, exact)` (selects impl, runs the rebuild trigger). Add `mod vector;` to `main.rs:6-20`.

3. **`src/db.rs`** — additive migration in `migrate()` (mirror `created_at`/`source` at `db.rs:77-82`):
   ```sql
   ALTER TABLE chunks ADD COLUMN vec_key INTEGER;
   CREATE INDEX IF NOT EXISTS chunks_vec_key ON chunks(vec_key);
   ```
   Backfill `vec_key` in Rust for rows where it `IS NULL` (compute from `id`). Add a reverse-lookup
   helper `chunk_ids_for_keys(&[u64]) -> Vec<(u64,String)>` (`SELECT id FROM chunks WHERE vec_key=?`) and
   a `(vec_key, embedding)` iterator for sidecar rebuild. Keep `all_embeddings()` (BruteForce + rebuild).

4. **`src/index.rs`** — open the `VectorIndex` once after `Db::open` (`~120`), before the file loop.
   - reindex branch (`~137-140`): also `remove_file(cfg.vector_path())` (stale sidecar must not survive
     a reindex; `clear_all` doesn't touch it).
   - per changed/new file: after `replace_chunks` (`~207`, returns OLD ids) `remove(key_for(old))` for
     every old id, then `add(key_for(c.id), &v)` inside the existing `set_embedding` loop (`~232-234`).
   - deletions loop (`~247-253`): `for id in db.delete_file(path) { vindex.remove(key_for(&id)) }`.
   - single `vindex.save()` right after `writer.commit()` / `wait_merging_threads()` (`~256-259`).
   - pin meta after the identity pin (`~152-155`): `vec_backend=usearch`, `vec_index_version`,
     `vec_dims`. **Do NOT bump `CHUNK_VERSION`** — that forces a 1.23 GB re-embed.
   - if you add a `vector_save_ms` timing field, update the stable-keys test (`~288-300`) in lockstep.

5. **`src/search.rs`** — `vec_search` (`~208`) and the `--smart` matrix path (the `cosine_topk` call at
   `~499`) call `vindex.search(qv, k)`, map `(u64,cosine)` → chunk_ids via the reverse lookup, drop
   misses, feed `hydrate`. **`rrf()` (`~215`) and the rerank stage (`~339`, `~533`) are untouched.**
   The `--smart` path no longer calls `all_embeddings()` at `~493` (BruteForce/`--exact` still can).

6. **`src/config.rs`** — `vector_path()` → `data_dir.join("vectors.usearch")` (next to
   `db_path`/`tantivy_dir`, `~67-73`); a `VEC_INDEX_VERSION` const for G4 pinning.

7. **`src/main.rs`** — add `--exact` to `search` (forces `BruteForceIndex`); extend `doctor` (`~365-372`)
   with a G5 cross-check line comparing `vindex.len()` to the embedded-chunk count; extend `status`
   (`~470-500`) to report the sidecar path/size.

## Migration (no re-embed — issue requirement)

The existing 768-dim f32 LE BLOBs **are** the bytes usearch needs. Embedding identity is unchanged
(`google/embeddinggemma-300m` / 768), so G4's `embed_model`/`embed_dims`/`tantivy_version` don't change
and `CHUNK_VERSION` does not bump → no forced `vagus reindex`. On first run after upgrade the additive
`vec_key` column backfills and the `.usearch` sidecar **builds once from the BLOBs with no model load**.
The sidecar is a pure derived cache (G2): rebuilt when missing, on `vec_backend`/`vec_index_version`/
`vec_dims` mismatch, or when `len() != count(embedded chunks)`.

## Breadcrumbs (G24 — same change)

- **New ADR `design/adr/0019-usearch-ann-backend.md`** — records this decision, the scored audit, the
  disqualifications, and the static-link verification. Required by G11 (adding a search-class dep).
- **Supersede** in `design/adr/0003-search-stack.md` the "brute-force … No ANN crate yet" /
  "revisit only if the corpus grows by orders of magnitude" lines (`:20,:43`).
- **`design/guardrails.md`**: G5 now covers **three** stores (tantivy + SQLite-vectors-as-source +
  usearch sidecar), with BLOBs authoritative and the sidecar a rebuildable derived cache; G11 note the
  ANN backend adopted via ADR 0019; G13 re-verify usearch's static link.
- **`design/tradeoffs.md` §E**, **`design/roadmap.md`** (move "ANN vector backend" from Deferred →
  shipped), **`CLAUDE.md`** invariants 5 & 8, and **`CHANGELOG.md`** (`## [Unreleased]` Added).
- Persist this plan to `design/plan-usearch-vector-backend.md` (per memory: finalized vagus plans live
  in `design/`).

## Verification

- `cargo build` (fetches/builds usearch C++ via cxx-build, C++17) → `cargo clippy --all-targets` →
  `cargo fmt`.
- **`otool -L target/release/vagus`** (macOS) / `ldd` (Linux): expect system dylibs + `libc++`/
  `libstdc++` only, **no `libomp`/`libonnxruntime.dylib`** — update the G13 note with the result.
- **Unit test** in `vector.rs`: build `UsearchIndex` and `BruteForceIndex` over a ~2–5k synthetic
  normalized fixture; assert `recall@10 ≥ 0.98` vs the exact oracle and `cosine == 1 - distance` (±1e-4).
- **End-to-end**: `cargo install --path .`; on an existing vault confirm the one-time sidecar build logs,
  then `vagus search "<query>" --mode vec --json` and `--mode hybrid` return sane hits;
  `vagus search "<query>" --exact` matches the pre-change ranking; `vagus doctor` shows the sidecar
  healthy and `vindex.len() == embedded`; `vagus reindex` rebuilds tantivy **and** the sidecar.
- Confirm `vagus search --smart` still works (prewarm threads only build ONNX models — they never touch
  the non-`Send+Sync` usearch `Index`).

## Risks / gotchas

- **IP distance is `1 - dot`, not raw dot** — wrong sign inverts relevance. Covered by the cosine test.
- usearch `Index` is **not `Send + Sync`** — never move it into the `--smart` prewarm threads.
- Approximate recall: at <500k with `expansion_search=64–128` it's near-exact; the recall test + the
  `--exact` fallback guard the "equal-or-better" acceptance criterion. Re-baseline nothing in `rrf()`.
- Young transitive SIMD crate — pin via `Cargo.lock`; `default-features=false` keeps the surface minimal.
- `[..16]` assumes ≥16 hex chars (always true for sha256) — `debug_assert!` it.
- file-backed mmap can SIGBUS on truncation — safe only because the sidecar lives under
  `~/.local/share/vagus` (internal volume), never iCloud (G1). Never point it at the vault.
