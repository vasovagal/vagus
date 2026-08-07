//! vagus — local-first PARA second brain: a hybrid-search CLI over a plain-Markdown vault.
//!
//! See `design/` and `CLAUDE.md` for the hard invariants. In particular: only Markdown lives in the
//! iCloud vault; the index/DB/model-cache live outside iCloud and are a rebuildable cache — except
//! the `ticks` counters and explicit `tick_runs`/`tick_events` presentation provenance in meta.db,
//! which are local user data (ADR 0021/G25).

mod chunk;
mod config;
mod db;
mod embed;
mod eval;
mod export;
mod index;
mod init;
mod lex;
mod notes;
mod path_safety;
mod plugin;
mod provenance;
mod relevance;
mod rerank;
#[cfg(feature = "generate")]
mod rewrite;
mod scope;
mod search;
mod skills;
mod ticks;
mod util;
mod vector;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};

use config::Config;
use db::Db;
use search::Mode;

/// clap parser for an explicit bounded relevance floor. `NaN` and infinities are rejected alongside
/// ordinary out-of-range values so a malformed threshold cannot silently retain/drop everything.
fn unit_interval(raw: &str) -> std::result::Result<f32, String> {
    let value: f32 = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a number"))?;
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(format!("must be a finite number in 0.0..=1.0 (got {raw})"))
    }
}

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
    /// Rebuild the whole derived index, or force-refresh a recent mtime window.
    Reindex {
        /// Force-reindex notes whose filesystem mtime is within this window (e.g. `10d`, `2w`,
        /// `6h`). The whole vault is snapshotted for new/deleted files, but older indexed notes are
        /// preserved instead of being re-embedded (ADR 0022).
        #[arg(long, value_name = "DURATION")]
        since: Option<String>,
    },
    /// Compact the tantivy index (force-merge segments, drop tombstones) without re-embedding.
    Compact,
    /// Search the vault (hybrid by default).
    Search {
        /// The query text.
        query: String,
        /// Which retriever(s) to use.
        #[arg(long, value_enum, default_value_t = Mode::Hybrid)]
        mode: Mode,
        /// Emit machine-readable JSON (stable shape unless explicit --tick-provenance wraps it).
        #[arg(long)]
        json: bool,
        /// Emit a versioned `{run,hits}` wrapper with truthful fused/source/rerank/final ranks for
        /// presentation ticks. Requires the exact+reranked full-body tier-2 pipeline (ADR 0021/G25).
        #[arg(long)]
        tick_provenance: bool,
        /// Max results: distinct notes by default; individual chunks with --chunks.
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
        /// Give the reranker up to N adjacent chunks on each side of the matched chunk (0-2).
        /// Search results still return only the matched chunk body. Requires --rerank or --smart.
        #[arg(long, default_value_t = 0, value_parser = rerank::parse_context_radius)]
        rerank_context: usize,
        /// Include each hit's full chunk body in the output (the `--json` skill path consumes this;
        /// human output prints the untruncated body). Default output is unchanged.
        #[arg(long)]
        full: bool,
        /// Drop hits scoring below this percent of the top hit (relative-to-top; mode-dependent feel).
        #[arg(long)]
        min_score: Option<f32>,
        /// Show each hit's bounded semantic relevance: finite EmbeddingGemma cosine clamped to
        /// [0,1]. A heuristic, not a probability; JSON names the policy. Opt-in so default
        /// human/JSON output stays stable. Unsupported with --mode bm25 or --smart, which has no
        /// retained original-query cosine.
        #[arg(long)]
        relevance: bool,
        /// Drop hits below this bounded semantic relevance floor (0.0..=1.0); implies --relevance.
        /// A positive floor drops hits without an original-query cosine and disables adaptive tidy.
        #[arg(long, value_parser = unit_interval)]
        min_relevance: Option<f32>,
        /// Tier-1 "smart" search: a local model expands the query (lex/vec/HyDE variants), each is
        /// retrieved and fused, then reranked. Offline, no coding agent. Implies --rerank. Requires the
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
        /// Print a per-stage timing breakdown to stderr (rewrite/embed/rerank load + compute, fuse,
        /// total). Diagnostic for `--smart`/`--rerank`; stdout and the `--json` shape are unchanged.
        #[arg(long)]
        timings: bool,
        /// Force exact brute-force semantic search in every mode instead of usearch HNSW (ADR 0019).
        /// Exact is already automatic below 10k embedded chunks; this is the large-corpus oracle.
        #[arg(long)]
        exact: bool,
        /// Return individual chunk hits instead of one best-chunk hit per note; --limit then counts
        /// chunks (the pre-0.7 behavior — ADR 0020).
        #[arg(long)]
        chunks: bool,
        /// Fill up to --limit by disabling the adaptive low-signal RRF-tail cutoff. Only affects
        /// plain hybrid note results; ranking and scores are unchanged (ADR 0023).
        #[arg(long)]
        exhaustive: bool,
    },
    /// Score the fixed current index against vault-specific JSONL relevance judgments (ADR 0024).
    Eval {
        /// JSONL labels: one query and its relevant vault-relative note paths per line.
        labels: PathBuf,
        /// Requested result depth. Metrics are P@k, R@k, RR@k/MRR@k, and optional nDCG@k.
        #[arg(long, default_value = "10", value_parser = eval::parse_positive_k)]
        k: usize,
        /// Which retriever(s) to evaluate.
        #[arg(long, value_enum, default_value_t = Mode::Hybrid)]
        mode: Mode,
        /// Evaluate the cross-encoder ordering (its capped-prefix policy is recorded in provenance).
        #[arg(long)]
        rerank: bool,
        /// Give the evaluated reranker up to N adjacent chunks on each side (0-2). Requires --rerank.
        #[arg(long, default_value_t = 0, value_parser = rerank::parse_context_radius)]
        rerank_context: usize,
        /// Force the exact cosine oracle; otherwise the normal scale-selected backend is recorded.
        #[arg(long)]
        exact: bool,
        /// Report the top hit's bounded semantic relevance diagnostic instead of its mode score.
        /// Unsupported with --mode bm25; ranking and metrics are unchanged (ADR 0026).
        #[arg(long)]
        relevance: bool,
        /// Emit stable schema-versioned JSON rather than the human table.
        #[arg(long)]
        json: bool,
    },
    /// Apply ADR 0025's fixed statistical gate to baseline/candidate eval JSON reports.
    EvalGate {
        /// Baseline report from unmodified RRF k=60 on the frozen held-out qrels/index.
        baseline: PathBuf,
        /// Candidate report from the alternate-fusion binary on the identical qrels/index/config.
        candidate: PathBuf,
        /// Emit a stable gate report as JSON. Rejected candidates still exit nonzero.
        #[arg(long)]
        json: bool,
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
        /// Additional producer metadata as a JSON object. Top-level keys become safe YAML frontmatter;
        /// Vagus-owned keys are rejected. Intended for integrations such as Corti.
        #[arg(long, value_name = "OBJECT")]
        frontmatter_json: Option<String>,
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
        /// With --suggest, emit JSON (for the /process-inbox skill). With --stats, emit the
        /// per-step timing breakdown as one stable JSON object instead of the table.
        #[arg(long)]
        json: bool,
        /// Show how a suggestion is computed (query text, search hits, folder derivation).
        #[arg(long)]
        thought_process: bool,
        /// After filing, print a per-step timing breakdown (enrich/move/index sub-steps + total).
        #[arg(long)]
        stats: bool,
    },
    /// Create or complete the vault's PARA folder layout (idempotent and fail-closed).
    Init {
        /// Use the fixed iCloud Drive Brain directory and symlink the configured vault path to it.
        #[arg(long)]
        icloud: bool,
    },
    /// Print a short guide to capturing, searching, and filing notes with PARA.
    Tutorial,
    /// Health check: vault, storage separation, model-cache presence, identity, and indexes.
    Doctor {
        /// Explicitly download and validate both ONNX models now. Plain doctor never downloads.
        #[arg(long)]
        fetch_models: bool,
    },
    /// Show index stats: counts, model/dims, paths, sizes.
    Status,
    /// Manage the bundled Claude Code / pi skills (create-note / search / process-inbox).
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Export the embedding matrix for offline analysis without loading a model or changing vectors.
    Vectors {
        #[command(subcommand)]
        action: VectorsAction,
    },
    /// List discovered `vagus-<name>` plugins on your PATH.
    Plugins,
    /// Record a usage tick for one or more notes (used by the /search skill after presenting results).
    Tick {
        /// Vault-relative note paths (as printed in search hits); absolute paths inside the vault are
        /// accepted. Optional when --events carries the presented paths and rank provenance.
        #[arg(num_args(0..))]
        paths: Vec<String>,
        /// Versioned JSON `{run,events}` copied from an explicit search --tick-provenance response.
        /// Counter, run, and event rows commit atomically (ADR 0021/G25).
        #[arg(long, value_name = "JSON")]
        events: Option<String>,
        /// Persist the explicit --query alongside the run. Off by default because query text is user
        /// content; rank/config provenance itself contains no query or body text.
        #[arg(long)]
        store_query: bool,
        /// Query text to persist; valid only with --store-query and --events.
        #[arg(long, value_name = "TEXT")]
        query: Option<String>,
        /// Emit the new totals as stable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Hall of fame: the most-used notes by usage ticks.
    Fame {
        /// Max notes to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Include ticked notes no longer in the index (deleted or renamed outside vagus).
        #[arg(long)]
        all: bool,
        /// Emit stable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Presentation-rank diagnostics grouped by exact pipeline and corpus (selection-biased; ADR 0021).
    Ticks {
        /// Max distinct ticked notes to include (a note can have several pipeline/corpus rows).
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Include ticked notes no longer in the index (deleted or renamed outside vagus).
        #[arg(long)]
        all: bool,
        /// Emit a stable schema-versioned report.
        #[arg(long)]
        json: bool,
    },
    /// Run an external `vagus-<name>` plugin (any subcommand that isn't builtin).
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Write the bundled skills into the selected agent's global skills directory.
    Install {
        /// Agent to install for (claude: ~/.claude/skills; pi: ~/.pi/agent/skills).
        #[arg(long, value_enum, default_value = "claude")]
        agent: skills::Agent,
        /// Install into this directory instead of the selected agent's default.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Replace symlinks / divergent files without backing up.
        #[arg(long)]
        force: bool,
    },
    /// List the bundled skills and whether they're installed for the selected agent.
    List {
        /// Agent whose global skills directory to inspect.
        #[arg(long, value_enum, default_value = "claude")]
        agent: skills::Agent,
    },
}

#[derive(Subcommand)]
enum VectorsAction {
    /// Dump every embedded chunk's vector + metadata into DIR (vectors.(npy|f32) + meta.jsonl +
    /// manifest.json), in deterministic (path, ord) order. Uses one coherent DB snapshot.
    Export {
        /// Output directory (created; must not be inside your vault or be a symlink). Refuses a
        /// non-empty directory unless --force; the manifest is published last.
        #[arg(long)]
        out: PathBuf,
        /// Matrix format: `npy` (NumPy v1.0, C-order f32) or `f32` (raw little-endian f32).
        #[arg(long, value_enum, default_value_t = export::ExportFormat::Npy)]
        format: export::ExportFormat,
        /// Overwrite a non-empty --out directory.
        #[arg(long)]
        force: bool,
        /// Emit a stable one-object JSON summary instead of the human summary.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Pure report comparison: no vault/config/model/index is needed or touched.
    if let Command::EvalGate {
        baseline,
        candidate,
        json,
    } = &cli.command
    {
        return eval::run_gate(baseline, candidate, *json);
    }
    let cfg = Config::load()?;
    // Every command that may open/create meta.db or a model cache enforces G1 first. Doctor performs
    // the same validation itself so it can print a diagnostic instead of failing before its report.
    if !matches!(&cli.command, Command::Doctor { .. }) {
        cfg.validate_storage_separation()?;
    }

    match cli.command {
        Command::Status => cmd_status(&cfg)?,
        Command::Index => cmd_index(&cfg)?,
        Command::Reindex { since } => cmd_reindex(&cfg, since.as_deref())?,
        Command::Compact => cmd_compact(&cfg)?,
        Command::Search {
            query,
            mode,
            json,
            tick_provenance,
            limit,
            no_index,
            verbose,
            all,
            rerank,
            rerank_context,
            full,
            min_score,
            relevance,
            min_relevance,
            smart,
            since,
            source,
            timings,
            exact,
            chunks,
            exhaustive,
        } => search::run(
            &cfg,
            &query,
            mode,
            json,
            tick_provenance,
            limit,
            no_index,
            verbose,
            all,
            full,
            rerank,
            rerank_context,
            min_score,
            relevance,
            min_relevance,
            smart,
            since.as_deref(),
            source.as_deref(),
            exact,
            timings,
            chunks,
            exhaustive,
        )?,
        Command::Eval {
            labels,
            k,
            mode,
            rerank,
            rerank_context,
            exact,
            relevance,
            json,
        } => eval::run(
            &cfg,
            &labels,
            k,
            mode,
            rerank,
            rerank_context,
            exact,
            relevance,
            json,
        )?,
        Command::EvalGate { .. } => unreachable!("eval-gate returned before loading config"),
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
            frontmatter_json,
            print_path,
            edit,
            no_edit,
        } => notes::add_note(
            &cfg,
            &title,
            &para,
            source.as_deref(),
            frontmatter_json.as_deref(),
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
            stats,
        } => notes::file(
            &cfg,
            &path,
            to.as_deref(),
            suggest,
            json,
            thought_process,
            stats,
        )?,
        Command::Init { icloud } => init::run(&cfg, icloud)?,
        Command::Tutorial => cmd_tutorial(&cfg),
        Command::Doctor { fetch_models } => cmd_doctor(&cfg, fetch_models)?,
        Command::Skills { action } => match action {
            SkillsAction::Install { agent, dir, force } => skills::install(agent, dir, force)?,
            SkillsAction::List { agent } => skills::list(agent)?,
        },
        Command::Vectors { action } => match action {
            VectorsAction::Export {
                out,
                format,
                force,
                json,
            } => export::export(&cfg, &out, format, force, json)?,
        },
        Command::Plugins => {
            let builtins: Vec<String> = Cli::command()
                .get_subcommands()
                .map(|c| c.get_name().to_string())
                .collect();
            plugin::list(&builtins)?;
        }
        Command::Tick {
            paths,
            events,
            store_query,
            query,
            json,
        } => ticks::tick(
            &cfg,
            &paths,
            events.as_deref(),
            store_query,
            query.as_deref(),
            json,
        )?,
        Command::Fame { limit, all, json } => ticks::fame(&cfg, limit, all, json)?,
        Command::Ticks { limit, all, json } => ticks::ticks_report(&cfg, limit, all, json)?,
        Command::External(argv) => plugin::dispatch(&cfg, &argv)?,
    }
    Ok(())
}

const EMBED_CACHE_REPO: &str = "models--onnx-community--embeddinggemma-300m-ONNX";
const EMBED_CACHE_FILES: &[&str] = &[
    "config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
    "onnx/model.onnx",
    "onnx/model.onnx_data",
];
const RERANK_CACHE_REPO: &str = "models--jinaai--jina-reranker-v1-turbo-en";
const RERANK_CACHE_FILES: &[&str] = &[
    "config.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
    "onnx/model.onnx",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelCacheState {
    Missing,
    Partial,
    Complete,
}

fn cmd_doctor(cfg: &Config, fetch_models: bool) -> Result<()> {
    fn line(label: &str, ok: bool, detail: &str) {
        println!("  [{}] {label}: {detail}", if ok { "ok" } else { "!!" });
    }

    println!("vagus doctor");
    let (vault_ok, vault_detail) = vault_health(&cfg.vault);
    line("vault", vault_ok, &vault_detail);

    let data_safe = storage_path_health(&cfg.data_dir, &cfg.vault);
    let cache_safe = storage_path_health(&cfg.cache_dir, &cfg.vault);
    line(
        "index outside vault",
        data_safe.is_ok(),
        &data_safe
            .as_ref()
            .map(|_| cfg.data_dir.display().to_string())
            .unwrap_or_else(|e| e.to_string()),
    );
    line(
        "model cache outside vault",
        cache_safe.is_ok(),
        &cache_safe
            .as_ref()
            .map(|_| cfg.cache_dir.display().to_string())
            .unwrap_or_else(|e| e.to_string()),
    );
    if let Err(e) = data_safe {
        bail!("refusing to open derived data at an unsafe path: {e}")
    }

    let db = Db::open(&cfg.db_path())?;
    let model = db
        .meta_get("embed_model")?
        .unwrap_or_else(|| "(unset)".into());
    let dims = db
        .meta_get("embed_dims")?
        .unwrap_or_else(|| "(unset)".into());
    let id_ok = model == config::EMBED_MODEL && dims == config::EMBED_DIMS.to_string();
    line("embed identity", id_ok, &format!("{model} / {dims}"));

    let seg = lex::Lex::open(&cfg.tantivy_dir()).and_then(|lex| lex.segment_stats());
    let seg_detail = match &seg {
        Ok(stats) => format!(
            "{} ({} segments, {} docs, {} deleted)",
            cfg.tantivy_dir().display(),
            stats.segments,
            stats.docs,
            stats.deleted
        ),
        Err(e) => format!("{} ({e})", cfg.tantivy_dir().display()),
    };
    line("tantivy index", seg.is_ok(), &seg_detail);

    let mut fetch_errors = Vec::new();
    if fetch_models {
        if let Err(e) = cache_safe {
            fetch_errors.push(format!("unsafe model-cache path: {e}"));
            line(
                "embedder model",
                false,
                "not fetched: model cache is inside the vault",
            );
            line(
                "reranker model",
                false,
                "not fetched: model cache is inside the vault",
            );
        } else {
            println!(
                "  fetching and validating models in {} (explicit network operation) …",
                cfg.cache_dir.display()
            );
            match validate_embedder(&cfg.cache_dir) {
                Ok(detail) => line("embedder model", true, &detail),
                Err(e) => {
                    line("embedder model", false, &e.to_string());
                    fetch_errors.push(format!("embedder: {e}"));
                }
            }
            match validate_reranker(&cfg.cache_dir) {
                Ok(detail) => line("reranker model", true, &detail),
                Err(e) => {
                    line("reranker model", false, &e.to_string());
                    fetch_errors.push(format!("reranker: {e}"));
                }
            }
        }
    } else {
        // Presence-only checks are deliberately pure filesystem reads. Never call fastembed model
        // constructors here: even a partial cache can make those constructors access the network.
        report_cache_state(
            "embedder model",
            &cfg.cache_dir,
            EMBED_CACHE_REPO,
            EMBED_CACHE_FILES,
            "~1.2 GB; downloads on first index/search",
            &line,
        );
        report_cache_state(
            "reranker model",
            &cfg.cache_dir,
            RERANK_CACHE_REPO,
            RERANK_CACHE_FILES,
            "~150 MB; downloads on first --rerank",
            &line,
        );
    }

    let files = db.count("SELECT count(*) FROM files")?;
    let chunks = db.count("SELECT count(*) FROM chunks")?;
    let embedded = db.count("SELECT count(*) FROM chunks WHERE embedding IS NOT NULL")?;
    line(
        "index counts",
        embedded == chunks,
        &format!("{files} files, {chunks} chunks, {embedded} embedded"),
    );
    line(
        "ticks",
        true,
        &format!(
            "{} orphaned counter path(s), {} orphaned event(s) (notes moved/deleted outside vagus)",
            db.orphan_tick_count()?,
            db.orphan_tick_event_count()?
        ),
    );

    let vector_path = cfg.vector_path();
    let (vector_ok, vector_detail) = if vector_path.exists() {
        match vector::UsearchIndex::view(&vector_path, config::EMBED_DIMS) {
            Ok(index) => {
                let count = vector::VectorIndex::len(&index);
                (
                    count as i64 == embedded,
                    format!(
                        "{} ({count} vectors, {embedded} embedded)",
                        vector_path.display()
                    ),
                )
            }
            Err(e) => (
                false,
                format!("{} (open failed: {e})", vector_path.display()),
            ),
        }
    } else {
        (
            embedded == 0,
            format!(
                "{} (missing — rebuilds on next `vagus index`)",
                vector_path.display()
            ),
        )
    };
    line("vector index (usearch)", vector_ok, &vector_detail);

    if let Ok(stats) = &seg
        && (stats.segments >= 8 || (stats.docs > 0 && stats.deleted >= stats.docs))
    {
        println!(
            "\n  fragmented: {} segments, {} deleted docs — run `vagus compact`.",
            stats.segments, stats.deleted
        );
    }

    println!("\nindex size ({}):", cfg.data_dir.display());
    let sizes = dir_size_by_ext(&cfg.data_dir);
    let (mut total_files, mut total_bytes) = (0_u64, 0_u64);
    for (ext, (count, bytes)) in &sizes {
        println!("  {ext:<10} {count:>4} file(s)  {:>10}", human_size(*bytes));
        total_files += count;
        total_bytes += bytes;
    }
    println!(
        "  {:<10} {total_files:>4} file(s)  {:>10}",
        "total",
        human_size(total_bytes)
    );

    if !fetch_errors.is_empty() {
        bail!(
            "model fetch/validation failed:\n  - {}",
            fetch_errors.join("\n  - ")
        )
    }
    Ok(())
}

fn vault_health(vault: &Path) -> (bool, String) {
    match std::fs::symlink_metadata(vault) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            false,
            format!(
                "{} — missing; run `vagus init --icloud` (or plain `vagus init`)",
                vault.display()
            ),
        ),
        Err(e) => (false, format!("{} ({e})", vault.display())),
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = std::fs::read_link(vault)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|e| format!("unreadable target: {e}"));
            if vault.is_dir() {
                (true, format!("{} → {target}", vault.display()))
            } else {
                (
                    false,
                    format!("{} → {target} (broken or not a directory)", vault.display()),
                )
            }
        }
        Ok(meta) if !meta.is_dir() => (false, format!("{} is not a directory", vault.display())),
        Ok(_) => {
            let canonical = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
            if cfg!(target_os = "macos") && !init::is_icloud_path(&canonical) {
                (
                    true,
                    format!(
                        "{} (local only; `vagus init --icloud` enables device sync)",
                        vault.display()
                    ),
                )
            } else {
                (true, vault.display().to_string())
            }
        }
    }
}

