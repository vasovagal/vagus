//! Vector index backend (ADR 0019).
//!
//! Semantic / hybrid search ranks chunks by cosine similarity. The backend is an embedded **usearch**
//! HNSW index, statically linked into the binary (no daemon, no dylib to fetch — G13/ADR 0014), with
//! an **exact brute-force** backend kept as the test oracle, the small-corpus path, and the `--exact`
//! escape hatch. The `.usearch` sidecar lives OUTSIDE iCloud (G1) and is a pure derived cache (G2):
//! the authoritative copy of every vector is the f32 BLOB column in `meta.db`, so the index is always
//! rebuildable from SQLite with no re-embed.
//!
//! Both backends expose the same [`VectorIndex::search`] seam returning `(vec_key, cosine)`; the search
//! module resolves keys back to chunk ids ([`Db::chunk_ids_for_keys`]) and feeds the ranked list into
//! RRF / rerank unchanged (G7/G8 — fusion never sees the backend choice).
//!
//! **Metric:** vectors are L2-normalized at insert (G7), so inner product == cosine. usearch's `IP`
//! distance is `1 - dot`, hence `cosine = 1.0 - distance`.

use std::path::Path;

use anyhow::{Context, Result};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::config::{Config, EMBED_DIMS};
use crate::db::Db;
use crate::util::key_for;

/// HNSW build parameters, sized for up to ~1M vectors (ADR 0019). `F32` quantization keeps full
/// precision (no recall loss); the f32 BLOBs stay authoritative regardless of this.
const CONNECTIVITY: usize = 16;
const EXPANSION_ADD: usize = 128;
const EXPANSION_SEARCH: usize = 64;

/// Below this many embedded chunks an exact scan is both faster and exact, so the query path uses
/// brute force regardless of whether a sidecar exists.
const SMALL_CORPUS: usize = 2_000;

/// Query-time seam: a ranked source of `(vec_key, cosine)` for a normalized query vector. Implemented
/// by both backends; consumed by `search.rs` and never by `rrf()` (G8).
pub trait VectorIndex {
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>>;
    fn len(&self) -> usize;
    #[allow(dead_code)] // paired with `len` (clippy::len_without_is_empty); handy for callers/tests
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn options(dims: usize) -> IndexOptions {
    IndexOptions {
        dimensions: dims,
        metric: MetricKind::IP, // normalized vectors ⇒ IP == cosine; usearch IP distance = 1 - dot
        quantization: ScalarKind::F32,
        connectivity: CONNECTIVITY,
        expansion_add: EXPANSION_ADD,
        expansion_search: EXPANSION_SEARCH,
        multi: false,
    }
}

// ---------------------------------------------------------------------------
// usearch HNSW backend
// ---------------------------------------------------------------------------

/// Embedded HNSW index. Use [`UsearchIndex::view`] for read-only mmap'd querying (fast cold-start) and
/// [`UsearchIndex::open_writable`] / [`UsearchIndex::rebuild_from_db`] for the indexer's mutations.
pub struct UsearchIndex {
    index: Index,
}

impl UsearchIndex {
    /// Open an existing sidecar read-only via mmap (`view`) — instant cold-start for querying.
    pub fn view(path: &Path, dims: usize) -> Result<Self> {
        let index = Index::new(&options(dims)).context("usearch: create index")?;
        index
            .view(&path.to_string_lossy())
            .with_context(|| format!("usearch: view {}", path.display()))?;
        Ok(Self { index })
    }

    /// Open for mutation: load an existing sidecar fully into RAM if present, else start empty.
    pub fn open_writable(path: &Path, dims: usize) -> Result<Self> {
        let index = Index::new(&options(dims)).context("usearch: create index")?;
        if path.exists() {
            index
                .load(&path.to_string_lossy())
                .with_context(|| format!("usearch: load {}", path.display()))?;
        }
        Ok(Self { index })
    }

