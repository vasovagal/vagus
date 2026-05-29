//! vagus — local-first PARA second brain: a hybrid-search CLI over a plain-Markdown vault.
//!
//! See `design/` and `CLAUDE.md` for the hard invariants. In particular: only Markdown lives in the
//! iCloud vault; the index/DB/model-cache are a rebuildable cache outside iCloud.

mod chunk;
mod config;
mod db;
mod embed;
mod index;
mod lex;
mod notes;
mod search;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::Config;
use db::Db;
use search::Mode;

#[derive(Parser)]
#[command(
    name = "vagus",
    version,
    about = "Local-first PARA second brain: hybrid full-text + semantic search over a Markdown vault"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Incremental index: sync changed/new/removed vault notes into the index.
    Index,
    /// Wipe the derived index and rebuild it from the vault.
    Reindex,
    /// Search the vault (hybrid by default).
    Search {
        /// The query text.
        query: String,
        /// Which retriever(s) to use.
        #[arg(long, value_enum, default_value_t = Mode::Hybrid)]
        mode: Mode,
        /// Emit machine-readable JSON (stable shape for the Claude Code skill).
        #[arg(long)]
        json: bool,
        /// Max results.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Skip the automatic incremental index refresh before searching.
        #[arg(long)]
        no_index: bool,
    },
    /// Create a new note in `00-Inbox/` and index it.
    AddNote {
        /// Note title (becomes part of the filename and the `title` frontmatter).
        title: String,
        /// PARA bucket to create in (default: the inbox).
        #[arg(long, default_value = "inbox")]
        para: String,
        /// Provenance to record in frontmatter (URL or where it came from).
        #[arg(long)]
        source: Option<String>,
        /// Print only the created file's absolute path (for the skill to consume).
        #[arg(long)]
        print_path: bool,
    },
    /// List notes currently in `00-Inbox/`.
    Inbox {
        #[arg(long)]
        json: bool,
    },
    /// Move a note into a PARA folder (enriching frontmatter), or suggest destinations.
    File {
        /// Path to the note (absolute, or relative to the vault).
        path: String,
        /// Destination PARA folder, e.g. `10-Projects/Website v2`.
        #[arg(long)]
        to: Option<String>,
        /// Instead of moving, suggest destinations.
        #[arg(long)]
        suggest: bool,
        /// With --suggest, emit JSON (for the /process-inbox skill).
        #[arg(long)]
        json: bool,
    },
    /// Print a short guide to capturing, searching, and filing notes with PARA.
    Tutorial,
    /// Health check: vault symlink, model cache, dylib, dims, index openable.
    Doctor,
    /// Show index stats: counts, model/dims, paths, sizes.
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;

    match cli.command {
        Command::Status => cmd_status(&cfg)?,
        Command::Index => cmd_index(&cfg, false)?,
        Command::Reindex => cmd_index(&cfg, true)?,
        Command::Search {
            query,
            mode,
            json,
            limit,
            no_index,
        } => search::run(&cfg, &query, mode, json, limit, no_index)?,
        Command::AddNote {
            title,
            para,
            source,
            print_path,
        } => notes::add_note(&cfg, &title, &para, source.as_deref(), print_path)?,
        Command::Inbox { json } => notes::inbox(&cfg, json)?,
        Command::File {
            path,
            to,
            suggest,
            json,
        } => notes::file(&cfg, &path, to.as_deref(), suggest, json)?,
        Command::Tutorial => cmd_tutorial(&cfg),
        Command::Doctor => cmd_doctor(&cfg)?,
    }
    Ok(())
}

