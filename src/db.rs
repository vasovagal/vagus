//! SQLite metadata + vector store (rusqlite, bundled SQLite).
//!
//! Holds `files` (for the mtime+sha256 incremental diff), `chunks` (text + heading + the embedding
//! as a BLOB), and `meta` (pinned embed model/dims + schema/index versions — guardrail G4).
//! This DB lives OUTSIDE iCloud (guardrail G1) and is a rebuildable cache (G2) — except the `ticks`
//! table, which is local user data (usage counts, ADR 0021/G25) and survives reindex alongside `meta`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::chunk::Chunk;

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;

CREATE TABLE IF NOT EXISTS files(
  path       TEXT PRIMARY KEY,   -- vault-relative, e.g. "00-Inbox/idea.md"
  mtime      REAL NOT NULL,      -- seconds since epoch
  sha256     TEXT NOT NULL,
  indexed_at INTEGER NOT NULL    -- unix secs
);

CREATE TABLE IF NOT EXISTS chunks(
  id           TEXT PRIMARY KEY,                                 -- sha256(path + '#' + ord)
  path         TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  ord          INTEGER NOT NULL,
  heading_path TEXT NOT NULL,
  body         TEXT NOT NULL,
  embedding    BLOB,                                             -- f32 LE, len = dims*4; NULL until embedded
  created_at   INTEGER,                                          -- unix secs; note-level (frontmatter `created`, else file mtime). G3 fallback. ADR 0017
  source       TEXT                                              -- note-level (frontmatter `source`); NULL when absent. ADR 0017
);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);

CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);

-- `--smart` query-expansion cache (ADR 0016). The local rewriter is deterministic (fixed seed), so
-- its typed lex/vec/hyde output is a pure function of the query + model identity + decoding params.
-- The `key` already folds all of those in (see `rewrite::expansion_cache_key`), so a model/param swap
-- yields a fresh key and stale rows are simply never read. Derived cache, fully rebuildable (G2);
-- independent of vault content (only the LLM step is cached, retrieval always runs live).
CREATE TABLE IF NOT EXISTS expansion_cache(
  key        TEXT PRIMARY KEY,   -- sha256(query + model/tokenizer ids + sampling params + token cap)
  value      TEXT NOT NULL,      -- JSON-serialized Vec<rewrite::Variant>
  created_at INTEGER NOT NULL    -- unix secs
);

