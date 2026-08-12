//! Capture (`add-note`), inbox listing, and assisted filing (`file`).
//!
//! Filing is the explicit, user-approved Organize step (ADR 0005), so writing/enriching frontmatter
//! here is allowed — distinct from G3 (never auto-edit a note during capture/index).

use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;

use crate::chunk::parse_frontmatter;
use crate::config::Config;
use crate::db::Db;
use crate::frontmatter::{is_vagus_owned, valid_producer_key};
use crate::index;
use crate::scope::Scope;
use crate::search::{self, Mode};
use crate::util::note_created_at_secs;

/// Child-process compatibility channel for integrations that need producer-owned frontmatter. A current
/// Vagus consumes it; older binaries harmlessly ignore it, unlike an unknown command-line flag.
const ADD_NOTE_FRONTMATTER_ENV: &str = "VAGUS_ADD_NOTE_FRONTMATTER_JSON";
/// Keep one integration from turning the small note header into an unbounded transport.
const MAX_EXTRA_FRONTMATTER_BYTES: usize = 64 * 1024;

/// Map a PARA keyword (for `add-note --para`) to its folder.
fn para_folder(para: &str) -> Result<&'static str> {
    Ok(match para.to_ascii_lowercase().as_str() {
        "inbox" => "00-Inbox",
        "project" | "projects" => "10-Projects",
        "area" | "areas" => "20-Areas",
        "resource" | "resources" => "30-Resources",
        "archive" => "40-Archive",
        other => bail!("unknown PARA bucket '{other}' (inbox|project|area|resource|archive)"),
    })
}

/// Map a destination folder (for `file --to`) back to a `para:` frontmatter value.
fn folder_para(to: &str) -> &'static str {
    match to.split('/').next().unwrap_or("") {
        "10-Projects" => "project",
        "20-Areas" => "area",
        "30-Resources" => "resource",
        "40-Archive" => "archive",
        _ => "inbox",
    }
}

fn slugify(title: &str) -> String {
    let mut s = String::new();
    let mut prev_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            s.push('-');
            prev_dash = true;
        }
    }
    let s = s.trim_matches('-').to_string();
    let s: String = s.chars().take(40).collect();
    if s.is_empty() { "note".into() } else { s }
}

/// Resolve a user-supplied path (absolute or vault-relative) to an absolute path.
fn resolve(cfg: &Config, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        cfg.vault.join(p)
    }
}

/// Vault-relative form of `p` — the canonical tick/index key. A lexical strip first (the common
/// spelling), then a canonicalized retry so alias spellings of absolute paths (the vault symlink's
/// real iCloud target, `/tmp` -> `/private/tmp`) key identically instead of stranding ticks
/// (ADR 0021/G25). Canonicalizes the parent + re-attaches the filename because the file itself may
/// already be gone (re-keying runs after the move).
pub(crate) fn vault_rel(cfg: &Config, p: &Path) -> String {
    if let Ok(rel) = p.strip_prefix(&cfg.vault) {
        return rel.to_string_lossy().to_string();
    }
    if p.is_absolute()
        && let Ok(vault) = fs::canonicalize(&cfg.vault)
        && let (Some(parent), Some(name)) = (p.parent(), p.file_name())
        && let Ok(cparent) = fs::canonicalize(parent)
        && let Ok(rel) = cparent.join(name).strip_prefix(&vault).map(Path::to_owned)
    {
        return rel.to_string_lossy().to_string();
    }
    p.to_string_lossy().to_string()
}