fn cmd_doctor(cfg: &Config) -> Result<()> {
    fn line(label: &str, ok: bool, detail: &str) {
        println!("  [{}] {label}: {detail}", if ok { "ok" } else { "!!" });
    }
    println!("vagus doctor");
    line(
        "vault",
        cfg.vault.exists(),
        &cfg.vault.display().to_string(),
    );
    line(
        "data dir",
        cfg.data_dir.exists(),
        &cfg.data_dir.display().to_string(),
    );

    let db = Db::open(&cfg.db_path())?;
    let model = db
        .meta_get("embed_model")?
        .unwrap_or_else(|| "(unset)".into());
    let dims = db
        .meta_get("embed_dims")?
        .unwrap_or_else(|| "(unset)".into());
    let id_ok = model == config::EMBED_MODEL && dims == config::EMBED_DIMS.to_string();
    line("embed identity", id_ok, &format!("{model} / {dims}"));

    line(
        "tantivy index",
        lex::Lex::open(&cfg.tantivy_dir()).is_ok(),
        &cfg.tantivy_dir().display().to_string(),
    );
    line(
        "onnx + model",
        embed::Embedder::new(&cfg.cache_dir).is_ok(),
        &cfg.cache_dir.display().to_string(),
    );

    let files = db.count("SELECT count(*) FROM files")?;
    let chunks = db.count("SELECT count(*) FROM chunks")?;
    let embedded = db.count("SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")?;
    line(
        "index counts",
        embedded == chunks,
        &format!("{files} files, {chunks} chunks, {embedded} embedded"),
    );

    // Guardrail G1: the index must not live inside the iCloud vault.
    let inside = cfg.data_dir.starts_with(&cfg.vault);
    line(
        "index outside vault",
        !inside,
        if inside {
            "INDEX IS INSIDE THE VAULT (G1 violation)"
        } else {
            "ok"
        },
    );
    Ok(())
}

fn cmd_index(cfg: &Config, reindex: bool) -> Result<()> {
    let stats = index::run(cfg, reindex)?;
    println!(
        "{}: {} new, {} changed, {} unchanged, {} removed",
        if reindex { "reindex" } else { "index" },
        stats.new,
        stats.changed,
        stats.unchanged,
        stats.removed
    );
    Ok(())
}

fn cmd_status(cfg: &Config) -> Result<()> {
    let db = Db::open(&cfg.db_path())?;
    let files = db.count("SELECT count(*) FROM files")?;
    let chunks = db.count("SELECT count(*) FROM chunks")?;
    let embedded = db.count("SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")?;
    let vault_ok = if cfg.vault.exists() { "ok" } else { "MISSING" };

    println!("vagus");
    println!("  vault       : {} [{}]", cfg.vault.display(), vault_ok);
    println!("  data dir    : {}", cfg.data_dir.display());
    println!("  model cache : {}", cfg.cache_dir.display());
    println!("  db          : {}", cfg.db_path().display());
    println!("  tantivy     : {}", cfg.tantivy_dir().display());
    println!(
        "  embed model : {} ({} dims)",
        config::EMBED_MODEL,
        config::EMBED_DIMS
    );
    println!("  files       : {files}");
    println!("  chunks      : {chunks} ({embedded} embedded)");
    println!();
    println!("New here? `vagus tutorial` walks through capture → search → file.");
    Ok(())
}

fn cmd_tutorial(cfg: &Config) {
    let vault = cfg.vault.display();
    println!(
        r#"vagus — your PARA second brain   (vault: {vault})

CAPTURE — zero ceremony:
  vim ~/brain/00-Inbox/my-idea.md     just write Markdown; no frontmatter needed
  vagus add-note "My idea"            …or let vagus create + index a note for you
  vagus index                         index anything you dropped in by hand

FIND:
  vagus search "that thing about X"   hybrid: keywords + meaning
  vagus search "..." --mode bm25      keyword-only   (--mode vec = semantic-only)

FILE into PARA — the periodic "organize" pass:
  vagus inbox                         see what's waiting in 00-Inbox
  vagus file 00-Inbox/<note>.md --suggest             where might it go?
  vagus file 00-Inbox/<note>.md --to "30-Resources/Coffee"
  (in Claude Code:  /process-inbox    proposes a home for each note)

PARA — file by how ACTIONABLE it is (first match wins):
  10-Projects   a goal with an end + deadline       ("Launch v2")
  20-Areas      an ongoing responsibility/standard  ("Health", "Finances")
  30-Resources  a topic of interest, no obligation  ("Coffee", "Rust")
  40-Archive    done / inactive — archive, never delete
  00-Inbox      staging only — process it toward empty

Notes are searchable the moment they're indexed, even before you file them."#
    );
}