-- Usage ticks (ADR 0021/G25): LOCAL USER DATA, not a derived cache — the one table in this DB
-- that is NOT rebuildable from the vault. Never wiped by clear_all(); deliberately NO foreign
-- key to files(path): foreign_keys=ON + ON DELETE CASCADE would wipe ticks on every reindex
-- (clear_all deletes all files rows) and on delete_file().
CREATE TABLE IF NOT EXISTS ticks(
  path       TEXT PRIMARY KEY,   -- vault-relative, same key space as files.path
  count      INTEGER NOT NULL,
  first_used INTEGER NOT NULL,   -- unix secs
  last_used  INTEGER NOT NULL    -- unix secs
);
"#;

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(SCHEMA).context("applying schema")?;
        Self::migrate(&conn).context("migrating schema")?;
        Ok(Self { conn })
    }

    /// Idempotent additive migrations for DBs created before a column existed. The CHUNK_VERSION bump
    /// clears *rows* (G4 auto-reindex) but never alters the table shape, so a pre-existing `chunks`
    /// table needs its new columns added here. `CREATE TABLE IF NOT EXISTS` covers fresh DBs.
    /// (ADR 0017: `created_at` + `source` for the `--since` / `--source` filters.)
    fn migrate(conn: &Connection) -> Result<()> {
        let have: std::collections::HashSet<String> = conn
            .prepare("PRAGMA table_info(chunks)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        if !have.contains("created_at") {
            conn.execute_batch("ALTER TABLE chunks ADD COLUMN created_at INTEGER;")?;
        }
        if !have.contains("source") {
            conn.execute_batch("ALTER TABLE chunks ADD COLUMN source TEXT;")?;
        }
        // `vec_key` (ADR 0019): the u64 usearch key for each chunk, derived from its id, with an index
        // for the reverse `key -> id` lookup at search time. Additive — no CHUNK_VERSION bump / re-embed.
        if !have.contains("vec_key") {
            conn.execute_batch(
                "ALTER TABLE chunks ADD COLUMN vec_key INTEGER;
                 CREATE INDEX IF NOT EXISTS chunks_vec_key ON chunks(vec_key);",
            )?;
        }
        Self::backfill_vec_keys(conn)?;
        Ok(())
    }

    /// One-time backfill of `vec_key` for rows that predate the column (G2-derived, no re-embed). New
    /// chunks get their key at insert time in `replace_chunks`, so after the first pass this is a fast
    /// indexed no-op. Guarded by a `LIMIT 1` probe so a normal open never scans the whole table.
    fn backfill_vec_keys(conn: &Connection) -> Result<()> {
        let pending: bool = conn
            .query_row(
                "SELECT 1 FROM chunks WHERE vec_key IS NULL LIMIT 1",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !pending {
            return Ok(());
        }
        let ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM chunks WHERE vec_key IS NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let tx = conn.unchecked_transaction()?;
        {
            let mut up = tx.prepare("UPDATE chunks SET vec_key=?1 WHERE id=?2")?;
            for id in &ids {
                up.execute(params![crate::util::key_for(id) as i64, id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // --- meta ---------------------------------------------------------------

    pub fn meta_get(&self, k: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row("SELECT v FROM meta WHERE k=?1", params![k], |r| {
                r.get::<_, String>(0)
            })
            .ok();
        Ok(v)
    }

    pub fn meta_set(&self, k: &str, v: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(k,v) VALUES(?1,?2) ON CONFLICT(k) DO UPDATE SET v=?2",
            params![k, v],
        )?;
        Ok(())
    }

    // --- expansion cache (`--smart`, ADR 0016) ------------------------------
    // Only the generate-gated smart path uses these, so gate them too (lean-build warning-free). The
    // `expansion_cache` table itself is created unconditionally so the DB schema is feature-invariant.

    /// Cached query-expansion payload for `key`, if present (the JSON blob; the caller deserializes).
    #[cfg(feature = "generate")]
    pub fn expansion_cache_get(&self, key: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row(
                "SELECT value FROM expansion_cache WHERE key=?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(v)
    }

    /// Store (or refresh) the expansion payload for `key`.
    #[cfg(feature = "generate")]
    pub fn expansion_cache_put(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO expansion_cache(key,value,created_at) VALUES(?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET value=?2, created_at=?3",
            params![key, value, crate::util::now_unix()],
        )?;
        Ok(())
    }

    // --- files --------------------------------------------------------------

    /// path -> (mtime, sha256) for every indexed file.
    pub fn existing_files(&self) -> Result<HashMap<String, (f64, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, mtime, sha256 FROM files")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (p, m, s) = row?;
            map.insert(p, (m, s));
        }
        Ok(map)
    }

    pub fn upsert_file(&self, path: &str, mtime: f64, sha256: &str, indexed_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files(path,mtime,sha256,indexed_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(path) DO UPDATE SET mtime=?2, sha256=?3, indexed_at=?4",
            params![path, mtime, sha256, indexed_at],
        )?;
        Ok(())
    }

    /// Delete a file and (via cascade) its chunks. Returns the chunk ids removed, so the caller can
    /// also drop them from the tantivy index (guardrail G5).
    pub fn delete_file(&self, path: &str) -> Result<Vec<String>> {
        let ids = self.chunk_ids_for(path)?;
        self.conn
            .execute("DELETE FROM files WHERE path=?1", params![path])?;
        Ok(ids)
    }

    // --- chunks -------------------------------------------------------------

    /// (path, heading_path, body) for a chunk id, if present.
    pub fn chunk_row(&self, id: &str) -> Result<Option<(String, String, String)>> {
        let row = self
            .conn
            .query_row(
                "SELECT path, heading_path, body FROM chunks WHERE id=?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Chunk rows whose id starts with `prefix`: (id, path, heading_path, body). LIMIT 2 — the
    /// caller only needs to distinguish unique from ambiguous. Callers validate `prefix` is hex
    /// first (no LIKE metacharacters).
    pub fn chunk_rows_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, heading_path, body FROM chunks WHERE id LIKE ?1 || '%' LIMIT 2",
        )?;
        let rows = stmt.query_map(params![prefix], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// All chunks of a note in document order: (id, heading_path, body).
    pub fn chunks_for_path(&self, path: &str) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, heading_path, body FROM chunks WHERE path=?1 ORDER BY ord")?;
        let rows = stmt.query_map(params![path], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Note-level filter fields (`created_at`, `source`) for a chunk id, if present. Read separately
    /// from `chunk_row` so the hot display join (path/heading/body) stays unchanged; the `--since` /
    /// `--source` post-rank filter (ADR 0017) only needs these for the candidate survivors.
    pub fn chunk_filter_fields(&self, id: &str) -> Result<Option<(Option<i64>, Option<String>)>> {
        let row = self
            .conn
            .query_row(
                "SELECT created_at, source FROM chunks WHERE id=?1",
                params![id],
                |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    pub fn chunk_ids_for(&self, path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM chunks WHERE path=?1")?;
        let rows = stmt.query_map(params![path], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replace all chunks for a file (delete-then-insert). Embeddings are left NULL here; the embed
    /// step fills them. `created_at` (unix secs) and `source` are **note-level** values (parsed from
    /// frontmatter, with a mtime fallback for `created_at` — G3/ADR 0017) attached to every chunk of
    /// the note. Returns the prior chunk ids (for tantivy cleanup, guardrail G5).
    pub fn replace_chunks(
        &self,
        path: &str,
        chunks: &[Chunk],
        created_at: Option<i64>,
        source: Option<&str>,
    ) -> Result<Vec<String>> {
        let old = self.chunk_ids_for(path)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM chunks WHERE path=?1", params![path])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO chunks(id,path,ord,heading_path,body,embedding,created_at,source,vec_key)
                 VALUES(?1,?2,?3,?4,?5,NULL,?6,?7,?8)",
            )?;
            for c in chunks {
                // `vec_key` is the usearch key derived from the id (ADR 0019); stored as i64 (SQLite has
                // no u64) and reinterpreted on read. Set at insert so it never needs backfilling later.
                stmt.execute(params![
                    c.id,
                    path,
                    c.ord as i64,
                    c.heading_path,
                    c.body,
                    created_at,
                    source,
                    crate::util::key_for(&c.id) as i64
                ])?;
            }
        }
        tx.commit()?;
        Ok(old)
    }

    /// All embedded chunks as (chunk_id, vector). Loaded into RAM for brute-force cosine.
    pub fn all_embeddings(&self) -> Result<Vec<(String, Vec<f32>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, embedding FROM chunks WHERE embedding IS NOT NULL")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, bytes) = row?;
            let v: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            out.push((id, v));
        }
        Ok(out)
    }

    /// Reverse `vec_key -> chunk id` map for a set of usearch keys (ADR 0019). usearch returns u64
    /// keys; the search path resolves them back to chunk ids through the indexed `vec_key` column.
    /// `keys` is small (top-k candidates), so a single `IN (...)` query is fine.
    pub fn chunk_ids_for_keys(&self, keys: &[u64]) -> Result<HashMap<u64, String>> {
        let mut map = HashMap::new();
        if keys.is_empty() {
            return Ok(map);
        }
        let placeholders = std::iter::repeat_n("?", keys.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT vec_key, id FROM chunks WHERE vec_key IN ({placeholders})");
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<i64> = keys.iter().map(|k| *k as i64).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (k, id) = row?;
            map.insert(k, id);
        }
        Ok(map)
    }

    pub fn set_embedding(&self, chunk_id: &str, vec: &[f32]) -> Result<()> {
        let bytes: Vec<u8> = vec.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn.execute(
            "UPDATE chunks SET embedding=?1 WHERE id=?2",
            params![bytes, chunk_id],
        )?;
        Ok(())
    }

    /// Wipe derived rows (for `reindex`). Keeps `meta`, `expansion_cache`, and `ticks` — `ticks` is
    /// user data (ADR 0021/G25); never add it to this batch.
    pub fn clear_all(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM chunks; DELETE FROM files;")?;
        Ok(())
    }

    // --- ticks (ADR 0021/G25: local user data, NOT a derived cache) ----------

    /// Record one usage tick for `path` (vault-relative); returns the new total. Unconditional —
    /// never gated on a `files` row: the files table is a cache (transiently empty mid-reindex)
    /// and rejecting user data against a stale cache would lose it.
    pub fn tick(&self, path: &str) -> Result<i64> {
        let now = crate::util::now_unix();
        let count = self.conn.query_row(
            "INSERT INTO ticks(path,count,first_used,last_used) VALUES(?1,1,?2,?2)
             ON CONFLICT(path) DO UPDATE SET count=count+1, last_used=excluded.last_used
             RETURNING count",
            params![path, now],
            |r| r.get::<_, i64>(0),
        )?;
        Ok(count)
    }

    /// Re-key ticks from `old` to `new` (`vagus file` moves — user data follows the note),
    /// merging counts when `new` already has a row. No-op when `old` has no row, and when
    /// `old == new` (re-filing to the current folder): the upsert below would conflict with the
    /// source row itself and the DELETE would then erase it.
    pub fn tick_rename(&mut self, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO ticks(path,count,first_used,last_used)
             SELECT ?2, count, first_used, last_used FROM ticks WHERE path=?1
             ON CONFLICT(path) DO UPDATE SET
               count      = count + excluded.count,
               first_used = MIN(first_used, excluded.first_used),
               last_used  = MAX(last_used,  excluded.last_used)",
            params![old, new],
        )?;
        tx.execute("DELETE FROM ticks WHERE path=?1", params![old])?;
        tx.commit()?;
        Ok(())
    }

    /// Most-ticked notes as (path, count, first_used, last_used, missing). `missing` = no `files`
    /// row (deleted or renamed outside vagus); such orphans are hidden unless `include_missing`.
    #[allow(clippy::type_complexity)]
    pub fn fame(
        &self,
        limit: usize,
        include_missing: bool,
    ) -> Result<Vec<(String, i64, i64, i64, bool)>> {
        let sql = format!(
            "SELECT t.path, t.count, t.first_used, t.last_used, (f.path IS NULL)
             FROM ticks t LEFT JOIN files f ON f.path = t.path
             {}
             ORDER BY t.count DESC, t.last_used DESC LIMIT ?1",
            if include_missing {
                ""
            } else {
                "WHERE f.path IS NOT NULL"
            }
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Ticked paths with no `files` row (notes moved/deleted outside vagus).
    pub fn orphan_tick_count(&self) -> Result<i64> {
        self.count(
            "SELECT COUNT(*) FROM ticks t LEFT JOIN files f ON f.path=t.path WHERE f.path IS NULL",
        )
    }

    // --- counts -------------------------------------------------------------

    pub fn count(&self, sql: &str) -> Result<i64> {
        Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TempDir;

    fn temp_db(tag: &str) -> (TempDir, Db) {
        let dir = TempDir::new(tag);
        let db = Db::open(&dir.path().join("meta.db")).unwrap();
        (dir, db)
    }

    fn chunk(path: &str, ord: usize) -> Chunk {
        Chunk {
            id: crate::util::sha256_hex(format!("{path}#{ord}").as_bytes()),
            ord,
            heading_path: String::new(),
            body: "body".into(),
        }
    }

    #[test]
    fn tick_increments_and_sets_timestamps() {
        let (_d, db) = temp_db("tick-inc");
        assert_eq!(db.tick("20-Areas/foo.md").unwrap(), 1);
        let (first0, last0) = db
            .conn
            .query_row(
                "SELECT first_used, last_used FROM ticks WHERE path='20-Areas/foo.md'",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(db.tick("20-Areas/foo.md").unwrap(), 2);
        let (first1, last1) = db
            .conn
            .query_row(
                "SELECT first_used, last_used FROM ticks WHERE path='20-Areas/foo.md'",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(first1, first0, "first_used unchanged by later ticks");
        assert!(first1 <= last1);
        assert!(last1 >= last0);
    }

    #[test]
    fn ticks_survive_clear_all() {
        // THE core regression guard: reindex (clear_all) must never destroy user data.
        let (_d, db) = temp_db("tick-clear");
        db.upsert_file("20-Areas/foo.md", 1.0, "sha", 1).unwrap();
        db.replace_chunks(
            "20-Areas/foo.md",
            &[chunk("20-Areas/foo.md", 0)],
            None,
            None,
        )
        .unwrap();
        db.tick("20-Areas/foo.md").unwrap();
        db.tick("20-Areas/foo.md").unwrap();

        db.clear_all().unwrap();

        assert_eq!(db.count("SELECT count(*) FROM files").unwrap(), 0);
        assert_eq!(db.count("SELECT count(*) FROM chunks").unwrap(), 0);
        let rows = db.fame(10, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "20-Areas/foo.md");
        assert_eq!(rows[0].1, 2, "tick count intact after clear_all");
    }

    #[test]
    fn ticks_survive_delete_file() {
        // Guards against anyone adding an FK onto files(path) later.
        let (_d, db) = temp_db("tick-del");
        db.upsert_file("20-Areas/foo.md", 1.0, "sha", 1).unwrap();
        db.tick("20-Areas/foo.md").unwrap();
        db.delete_file("20-Areas/foo.md").unwrap();
        assert_eq!(db.count("SELECT count(*) FROM ticks").unwrap(), 1);
        assert_eq!(db.orphan_tick_count().unwrap(), 1);
    }

    #[test]
    fn tick_rename_merges_counts() {
        let (_d, mut db) = temp_db("tick-rename");
        for _ in 0..3 {
            db.tick("00-Inbox/a.md").unwrap();
        }
        db.tick("30-Resources/a.md").unwrap();
        // Force distinct timestamps so MIN/MAX are observable.
        db.conn
            .execute_batch(
                "UPDATE ticks SET first_used=100, last_used=200 WHERE path='00-Inbox/a.md';
                 UPDATE ticks SET first_used=150, last_used=150 WHERE path='30-Resources/a.md';",
            )
            .unwrap();
        db.tick_rename("00-Inbox/a.md", "30-Resources/a.md")
            .unwrap();

        let rows = db.fame(10, true).unwrap();
        assert_eq!(rows.len(), 1, "old row gone");
        let (path, count, first, last, _) = rows[0].clone();
        assert_eq!(path, "30-Resources/a.md");
        assert_eq!(count, 4);
        assert_eq!(first, 100, "merged first_used = MIN");
        assert_eq!(last, 200, "merged last_used = MAX");

        // Renaming an unticked path is a no-op that leaves the destination untouched.
        db.tick_rename("00-Inbox/unticked.md", "30-Resources/a.md")
            .unwrap();
        let rows = db.fame(10, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, 4);
    }

    #[test]
    fn tick_rename_same_path_keeps_ticks() {
        // `vagus file <note> --to <its current folder>` re-keys old==new; without the guard the
        // upsert conflicts with the source row and the DELETE erases it.
        let (_d, mut db) = temp_db("tick-rename-same");
        for _ in 0..3 {
            db.tick("20-Areas/a.md").unwrap();
        }
        db.tick_rename("20-Areas/a.md", "20-Areas/a.md").unwrap();

        let rows = db.fame(10, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "20-Areas/a.md");
        assert_eq!(rows[0].1, 3, "count neither doubled nor deleted");
    }

    #[test]
    fn fame_hides_missing_by_default() {
        let (_d, db) = temp_db("fame-missing");
        db.upsert_file("a.md", 1.0, "sha", 1).unwrap();
        db.upsert_file("b.md", 1.0, "sha", 1).unwrap();
        for _ in 0..3 {
            db.tick("a.md").unwrap();
        }
        for _ in 0..2 {
            db.tick("b.md").unwrap();
        }
        db.tick("gone.md").unwrap(); // no files row: an orphan
        db.conn
            .execute_batch(
                "UPDATE ticks SET last_used=10 WHERE path='a.md';
                 UPDATE ticks SET last_used=30, count=3 WHERE path='b.md';
                 UPDATE ticks SET last_used=20, count=3 WHERE path='gone.md';",
            )
            .unwrap();

        let visible = db.fame(10, false).unwrap();
        assert!(
            visible.iter().all(|r| r.0 != "gone.md"),
            "orphan hidden by default"
        );

        let all = db.fame(10, true).unwrap();
        // count DESC, then last_used DESC: b (3,30), gone (3,20), a (3,10).
        assert_eq!(
            all.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["b.md", "gone.md", "a.md"]
        );
        let gone = all.iter().find(|r| r.0 == "gone.md").unwrap();
        assert!(gone.4, "orphan carries missing=true");
        assert!(!all.iter().find(|r| r.0 == "a.md").unwrap().4);
    }
}
