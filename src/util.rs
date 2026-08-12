//! Small shared helpers.

use std::collections::HashMap;
use std::fmt::{Display, Formatter, Write as _};
use std::io::Read as _;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDateTime, TimeZone};
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

const MINUTE_SECS: i64 = 60;
const HOUR_SECS: i64 = 60 * MINUTE_SECS;
const DAY_SECS: i64 = 24 * HOUR_SECS;
const WEEK_SECS: i64 = 7 * DAY_SECS;
const MONTH_SECS: i64 = 30 * DAY_SECS;
const YEAR_SECS: i64 = 365 * DAY_SECS;
const DURATION_HELP: &str =
    "use NUMBER plus s, min, h, d, w, m, or y (m=30 days, y=365 days), or a bare number of days";

/// Parse the shared `--since` duration grammar into seconds.
///
/// `m` deliberately means a 30-day month (not minutes) and `y` means 365 days; minutes remain
/// available as `min`. Units are case-insensitive, and a bare integer means days (`7` == `7d`).
pub fn parse_duration(input: &str) -> anyhow::Result<i64> {
    let spec = input.trim();
    if spec.is_empty() {
        anyhow::bail!("empty duration ({DURATION_HELP})");
    }

    // Split only on ASCII digits. Besides rejecting signs, decimals, and embedded whitespace, this
    // avoids byte-slicing at an invalid boundary when a malformed operand ends in Unicode.
    let unit_start = spec
        .char_indices()
        .find_map(|(index, c)| (!c.is_ascii_digit()).then_some(index))
        .unwrap_or(spec.len());
    let (number, unit) = spec.split_at(unit_start);
    if number.is_empty() {
        anyhow::bail!("invalid duration {spec:?} ({DURATION_HELP})");
    }
    let amount: i64 = number
        .parse()
        .map_err(|_| anyhow::anyhow!("duration too large: {spec:?}"))?;
    let unit_secs = match unit.to_ascii_lowercase().as_str() {
        "" | "d" => DAY_SECS,
        "s" => 1,
        "min" => MINUTE_SECS,
        "h" => HOUR_SECS,
        "w" => WEEK_SECS,
        "m" => MONTH_SECS,
        "y" => YEAR_SECS,
        _ => anyhow::bail!("invalid duration {spec:?} ({DURATION_HELP})"),
    };
    amount
        .checked_mul(unit_secs)
        .ok_or_else(|| anyhow::anyhow!("duration too large: {spec:?}"))
}

/// A validated relative duration accepted by every applicable `--since` flag. Parsing this type in
/// clap keeps validation and unit semantics identical across reindexing, search, and inbox listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinceDuration {
    spec: String,
    seconds: i64,
}

impl SinceDuration {
    #[cfg(test)]
    fn seconds(&self) -> i64 {
        self.seconds
    }

    /// Compute `now - duration`. Saturation makes a huge valid duration mean "from the beginning".
    pub fn cutoff(&self) -> i64 {
        now_unix().saturating_sub(self.seconds)
    }

    #[cfg(test)]
    fn cutoff_from(&self, now: i64) -> i64 {
        now.saturating_sub(self.seconds)
    }
}

impl FromStr for SinceDuration {
    type Err = String;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        let spec = input.trim();
        let seconds = parse_duration(spec).map_err(|error| error.to_string())?;
        Ok(Self {
            spec: spec.to_owned(),
            seconds,
        })
    }
}

impl Display for SinceDuration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.spec)
    }
}

/// Note-level creation timestamp shared by indexing and filesystem-backed listings. Valid Vagus
/// frontmatter uses local `%Y-%m-%dT%H:%M`; absent/unparseable values fall back to filesystem mtime
/// so frontmatter-free notes remain `--since`-filterable (ADR 0017/G3).
pub fn note_created_at_secs(created: Option<&str>, mtime: f64) -> i64 {
    if let Some(raw) = created
        && let Ok(naive) = NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%dT%H:%M")
        && let Some(datetime) = Local.from_local_datetime(&naive).single()
    {
        return datetime.timestamp();
    }
    mtime as i64
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    #[test]
    fn shared_since_units_include_hours_days_months_and_years() {
        assert_eq!(parse_duration("10h").unwrap(), 10 * HOUR_SECS);
        assert_eq!(parse_duration("5d").unwrap(), 5 * DAY_SECS);
        assert_eq!(parse_duration("3m").unwrap(), 3 * MONTH_SECS);
        assert_eq!(parse_duration("1y").unwrap(), YEAR_SECS);

        // Preserve the useful smaller/week units without keeping the old ambiguous `m=minutes`.
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("30min").unwrap(), 30 * MINUTE_SECS);
        assert_eq!(parse_duration("2w").unwrap(), 2 * WEEK_SECS);
        assert_eq!(parse_duration("7").unwrap(), 7 * DAY_SECS);
        assert_eq!(parse_duration("3D").unwrap(), 3 * DAY_SECS);
        assert_eq!(parse_duration("  5H ").unwrap(), 5 * HOUR_SECS);
    }

    #[test]
    fn shared_since_parser_rejects_malformed_and_overflowing_operands() {
        for invalid in [
            "",
            "abc",
            "10x",
            "1.5d",
            "10 d",
            "-3d",
            "m",
            "10é",
            "999999999999999999999999999999999999999y",
        ] {
            assert!(
                invalid.parse::<SinceDuration>().is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn validated_since_duration_preserves_operand_and_saturates_cutoff() {
        let duration: SinceDuration = " 3m ".parse().unwrap();
        assert_eq!(duration.to_string(), "3m");
        assert_eq!(duration.seconds(), 90 * DAY_SECS);
        assert_eq!(duration.cutoff_from(100), -7_775_900);
        assert_eq!(duration.cutoff_from(i64::MIN), i64::MIN);
    }
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
