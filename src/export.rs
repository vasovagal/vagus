//! `vagus vectors export`: coherent dump of the embedding matrix for offline analysis.
//!
//! Writes three files into an explicit `--out`: the vector matrix (`vectors.npy`, NumPy v1.0 C-order
//! f32, or raw LE f32 `vectors.f32` with `--format f32`), `meta.jsonl` (one row per matrix row, same
//! order), and `manifest.json` (embedding identity + shape). SQLite rows and identity are read from one
//! transaction and streamed through fresh staging files. The manifest is published last as the
//! completion marker. Output paths are resolved fail-closed and may never enter the Markdown vault
//! (G1). Instrumentation only — no embed/index/RRF change, so no new ADR.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::config::Config;
use crate::db::Db;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ExportFormat {
    /// NumPy `.npy` v1.0 (C-order float32) — `numpy.load()` reads it directly.
    Npy,
    /// Raw little-endian f32, `N*dims*4` bytes, no header.
    F32,
}

impl ExportFormat {
    fn tag(self) -> &'static str {
        match self {
            ExportFormat::Npy => "npy",
            ExportFormat::F32 => "f32",
        }
    }

    fn vectors_file(self) -> &'static str {
        match self {
            ExportFormat::Npy => "vectors.npy",
            ExportFormat::F32 => "vectors.f32",
        }
    }
}

/// `manifest.json`: the embedding space (G4 identity) + array shape a consumer needs.
#[derive(Serialize)]
struct Manifest<'a> {
    embed_model: &'a str,
    dims: usize,
    count: usize,
    skipped_unembedded: i64,
    order: &'a str,
    dtype: &'a str,
    vectors_file: &'a str,
    format: &'a str,
}

/// One `meta.jsonl` line, in matrix-row order.
#[derive(Serialize)]
struct MetaRow<'a> {
    i: usize,
    chunk_id: &'a str,
    path: &'a str,
    ord: i64,
    heading: &'a str,
    blen: usize,
    created_at: Option<i64>,
    source: Option<&'a str>,
    vec_key: Option<u64>,
}

/// Stable `--json` summary (one object).
#[derive(Serialize)]
struct Summary<'a> {
    out: String,
    count: usize,
    dims: usize,
    embed_model: &'a str,
    format: &'a str,
    skipped: i64,
    files: [&'a str; 3],
}

struct Snapshot {
    embed_model: String,
    dims: usize,
    count: usize,
    skipped: i64,
}

/// Fresh sibling staging directory. It is removed on every error path; successful publication either
/// renames it into place or disarms cleanup after moving all payloads out.
struct StageDir {
    path: PathBuf,
    cleanup: bool,
}

