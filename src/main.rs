//! vagus — local-first PARA second brain: a hybrid-search CLI over a plain-Markdown vault.
//!
//! See `design/` and `CLAUDE.md` for the hard invariants. In particular: only Markdown lives in the
//! iCloud vault; the index/DB/model-cache are a rebuildable cache outside iCloud.

mod chunk;
mod config;
mod db;
mod index;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use config::Config;
use db::Db;

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
        /// Instead of moving, print ranked destination suggestions as JSON.
        #[arg(long)]
        suggest: bool,
    },
    /// Health check: vault symlink, model cache, dylib, dims, index openable.
    Doctor,
    /// Show index stats: counts, model/dims, paths, sizes.
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    /// BM25 + semantic, fused with RRF.
    Hybrid,
    /// Full-text (BM25) only.
    Bm25,
    /// Semantic (embeddings) only.
    Vec,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;

    match cli.command {
        Command::Status => cmd_status(&cfg)?,
        Command::Index => cmd_index(&cfg, false)?,
        Command::Reindex => cmd_index(&cfg, true)?,
        Command::Search { .. } => todo("search"),
        Command::AddNote { .. } => todo("add-note"),
        Command::Inbox { .. } => todo("inbox"),
        Command::File { .. } => todo("file"),
        Command::Doctor => todo("doctor"),
    }
    Ok(())
}

fn todo(what: &str) {
    eprintln!("vagus: `{what}` is not implemented yet");
    std::process::exit(2);
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
    println!("  embed model : {} ({} dims)", config::EMBED_MODEL, config::EMBED_DIMS);
    println!("  files       : {files}");
    println!("  chunks      : {chunks} ({embedded} embedded)");
    Ok(())
}
