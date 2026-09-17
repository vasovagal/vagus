//! Bundled agent skills, embedded at compile time and installable for Claude Code or pi.
//!
//! Each `SKILL.md` is pulled in with `include_str!` (relative to this file), so the skills version
//! WITH the binary — `brew install vagus && vagus skills install` is the whole setup, no clone, no
//! symlink. Editing `skills/<name>/SKILL.md` and rebuilding updates the embedded copy.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::util::sha256_hex;
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
        name: "vagus-create-note",
        body: include_str!("../skills/vagus-create-note/SKILL.md"),
    },
    Skill {
        name: "vagus-search",
        body: include_str!("../skills/vagus-search/SKILL.md"),
    },
    Skill {
        name: "vagus-process-inbox",
        body: include_str!("../skills/vagus-process-inbox/SKILL.md"),
    },
];

// Verified source snapshots from 202b663 (v0.14.1), 1cac509 and e519bf7. Exact bytes,
// not a generic name or a forgeable source comment, determine migration ownership.
const LEGACY: &[(&str, &[&str])] = &[
    (
        "create-note",
        &[
            "d9f7a8e2298d3ce6009ac55fd61a47ab9b7fb777152962f0ac97781c3a12955b",
            "1a5c684f69385101b3ebe9f03e457b3ca56be267116ddb346e4fd37a1415b370",
        ],
    ),
    (
        "search",
        &[
            "94073675bacbac555bf57e2fed451ea495613f2583c7577801e0f5a0e2a4e10e",
            "28512d5f9a4a1cab68a917ac94fd4ef6635ad6c68acf838b6eb1fc64b335a99d",
            "924fadae3b692428ef9fee4c3e6a0ed7a243548ed979c04163f5e7064d93236b",
        ],
    ),
    (
        "process-inbox",
        &[
            "d1e35bc1f534aa20a94d74a2246af294a81011c462636b4b8819aece75db4ec7",
            "77a9e48d3861ae9d8d19ab6ec3a11b7c5adbb2ff2f1f9fd6b6439cb09eb297c2",
        ],
    ),
];

