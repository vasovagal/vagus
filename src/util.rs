//! Small shared helpers.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Lowercase hex of the SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Seconds since the Unix epoch (for `indexed_at`).
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Stable `u64` key for the usearch vector index, derived from a chunk id (ADR 0019).
///
/// `chunk_id` is the lowercase hex SHA-256 of `path + '#' + ord` (see `chunk.rs` / [`sha256_hex`]),
/// so the first 16 hex chars are a uniform 64-bit prefix. Deriving the key this way means it is
/// recomputable from any id we already hold — including the *old* ids returned on a delete/replace —
/// so vector removals (guardrail G5) need no extra lookup. Collision probability over <1M keys is
/// ~1.4e-8 (birthday bound n²/2⁶⁵), negligible at vagus scale. The reverse `key -> id` map used at
/// search time lives in the indexed `chunks.vec_key` column.
pub fn key_for(chunk_id: &str) -> u64 {
    debug_assert!(
        chunk_id.len() >= 16,
        "chunk id should be >=16 hex chars (sha256), got {chunk_id:?}"
    );
    let prefix = chunk_id.get(..16).unwrap_or(chunk_id);
    u64::from_str_radix(prefix, 16).unwrap_or(0)
}

/// Unique per-test scratch dir under the OS temp dir, removed on drop (no `tempfile` dep).
#[cfg(test)]
pub mod testdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "vagus-test-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
