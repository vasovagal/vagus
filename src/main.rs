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
mod plugin;
mod rerank;
#[cfg(feature = "generate")]
mod rewrite;
mod scope;
mod search;
mod skills;
mod util;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

use config::Config;
use db::Db;
use search::Mode;

#[derive(Parser)]
#[command(
    name = "vagus",
    version,
    about = "Local-first PARA second brain: hybrid full-text + semantic search over a Markdown vault",
    after_help = concat!(
        "Plugins: any `vagus-<name>` on your PATH runs as `vagus <name>` (see `vagus plugins`).\n",
        "Home & docs: ",
        env!("CARGO_PKG_REPOSITORY")
    )
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
    /// Compact the tantivy index (force-merge segments, drop tombstones) without re-embedding.
    Compact,
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
        /// Show full detail: full vault path, full heading breadcrumb, and the full snippet (the
        /// pre-compaction layout — no width truncation, no same-note grouping).
        #[arg(long, short = 'l')]
        verbose: bool,
        /// Show results from every context, ignoring any inherited .vagus exclusion rules.
        #[arg(long)]
        all: bool,
        /// Reorder results with the in-core cross-encoder reranker (tier-1; loads a ~150MB model on
        /// first use). Re-scores a deeper candidate pool against full chunk bodies. RRF is untouched.
        #[arg(long)]
        rerank: bool,
        /// Include each hit's full chunk body in the output (the `--json` skill path consumes this;
        /// human output prints the untruncated body). Default output is unchanged.
        #[arg(long)]
        full: bool,
        /// Drop hits scoring below this percent of the top hit (relative-to-top; mode-dependent feel).
        #[arg(long)]
        min_score: Option<f32>,
        /// Tier-1 "smart" search: a local model expands the query (lex/vec/HyDE variants), each is
        /// retrieved and fused, then reranked. Offline, no Claude. Implies --rerank. Requires the
        /// `generate` build feature (falls back to --rerank if absent).
        #[arg(long)]
        smart: bool,
        /// Keep only notes created within this window (e.g. `10d`, `2w`, `6h`, `30m`, `90s`, or a
        /// bare number of days). Uses the frontmatter `created` time, falling back to file mtime for
        /// notes without it (ADR 0017). A post-rank filter — ranking (RRF) is unchanged.
        #[arg(long, value_name = "DURATION")]
        since: Option<String>,
        /// Keep only notes whose frontmatter `source` matches (case-insensitive). Notes without a
        /// `source` are excluded when this is set (ADR 0017). A post-rank filter — RRF is unchanged.
        #[arg(long, value_name = "STR")]
        source: Option<String>,
    },
    /// Expand a query into typed lex:/vec:/hyde: variants with the local model (tier-1 rewriter).
    Rewrite {
        /// The query to expand.
        query: String,
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
        /// Open the new note in $VISUAL/$EDITOR, then re-index it.
        #[arg(long, short = 'e')]
        edit: bool,
        /// Never open an editor (even when run interactively).
        #[arg(long)]
        no_edit: bool,
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
        /// Show how a suggestion is computed (query text, search hits, folder derivation).
        #[arg(long)]
        thought_process: bool,
    },
    /// Print a short guide to capturing, searching, and filing notes with PARA.
    Tutorial,
    /// Health check: vault symlink, model cache, dylib, dims, index openable.
    Doctor,
    /// Show index stats: counts, model/dims, paths, sizes.
    Status,
    /// Manage the bundled Claude Code skills (create-note / search / process-inbox).
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// List discovered `vagus-<name>` plugins on your PATH.
    Plugins,
    /// Run an external `vagus-<name>` plugin (any subcommand that isn't builtin).
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Write the bundled skills into ~/.claude/skills (or $CLAUDE_CONFIG_DIR, or --dir).
    Install {
        /// Install into this directory instead of ~/.claude/skills.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Replace symlinks / divergent files without backing up.
        #[arg(long)]
        force: bool,
    },
    /// List the bundled skills and whether they're installed.
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;

    match cli.command {
        Command::Status => cmd_status(&cfg)?,
        Command::Index => cmd_index(&cfg, false)?,
        Command::Reindex => cmd_index(&cfg, true)?,
        Command::Compact => cmd_compact(&cfg)?,
        Command::Search {
            query,
            mode,
            json,
            limit,
            no_index,
            verbose,
            all,
            rerank,
            full,
            min_score,
            smart,
            since,
            source,
        } => search::run(
            &cfg, &query, mode, json, limit, no_index, verbose, all, full, rerank, min_score,
            smart, since.as_deref(), source.as_deref(),
        )?,
        Command::Rewrite { query } => {
            #[cfg(feature = "generate")]
            {
                rewrite::run_cli(&cfg, &query)?;
            }
            #[cfg(not(feature = "generate"))]
            {
                let _ = query;
                anyhow::bail!(
                    "this build has no local rewriter (compiled without the `generate` feature)"
                );
            }
        }
        Command::AddNote {
            title,
            para,
            source,
            print_path,
            edit,
            no_edit,
        } => notes::add_note(
            &cfg,
            &title,
            &para,
            source.as_deref(),
            print_path,
            edit,
            no_edit,
        )?,
        Command::Inbox { json } => notes::inbox(&cfg, json)?,
        Command::File {
            path,
            to,
            suggest,
            json,
            thought_process,
        } => notes::file(&cfg, &path, to.as_deref(), suggest, json, thought_process)?,
        Command::Tutorial => cmd_tutorial(&cfg),
        Command::Doctor => cmd_doctor(&cfg)?,
        Command::Skills { action } => match action {
            SkillsAction::Install { dir, force } => skills::install(dir, force)?,
            SkillsAction::List => skills::list()?,
        },
        Command::Plugins => {
            let builtins: Vec<String> = Cli::command()
                .get_subcommands()
                .map(|c| c.get_name().to_string())
                .collect();
            plugin::list(&builtins)?;
        }
        Command::External(argv) => plugin::dispatch(&cfg, &argv)?,
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

    let seg = lex::Lex::open(&cfg.tantivy_dir()).and_then(|l| l.segment_stats());
    let seg_detail = match &seg {
        Ok(s) => format!(
            "{} ({} segments, {} docs, {} deleted)",
            cfg.tantivy_dir().display(),
            s.segments,
            s.docs,
            s.deleted
        ),
        Err(_) => cfg.tantivy_dir().display().to_string(),
    };
    line("tantivy index", seg.is_ok(), &seg_detail);
    line(
        "onnx + model",
        embed::Embedder::new(&cfg.cache_dir).is_ok(),
        &cfg.cache_dir.display().to_string(),
    );
    // The cross-encoder reranker is opt-in (tier-1, `--rerank`); report whether its model is already
    // cached WITHOUT instantiating it (that would force the ~150MB download). `ok` is informational.
    let rerank_cached = std::fs::read_dir(&cfg.cache_dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_lowercase();
                n.contains("rerank") || n.contains("jina")
            })
        })
        .unwrap_or(false);
    line(
        "reranker (opt-in)",
        true,
        &format!(
            "jina-reranker-v1-turbo-en — {}",
            if rerank_cached {
                "cached"
            } else {
                "downloads on first --rerank"
            }
        ),
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

    // Fragmentation hint: per-file commits accumulate segments + tombstones over time.
    if let Ok(s) = &seg
        && (s.segments >= 8 || (s.docs > 0 && s.deleted >= s.docs))
    {
        println!(
            "\n  fragmented: {} segments, {} deleted docs — run `vagus compact`.",
            s.segments, s.deleted
        );
    }

    // Disk usage of the derived index (~/.local/share/vagus), by file type.
    println!("\nindex size ({}):", cfg.data_dir.display());
    let sizes = dir_size_by_ext(&cfg.data_dir);
    let (mut total_n, mut total_b) = (0u64, 0u64);
    for (ext, (n, b)) in &sizes {
        println!("  {ext:<10} {n:>4} file(s)  {:>10}", human_size(*b));
        total_n += n;
        total_b += b;
    }
    println!(
        "  {:<10} {total_n:>4} file(s)  {:>10}",
        "total",
        human_size(total_b)
    );
    Ok(())
}

