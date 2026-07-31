//! Small shared helpers.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
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

/// Stable corpus identity over sorted `(vault-relative path, note-content hash)` pairs. Shared by
/// `vagus eval` and explicit tick provenance so both name an identical index generation.
pub fn corpus_fingerprint(files: &HashMap<String, (f64, String)>) -> String {
    let mut rows: Vec<(&String, &String)> =
        files.iter().map(|(path, (_, hash))| (path, hash)).collect();
    rows.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut bytes = Vec::new();
    for (path, hash) in rows {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(hash.as_bytes());
        bytes.push(b'\n');
    }
    sha256_hex(&bytes)
}

/// SHA-256 of the running executable, cached process-wide. Provenance must distinguish two
/// unreleased builds that share the same semantic version but implement different ranking code.
pub fn executable_fingerprint() -> Result<String> {
    static HASH: OnceLock<String> = OnceLock::new();
    if let Some(hash) = HASH.get() {
        return Ok(hash.clone());
    }
    let executable =
        std::env::current_exe().context("resolving current executable for provenance")?;
    let mut file = std::fs::File::open(&executable)
        .with_context(|| format!("opening executable {} for provenance", executable.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hashing executable {}", executable.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hash, "{byte:02x}");
    }
    let _ = HASH.set(hash.clone());
    Ok(hash)
}

/// Seconds since the Unix epoch (for `indexed_at`).
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse a relative duration into seconds. Accepts one number+unit token — `30s`, `90m`, `6h`,
/// `10d`, `2w` — or a bare integer interpreted as days (`7` == `7d`). Shared by search filtering
/// and mtime-windowed reindexing (ADR 0022).
pub fn parse_duration(input: &str) -> anyhow::Result<i64> {
    let s = input.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration (use e.g. 10d, 2w, 6h, 30m, 90s, or a bare number of days)");
    }
    let (num_str, unit_secs): (&str, i64) = match s.chars().last().unwrap() {
        c if c.is_ascii_digit() => (s, 86_400), // bare number -> days
        's' | 'S' => (&s[..s.len() - 1], 1),
        'm' | 'M' => (&s[..s.len() - 1], 60),
        'h' | 'H' => (&s[..s.len() - 1], 3_600),
        'd' | 'D' => (&s[..s.len() - 1], 86_400),
        'w' | 'W' => (&s[..s.len() - 1], 604_800),
        other => anyhow::bail!(
            "invalid duration unit {other:?} in {s:?} (use s, m, h, d, w, or a bare number of days)"
        ),
    };
    // Parse the numeric part as-is (no inner trim) so embedded whitespace like "10 d" is rejected.
    let n: i64 = num_str.parse().map_err(|_| {
        anyhow::anyhow!("invalid duration {s:?} (expected e.g. 10d, 2w, 6h, 30m, 90s, or a number)")
    })?;
    if n < 0 {
        anyhow::bail!("duration must not be negative: {s:?}");
    }
    n.checked_mul(unit_secs)
        .ok_or_else(|| anyhow::anyhow!("duration too large: {s:?}"))
}

/// Compute a relative-duration cutoff in Unix seconds (`now - duration`). Saturation makes an
/// extremely large but otherwise valid duration mean "from the beginning" rather than overflowing.
pub fn since_cutoff(spec: &str) -> anyhow::Result<i64> {
    Ok(now_unix().saturating_sub(parse_duration(spec)?))
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