/// First `# heading` or, failing that, the filename stem.
fn note_title(p: &Path) -> String {
    if let Ok(text) = fs::read_to_string(p) {
        for line in text.lines() {
            if let Some(h) = line.strip_prefix("# ") {
                return h.trim().to_string();
            }
        }
    }
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Body text minus a leading YAML frontmatter block (for use as a `--suggest` query).
fn note_text(p: &Path) -> String {
    let content = fs::read_to_string(p).unwrap_or_default();
    let body = strip_frontmatter(&content).1;
    body.chars().take(800).collect()
}

/// Split a leading `---`…`---` frontmatter block. Returns (frontmatter_lines, body).
fn strip_frontmatter(content: &str) -> (Vec<String>, String) {
    let mut lines = content.lines();
    if lines.next() == Some("---") {
        let mut fm = Vec::new();
        for line in lines.by_ref() {
            if line.trim_end() == "---" {
                let body: String = lines.collect::<Vec<_>>().join("\n");
                return (fm, body.trim_start_matches('\n').to_string());
            }
            fm.push(line.to_string());
        }
        // No closing delimiter: treat the whole thing as body.
    }
    (Vec::new(), content.to_string())
}

fn upsert(lines: &mut Vec<String>, key: &str, val: &str) {
    let prefix = format!("{key}:");
    if let Some(line) = lines
        .iter_mut()
        .find(|l| l.trim_start().starts_with(&prefix))
    {
        *line = format!("{key}: {val}");
    } else {
        lines.push(format!("{key}: {val}"));
    }
}

// --- add-note ---------------------------------------------------------------

/// Render an optional JSON object as frontmatter lines. Each value stays JSON-encoded: JSON scalars,
/// arrays, and objects are valid YAML flow values, while quoting/escaping prevents a producer value from
/// injecting another YAML line. The top-level key grammar is deliberately smaller than YAML's.
fn render_extra_frontmatter(json: Option<&str>) -> Result<String> {
    let Some(json) = json.map(str::trim).filter(|json| !json.is_empty()) else {
        return Ok(String::new());
    };
    if json.len() > MAX_EXTRA_FRONTMATTER_BYTES {
        bail!(
            "--frontmatter-json is too large ({} bytes; maximum {})",
            json.len(),
            MAX_EXTRA_FRONTMATTER_BYTES
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(json).context("parsing --frontmatter-json")?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("--frontmatter-json must be a JSON object"))?;

    let mut rendered = String::new();
    for (key, value) in object {
        if !valid_producer_key(key) {
            bail!(
                "invalid frontmatter key {key:?}; use ASCII letters, digits, `_`, or `-`, starting with a letter or `_`"
            );
        }
        if is_vagus_owned(key) {
            bail!("frontmatter key {key:?} is owned by vagus and cannot be overridden");
        }
        rendered.push_str(key);
        rendered.push_str(": ");
        rendered.push_str(&serde_json::to_string(value)?);
        rendered.push('\n');
    }
    if rendered.len() > MAX_EXTRA_FRONTMATTER_BYTES {
        bail!(
            "rendered --frontmatter-json is too large ({} bytes; maximum {})",
            rendered.len(),
            MAX_EXTRA_FRONTMATTER_BYTES
        );
    }
    Ok(rendered)
}

#[allow(clippy::too_many_arguments)]
pub fn add_note(
    cfg: &Config,
    title: &str,
    para: &str,
    source: Option<&str>,
    frontmatter_json: Option<&str>,
    print_path: bool,
    edit: bool,
    no_edit: bool,
) -> Result<()> {
    // CLI input wins over the compatibility environment. Validate before creating a directory or note, so
    // malformed producer metadata has no filesystem side effect.
    let env_frontmatter = if frontmatter_json.is_none() {
        std::env::var(ADD_NOTE_FRONTMATTER_ENV).ok()
    } else {
        None
    };
    let extra_frontmatter =
        render_extra_frontmatter(frontmatter_json.or(env_frontmatter.as_deref()))?;
    let folder = para_folder(para)?;
    let dir = cfg.vault.join(folder);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let now = Local::now();
    let filename = format!("{}-{}.md", now.format("%Y%m%d-%H%M%S"), slugify(title));
    let path = dir.join(&filename);

    // Body from stdin when piped (e.g. the create-note skill's heredoc).
    let piped = !std::io::stdin().is_terminal();
    let mut body = String::new();
    if piped {
        std::io::stdin().read_to_string(&mut body)?;
    }

    let mut fm = format!(
        "---\ncreated: {}\nstatus: inbox\n",
        now.format("%Y-%m-%dT%H:%M")
    );
    if let Some(src) = source {
        fm.push_str(&format!("source: {src}\n"));
    }
    fm.push_str(&extra_frontmatter);
    fm.push_str("---\n\n");
    let content = format!("{fm}# {title}\n\n{}\n", body.trim());
    fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;

    // Open the editor: with --edit, or by default in an interactive session — so `vagus add-note X`
    // drops you straight into the note. Suppressed by --print-path, a piped body, or --no-edit.
    let interactive = !piped && std::io::stdout().is_terminal();
    let mut opened = false;
    if !print_path && !no_edit && (edit || interactive) {
        match open_editor(&path) {
            Ok(true) => opened = true,
            Ok(false) => {
                if edit {
                    eprintln!("vagus: set $VISUAL or $EDITOR to use --edit");
                }
            }
            Err(e) => eprintln!("vagus: {e:#}"),
        }
    }

    index::run(cfg, index::IndexMode::Incremental)?; // index after edit: new content is searchable

    if print_path {
        println!("{}", path.display());
    } else if opened {
        println!("saved {}", path.display());
    } else {
        println!("created {}", path.display());
    }
    Ok(())
}

/// Open `path` in `$VISUAL`/`$EDITOR` and wait for it to close. Returns `Ok(false)` if neither is set.
fn open_editor(path: &Path) -> Result<bool> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    let Some(editor) = editor else {
        return Ok(false);
    };
    // Split so "zed --wait" / "code --wait" / "vim" all work; append the note path.
    let mut parts = editor.split_whitespace();
    let prog = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(prog)
        .args(parts)
        .arg(path)
        .status()
        .with_context(|| format!("launching editor `{editor}`"))?;
    if !status.success() {
        eprintln!("vagus: editor exited with {status}");
    }
    Ok(true)
}

// --- inbox ------------------------------------------------------------------

fn inbox_items(cfg: &Config, since: Option<i64>) -> Result<Vec<(String, String)>> {
    let dir = cfg.vault.join("00-Inbox");
    let mut items = Vec::new();
    if dir.exists() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            if let Some(cutoff) = since {
                let mtime = fs::metadata(&path)?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_secs_f64())
                    .unwrap_or(0.0);
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("reading {} for --since", path.display()))?;
                let frontmatter = parse_frontmatter(&text);
                if note_created_at_secs(frontmatter.created.as_deref(), mtime) < cutoff {
                    continue;
                }
            }
            items.push((vault_rel(cfg, &path), note_title(&path)));
        }
    }
    items.sort();
    Ok(items)
}