    /// Build a fresh index from every f32 BLOB in the DB (no re-embed). The one-time backfill / reindex
    /// path: keys are derived from chunk ids, so this is a pure repack of the authoritative vectors.
    pub fn rebuild_from_db(db: &Db, dims: usize) -> Result<Self> {
        let all = db.all_embeddings()?;
        let index = Index::new(&options(dims)).context("usearch: create index")?;
        index.reserve(all.len().max(1))?;
        for (id, vec) in &all {
            if vec.len() == dims {
                index.add(key_for(id), vec)?;
            }
        }
        Ok(Self { index })
    }

    /// Grow capacity geometrically so a run of `add`s doesn't reallocate every call.
    fn ensure_capacity(&self, additional: usize) -> Result<()> {
        let needed = self.index.size() + additional;
        if needed > self.index.capacity() {
            self.index.reserve(needed.next_power_of_two().max(1024))?;
        }
        Ok(())
    }

    pub fn add(&self, key: u64, vec: &[f32]) -> Result<()> {
        self.ensure_capacity(1)?;
        self.index.add(key, vec).context("usearch: add")?;
        Ok(())
    }

    /// Remove a key if present. usearch returns the count removed; an absent key (count 0) is fine.
    pub fn remove(&self, key: u64) -> Result<()> {
        self.index.remove(key).context("usearch: remove")?;
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.index
            .save(&path.to_string_lossy())
            .with_context(|| format!("usearch: save {}", path.display()))?;
        Ok(())
    }
}

impl VectorIndex for UsearchIndex {
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>> {
        if self.index.size() == 0 || k == 0 {
            return Ok(Vec::new());
        }
        let m = self.index.search(query, k).context("usearch: search")?;
        Ok(m.keys
            .into_iter()
            .zip(m.distances)
            .map(|(key, dist)| (key, 1.0 - dist)) // IP distance = 1 - dot; vectors normalized ⇒ cosine
            .collect())
    }

