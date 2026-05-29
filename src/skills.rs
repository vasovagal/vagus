//! Bundled Claude Code skills, embedded at compile time and installable on demand.
//!
//! Each `SKILL.md` is pulled in with `include_str!` (relative to this file), so the skills version
//! WITH the binary — `brew install vagus && vagus skills install` is the whole setup, no clone, no
//! symlink. Editing `skills/<name>/SKILL.md` and rebuilding updates the embedded copy.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub struct Skill {
    pub name: &'static str,
    pub body: &'static str,
}

pub const BUNDLED: &[Skill] = &[
    Skill {
        name: "create-note",
        body: include_str!("../skills/create-note/SKILL.md"),
    },
    Skill {
        name: "search",
        body: include_str!("../skills/search/SKILL.md"),
    },
    Skill {
        name: "process-inbox",
        body: include_str!("../skills/process-inbox/SKILL.md"),
    },
];

/// Resolve the skills dir: `--dir` override, else `$CLAUDE_CONFIG_DIR/skills`, else `~/.claude/skills`.
pub fn skills_dir(override_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(d) = override_dir {
        return Ok(d);
    }
    if let Some(c) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Ok(PathBuf::from(c).join("skills"));
    }
    let home = dirs::home_dir().context("cannot resolve home directory")?;
    Ok(home.join(".claude/skills"))
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// The `description:` line from a SKILL.md's YAML frontmatter, if any.
fn description(body: &str) -> Option<&str> {
    body.lines()
        .find_map(|l| l.strip_prefix("description:").map(str::trim))
}

/// Write the bundled skills into the resolved skills dir.
///
/// Per skill (safe + idempotent): a symlinked target is skipped (protects the repo dev symlinks)
/// unless `--force`; an identical file is left alone; a divergent file is backed up to `SKILL.md.bak`
/// (unless `--force`) then overwritten; a missing file is created.
pub fn install(override_dir: Option<PathBuf>, force: bool) -> Result<()> {
    let root = skills_dir(override_dir)?;
    println!(
        "installing {} skills into {}",
        BUNDLED.len(),
        root.display()
    );

    for s in BUNDLED {
        let sdir = root.join(s.name);
        let path = sdir.join("SKILL.md");

        if is_symlink(&sdir) || is_symlink(&path) {
            if !force {
                println!("  skipped (symlink)  {}", sdir.display());
                continue;
            }
            // --force: replace the symlink with a real install (unlink, don't follow into the target).
            if is_symlink(&sdir) {
                let _ = std::fs::remove_file(&sdir);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }

        std::fs::create_dir_all(&sdir).with_context(|| format!("creating {}", sdir.display()))?;
        let action = match std::fs::read_to_string(&path) {
            Ok(cur) if cur == s.body => {
                println!("  up to date  {}", path.display());
                continue;
            }
            Ok(_) if !force => {
                std::fs::rename(&path, sdir.join("SKILL.md.bak"))
                    .with_context(|| format!("backing up {}", path.display()))?;
                "updated (backed up to SKILL.md.bak)"
            }
            Ok(_) => "updated",
            Err(_) => "installed",
        };
        std::fs::write(&path, s.body).with_context(|| format!("writing {}", path.display()))?;
        println!("  {action}  {}", path.display());
    }

    println!("(Claude Code loads ~/.claude/skills automatically — no restart needed.)");
    Ok(())
}

/// List the bundled skills + their install status in the default skills dir.
pub fn list() -> Result<()> {
    let root = skills_dir(None)?;
    println!("bundled skills (install dir: {}):", root.display());
    for s in BUNDLED {
        let sdir = root.join(s.name);
        let status = if is_symlink(&sdir) {
            "symlinked"
        } else {
            match std::fs::read_to_string(sdir.join("SKILL.md")) {
                Ok(c) if c == s.body => "installed",
                Ok(_) => "outdated",
                Err(_) => "not installed",
            }
        };
        let desc: String = description(s.body).unwrap_or("").chars().take(80).collect();
        println!("  {:<14} [{:<13}] {desc}…", s.name, status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_skills_are_embedded() {
        assert_eq!(BUNDLED.len(), 3);
        for s in BUNDLED {
            assert!(!s.body.trim().is_empty(), "{} is empty", s.name);
            assert!(
                s.body.starts_with("---"),
                "{} is missing YAML frontmatter",
                s.name
            );
            assert!(
                description(s.body).is_some(),
                "{} has no description",
                s.name
            );
        }
    }
}