pub fn inbox(cfg: &Config, json: bool, since: Option<i64>) -> Result<()> {
    let items = inbox_items(cfg, since)?;

    if json {
        let arr: Vec<_> = items
            .iter()
            .map(|(path, title)| serde_json::json!({ "path": path, "title": title }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else if items.is_empty() {
        println!("inbox is empty 🎉");
    } else {
        for (path, title) in &items {
            println!("- {title}  [{path}]");
        }
    }
    Ok(())
}

// --- file (assisted filing) -------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn file(
    cfg: &Config,
    path: &str,
    to: Option<&str>,
    suggest: bool,
    json: bool,
    thought_process: bool,
    stats: bool,
) -> Result<()> {
    let src = resolve(cfg, path);
    if !src.exists() {
        bail!("note not found: {}", src.display());
    }

    // --thought-process implies a suggestion (it explains how one is computed).
    if suggest || thought_process {
        return suggest_dest(cfg, &src, json, thought_process);
    }

    let to = to.ok_or_else(|| anyhow!("`--to <folder>` is required (or use `--suggest`)"))?;
    let dest_dir = cfg.vault.join(to);
    fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(
        src.file_name()
            .ok_or_else(|| anyhow!("bad source filename"))?,
    );

    let total_start = Instant::now();

    let t0 = Instant::now();
    enrich_frontmatter(&src, to)?;
    let enrich_ms = elapsed_ms(t0);

    let t0 = Instant::now();
    fs::rename(&src, &dest).with_context(|| format!("moving to {}", dest.display()))?;
    let move_ms = elapsed_ms(t0);

    rekey_ticks(cfg, &src, &dest);

    // reconcile: old path removed, new path indexed. Capture per-step index timings only when asked.
    let mut idx = stats.then(index::IndexTimings::default);
    index::run_timed(cfg, index::IndexMode::Incremental, idx.as_mut())?;

    let dest_rel = vault_rel(cfg, &dest);

    if stats {
        let idx = idx.unwrap_or_default();
        let total_ms = elapsed_ms(total_start);
        if json {
            // Stable shape (G13): emitted only on the `--stats --json` path; the default and
            // `--suggest --json` outputs are untouched.
            let out = serde_json::json!({
                "filed": path,
                "dest": dest_rel,
                "timings": {
                    "enrich_ms": enrich_ms,
                    "move_ms": move_ms,
                    "chunk_ms": idx.chunk_ms,
                    "replace_chunks_ms": idx.replace_chunks_ms,
                    "tantivy_add_ms": idx.tantivy_add_ms,
                    "embed_ms": idx.embed_ms,
                    "insert_embedding_ms": idx.insert_embedding_ms,
                    "commit_ms": idx.commit_ms,
                    "total_ms": total_ms,
                },
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!("filed {path} → {dest_rel}");
            let rows = [
                ("enrich", enrich_ms),
                ("move", move_ms),
                ("chunk", idx.chunk_ms),
                ("replace_chunks", idx.replace_chunks_ms),
                ("tantivy_add", idx.tantivy_add_ms),
                ("embed", idx.embed_ms),
                ("insert_embedding", idx.insert_embedding_ms),
                ("commit", idx.commit_ms),
            ];
            let width = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
            for (label, ms) in rows {
                println!("  {label:<width$}  {ms:>8.1} ms");
            }
            println!("  {:<width$}  {total_ms:>8.1} ms", "total");
        }
    } else {
        println!("filed {} → {}", path, dest_rel);
    }
    Ok(())
}

/// Milliseconds since `start`, as `f64`.
fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Re-key local usage counters and provenance paths (ADR 0021/G25) so user data follows the note. Fail-soft: the
/// file has already moved, so a re-key failure warns on stderr and never fails the filing (`doctor`
/// surfaces any resulting orphans).
fn rekey_ticks(cfg: &Config, src: &Path, dest: &Path) {
    let src_rel = vault_rel(cfg, src);
    let dest_rel = vault_rel(cfg, dest);
    if let Err(e) = Db::open(&cfg.db_path()).and_then(|mut db| db.tick_rename(&src_rel, &dest_rel))
    {
        eprintln!("warning: could not move local usage data {src_rel} -> {dest_rel}: {e}");
    }
}

/// Set/insert `status: active`, `para: <bucket>`, `modified: <now>` while preserving other fields.
fn enrich_frontmatter(src: &Path, to: &str) -> Result<()> {
    let content = fs::read_to_string(src)?;
    let (mut fm, body) = strip_frontmatter(&content);
    upsert(&mut fm, "status", "active");
    upsert(&mut fm, "para", folder_para(to));
    upsert(
        &mut fm,
        "modified",
        &Local::now().format("%Y-%m-%dT%H:%M").to_string(),
    );
    let new = format!("---\n{}\n---\n\n{}\n", fm.join("\n"), body.trim_start());
    fs::write(src, new)?;
    Ok(())
}

/// Suggest PARA destinations: folders of similar existing notes (hybrid search) first, then the
/// vault's existing PARA folders, with a bucket-list fallback so the answer is never empty.
/// `explain` (--thought-process) prints the inputs: query text, search hits, and folder derivation.
fn suggest_dest(cfg: &Config, src: &Path, json: bool, explain: bool) -> Result<()> {
    let self_rel = vault_rel(cfg, src);
    let query_text = note_text(src);
    let (hits, _, _) = search::query(
        cfg,
        &query_text,
        Mode::Hybrid,
        12,
        &Scope::none(),
        false,
        false,
        0,     // no rerank context on the filing path (rerank is off)
        None,  // no --since for filing suggestions (ADR 0017)
        None,  // no --source for filing suggestions (ADR 0017)
        false, // approximate (HNSW) is fine for filing suggestions (ADR 0019)
        false, // no --timings on the filing path
        true,  // chunk-level: filing folds folders itself, unchanged by note dedup (ADR 0020)
        false, // no --min-score floor on the filing path
    )
    .unwrap_or_default();

    // Folders of similar notes (scored), then existing PARA folders not already covered (score 0).
    let mut similar: Vec<(String, f32)> = Vec::new();
    for h in &hits {
        if h.path == self_rel {
            continue;
        }
        let folder = parent_folder(&h.path);
        if folder.is_empty() || folder.starts_with("00-Inbox") {
            continue;
        }
        if !similar.iter().any(|(f, _)| f == &folder) {
            similar.push((folder, h.score));
        }
    }
    let existing = existing_para_folders(cfg);
    let fallback: Vec<String> = existing
        .iter()
        .filter(|f| !similar.iter().any(|(s, _)| s == *f))
        .cloned()
        .collect();

    if explain {
        let trace = render_trace(&self_rel, &query_text, &hits, &similar, &existing);
        // Keep stdout machine-clean under --json; otherwise show it inline.
        if json {
            eprint!("{trace}");
        } else {
            print!("{trace}");
        }
    }

    if json {
        let mut arr: Vec<serde_json::Value> = Vec::new();
        for (folder, score) in &similar {
            arr.push(serde_json::json!({ "folder": folder, "score": score }));
        }
        for folder in &fallback {
            arr.push(serde_json::json!({ "folder": folder, "score": 0.0 }));
        }
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }

    println!("Where should {self_rel} go?\n");
    if !similar.is_empty() {
        println!("Most similar to notes already in:");
        for (folder, score) in &similar {
            println!("  {folder}   (similar · {score:.2})");
        }
        if !fallback.is_empty() {
            println!("\nOther PARA folders:");
            for folder in &fallback {
                println!("  {folder}");
            }
        }
    } else if !fallback.is_empty() {
        println!("No similar notes yet — pick a PARA folder:");
        for folder in &fallback {
            println!("  {folder}");
        }
    } else {
        println!("No PARA folders yet — pick a bucket (a subfolder is created as needed):");
        for b in [
            "10-Projects/<project>",
            "20-Areas/<area>",
            "30-Resources/<topic>",
            "40-Archive",
        ] {
            println!("  {b}");
        }
    }
    println!("\nFile it:");
    println!("  vagus file \"{self_rel}\" --to \"<one of the above>\"");
    Ok(())
}

/// Human-readable "thought process" for `--thought-process`: the query text, the hybrid-search hits,
/// and how those became folder suggestions.
fn render_trace(
    self_rel: &str,
    query_text: &str,
    hits: &[search::Hit],
    similar: &[(String, f32)],
    existing: &[String],
) -> String {
    use std::fmt::Write as _;
    let mut t = String::new();
    let _ = writeln!(t, "── thought process ──");

    let preview: String = query_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.trim().is_empty() {
        let _ = writeln!(
            t,
            "query (note body): (empty — nothing to compare on; add some text)"
        );
    } else {
        let shown: String = preview.chars().take(160).collect();
        let more = if preview.chars().count() > 160 {
            "…"
        } else {
            ""
        };
        let _ = writeln!(t, "query (note body): \"{shown}{more}\"");
    }

    if hits.is_empty() {
        let _ = writeln!(
            t,
            "hybrid search hits: none (nothing else is indexed to compare against)"
        );
    } else {
        let _ = writeln!(t, "hybrid search hits:");
        for h in hits {
            let loc = if h.heading.is_empty() {
                h.path.clone()
            } else {
                format!("{} › {}", h.path, h.heading)
            };
            let note = if h.path == self_rel {
                "  ← self (skipped)"
            } else if h.path.starts_with("00-Inbox") {
                "  ← inbox (skipped)"
            } else {
                ""
            };
            let _ = writeln!(t, "  {:.3}  {loc}{note}", h.score);
        }
    }

    let sim = if similar.is_empty() {
        "none".to_string()
    } else {
        similar
            .iter()
            .map(|(f, s)| format!("{f} ({s:.2})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(t, "→ folders from similar notes: {sim}");
    let ex = if existing.is_empty() {
        "none".to_string()
    } else {
        existing.join(", ")
    };
    let _ = writeln!(t, "→ existing PARA folders in vault: {ex}");
    if similar.is_empty() {
        let _ = writeln!(
            t,
            "  (no similar filed notes → suggesting your PARA folders / buckets)"
        );
    }
    let _ = writeln!(t, "─────────────────────\n");
    t
}

fn parent_folder(rel: &str) -> String {
    Path::new(rel)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Existing PARA destinations in the vault: each bucket's immediate subfolders, or the bucket root
/// itself when it has none yet.
fn existing_para_folders(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for bucket in ["10-Projects", "20-Areas", "30-Resources", "40-Archive"] {
        let dir = cfg.vault.join(bucket);
        if !dir.exists() {
            continue;
        }
        let mut subs: Vec<String> = Vec::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    subs.push(format!("{bucket}/{}", e.file_name().to_string_lossy()));
                }
            }
        }
        subs.sort();
        if subs.is_empty() {
            out.push(bucket.to_string());
        } else {
            out.extend(subs);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TempDir;

    #[test]
    fn inbox_since_uses_created_frontmatter() {
        let dir = TempDir::new("inbox-since");
        let cfg = Config {
            vault: dir.path().join("vault"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };
        let inbox = cfg.vault.join("00-Inbox");
        fs::create_dir_all(&inbox).unwrap();
        fs::write(
            inbox.join("old.md"),
            "---\ncreated: 2000-01-01T00:00\n---\n# Old\n",
        )
        .unwrap();
        fs::write(
            inbox.join("recent.md"),
            "---\ncreated: 2099-01-01T00:00\n---\n# Recent\n",
        )
        .unwrap();
        fs::write(inbox.join("bare.md"), "# Bare\n").unwrap();

        // The bare note falls back to its current filesystem mtime (before this 2033 cutoff).
        let items = inbox_items(&cfg, Some(2_000_000_000)).unwrap();
        assert_eq!(items, [("00-Inbox/recent.md".into(), "Recent".into())]);
    }

    #[test]
    fn producer_frontmatter_is_yaml_safe_and_structured() {
        let rendered = render_extra_frontmatter(Some(
            r#"{"corti":{"version":"0.12.0","mode":"live","nested":{"threshold":0.5}},"external-id":"line 1\nstatus: hacked"}"#,
        ))
        .unwrap();
        assert!(
            rendered.contains(
                r#"corti: {"mode":"live","nested":{"threshold":0.5},"version":"0.12.0"}"#
            )
        );
        assert!(rendered.contains(r#"external-id: "line 1\nstatus: hacked""#));
        assert_eq!(
            rendered.lines().count(),
            2,
            "a value cannot inject YAML lines"
        );
    }

    #[test]
    fn rendered_producer_frontmatter_becomes_searchable_metadata() {
        let rendered = render_extra_frontmatter(Some(
            r#"{"corti":{"models":{"asr":{"id":"nvidia/parakeet-tdt-0.6b-v3"}}}}"#,
        ))
        .unwrap();
        let note = format!("---\n{rendered}---\n\n# Transcript\n\nspoken words\n");
        let chunks = crate::chunk::chunk_markdown("transcript.md", &note);
        let metadata = chunks
            .iter()
            .find(|chunk| chunk.kind == crate::chunk::ChunkKind::ProducerMetadata)
            .unwrap();
        assert_eq!(metadata.heading_path, "Frontmatter > corti");
        assert!(metadata.body.contains("parakeet-tdt-0.6b-v3"));
    }

    #[test]
    fn producer_frontmatter_must_be_an_object_with_safe_nonreserved_keys() {
        for invalid in [
            r#"["not", "an", "object"]"#,
            r#"{"status":"active"}"#,
            r#"{"created":"sometime"}"#,
            r#"{"bad:key":1}"#,
            r#"{"9bad":1}"#,
            "{not json}",
        ] {
            assert!(
                render_extra_frontmatter(Some(invalid)).is_err(),
                "accepted {invalid}"
            );
        }
        assert_eq!(render_extra_frontmatter(None).unwrap(), "");
        assert_eq!(render_extra_frontmatter(Some("{}")).unwrap(), "");
    }

    // Alias spellings of absolute paths (vault symlink's real target, /tmp -> /private/tmp) must
    // key identically to the plain spelling — both when cfg.vault is the symlink (the ~/brain
    // layout) and when the input path goes through one.
    #[cfg(unix)]
    #[test]
    fn vault_rel_resolves_alias_spellings() {
        let dir = TempDir::new("vault-alias");
        let vault = dir.path().join("vault");
        fs::create_dir_all(vault.join("20-Areas")).unwrap();
        fs::write(vault.join("20-Areas/foo.md"), "# foo\n").unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&vault, &alias).unwrap();
        let mk = |vault: &Path| Config {
            vault: vault.to_path_buf(),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };

        // Path spelled through the alias; vault configured as the real dir.
        let cfg = mk(&vault);
        assert_eq!(
            vault_rel(&cfg, &alias.join("20-Areas/foo.md")),
            "20-Areas/foo.md"
        );
        // Vault configured as the symlink (~/brain), path spelled via the real target.
        let cfg = mk(&alias);
        assert_eq!(
            vault_rel(&cfg, &vault.join("20-Areas/foo.md")),
            "20-Areas/foo.md"
        );
        // The file itself may already be gone (re-key runs after the move) — parent still resolves.
        assert_eq!(
            vault_rel(&cfg, &vault.join("20-Areas/gone.md")),
            "20-Areas/gone.md"
        );
        // Relative inputs are untouched.
        assert_eq!(
            vault_rel(&cfg, Path::new("20-Areas/foo.md")),
            "20-Areas/foo.md"
        );
    }

    // `vagus file` move re-keys ticks to the destination path (ADR 0021/G25). Exercises
    // `rekey_ticks` directly — the full `file()` path needs the embedder, which the merge-logic
    // db tests (`tick_rename_merges_counts`) plus this cover without it.
    #[test]
    fn file_move_rekeys_ticks() {
        let dir = TempDir::new("rekey");
        let cfg = Config {
            vault: dir.path().join("vault"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
        };
        {
            let db = Db::open(&cfg.db_path()).unwrap();
            db.upsert_file("30-Resources/a.md", 1.0, "sha", 1).unwrap();
            db.tick("00-Inbox/a.md").unwrap();
            db.tick("00-Inbox/a.md").unwrap();
        }

        rekey_ticks(
            &cfg,
            &cfg.vault.join("00-Inbox/a.md"),
            &cfg.vault.join("30-Resources/a.md"),
        );

        let db = Db::open(&cfg.db_path()).unwrap();
        let rows = db.fame(10, false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "30-Resources/a.md", "ticks follow the note");
        assert_eq!(rows[0].1, 2, "count carried over");
        assert_eq!(db.orphan_tick_count().unwrap(), 0, "no orphan left behind");
    }
}
