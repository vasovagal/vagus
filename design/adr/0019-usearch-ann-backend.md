# ADR 0019 — Vector backend: embedded usearch HNSW (statically linked)

- **Status:** Accepted (2026-06-07); **amended 2026-07-30** — exact scan is automatic below
  10,000 embedded chunks after a corpus-grounded HNSW miss, and `--exact` is enforced in every mode;
  **amended 2026-07-31** — forced-refresh mutations count toward the one end-of-run sidecar save.
  Supersedes the "brute-force exact cosine; no ANN crate yet" stance of
  [ADR 0003](./0003-search-stack.md) for the *vector* component (BM25 + RRF remain unchanged).

## Context

Semantic / hybrid search ranked chunks by a **brute-force O(n) cosine scan**: every query reloaded
*all* embedding BLOBs from SQLite into a fresh `Vec`-of-`Vec`s (`db::all_embeddings`) and did a full
`O(N log N)` sort (`search::cosine_topk`). [ADR 0003](./0003-search-stack.md) deliberately deferred an
ANN backend ("revisit only if the corpus grows by orders of magnitude"), and at tens of thousands of
chunks that was right.

[Issue #5](https://github.com/vasovagal/vagus/issues/5) asked to embed a **real, statically-linked
vector/ANN database** into the single binary, and the maintainer set a **>500k-chunk growth
trajectory** — which moves vagus out of the "brute force is plainly enough" regime and justifies a true
ANN index now. The firm constraint is unchanged: it must link **statically into the one offline
`vagus` binary** — no daemon, no system package, no dylib to fetch — consistent with the self-contained
identity ([ADR 0014](./0014-self-contained-universe.md)) and the verified `ort`/onnxruntime precedent
(G13).

## Options considered

A 20-agent audit scored every serious Rust-compatible candidate on **popular / technical excellence /
speed** under the static-single-binary constraint (full study: `design/tradeoffs.md §E`):

| Candidate | Kind | Static-link verdict | Why not chosen |
|---|---|---|---|
| **usearch** | C++ HNSW (cxx-build) | ✅ clean (OS + C++ runtime only, `openmp` off) | **chosen** |
| tuned brute-force | pure Rust | ✅ cleanest | kept as the exact fallback/oracle, not the primary at >500k |
| sqlite-vec | C ext (`-DSQLITE_CORE`) | ✅ in `meta.db` | stable release is still brute-force — not the ANN wanted now |
| hnsw_rs | pure Rust HNSW | ✅ | **no delete API** → breaks G5 on edited/removed notes |
| instant-distance | pure Rust HNSW | ✅ | rebuild-only (no incremental insert/delete); frozen since 2023 |
| arroy | Rust + LMDB | ✅ | deprecated by Meilisearch for `hannoy`; RP-trees weak at 768-dim |
| faiss-rs | C++ + BLAS | ❌ | `static` feature still emits dynamic `gomp/blas/lapack` (no macOS branch) → Homebrew dylibs |
| lancedb / lance | Rust + Arrow | ❌ | build needs system `protoc` or cmake+C++ via `protobuf-src`; heavy |
| hora | pure Rust | ✅ | cosine metric **NaN-panics** (unfixed since 2021); abandoned |

usearch wins on the maintainer's three axes for the >500k trajectory: best-in-class embedded HNSW,
incremental add **and** remove (G5-compatible), f16/i8 quantization headroom, mmap `view()` for instant
cold-start, and a genuinely clean static link.

## Decision

**Adopt `usearch` (pinned `=2.25.3`) as the vector backend, behind a `VectorIndex` trait seam**
(`src/vector.rs`). Concretely:

- **Static link, single binary.** Default features compile usearch's C++ via `cxx-build` (no cmake, no
  bindgen, no prebuilt download) and the SIMD via the `numkong` crate, which **static-links**
  (`rustc-link-lib=static=numkong`, runtime CPU-feature dispatch — no `dlopen`). **`openmp` stays OFF**
  — it is the only feature that emits `rustc-link-lib=dylib=omp`, which would break the self-contained
  binary. Needs a C++17 toolchain at build time, the same class of prerequisite as `ort`. **Verified
  (darwin-arm64):** `otool -L` shows OS frameworks + `libc++` only — no `libomp`, no
  `libonnxruntime.dylib` (G13).