fn storage_path_health(path: &Path, vault: &Path) -> Result<()> {
    if crate::path_safety::at_or_within(path, vault)? {
        bail!(
            "{} resolves inside vault {} (G1 violation)",
            path.display(),
            vault.display()
        )
    }
    Ok(())
}

fn model_cache_state(cache_dir: &Path, repo: &str, required: &[&str]) -> Result<ModelCacheState> {
    let repo_dir = cache_dir.join(repo);
    match std::fs::symlink_metadata(&repo_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ModelCacheState::Missing),
        Err(e) => return Err(e.into()),
        Ok(meta) if !meta.is_dir() => return Ok(ModelCacheState::Partial),
        Ok(_) => {}
    }
    let snapshots = repo_dir.join("snapshots");
    let entries = match std::fs::read_dir(&snapshots) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ModelCacheState::Partial),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let mut complete = true;
        for relative in required {
            match std::fs::metadata(entry.path().join(relative)) {
                Ok(meta) if meta.is_file() && meta.len() > 0 => {}
                Ok(_) => complete = false,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => complete = false,
                Err(e) => return Err(e.into()),
            }
        }
        if complete {
            return Ok(ModelCacheState::Complete);
        }
    }
    Ok(ModelCacheState::Partial)
}

fn report_cache_state(
    label: &str,
    cache_dir: &Path,
    repo: &str,
    required: &[&str],
    missing_detail: &str,
    line: &impl Fn(&str, bool, &str),
) {
    match model_cache_state(cache_dir, repo, required) {
        Ok(ModelCacheState::Complete) => line(
            label,
            true,
            "complete on disk (presence-only; plain doctor never loads/downloads models)",
        ),
        Ok(ModelCacheState::Missing) => line(
            label,
            true,
            &format!("not downloaded ({missing_detail}; use `vagus doctor --fetch-models`)"),
        ),
        Ok(ModelCacheState::Partial) => line(
            label,
            false,
            "partial cache; use `vagus doctor --fetch-models` to repair and validate it",
        ),
        Err(e) => line(label, false, &format!("cache inspection failed: {e}")),
    }
}

