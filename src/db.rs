//! SQLite metadata + vector store (rusqlite, bundled SQLite).
//!
//! Holds `files` (for the mtime+sha256 incremental diff), `chunks` (text + heading + the embedding
//! as a BLOB), and `meta` (pinned embed model/dims + schema/index versions — guardrail G4).
//! This DB lives OUTSIDE iCloud (guardrail G1) and is a rebuildable cache (G2).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::chunk::Chunk;

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

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
  embedding    BLOB                                              -- f32 LE, len = dims*4; NULL until embedded
);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);

CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT NOT NULL);
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
        Ok(Self { conn })
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
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
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
    /// step fills them. Returns the prior chunk ids (for tantivy cleanup, guardrail G5).
    pub fn replace_chunks(&self, path: &str, chunks: &[Chunk]) -> Result<Vec<String>> {
        let old = self.chunk_ids_for(path)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM chunks WHERE path=?1", params![path])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO chunks(id,path,ord,heading_path,body,embedding)
                 VALUES(?1,?2,?3,?4,?5,NULL)",
            )?;
            for c in chunks {
                stmt.execute(params![c.id, path, c.ord as i64, c.heading_path, c.body])?;
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

    pub fn set_embedding(&self, chunk_id: &str, vec: &[f32]) -> Result<()> {
        let bytes: Vec<u8> = vec.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn.execute(
            "UPDATE chunks SET embedding=?1 WHERE id=?2",
            params![bytes, chunk_id],
        )?;
        Ok(())
    }

    /// Wipe derived rows (for `reindex`). Keeps `meta`.
    pub fn clear_all(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM chunks; DELETE FROM files;")?;
        Ok(())
    }

    // --- counts -------------------------------------------------------------

    pub fn count(&self, sql: &str) -> Result<i64> {
        Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))?)
    }
}