- **`.usearch` sidecar**, `~/.local/share/vagus/vectors.usearch` — OUTSIDE iCloud (G1), a pure derived
  cache (G2). **The f32 BLOBs in `meta.db` stay the authoritative copy**; the sidecar is always
  rebuildable from them with no re-embed. (usearch is not in `meta.db` — that is the one fit cost vs
  sqlite-vec, accepted because sqlite-vec offers no ANN.)
- **Metric `IP`, `ScalarKind::F32`, `connectivity=16`, `expansion_add=128`, `expansion_search=64`.**
  Vectors are L2-normalized (G7) so inner product == cosine; usearch's `IP` distance is `1 - dot`, so
  **`cosine = 1.0 - distance`**.
- **u64 keys** derived from the chunk id (`u64::from_str_radix(&sha256_id[..16], 16)`, `util::key_for`)
  — recomputable from any id we hold (so removals need no lookup), collision prob ≈ 1.4e-8 at 1M. An
  indexed `chunks.vec_key` column carries the reverse `key → id` map for search-result resolution.
- **Consistency (G5):** the indexer mutates the sidecar in lockstep with SQLite + tantivy — per changed
  file, remove the old keys then add the new; per deleted file, remove its keys; one `save()` after the
  single tantivy `commit()`. New, changed, deleted, **and forced-refreshed** files all trigger that
  save. A `reindex`, a missing/identity-mismatched sidecar, or a size drift
  triggers a full rebuild-from-BLOBs instead.
- **Identity pinning (G4):** `vec_backend` / `vec_index_version` / `vec_dims` in `meta`. A mismatch
  rebuilds the sidecar from the BLOBs — **no re-embed, no `CHUNK_VERSION` bump** (the vectors are
  unchanged).
- **Exact fallback / `--exact`.** A tuned exact brute-force `BruteForceIndex` (contiguous matrix +
  `select_nth_unstable` top-k) implements the same trait. It is the test oracle, the automatic path
  below **10,000 embedded chunks**, the fallback when the sidecar is missing/broken, and the explicit
  `vagus search --exact` ground-truth escape at/above the cutoff. The flag is forwarded through plain,
  reranked, and `--smart` search alike. HNSW is approximate: synthetic recall@10 ≥0.98 does not prevent
  a query-specific miss. Both backends order equal cosine scores by stable ascending `vec_key`.
- **RRF / rerank untouched (G7/G8).** The backend only feeds the cosine-rank *source* list;
  `search::rrf()` and the cross-encoder stage never see the backend choice.

## 2026-07-30 corpus and cutoff evidence

A stratified audit of the live 471-note / 4,023-chunk vault sampled every PARA tier and fixed a known
answer before each query. The snapshot predates indexing the resulting benchmark report (avoiding
self-retrieval leakage); primary paths and exact queries are pinned here:

| Query | Primary answer note | Behavior exercised |
|---|---|---|
| `What setting stops an Argo CD sync from running forever, and why does it break retries?` | `30-Resources/ArgoCD/20260530-010529-argocd-stuck-operation.md` | terms + causal semantics |
| `How can I keep the main coding-agent conversation clean while isolating noisy exploratory debugging?` | `30-Resources/Claude Code/20260714-170940-rx-workflow-disposable-interactive-workt.md` | workflow paraphrase |
| `Why did NDP Sydney still fail to pull Kubernetes images after Private Google Access was working?` | `00-Inbox/20260727-202411-ndp-prod-external-dns-viasat-io-dns-auth.md` | long incident note |
| `What safeguard makes etcd read-only before low disk space can corrupt its data, and how do we recover?` | `20-Areas/Viasat/20260619-130132-zoom-call-2026-06-19-12-30.md` | noisy transcript |
| `Why did changing GitHub Actions runner images trigger a Rust build even though Gemfile.lock was identical?` | `40-Archive/20260617-231154-red-candle-gem-needs-rust-to-build-no-pr.md` | corrected postmortem |