impl StageDir {
    fn create(parent: &Path) -> Result<Self> {
        for attempt in 0..1024_u16 {
            let path = parent.join(format!(
                ".vagus-vector-export-stage-{}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        cleanup: true,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
            }
        }
        bail!("could not allocate a fresh export staging directory")
    }

    fn disarm(&mut self) {
        self.cleanup = false;
    }
}

impl Drop for StageDir {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub fn export(
    cfg: &Config,
    out: &Path,
    format: ExportFormat,
    force: bool,
    json: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let out = prepare_output(out, &cfg.vault, &cwd, force)?;
    let parent = out
        .parent()
        .ok_or_else(|| anyhow!("--out {} has no parent directory", out.display()))?;
    let mut stage = StageDir::create(parent)?;

    // Db::open can apply the same derived-cache schema migrations as doctor/status. The actual export
    // then reads identity, counts, and rows from one coherent transaction and never loads a model.
    let db = Db::open(&cfg.db_path())?;
    let snapshot = write_staged_snapshot(&db, &stage.path, format, || {})?;

    // Re-resolve after staging so an intervening symlink/path change fails closed before publication.
    let checked = safe_output_path_with_cwd(&out, &cfg.vault, &cwd)?;
    if checked != out {
        bail!(
            "--out changed while exporting ({} -> {}); refusing to publish",
            out.display(),
            checked.display()
        );
    }
    publish_staged(&mut stage, &out, force, format.vectors_file())?;

    if json {
        let summary = Summary {
            out: out.display().to_string(),
            count: snapshot.count,
            dims: snapshot.dims,
            embed_model: &snapshot.embed_model,
            format: format.tag(),
            skipped: snapshot.skipped,
            files: [format.vectors_file(), "meta.jsonl", "manifest.json"],
        };
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "exported {} vectors ({} dims, {}) -> {}",
            snapshot.count,
            snapshot.dims,
            snapshot.embed_model,
            out.display()
        );
        if snapshot.skipped > 0 {
            println!("  skipped {} unembedded chunk(s)", snapshot.skipped);
        }
        println!(
            "  files: {}, meta.jsonl, manifest.json",
            format.vectors_file()
        );
    }
    Ok(())
}

fn prepare_output(out: &Path, vault: &Path, cwd: &Path, force: bool) -> Result<PathBuf> {
    let out = safe_output_path_with_cwd(out, vault, cwd)?;
    if out.parent().is_none() {
        bail!("refusing to use a filesystem root as --out")
    }

    if let Ok(meta) = fs::symlink_metadata(&out) {
        if meta.file_type().is_symlink() {
            bail!(
                "--out {} is a symlink; choose a real directory",
                out.display()
            )
        }
        if !meta.is_dir() {
            bail!("--out {} exists and is not a directory", out.display())
        }
        if !force && dir_non_empty(&out)? {
            bail!(
                "--out {} is not empty; pass --force to overwrite",
                out.display()
            )
        }
    }

    let parent = out
        .parent()
        .ok_or_else(|| anyhow!("--out {} has no parent directory", out.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;

    // Creating missing ancestors can expose a symlink alias not visible in the first lexical pass.
    let checked = safe_output_path_with_cwd(&out, vault, cwd)?;
    if checked != out {
        bail!(
            "--out resolved differently after creating its parent ({} -> {})",
            out.display(),
            checked.display()
        )
    }
    Ok(out)
}

/// Resolve relative paths, lexical `..`, existing symlink ancestors, and unresolved suffixes. Refuse
/// any destination at or below the similarly resolved vault path, including when the vault does not
/// exist yet. Errors fail closed rather than turning an unreadable path into "outside".
fn safe_output_path_with_cwd(out: &Path, vault: &Path, cwd: &Path) -> Result<PathBuf> {
    let out_abs = crate::path_safety::absolute_from(out, cwd)?;
    if let Ok(meta) = fs::symlink_metadata(&out_abs)
        && meta.file_type().is_symlink()
    {
        bail!(
            "--out {} is a symlink; choose a real directory",
            out_abs.display()
        )
    }
    let out_resolved = crate::path_safety::resolve_from(&out_abs, cwd)?;
    let vault_resolved = crate::path_safety::resolve_from(vault, cwd)?;
    if out_resolved.starts_with(&vault_resolved) {
        bail!(
            "--out {} is inside the vault ({}) — G1: the vault holds Markdown only",
            out.display(),
            vault.display()
        )
    }
    Ok(out_resolved)
}

fn dir_non_empty(path: &Path) -> Result<bool> {
    let mut entries =
        fs::read_dir(path).with_context(|| format!("inspecting {}", path.display()))?;
    Ok(entries.next().transpose()?.is_some())
}

fn create_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating fresh staging file {}", path.display()))
}

fn flush_and_sync(mut writer: BufWriter<File>, path: &Path) -> Result<()> {
    writer
        .flush()
        .with_context(|| format!("flushing {}", path.display()))?;
    writer
        .get_ref()
        .sync_all()
        .with_context(|| format!("syncing {}", path.display()))
}

/// Stream one coherent SQLite snapshot into fresh staged artifacts. `before_rows` exists solely to
/// exercise snapshot isolation in tests; production passes a no-op.
fn write_staged_snapshot<F>(
    db: &Db,
    stage: &Path,
    format: ExportFormat,
    before_rows: F,
) -> Result<Snapshot>
where
    F: FnOnce(),
{
    let tx = db
        .conn
        .unchecked_transaction()
        .context("starting export read transaction")?;

    let embed_model: String = tx
        .query_row("SELECT v FROM meta WHERE k='embed_model'", [], |r| r.get(0))
        .optional()?
        .ok_or_else(|| anyhow!("embedding identity is not pinned — run `vagus reindex`"))?;

    let dims: usize = match tx
        .query_row("SELECT v FROM meta WHERE k='embed_dims'", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        Some(raw) => raw
            .parse()
            .with_context(|| format!("invalid pinned embed_dims {raw:?}; reindex"))?,
        None => {
            let bytes: Option<i64> = tx
                .query_row(
                    "SELECT length(embedding) FROM chunks WHERE embedding IS NOT NULL LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            match bytes {
                Some(n) if n > 0 && n % 4 == 0 => usize::try_from(n / 4)?,
                _ => bail!(
                    "nothing embedded and no embedding dimensions pinned — run `vagus reindex`"
                ),
            }
        }
    };
    if dims == 0 {
        bail!("pinned embed_dims is zero; reindex")
    }
    let expected_bytes = dims
        .checked_mul(4)
        .ok_or_else(|| anyhow!("pinned embed_dims is too large"))?;

    let count_i64: i64 = tx.query_row(
        "SELECT count(*) FROM chunks WHERE embedding IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let count = usize::try_from(count_i64).context("negative embedded row count")?;
    let skipped: i64 = tx.query_row(
        "SELECT count(*) FROM chunks WHERE embedding IS NULL",
        [],
        |r| r.get(0),
    )?;

    // The transaction has performed reads and therefore fixed its WAL snapshot before the hook.
    before_rows();

    let vectors_file = format.vectors_file();
    let vectors_path = stage.join(vectors_file);
    let meta_path = stage.join("meta.jsonl");
    let manifest_path = stage.join("manifest.json");
    let mut vectors = BufWriter::new(create_new(&vectors_path)?);
    let mut meta = BufWriter::new(create_new(&meta_path)?);
    if format == ExportFormat::Npy {
        vectors.write_all(&npy_header(count, dims))?;
    }

    let actual = {
        let mut stmt = tx.prepare(
            "SELECT id, path, ord, heading_path, length(body), created_at, source, vec_key, embedding
             FROM chunks WHERE embedding IS NOT NULL ORDER BY path, ord",
        )?;
        let mut rows = stmt.query([])?;
        let mut i = 0_usize;
        while let Some(row) = rows.next()? {
            let chunk_id: String = row.get(0)?;
            let path: String = row.get(1)?;
            let ord: i64 = row.get(2)?;
            let heading: String = row.get(3)?;
            let blen_i64: i64 = row.get(4)?;
            let created_at: Option<i64> = row.get(5)?;
            let source: Option<String> = row.get(6)?;
            let vec_key: Option<i64> = row.get(7)?;
            let embedding: Vec<u8> = row.get(8)?;

            if embedding.len() != expected_bytes {
                bail!(
                    "chunk {chunk_id} has a {}-byte embedding, expected {expected_bytes} (dims {dims}) — DB embedding space is inconsistent (G4); reindex",
                    embedding.len()
                )
            }
            vectors.write_all(&embedding)?;
            let row = MetaRow {
                i,
                chunk_id: &chunk_id,
                path: &path,
                ord,
                heading: &heading,
                blen: usize::try_from(blen_i64).context("negative chunk body length")?,
                created_at,
                source: source.as_deref(),
                vec_key: vec_key.map(|key| key as u64),
            };
            serde_json::to_writer(&mut meta, &row)?;
            meta.write_all(b"\n")?;
            i += 1;
        }
        i
    };
    if actual != count {
        bail!("snapshot row count changed unexpectedly: counted {count}, streamed {actual}")
    }
    flush_and_sync(vectors, &vectors_path)?;
    flush_and_sync(meta, &meta_path)?;

    let manifest = Manifest {
        embed_model: &embed_model,
        dims,
        count,
        skipped_unembedded: skipped,
        order: "path,ord",
        dtype: "<f4",
        vectors_file,
        format: format.tag(),
    };
    let mut manifest_writer = BufWriter::new(create_new(&manifest_path)?);
    serde_json::to_writer_pretty(&mut manifest_writer, &manifest)?;
    manifest_writer.write_all(b"\n")?;
    flush_and_sync(manifest_writer, &manifest_path)?;

    tx.commit().context("finishing export read transaction")?;
    Ok(Snapshot {
        embed_model,
        dims,
        count,
        skipped,
    })
}

/// Publish staged files. For an existing directory the old manifest is removed first, payloads are
/// replaced by rename (never opened through a leaf symlink), and the new manifest is renamed last.
/// Thus any interrupted/failed forced export lacks the completion marker instead of blessing a mixed
/// dataset. A new output directory is published with one directory rename.
fn publish_staged(stage: &mut StageDir, out: &Path, force: bool, vectors_file: &str) -> Result<()> {
    match fs::symlink_metadata(out) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(&stage.path, out)
                .with_context(|| format!("publishing {}", out.display()))?;
            stage.disarm();
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", out.display())),
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "--out {} became a symlink; refusing to publish",
                out.display()
            )
        }
        Ok(meta) if !meta.is_dir() => {
            bail!("--out {} is not a directory", out.display())
        }
        Ok(_) => {}
    }
    if !force && dir_non_empty(out)? {
        bail!(
            "--out {} became non-empty; pass --force to overwrite",
            out.display()
        )
    }

