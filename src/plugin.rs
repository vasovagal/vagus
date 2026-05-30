//! External `vagus-<name>` plugin dispatch + discovery.
//!
//! Core spawns `vagus-<name>` as a child, streams its NDJSON events from stdout, renders progress to
//! stderr, and indexes whatever notes it wrote. See `docs/plugin-contract.md` and ADRs 0010/0011.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use vagus_plugin_protocol::{self as proto, Event, LogLevel};

use crate::config::Config;
use crate::index;

/// Dispatch `vagus <name> <rest…>` to `vagus-<name>` on `$PATH` (child + NDJSON event stream).
pub fn dispatch(cfg: &Config, argv: &[OsString]) -> Result<()> {
    let (name, rest) = argv.split_first().context("empty plugin invocation")?;
    let name = name.to_string_lossy();
    let bin = format!("vagus-{name}");

    let mut cmd = Command::new(&bin);
    cmd.args(rest)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped());
    export_env(&mut cmd, cfg);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "unknown subcommand '{name}': no builtin and no '{bin}' on PATH — see 'vagus plugins'"
        ),
        Err(e) => return Err(e).with_context(|| format!("spawning {bin}")),
    };

    let reader = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();

    let mut saw_note = false;
    let mut no_index = false;
    let mut result_ok = true;
    let mut result_summary: Option<serde_json::Value> = None;

    for line in reader.lines() {
        let line = line?;
        match Event::parse_line(&line) {
            Some(Event::Log { level, msg }) => {
                let tag = match level {
                    LogLevel::Info => "info",
                    LogLevel::Warn => "warn",
                    LogLevel::Error => "error",
                };
                let _ = writeln!(err, "{tag}: {msg}");
            }
            Some(Event::Progress { done, total, msg }) => {
                let _ = match total {
                    Some(t) => writeln!(err, "  [{done}/{t}] {msg}"),
                    None => writeln!(err, "  [{done}] {msg}"),
                };
            }
            // Any note (write/append/delete) means the vault changed → an index pass is warranted.
            Some(Event::Note { .. }) => saw_note = true,
            Some(Event::Result {
                ok,
                summary,
                no_index: ni,
                ..
            }) => {
                result_ok = ok;
                result_summary = summary;
                no_index = ni;
            }
            // Not a known event → echo verbatim (lets a text-only plugin work unchanged).
            None => {
                let _ = writeln!(out, "{line}");
            }
        }
    }

    let status = child.wait().with_context(|| format!("waiting on {bin}"))?;
    if !status.success() {
        // Propagate the plugin's own exit code.
        std::process::exit(status.code().unwrap_or(1));
    }
    if !result_ok {
        let detail = result_summary.map(|s| format!(": {s}")).unwrap_or_default();
        bail!("{bin} reported failure{detail}");
    }
    if let Some(s) = &result_summary {
        let _ = writeln!(err, "{name}: {s}");
    }

    // Core-side indexing: the plugin emits `note` events and never re-enters vagus itself.
    if saw_note && !no_index {
        let stats = index::run(cfg, false)?;
        println!(
            "indexed: {} new, {} changed, {} unchanged, {} removed",
            stats.new, stats.changed, stats.unchanged, stats.removed
        );
    }
    Ok(())
}

fn export_env(cmd: &mut Command, cfg: &Config) {
    if let Ok(exe) = std::env::current_exe() {
        cmd.env(proto::ENV_VAGUS_BIN, exe);
    }
    cmd.env(proto::ENV_VAULT, &cfg.vault);
    cmd.env(proto::ENV_DATA_DIR, &cfg.data_dir);
    // XDG-style, to match vagus core's own data dir (~/.local/share even on macOS).
    let cfg_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")));
    if let Some(d) = cfg_dir {
        cmd.env(proto::ENV_CONFIG_DIR, d.join("vagus"));
    }
    cmd.env(proto::ENV_VERSION, env!("CARGO_PKG_VERSION"));
    cmd.env(proto::ENV_PROTOCOL, proto::PROTOCOL_NDJSON);
    cmd.env(proto::ENV_CONTRACT, proto::CONTRACT_VERSION.to_string());
}

/// `vagus plugins`: list discovered `vagus-*` executables on `$PATH`. `builtins` is used to flag
/// plugins that are shadowed by a builtin subcommand (clap matches builtins first).
pub fn list(builtins: &[String]) -> Result<()> {
    let found = discover();
    if found.is_empty() {
        println!(
            "No plugins found. Drop a `vagus-<name>` executable on your PATH \
             (e.g. `brew install vagus-slack`)."
        );
        return Ok(());
    }
    println!("Plugins (vagus-<name> on PATH):\n");
    for p in &found {
        let desc = p.describe().map(|d| format!("  — {d}")).unwrap_or_default();
        let shadow = if builtins.iter().any(|b| b == &p.name) {
            "  (shadowed by builtin — not reachable via `vagus`)"
        } else {
            ""
        };
        println!("  {:<12} {}{desc}{shadow}", p.name, p.path.display());
    }
    Ok(())
}

struct Plugin {
    name: String,
    path: PathBuf,
}

impl Plugin {
    /// Best-effort one-liner via the `__describe` convention. A misbehaving plugin just yields no
    /// description (no timeout in v1 — plugins are expected to answer instantly).
    fn describe(&self) -> Option<String> {
        let out = Command::new(&self.path)
            .arg(proto::DESCRIBE_SUBCOMMAND)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
    }
}

/// Every `vagus-<name>` executable on `$PATH`, first-on-PATH winning, sorted by name.
fn discover() -> Vec<Plugin> {
    let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
    let Some(path) = std::env::var_os("PATH") else {
        return vec![];
    };
    for dir in std::env::split_paths(&path) {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let fname = entry.file_name();
            let fname = fname.to_string_lossy();
            let Some(name) = fname.strip_prefix("vagus-") else {
                continue;
            };
            // Skip build artifacts like `vagus-slack.d` and the empty case.
            if name.is_empty() || name.contains('.') {
                continue;
            }
            if !is_executable(&entry) {
                continue;
            }
            seen.entry(name.to_string()).or_insert_with(|| entry.path());
        }
    }
    seen.into_iter()
        .map(|(name, path)| Plugin { name, path })
        .collect()
}

#[cfg(unix)]
fn is_executable(entry: &std::fs::DirEntry) -> bool {
    use std::os::unix::fs::PermissionsExt;
    entry
        .metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(entry: &std::fs::DirEntry) -> bool {
    entry.metadata().map(|m| m.is_file()).unwrap_or(false)
}
