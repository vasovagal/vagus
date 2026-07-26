//! Bundled agent skills, embedded at compile time and installable for Claude Code or pi.
//!
//! Each `SKILL.md` is pulled in with `include_str!` (relative to this file), so the skills version
//! WITH the binary — `brew install vagus && vagus skills install` is the whole setup, no clone, no
//! symlink. Editing `skills/<name>/SKILL.md` and rebuilding updates the embedded copy.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::ValueEnum;

pub struct Skill {
    pub name: &'static str,
    pub body: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Agent {
    #[value(alias = "claude-code")]
    Claude,
    Pi,
}

impl Agent {
    fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Pi => "pi",
        }
    }

    fn activation_hint(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code discovers newly installed skills automatically.",
            Self::Pi => "pi discovers skills on startup; run `/reload` in an existing session.",
        }
    }
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

/// Resolve the skills dir: `--dir` override, then the selected agent's config env/default.
pub fn skills_dir(agent: Agent, override_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(d) = override_dir {
        return Ok(d);
    }
    let home = dirs::home_dir().context("cannot resolve home directory")?;
    let get_env = |key: &str| std::env::var_os(key);
    Ok(default_skills_dir(agent, &home, get_env))
}

fn default_skills_dir(
    agent: Agent,
    home: &Path,
    get_env: impl Fn(&str) -> Option<OsString>,
) -> PathBuf {
    match agent {
        Agent::Claude => get_env("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"))
            .join("skills"),
        Agent::Pi => get_env("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".pi/agent"))
            .join("skills"),
    }
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
pub fn install(agent: Agent, override_dir: Option<PathBuf>, force: bool) -> Result<()> {
    let root = skills_dir(agent, override_dir)?;
    println!(
        "installing {} skills for {} into {}",
        BUNDLED.len(),
        agent.display_name(),
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

    println!("({})", agent.activation_hint());
    Ok(())
}

/// List the bundled skills + their install status in the selected agent's default skills dir.
pub fn list(agent: Agent) -> Result<()> {
    let root = skills_dir(agent, None)?;
    println!(
        "bundled skills for {} (install dir: {}):",
        agent.display_name(),
        root.display()
    );
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
    fn default_dirs_are_agent_specific() {
        let home = Path::new("/home/test");
        assert_eq!(
            default_skills_dir(Agent::Claude, home, |_| None),
            PathBuf::from("/home/test/.claude/skills")
        );
        assert_eq!(
            default_skills_dir(Agent::Pi, home, |_| None),
            PathBuf::from("/home/test/.pi/agent/skills")
        );
    }

    #[test]
    fn default_dirs_honor_agent_config_env() {
        let home = Path::new("/home/test");
        assert_eq!(
            default_skills_dir(Agent::Claude, home, |key| {
                (key == "CLAUDE_CONFIG_DIR").then(|| OsString::from("/tmp/claude"))
            }),
            PathBuf::from("/tmp/claude/skills")
        );
        assert_eq!(
            default_skills_dir(Agent::Pi, home, |key| {
                (key == "PI_CODING_AGENT_DIR").then(|| OsString::from("/tmp/pi"))
            }),
            PathBuf::from("/tmp/pi/skills")
        );
    }

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
        // The search skill must record usage ticks (ADR 0021) — a `tick` command rename without a
        // SKILL.md update fails here at build time.
        let search = BUNDLED.iter().find(|s| s.name == "search").unwrap();
        assert!(
            search.body.contains("vagus tick "),
            "search skill no longer invokes `vagus tick`"
        );
    }
}