    // Invalidate any prior completed generation before changing either payload.
    remove_leaf_if_present(&out.join("manifest.json"))?;
    install_staged_file(&stage.path.join(vectors_file), &out.join(vectors_file))?;
    install_staged_file(&stage.path.join("meta.jsonl"), &out.join("meta.jsonl"))?;
    // Completion marker is always last.
    install_staged_file(
        &stage.path.join("manifest.json"),
        &out.join("manifest.json"),
    )?;
    fs::remove_dir(&stage.path)
        .with_context(|| format!("removing staging directory {}", stage.path.display()))?;
    stage.disarm();
    Ok(())
}

fn remove_leaf_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            bail!("refusing to replace directory artifact {}", path.display())
        }
        Ok(_) => fs::remove_file(path).with_context(|| format!("removing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("inspecting {}", path.display())),
    }
}

fn install_staged_file(src: &Path, dest: &Path) -> Result<()> {
    remove_leaf_if_present(dest)?;
    fs::rename(src, dest)
        .with_context(|| format!("publishing {} -> {}", src.display(), dest.display()))
}

/// NumPy v1.0 header for a C-order (row-major) float32 array of shape `(n, d)`. Dependency-free.
///
/// Layout: magic `\x93NUMPY`, version `\x01\x00`, u16-LE header length, then the ASCII dict padded
/// with spaces so the whole preamble is a multiple of 64 bytes and ends in `\n`.
fn npy_header(n: usize, d: usize) -> Vec<u8> {
    let dict = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({n}, {d}), }}");
    const PREAMBLE: usize = 6 + 2 + 2;
    let unpadded = PREAMBLE + dict.len() + 1;
    let pad = (64 - unpadded % 64) % 64;
    let mut header = dict.into_bytes();
    header.extend(std::iter::repeat_n(b' ', pad));
    header.push(b'\n');

    let mut out = Vec::with_capacity(PREAMBLE + header.len());
    out.extend_from_slice(b"\x93NUMPY");
    out.extend_from_slice(&[0x01, 0x00]);
    out.extend_from_slice(&(header.len() as u16).to_le_bytes());
    out.extend_from_slice(&header);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::util::testdir::TempDir;

    #[test]
    fn npy_header_shape_and_alignment() {
        let h = npy_header(2, 3);
        assert_eq!(&h[..6], b"\x93NUMPY");
        assert_eq!(&h[6..8], &[0x01, 0x00], "version 1.0");
        assert_eq!(h.len() % 64, 0, "preamble is 64-byte aligned");
        assert_eq!(*h.last().unwrap(), b'\n', "header ends with newline");
        let hlen = u16::from_le_bytes([h[8], h[9]]) as usize;
        assert_eq!(10 + hlen, h.len(), "declared header length matches");
        let dict = std::str::from_utf8(&h[10..]).unwrap();
        assert!(dict.contains("'descr': '<f4'"));
        assert!(dict.contains("'fortran_order': False"));
        assert!(
            dict.contains("'shape': (2, 3), "),
            "shape tuple present: {dict:?}"
        );
    }

    #[test]
    fn summary_json_shape_is_stable() {
        let s = Summary {
            out: "/tmp/x".into(),
            count: 3,
            dims: 768,
            embed_model: "m",
            format: "npy",
            skipped: 1,
            files: ["vectors.npy", "meta.jsonl", "manifest.json"],
        };
        let v = serde_json::to_value(&s).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "count",
                "dims",
                "embed_model",
                "files",
                "format",
                "out",
                "skipped"
            ]
        );
        assert_eq!(obj["files"].as_array().unwrap().len(), 3);
    }

    fn cfg_with_db(tag: &str) -> (TempDir, Config) {
        let dir = TempDir::new(tag);
        let cfg = Config {
            vault: dir.path().join("vault"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };
        (dir, cfg)
    }

    fn chunk(path: &str, ord: usize, body: &str) -> Chunk {
        Chunk {
            id: crate::util::sha256_hex(format!("{path}#{ord}").as_bytes()),
            ord,
            heading_path: format!("H{ord}"),
            body: body.into(),
        }
    }

    fn seed(db: &Db) {
        db.meta_set("embed_model", "test/model").unwrap();
        db.meta_set("embed_dims", "3").unwrap();
        db.upsert_file("b.md", 1.0, "sha", 1).unwrap();
        db.upsert_file("a.md", 1.0, "sha", 1).unwrap();
        db.replace_chunks("b.md", &[chunk("b.md", 0, "beta")], None, None)
            .unwrap();
        db.replace_chunks(
            "a.md",
            &[chunk("a.md", 0, "one"), chunk("a.md", 1, "two")],
            Some(42),
            Some("web"),
        )
        .unwrap();
        for c in [chunk("b.md", 0, "beta"), chunk("a.md", 0, "one")] {
            db.set_embedding(&c.id, &[1.0, 2.0, 3.0]).unwrap();
        }
        db.set_embedding(&chunk("a.md", 1, "two").id, &[4.0, 5.0, 6.0])
            .unwrap();
    }

    #[test]
    fn export_npy_roundtrip() {
        let (dir, cfg) = cfg_with_db("export-npy");
        let db = Db::open(&cfg.db_path()).unwrap();
        seed(&db);

        let out = dir.path().join("out");
        export(&cfg, &out, ExportFormat::Npy, false, false).unwrap();

        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["embed_model"], "test/model");
        assert_eq!(manifest["dims"], 3);
        assert_eq!(manifest["count"], 3);
        assert_eq!(manifest["order"], "path,ord");
        assert_eq!(manifest["vectors_file"], "vectors.npy");

        let meta = fs::read_to_string(out.join("meta.jsonl")).unwrap();
        let rows: Vec<serde_json::Value> = meta
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let order: Vec<(&str, i64)> = rows
            .iter()
            .map(|row| (row["path"].as_str().unwrap(), row["ord"].as_i64().unwrap()))
            .collect();
        assert_eq!(order, [("a.md", 0), ("a.md", 1), ("b.md", 0)]);
        assert_eq!(rows[0]["i"], 0);
        assert_eq!(rows[0]["created_at"], 42);
        assert_eq!(rows[0]["source"], "web");
        assert_eq!(rows[0]["blen"], 3);

        let npy = fs::read(out.join("vectors.npy")).unwrap();
        let hlen = u16::from_le_bytes([npy[8], npy[9]]) as usize;
        let header_end = 10 + hlen;
        assert_eq!(header_end % 64, 0);
        assert_eq!(npy.len(), header_end + 3 * 3 * 4);
        let first: Vec<f32> = npy[header_end..header_end + 12]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(first, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn export_f32_raw_bytes() {
        let (dir, cfg) = cfg_with_db("export-f32");
        let db = Db::open(&cfg.db_path()).unwrap();
        seed(&db);
        let out = dir.path().join("out");
        export(&cfg, &out, ExportFormat::F32, false, false).unwrap();
        assert_eq!(fs::read(out.join("vectors.f32")).unwrap().len(), 3 * 3 * 4);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["vectors_file"], "vectors.f32");
        assert_eq!(manifest["format"], "f32");
    }

    #[test]
    fn export_refuses_nonempty_out_without_force() {
        let (dir, cfg) = cfg_with_db("export-nonempty");
        let db = Db::open(&cfg.db_path()).unwrap();
        seed(&db);
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("keep.txt"), "x").unwrap();
        assert!(export(&cfg, &out, ExportFormat::Npy, false, false).is_err());
        export(&cfg, &out, ExportFormat::Npy, true, false).unwrap();
        assert!(out.join("vectors.npy").exists());
        assert!(
            out.join("keep.txt").exists(),
            "force replaces only export artifacts"
        );
    }

    #[test]
    fn path_resolution_rejects_relative_missing_and_dotdot_vault_destinations() {
        let (dir, cfg) = cfg_with_db("export-paths");
        fs::create_dir_all(&cfg.vault).unwrap();
        assert!(safe_output_path_with_cwd(Path::new("export"), &cfg.vault, &cfg.vault).is_err());

        let missing_vault = dir.path().join("missing-vault");
        assert!(
            safe_output_path_with_cwd(&missing_vault.join("export"), &missing_vault, dir.path())
                .is_err()
        );
        assert!(!missing_vault.exists());

        let crafted = dir.path().join("outside/new/../../vault/export");
        assert!(safe_output_path_with_cwd(&crafted, &cfg.vault, dir.path()).is_err());
    }

    #[test]
    fn export_refuses_out_inside_vault_before_creating_it() {
        let (_dir, cfg) = cfg_with_db("export-in-vault");
        let db = Db::open(&cfg.db_path()).unwrap();
        seed(&db);
        let out = cfg.vault.join("export");
        let err = export(&cfg, &out, ExportFormat::Npy, false, false).unwrap_err();
        assert!(err.to_string().contains("G1"), "err: {err}");
        assert!(
            !cfg.vault.exists(),
            "must not create even a missing vault ancestor"
        );
    }

    #[cfg(unix)]
    #[test]
    fn force_replaces_artifact_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let (dir, cfg) = cfg_with_db("export-leaf-symlink");
        let db = Db::open(&cfg.db_path()).unwrap();
        seed(&db);
        fs::create_dir_all(&cfg.vault).unwrap();
        let note = cfg.vault.join("note.md");
        fs::write(&note, "do not overwrite").unwrap();
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        symlink(&note, out.join("vectors.npy")).unwrap();

        export(&cfg, &out, ExportFormat::Npy, true, false).unwrap();
        assert_eq!(fs::read_to_string(note).unwrap(), "do not overwrite");
        assert!(
            !fs::symlink_metadata(out.join("vectors.npy"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn directory_inspection_errors_are_not_treated_as_empty() {
        let dir = TempDir::new("export-read-dir-error");
        assert!(dir_non_empty(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn failed_publish_removes_old_completion_marker() {
        let dir = TempDir::new("export-publish-failure");
        let parent = dir.path();
        let out = parent.join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("manifest.json"), "old valid manifest").unwrap();
        fs::write(out.join("vectors.npy"), "old vectors").unwrap();
        fs::write(out.join("meta.jsonl"), "old metadata").unwrap();

        let mut stage = StageDir::create(parent).unwrap();
        fs::write(stage.path.join("vectors.npy"), "new vectors").unwrap();
        // meta.jsonl is deliberately absent: publication fails after vectors move.
        assert!(publish_staged(&mut stage, &out, true, "vectors.npy").is_err());
        assert!(!out.join("manifest.json").exists());
    }

    #[test]
    fn rows_and_identity_come_from_one_sqlite_snapshot() {
        let (dir, cfg) = cfg_with_db("export-snapshot");
        let db = Db::open(&cfg.db_path()).unwrap();
        seed(&db);
        let stage = dir.path().join("stage");
        fs::create_dir(&stage).unwrap();
        let db_path = cfg.db_path();
        let first = chunk("a.md", 0, "one").id;

        let snapshot = write_staged_snapshot(&db, &stage, ExportFormat::F32, || {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            conn.execute("UPDATE meta SET v='new/model' WHERE k='embed_model'", [])
                .unwrap();
            let bytes: Vec<u8> = [9.0_f32, 9.0, 9.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect();
            conn.execute(
                "UPDATE chunks SET embedding=?1 WHERE id=?2",
                rusqlite::params![bytes, first],
            )
            .unwrap();
        })
        .unwrap();

        assert_eq!(snapshot.embed_model, "test/model");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(stage.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["embed_model"], "test/model");
        let raw = fs::read(stage.join("vectors.f32")).unwrap();
        let old_first = [1.0_f32, 2.0, 3.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(&raw[..12], old_first.as_slice());
    }
}