The etcd query was the decisive failure. HNSW omitted the answer chunk from **120 vector candidates**.
BM25 still found it at source rank 2, but one-channel RRF put it at fused rank 10 and the adaptive knee
returned four notes without it. Exact cosine found it at vector rank 10 / fused rank 3; the same knee
then returned three notes including the answer.

| Query | v0.10 HNSW count / target / est. tokens | exact-first count / target / est. tokens |
|---|---:|---:|
| Argo CD | 10 / 1 / 2,542 | 10 / 1 / 2,542 |
| agent context | 10 / 1 / 4,898 | 10 / 1 / 4,078 |
| NDP Sydney | 10 / 1 / 4,945 | 10 / 1 / 4,945 |
| etcd safeguard | 4 / **miss** / 1,764 | 3 / **3** / 1,565 |
| runner/Rust | 10 / 1 / 4,793 | 10 / 1 / 4,793 |

Primary-answer recall improved **4/5→5/5**, MRR **0.800→0.867**, and estimated body context
**18,942→17,923 tokens (−5.38%)**. Tokens are `ceil(decoded body chars/4)`, not provider billing.

Eight interleaved optimized one-shot runs at 4,023 chunks measured HNSW at 1,000.5 ms median and exact
at 1,043.2 ms: **+42.7 ms / +4.3%**, mostly one-shot embedder load. The cutoff itself is independently
reproducible with:

```sh
cargo test --release benchmark_exact_backend_at_cutoff -- --ignored --nocapture
```

Five repeated 10,000×768 runs (a ~30 MiB matrix) measured median SQLite load **20.5 ms**, median exact
search@120 **6.1 ms**, and load+search **26.6 ms** on Apple Silicon. This reconciles N4: a bare one-shot
command is approximately one second at current scale, while the near-cutoff vector backend consumes
only tens of milliseconds. **10k is a conservative policy ceiling, not the measured algorithmic
crossover** (which is much higher): it gives the current corpus ~2.5× exact-growth headroom while
bounding the per-command matrix near 30 MiB before the low-hundreds-of-thousands trajectory. Moving
it requires both a near-cutoff load/search benchmark and corpus qrels; aggregate ANN recall alone is
insufficient. HNSW still takes over at 10k for the >500k trajectory.

## Consequences

- True HNSW headroom at/above 10k and to the low-hundreds-of-thousands, while personal-scale vaults
  and explicit `--exact` retain exact recall. `vagus doctor` still cross-checks sidecar count (G5).
- A **third on-disk store** to keep consistent (tantivy + SQLite-vectors + `.usearch`). Mitigated by:
  BLOBs authoritative + sidecar rebuildable (G2), one save per index run, and the doctor cross-check.
- **Incremental-index cost:** mutating the sidecar `load`s it into RAM and `save`s the whole file each
  run; at hundreds of thousands of vectors that is a multi-hundred-MB rewrite per `vagus index`. Tunable
  later (batch writes, `expansion_*`); acceptable at current scale.
- Adds a C++17 build prerequisite and the young `numkong` SIMD crate (pinned via `Cargo.lock`;
  `default-features=false` is the conservative escape hatch). Re-verify the artifact with `otool -L` /
  `ldd` on any usearch/platform bump (G13).
- G11 (no heavyweight search dependency without an ADR) is satisfied by *this* ADR; G5 now spans three
  stores; ADR 0003 / `tradeoffs.md §E` / `roadmap.md` are updated in the same change (G24).