    fn len(&self) -> usize {
        self.index.size()
    }
}

// ---------------------------------------------------------------------------
// Exact brute-force backend (oracle / fallback / --exact)
// ---------------------------------------------------------------------------

/// Exact cosine via a contiguous in-RAM f32 matrix loaded once from the BLOBs. Bounded top-k via
/// `select_nth_unstable` (no full sort over N). This is the ground-truth oracle for the recall test,
/// the automatic fallback when the sidecar is missing, the small-corpus path, and `--exact`.
pub struct BruteForceIndex {
    keys: Vec<u64>,
    mat: Vec<f32>, // row-major, keys.len() × dims
    dims: usize,
}

impl BruteForceIndex {
    pub fn load(db: &Db, dims: usize) -> Result<Self> {
        let all = db.all_embeddings()?;
        let mut keys = Vec::with_capacity(all.len());
        let mut mat = Vec::with_capacity(all.len() * dims);
        for (id, v) in all {
            if v.len() == dims {
                keys.push(key_for(&id));
                mat.extend_from_slice(&v);
            }
        }
        Ok(Self { keys, mat, dims })
    }
}

impl VectorIndex for BruteForceIndex {
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>> {
        if self.keys.is_empty() || k == 0 || query.len() != self.dims {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(u64, f32)> = self
            .keys
            .iter()
            .enumerate()
            .map(|(i, &key)| {
                let row = &self.mat[i * self.dims..(i + 1) * self.dims];
                let dot: f32 = row.iter().zip(query).map(|(a, b)| a * b).sum(); // normalized ⇒ cosine
                (key, dot)
            })
            .collect();
        let k = k.min(scored.len());
        if k < scored.len() {
            scored.select_nth_unstable_by(k, |a, b| b.1.total_cmp(&a.1));
            scored.truncate(k);
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(scored)
    }

    fn len(&self) -> usize {
        self.keys.len()
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Choose the query-time backend. Exact brute force when `exact` is forced, when the sidecar is
/// missing, or for a small corpus; otherwise the mmap'd usearch HNSW view. Any usearch open error
/// falls back to brute force so search never hard-fails (G2: the BLOBs are always sufficient).
pub fn open_for_search(cfg: &Config, db: &Db, exact: bool) -> Result<Box<dyn VectorIndex>> {
    let path = cfg.vector_path();
    let embedded = db.count("SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")? as usize;
    if !exact && embedded >= SMALL_CORPUS && path.exists() {
        match UsearchIndex::view(&path, EMBED_DIMS) {
            Ok(idx) => return Ok(Box::new(idx)),
            Err(_) => { /* fall through to the exact backend */ }
        }
    }
    Ok(Box::new(BruteForceIndex::load(db, EMBED_DIMS)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize(v: &mut [f32]) {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            for x in v.iter_mut() {
                *x /= n;
            }
        }
    }

    /// Deterministic pseudo-random normalized vectors (no `rand` dep, no `Math.random`).
    fn fixture(n: usize, dims: usize) -> Vec<(u64, Vec<f32>)> {
        let mut out = Vec::with_capacity(n);
        let mut state: u64 = 0x9e3779b97f4a7c15;
        for i in 0..n {
            let mut v = vec![0.0f32; dims];
            for x in v.iter_mut() {
                // xorshift64
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *x = ((state >> 40) as f32 / (1u64 << 24) as f32) - 0.5;
            }
            normalize(&mut v);
            out.push((i as u64 + 1, v)); // keys are 1-based (0 is a valid-but-avoid sentinel)
        }
        out
    }

    fn brute_topk(data: &[(u64, Vec<f32>)], q: &[f32], k: usize) -> Vec<u64> {
        let mut s: Vec<(u64, f32)> = data
            .iter()
            .map(|(key, v)| (*key, v.iter().zip(q).map(|(a, b)| a * b).sum::<f32>()))
            .collect();
        s.sort_by(|a, b| b.1.total_cmp(&a.1));
        s.into_iter().take(k).map(|(key, _)| key).collect()
    }

    #[test]
    fn usearch_recall_and_cosine_sign_match_exact_oracle() {
        let dims = 64;
        let data = fixture(3_000, dims);

        // Build a usearch index directly (no DB) over the fixture.
        let index = Index::new(&options(dims)).unwrap();
        index.reserve(data.len()).unwrap();
        for (key, v) in &data {
            index.add(*key, v).unwrap();
        }
        let usearch = UsearchIndex { index };

        let mut total = 0usize;
        let mut hit = 0usize;
        // Use the first 50 vectors as queries; each must retrieve at least itself + neighbours.
        for (qkey, q) in data.iter().take(50) {
            let exact = brute_topk(&data, q, 10);
            let got = usearch.search(q, 10).unwrap();
            // cosine sign: the top hit for a query that IS in the set must be itself with cosine ≈ 1.
            let (top_key, top_cos) = got[0];
            assert_eq!(
                top_key, *qkey,
                "nearest neighbour of a stored vector is itself"
            );
            assert!(
                (top_cos - 1.0).abs() < 1e-3,
                "self-cosine should be ≈1, got {top_cos} (sign/conversion bug?)"
            );
            let got_keys: Vec<u64> = got.iter().map(|(key, _)| *key).collect();
            for key in &exact {
                total += 1;
                if got_keys.contains(key) {
                    hit += 1;
                }
            }
        }
        let recall = hit as f32 / total as f32;
        assert!(recall >= 0.98, "recall@10 = {recall} (< 0.98)");
    }

    #[test]
    fn brute_force_matches_oracle_exactly() {
        let dims = 64;
        let data = fixture(500, dims);
        let bf = BruteForceIndex {
            keys: data.iter().map(|(k, _)| *k).collect(),
            mat: data.iter().flat_map(|(_, v)| v.clone()).collect(),
            dims,
        };
        let (_, q) = &data[7];
        let exact = brute_topk(&data, q, 10);
        let got: Vec<u64> = bf
            .search(q, 10)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(got, exact, "brute-force backend must be exact");
    }
}