/// Total file count + bytes per file extension under `root` (recursive).
fn dir_size_by_ext(root: &Path) -> BTreeMap<String, (u64, u64)> {
    let mut map: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for e in walkdir::WalkDir::new(root).into_iter().flatten() {
        if e.file_type().is_file() {
            let key = e
                .path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| format!(".{x}"))
                .unwrap_or_else(|| "(no ext)".to_string());
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            let entry = map.entry(key).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += size;
        }
    }
    map
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut b = bytes as f64;
    let mut i = 0;
    while b >= 1024.0 && i < UNITS.len() - 1 {
        b /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{b:.1} {}", UNITS[i])
    }
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

fn cmd_compact(cfg: &Config) -> Result<()> {
    let before = lex::Lex::open(&cfg.tantivy_dir())?.segment_stats()?;
    lex::Lex::open(&cfg.tantivy_dir())?.compact()?;
    let after = lex::Lex::open(&cfg.tantivy_dir())?.segment_stats()?;
    println!(
        "compacted: {} → {} segments, {} → {} deleted docs",
        before.segments, after.segments, before.deleted, after.deleted
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
  vagus add-note "My idea"            create the note + open it in $EDITOR, then index
  vagus index                         index anything you dropped in by hand

FIND:
  vagus search "that thing about X"   hybrid: keywords + meaning
  vagus search "..." --mode bm25      keyword-only   (--mode vec = semantic-only)
  vagus search "..." --rerank         sharper ordering via a local cross-encoder (no cloud)
  vagus search "..." --smart          local query expansion + HyDE + rerank (offline, no Claude)

FILE into PARA — the periodic "organize" pass:
  vagus inbox                         see what's waiting in 00-Inbox
  vagus file 00-Inbox/<note>.md --suggest             where might it go? (--thought-process = why)
  vagus file 00-Inbox/<note>.md --to "30-Resources/Coffee"
  (in Claude Code:  /process-inbox    proposes a home for each note)

PARA — file by how ACTIONABLE it is (first match wins):
  10-Projects   a goal with an end + deadline       ("Launch v2")
  20-Areas      an ongoing responsibility/standard  ("Health", "Finances")
  30-Resources  a topic of interest, no obligation  ("Coffee", "Rust")
  40-Archive    done / inactive — archive, never delete
  00-Inbox      staging only — process it toward empty

Notes are searchable the moment they're indexed, even before you file them.

Claude Code skills (/create-note · /search · /process-inbox):  vagus skills install"#
    );
}