/// Retire only known regular legacy files, after the replacement skill is installed.
/// Unknown edits and symlinks (including parents) stay untouched even with --force.
fn retire_legacy(root: &Path, name: &str) -> Result<()> {
    let old_name = name.strip_prefix("vagus-").expect("bundled skill prefix");
    let path = root.join(old_name).join("SKILL.md");
    let warn = || {
        eprintln!(
            "  preserved legacy skill {}; reconcile manually to avoid duplicate activation",
            path.display()
        );
    };
    if path.ancestors().any(is_symlink) {
        warn();
        return Ok(());
    }
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", path.display())),
    };
    if !metadata.is_file() {
        warn();
        return Ok(());
    }
    let body = std::fs::read(&path)?;
    let digest = sha256_hex(&body);
    let known = LEGACY
        .iter()
        .any(|(n, hashes)| *n == old_name && hashes.contains(&digest.as_str()));
    if !known {
        warn();
        return Ok(());
    }

    // A sibling directory with no SKILL.md files is outside the skills discovery tree.
    // create_new refuses collisions, including dangling symlinks; no old backup is overwritten.
    let backup_dir = root
        .parent()
        .context("skills directory has no parent")?
        .join(".vagus-skill-backups");
    anyhow::ensure!(
        !backup_dir.ancestors().any(is_symlink),
        "legacy backup directory has a symlink: {}",
        backup_dir.display()
    );
    std::fs::create_dir_all(&backup_dir)?;
    let backup = backup_dir.join(format!("{old_name}.SKILL.md.bak"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .with_context(|| {
            format!(
                "backing up {} to {}; original preserved",
                path.display(),
                backup.display()
            )
        })?;
    file.write_all(&body)?;
    file.sync_all()?;
    std::fs::remove_file(&path).with_context(|| format!("retiring {}", path.display()))?;
    println!(
        "  retired legacy {} (backup: {})",
        path.display(),
        backup.display()
    );
    // Leave the directory and all companion files alone.
    Ok(())
}

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
    let root = std::path::absolute(skills_dir(agent, override_dir)?)?;
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
                retire_legacy(&root, s.name)?;
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
        retire_legacy(&root, s.name)?;
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

    use crate::util::testdir::TempDir;

    const LEGACY_BODIES: &[(&str, &str)] = &[
        (
            "create-note",
            include_str!("fixtures/legacy-skills/create-note.md"),
        ),
        ("search", include_str!("fixtures/legacy-skills/search.md")),
        (
            "process-inbox",
            include_str!("fixtures/legacy-skills/process-inbox.md"),
        ),
    ];

    fn put(root: &Path, name: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn install_renamed_inventory_and_retire_known_legacy_idempotently() {
        for agent in [Agent::Claude, Agent::Pi] {
            let dir = TempDir::new("skill-upgrade");
            let parent = dir.path().canonicalize().unwrap();
            let root = parent.join("skills");
            for (name, body) in LEGACY_BODIES {
                put(&root, name, body);
                std::fs::write(root.join(name).join("custom.txt"), "keep me").unwrap();
            }
            for _ in 0..2 {
                install(agent, Some(root.clone()), false).unwrap();
                for skill in BUNDLED {
                    assert_eq!(
                        std::fs::read_to_string(root.join(skill.name).join("SKILL.md")).unwrap(),
                        skill.body
                    );
                }
                for (name, body) in LEGACY_BODIES {
                    assert!(!root.join(name).join("SKILL.md").exists());
                    assert_eq!(
                        std::fs::read_to_string(
                            parent
                                .join(".vagus-skill-backups")
                                .join(format!("{name}.SKILL.md.bak"))
                        )
                        .unwrap(),
                        *body
                    );
                    assert_eq!(
                        std::fs::read_to_string(root.join(name).join("custom.txt")).unwrap(),
                        "keep me"
                    );
                }
            }
        }
    }

    #[test]
    fn install_preserves_unknown_and_custom_legacy_even_when_forced() {
        let dir = TempDir::new("skill-custom-legacy");
        let root = dir.path().canonicalize().unwrap().join("skills");
        let custom = format!("{}\nPersonal instruction\n", LEGACY_BODIES[0].1);
        let path = put(&root, "create-note", &custom);
        let unrelated = put(&root, "search", "My unrelated search skill");
        for force in [false, true] {
            install(Agent::Pi, Some(root.clone()), force).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), custom);
            assert_eq!(
                std::fs::read_to_string(&unrelated).unwrap(),
                "My unrelated search skill"
            );
        }
    }

    #[test]
    fn install_keeps_new_name_custom_edit_as_backup() {
        let dir = TempDir::new("skill-custom-current");
        let root = dir.path().canonicalize().unwrap().join("skills");
        put(&root, "vagus-search", "My customized Vagus search");
        install(Agent::Pi, Some(root.clone()), false).unwrap();
        install(Agent::Pi, Some(root.clone()), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("vagus-search/SKILL.md.bak")).unwrap(),
            "My customized Vagus search"
        );
    }

    #[test]
    fn legacy_backup_collision_or_failure_preserves_original() {
        for collision in [false, true] {
            let dir = TempDir::new("skill-backup-failure");
            let parent = dir.path().canonicalize().unwrap();
            let root = parent.join("skills");
            let original = put(&root, "create-note", LEGACY_BODIES[0].1);
            let backup_dir = parent.join(".vagus-skill-backups");
            let blocker = if collision {
                std::fs::create_dir_all(&backup_dir).unwrap();
                backup_dir.join("create-note.SKILL.md.bak")
            } else {
                backup_dir
            };
            std::fs::write(&blocker, "prior data").unwrap();
            assert!(install(Agent::Pi, Some(root.clone()), true).is_err());
            assert_eq!(
                std::fs::read_to_string(original).unwrap(),
                LEGACY_BODIES[0].1
            );
            assert_eq!(std::fs::read_to_string(blocker).unwrap(), "prior data");
        }
    }

    #[cfg(unix)]
    #[test]
    fn legacy_symlinks_and_symlinked_parents_are_never_retired() {
        use std::os::unix::fs::symlink;
        for kind in ["file", "directory", "parent", "backup"] {
            let dir = TempDir::new("skill-legacy-symlink");
            let parent = dir.path().canonicalize().unwrap();
            let real = parent.join("real");
            let original = put(&real, "create-note", LEGACY_BODIES[0].1);
            let root = parent.join("skills");
            std::fs::create_dir_all(&root).unwrap();
            match kind {
                "file" => {
                    std::fs::create_dir_all(root.join("create-note")).unwrap();
                    symlink(&original, root.join("create-note/SKILL.md")).unwrap();
                }
                "directory" => symlink(real.join("create-note"), root.join("create-note")).unwrap(),
                "parent" => {
                    std::fs::remove_dir(&root).unwrap();
                    symlink(&real, &root).unwrap();
                }
                "backup" => {
                    put(&root, "create-note", LEGACY_BODIES[0].1);
                    symlink(&real, parent.join(".vagus-skill-backups")).unwrap();
                }
                _ => unreachable!(),
            }
            for force in [false, true] {
                let result = install(Agent::Pi, Some(root.clone()), force);
                assert_eq!(result.is_err(), kind == "backup");
                assert_eq!(
                    std::fs::read_to_string(&original).unwrap(),
                    LEGACY_BODIES[0].1
                );
                assert_eq!(
                    std::fs::read_to_string(root.join("create-note/SKILL.md")).unwrap(),
                    LEGACY_BODIES[0].1
                );
            }
        }
    }

    #[test]
    fn skill_trigger_and_safety_contracts_are_explicit() {
        // Static wording fixtures, not measurements of a host model's actual routing.
        let capture = BUNDLED
            .iter()
            .find(|s| s.name == "vagus-create-note")
            .unwrap();
        let search = BUNDLED.iter().find(|s| s.name == "vagus-search").unwrap();
        let inbox = BUNDLED
            .iter()
            .find(|s| s.name == "vagus-process-inbox")
            .unwrap();
        for (skill, phrases) in [
            (
                capture,
                &[
                    "make a note",
                    "a note of this finding",
                    "save this for later",
                    "my notes",
                    "do not create without capture intent",
                ][..],
            ),
            (
                search,
                &[
                    "find that idea in my notes",
                    "what did I write about X?",
                    "look up a note",
                    "Not for creating a note",
                ][..],
            ),
            (
                inbox,
                &[
                    "organize my notes",
                    "explicit invocation",
                    "confirmation for each move",
                ][..],
            ),
        ] {
            let desc = description(skill.body).unwrap();
            for phrase in phrases
                .iter()
                .chain(["repo documentation", "release notes", "another notes app"].iter())
            {
                assert!(desc.contains(phrase), "{} missing {phrase}", skill.name);
            }
            assert!(skill.body.contains(&format!("name: {}\n", skill.name)));
            assert!(
                skill
                    .body
                    .contains(&format!("skills/{}/SKILL.md", skill.name))
            );
            assert!(skill.body.contains(&format!(
                "disable-model-invocation: {}",
                skill.name == "vagus-process-inbox"
            )));
        }
        assert!(capture.body.contains("vagus add-note"));
        assert!(capture.body.contains("before retrying"));
        assert!(
            capture
                .body
                .contains("Do **not** hand-write YAML frontmatter")
        );
        assert!(capture.body.contains("/skill:vagus-process-inbox"));
        assert!(inbox.body.contains("Never move without an OK"));
        assert!(search.body.contains("No third retrieval"));
        assert!(!search.body.contains("tick it anyway"));
        assert!(search.body.contains("The step-4 retry remains unticked"));
        assert!(search.body.contains("preserve the exact `--since` window"));
        assert!(search.body.contains("Body evidence wins"));
        assert!(search.body.contains("Never pad"));
        assert!(
            search
                .body
                .contains("not dates mentioned inside note bodies")
        );
    }

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
        let search = BUNDLED.iter().find(|s| s.name == "vagus-search").unwrap();
        assert!(
            search.body.contains("--tick-provenance"),
            "search skill lost explicit provenance retrieval"
        );
        assert!(
            search.body.contains("vagus tick --events"),
            "search skill no longer atomically records cited events"
        );
        assert!(
            !search.body.contains("--store-query"),
            "search skill must not opt into query-content storage"
        );
        assert!(
            search.body.contains("--since <duration>"),
            "search skill must teach agents native time filtering"
        );
        assert!(
            search
                .body
                .contains("`--since` search must omit `--tick-provenance`"),
            "filtered search must not claim unsupported rank provenance"
        );

        let process_inbox = BUNDLED
            .iter()
            .find(|s| s.name == "vagus-process-inbox")
            .unwrap();
        assert!(
            process_inbox
                .body
                .contains("inbox --json --since <duration>"),
            "vagus-process-inbox skill must teach agents native time filtering"
        );
    }
}
