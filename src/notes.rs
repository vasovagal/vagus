//! Capture (`add-note`), inbox listing, and assisted filing (`file`).
//!
//! Filing is the explicit, user-approved Organize step (ADR 0005), so writing/enriching frontmatter
//! here is allowed — distinct from G3 (never auto-edit a note during capture/index).

use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;

use crate::config::Config;
use crate::index;
use crate::scope::Scope;
use crate::search::{self, Mode};

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

fn vault_rel(cfg: &Config, p: &Path) -> String {
    p.strip_prefix(&cfg.vault)
        .unwrap_or(p)
        .to_string_lossy()
        .to_string()
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

#[allow(clippy::too_many_arguments)]
pub fn add_note(
    cfg: &Config,
    title: &str,
    para: &str,
    source: Option<&str>,
    print_path: bool,
    edit: bool,
    no_edit: bool,
) -> Result<()> {
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

    index::run(cfg, false)?; // index after the edit, so new content is searchable

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

pub fn inbox(cfg: &Config, json: bool) -> Result<()> {
    let dir = cfg.vault.join("00-Inbox");
    let mut items: Vec<(String, String)> = Vec::new();
    if dir.exists() {
        for entry in fs::read_dir(&dir)? {
            let p = entry?.path();
            if p.extension().and_then(|e| e.to_str()) == Some("md") {
                items.push((vault_rel(cfg, &p), note_title(&p)));
            }
        }
    }
    items.sort();

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

pub fn file(
    cfg: &Config,
    path: &str,
    to: Option<&str>,
    suggest: bool,
    json: bool,
    thought_process: bool,
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

    enrich_frontmatter(&src, to)?;
    fs::rename(&src, &dest).with_context(|| format!("moving to {}", dest.display()))?;
    index::run(cfg, false)?; // reconcile: old path removed, new path indexed

    println!("filed {} → {}", path, vault_rel(cfg, &dest));
    Ok(())
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
    let (hits, _) = search::query(cfg, &query_text, Mode::Hybrid, 12, &Scope::none()).unwrap_or_default();

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
