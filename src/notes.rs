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

pub fn add_note(
    cfg: &Config,
    title: &str,
    para: &str,
    source: Option<&str>,
    print_path: bool,
) -> Result<()> {
    let folder = para_folder(para)?;
    let dir = cfg.vault.join(folder);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let now = Local::now();
    let filename = format!("{}-{}.md", now.format("%Y%m%d-%H%M%S"), slugify(title));
    let path = dir.join(&filename);

    // Body from stdin when piped (e.g. the create-note skill's heredoc).
    let mut body = String::new();
    if !std::io::stdin().is_terminal() {
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

    index::run(cfg, false)?; // pick up + embed the new note

    if print_path {
        println!("{}", path.display());
    } else {
        println!("created {}", vault_rel(cfg, &path));
    }
    Ok(())
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

pub fn file(cfg: &Config, path: &str, to: Option<&str>, suggest: bool, json: bool) -> Result<()> {
    let src = resolve(cfg, path);
    if !src.exists() {
        bail!("note not found: {}", src.display());
    }

    if suggest {
        return suggest_dest(cfg, &src, json);
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
fn suggest_dest(cfg: &Config, src: &Path, json: bool) -> Result<()> {
    let self_rel = vault_rel(cfg, src);

    fn add(ranked: &mut Vec<(String, f32)>, folder: String, score: f32) {
        if !folder.is_empty()
            && !folder.starts_with("00-Inbox")
            && !ranked.iter().any(|(f, _)| f == &folder)
        {
            ranked.push((folder, score));
        }
    }

    let mut ranked: Vec<(String, f32)> = Vec::new();
    // 1. folders of notes similar to this one
    if let Ok(hits) = search::query(cfg, &note_text(src), Mode::Hybrid, 12) {
        for h in hits {
            if h.path != self_rel {
                add(&mut ranked, parent_folder(&h.path), h.score);
            }
        }
    }
    // 2. existing PARA folders already in the vault
    for folder in existing_para_folders(cfg) {
        add(&mut ranked, folder, 0.0);
    }

    if json {
        let arr: Vec<_> = ranked
            .iter()
            .map(|(folder, score)| serde_json::json!({ "folder": folder, "score": score }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }

    println!("Where should {self_rel} go?\n");
    if ranked.is_empty() {
        println!("Your PARA folders are empty — pick a bucket (a subfolder is created as needed):");
        for b in [
            "10-Projects/<project>",
            "20-Areas/<area>",
            "30-Resources/<topic>",
            "40-Archive",
        ] {
            println!("  {b}");
        }
    } else {
        println!("Suggestions (similar notes first):");
        for (folder, score) in &ranked {
            if *score > 0.0 {
                println!("  {folder}   (similar · {score:.2})");
            } else {
                println!("  {folder}");
            }
        }
    }
    println!("\nFile it:");
    println!("  vagus file \"{self_rel}\" --to \"<one of the above>\"");
    Ok(())
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