fn validate_embedder(cache_dir: &Path) -> Result<String> {
    let mut embedder = embed::Embedder::new(cache_dir)?;
    let vector = embedder.embed_query("vagus doctor local model validation")?;
    if vector.len() != config::EMBED_DIMS || vector.iter().any(|value| !value.is_finite()) {
        bail!(
            "embedder returned {} values; expected {} finite values",
            vector.len(),
            config::EMBED_DIMS
        )
    }
    if model_cache_state(cache_dir, EMBED_CACHE_REPO, EMBED_CACHE_FILES)?
        != ModelCacheState::Complete
    {
        bail!("embedder loaded but its on-disk cache is incomplete")
    }
    Ok(format!(
        "{} — downloaded, loaded, and verified at {} dims",
        config::EMBED_MODEL,
        vector.len()
    ))
}

fn validate_reranker(cache_dir: &Path) -> Result<String> {
    let mut reranker = rerank::Reranker::new(cache_dir, 0)?;
    let scores = reranker.rerank(
        "vagus doctor local model validation",
        &["local model validation".to_string()],
    )?;
    if scores.len() != 1 || !scores[0].1.is_finite() {
        bail!("reranker did not return one finite validation score")
    }
    if model_cache_state(cache_dir, RERANK_CACHE_REPO, RERANK_CACHE_FILES)?
        != ModelCacheState::Complete
    {
        bail!("reranker loaded but its on-disk cache is incomplete")
    }
    Ok("jina-reranker-v1-turbo-en — downloaded, loaded, and verified".into())
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

fn cmd_index(cfg: &Config) -> Result<()> {
    let stats = index::run(cfg, index::IndexMode::Incremental)?;
    println!(
        "index: {} new, {} changed, {} repaired, {} unchanged, {} removed",
        stats.new, stats.changed, stats.refreshed, stats.unchanged, stats.removed
    );
    Ok(())
}

fn cmd_reindex(cfg: &Config, since: Option<&str>) -> Result<()> {
    let Some(spec) = since else {
        let stats = index::run(cfg, index::IndexMode::Full)?;
        println!(
            "reindex: {} new, {} changed, {} unchanged, {} removed",
            stats.new, stats.changed, stats.unchanged, stats.removed
        );
        return Ok(());
    };

    let cutoff = util::since_cutoff(spec)?;
    let stats = index::run(cfg, index::IndexMode::Since { cutoff })?;
    if stats.full_reindex {
        // A chunk-format mismatch cannot be repaired partially (G4), so the existing auto-reindex
        // upgrades the requested window to the mandatory full rebuild and says so on stderr.
        println!(
            "reindex (full): {} new, {} changed, {} unchanged, {} removed",
            stats.new, stats.changed, stats.unchanged, stats.removed
        );
    } else {
        println!(
            "reindex --since {spec}: {} selected of {}, {} refreshed, {} new, {} changed, {} unchanged, {} removed",
            stats.selected,
            stats.scanned,
            stats.refreshed,
            stats.new,
            stats.changed,
            stats.unchanged,
            stats.removed
        );
    }
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
    let vpath = cfg.vector_path();
    let vsize = std::fs::metadata(&vpath)
        .map(|m| human_size(m.len()))
        .unwrap_or_else(|_| "missing".into());
    println!("  vectors     : {} [{}]", vpath.display(), vsize);
    println!(
        "  embed model : {} ({} dims)",
        config::EMBED_MODEL,
        config::EMBED_DIMS
    );
    println!("  files       : {files}");
    println!("  chunks      : {chunks} ({embedded} embedded)");
    let tick_notes = db.count("SELECT count(*) FROM ticks")?;
    let tick_total = db.count("SELECT COALESCE(SUM(count),0) FROM ticks")?;
    let tick_runs = db.count("SELECT count(*) FROM tick_runs")?;
    let tick_events = db.count("SELECT count(*) FROM tick_events")?;
    println!(
        "  ticks       : {tick_notes} notes / {tick_total} total / {tick_runs} provenance runs / {tick_events} events"
    );
    println!();
    println!("New here? `vagus tutorial` walks through capture → search → file.");
    Ok(())
}

fn cmd_tutorial(cfg: &Config) {
    let vault = cfg.vault.display();
    println!(
        r#"vagus — your PARA second brain   (vault: {vault})

SETUP — once:
  vagus init                          create the vault with the PARA folders
  vagus init --icloud                 use iCloud Drive and symlink the friendly vault path
  vagus doctor                        network-free health check (--fetch-models is explicit)

CAPTURE — zero ceremony:
  vim ~/brain/00-Inbox/my-idea.md     just write Markdown; no frontmatter needed
  vagus add-note "My idea"            create the note + open it in $EDITOR, then index
  vagus index                         index anything you dropped in by hand

FIND:
  vagus search "that thing about X"   hybrid: keywords + meaning
  vagus search "..." --mode bm25      keyword-only   (--mode vec = semantic-only)
  vagus search "..." --rerank         sharper ordering via a local cross-encoder (no cloud)
  vagus search "..." --smart          local query expansion + HyDE + rerank (offline, no agent)

FILE into PARA — the periodic "organize" pass:
  vagus inbox                         see what's waiting in 00-Inbox
  vagus file 00-Inbox/<note>.md --suggest             where might it go? (--thought-process = why)
  vagus file 00-Inbox/<note>.md --to "30-Resources/Coffee"
  (agent skill: /process-inbox in Claude Code; /skill:process-inbox in pi)

PARA — file by how ACTIONABLE it is (first match wins):
  10-Projects   a goal with an end + deadline       ("Launch v2")
  20-Areas      an ongoing responsibility/standard  ("Health", "Finances")
  30-Resources  a topic of interest, no obligation  ("Coffee", "Rust")
  40-Archive    done / inactive — archive, never delete
  00-Inbox      staging only — process it toward empty

Notes are searchable the moment they're indexed, even before you file them.

Agent skills (Claude Code / pi):  vagus skills install [--agent claude|pi]"#
    );
}

#[cfg(test)]
mod doctor_tests {
    use super::*;
    use crate::util::testdir::TempDir;

    fn cache_snapshot(root: &Path, repo: &str, files: &[&str]) {
        let snapshot = root.join(repo).join("snapshots/revision");
        for relative in files {
            let path = snapshot.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"present").unwrap();
        }
    }

    #[test]
    fn cache_state_distinguishes_missing_partial_and_complete() {
        let dir = TempDir::new("doctor-model-cache");
        assert_eq!(
            model_cache_state(dir.path(), EMBED_CACHE_REPO, EMBED_CACHE_FILES).unwrap(),
            ModelCacheState::Missing
        );
        std::fs::create_dir_all(dir.path().join(EMBED_CACHE_REPO)).unwrap();
        assert_eq!(
            model_cache_state(dir.path(), EMBED_CACHE_REPO, EMBED_CACHE_FILES).unwrap(),
            ModelCacheState::Partial
        );
        cache_snapshot(dir.path(), EMBED_CACHE_REPO, EMBED_CACHE_FILES);
        assert_eq!(
            model_cache_state(dir.path(), EMBED_CACHE_REPO, EMBED_CACHE_FILES).unwrap(),
            ModelCacheState::Complete
        );
        std::fs::write(
            dir.path()
                .join(EMBED_CACHE_REPO)
                .join("snapshots/revision/onnx/model.onnx"),
            [],
        )
        .unwrap();
        assert_eq!(
            model_cache_state(dir.path(), EMBED_CACHE_REPO, EMBED_CACHE_FILES).unwrap(),
            ModelCacheState::Partial
        );
    }

    #[test]
    fn regular_file_is_not_a_healthy_vault() {
        let dir = TempDir::new("doctor-vault-file");
        let vault = dir.path().join("brain");
        std::fs::write(&vault, "not a directory").unwrap();
        let (ok, detail) = vault_health(&vault);
        assert!(!ok);
        assert!(detail.contains("not a directory"));
    }
}

#[cfg(test)]
mod eval_cli_tests {
    use super::*;

    #[test]
    fn eval_k_is_bounded_during_cli_parsing() {
        assert!(Cli::try_parse_from(["vagus", "eval", "labels.jsonl", "--k", "0"]).is_err());
        assert!(Cli::try_parse_from(["vagus", "eval", "labels.jsonl", "--k", "1001"]).is_err());
        let cli = Cli::try_parse_from([
            "vagus",
            "eval",
            "labels.jsonl",
            "--k",
            "20",
            "--mode",
            "vec",
            "--exact",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Eval {
                k: 20,
                mode: Mode::Vec,
                exact: true,
                json: true,
                ..
            }
        ));
    }

    #[test]
    fn relevance_floor_is_finite_and_bounded_during_cli_parsing() {
        for invalid in ["-0.1", "1.1", "NaN", "inf", "nope"] {
            assert!(
                Cli::try_parse_from(["vagus", "search", "query", "--min-relevance", invalid])
                    .is_err(),
                "accepted {invalid}"
            );
        }
        let cli =
            Cli::try_parse_from(["vagus", "search", "query", "--min-relevance", "0.3"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Search {
                min_relevance: Some(value),
                ..
            } if value == 0.3
        ));
    }

    #[test]
    fn rerank_context_is_bounded_during_cli_parsing() {
        assert!(
            Cli::try_parse_from(["vagus", "search", "query", "--rerank-context", "3"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["vagus", "eval", "labels.jsonl", "--rerank-context", "3"])
                .is_err()
        );
        let cli = Cli::try_parse_from([
            "vagus",
            "search",
            "query",
            "--rerank",
            "--rerank-context",
            "2",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Search {
                rerank: true,
                rerank_context: 2,
                ..
            }
        ));
    }

    #[test]
    fn presentation_provenance_commands_parse_only_explicit_flags() {
        let search =
            Cli::try_parse_from(["vagus", "search", "query", "--tick-provenance"]).unwrap();
        assert!(matches!(
            search.command,
            Command::Search {
                tick_provenance: true,
                ..
            }
        ));

        let tick = Cli::try_parse_from([
            "vagus",
            "tick",
            "a.md",
            "--events",
            "{}",
            "--query",
            "private",
            "--store-query",
        ])
        .unwrap();
        assert!(matches!(
            tick.command,
            Command::Tick {
                events: Some(_),
                store_query: true,
                query: Some(_),
                ..
            }
        ));

        let report =
            Cli::try_parse_from(["vagus", "ticks", "--limit", "7", "--all", "--json"]).unwrap();
        assert!(matches!(
            report.command,
            Command::Ticks {
                limit: 7,
                all: true,
                json: true
            }
        ));
    }

    #[test]
    fn eval_gate_requires_two_reports_and_accepts_json_flag() {
        assert!(Cli::try_parse_from(["vagus", "eval-gate", "baseline.json"]).is_err());
        let cli = Cli::try_parse_from([
            "vagus",
            "eval-gate",
            "baseline.json",
            "candidate.json",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::EvalGate {
                baseline,
                candidate,
                json: true,
            } if baseline.as_path() == Path::new("baseline.json")
                && candidate.as_path() == Path::new("candidate.json")
        ));
    }
}
