//! SDK for building **vagus plugins** — standalone `vagus-<name>` binaries that `vagus` core
//! dispatches to (the git/`kubectl`/`gh`-extension pattern). See `docs/plugin-contract.md`.
//!
//! Using this crate is optional: a plugin in any language can speak the protocol directly. In Rust it
//! saves you from re-implementing the wire format, the env contract, and the vault-write guard.
//!
//! ## Minimal plugin
//! ```no_run
//! use vagus_plugin::{Emitter, describe, is_describe};
//!
//! fn main() -> std::io::Result<()> {
//!     let args: Vec<String> = std::env::args().skip(1).collect();
//!     if is_describe(&args) {
//!         describe("vagus-hello — example plugin");
//!         return Ok(());
//!     }
//!     let mut out = Emitter::from_env();
//!     out.progress(1, Some(1), "working");
//!     out.write_note("30-Resources/hello/note.md", "# hi\n\nfrom a plugin\n")?;
//!     out.result_ok(serde_json::json!({ "notes": 1 }));
//!     Ok(())
//! }
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};

use protocol::{Event, LogLevel, NoteAction};
pub use vagus_plugin_protocol as protocol;

pub use protocol::DESCRIBE_SUBCOMMAND;

/// True when the first arg is the [`DESCRIBE_SUBCOMMAND`].
pub fn is_describe(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some(DESCRIBE_SUBCOMMAND)
}

/// Print a one-line description for `vagus plugins` discovery.
pub fn describe(text: &str) {
    println!("{text}");
}

/// True when core launched us in NDJSON protocol mode (vs. a direct standalone run).
pub fn protocol_mode() -> bool {
    std::env::var(protocol::ENV_PROTOCOL).as_deref() == Ok(protocol::PROTOCOL_NDJSON)
}

/// Path to the `vagus` binary for callbacks (e.g. indexing standalone). Falls back to `vagus` on PATH.
pub fn vagus_bin() -> PathBuf {
    std::env::var_os(protocol::ENV_VAGUS_BIN)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vagus"))
}

/// Resolved vault root. Prefers `$VAGUS_VAULT` (set by core); falls back to the documented `~/brain`
/// symlink for standalone runs. Returns `None` only if neither is resolvable.
pub fn vault() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(protocol::ENV_VAULT) {
        return Some(PathBuf::from(v));
    }
    dirs::home_dir().map(|h| h.join("brain"))
}

/// This plugin's own config dir: `~/.config/vagus-<name>/` (XDG, even on macOS — to sit alongside
/// vagus core, which deliberately uses XDG paths). Core never reads it. Respects `$XDG_CONFIG_HOME`.
pub fn config_dir(plugin_name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
    Some(base.join(format!("vagus-{plugin_name}")))
}

/// This plugin's own state/cache dir: `~/.local/share/vagus-<name>/` (XDG) — **outside iCloud** (G1).
/// Respects `$XDG_DATA_HOME`.
pub fn data_dir(plugin_name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))?;
    Some(base.join(format!("vagus-{plugin_name}")))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Emit NDJSON events on stdout (core is parsing them).
    Ndjson,
    /// Standalone run: render human text on stderr; the final result prints to stdout.
    Human,
}

/// Emits [`Event`]s to core (NDJSON mode) or renders them for a human (standalone mode).
///
/// Construct with [`Emitter::from_env`]. The mode is chosen by [`protocol_mode`]; callers write the
/// same code either way.
pub struct Emitter {
    mode: Mode,
}

impl Default for Emitter {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Emitter {
    pub fn from_env() -> Self {
        Self {
            mode: if protocol_mode() {
                Mode::Ndjson
            } else {
                Mode::Human
            },
        }
    }

    fn send(&self, ev: &Event) {
        match self.mode {
            Mode::Ndjson => {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{}", ev.to_line());
                let _ = out.flush();
            }
            Mode::Human => self.render_human(ev),
        }
    }

    fn render_human(&self, ev: &Event) {
        match ev {
            Event::Log { level, msg } => {
                let tag = match level {
                    LogLevel::Info => "info",
                    LogLevel::Warn => "warn",
                    LogLevel::Error => "error",
                };
                eprintln!("{tag}: {msg}");
            }
            Event::Progress { done, total, msg } => match total {
                Some(t) => eprintln!("[{done}/{t}] {msg}"),
                None => eprintln!("[{done}] {msg}"),
            },
            // Notes are silent in human mode (the file write is the visible effect).
            Event::Note { .. } => {}
            Event::Result { ok, summary, .. } => {
                let status = if *ok { "ok" } else { "FAILED" };
                match summary {
                    Some(s) => println!("{status}: {s}"),
                    None => println!("{status}"),
                }
            }
        }
    }

    pub fn log(&self, level: LogLevel, msg: impl Into<String>) {
        self.send(&Event::Log {
            level,
            msg: msg.into(),
        });
    }
    pub fn info(&self, msg: impl Into<String>) {
        self.log(LogLevel::Info, msg);
    }
    pub fn warn(&self, msg: impl Into<String>) {
        self.log(LogLevel::Warn, msg);
    }
    pub fn error(&self, msg: impl Into<String>) {
        self.log(LogLevel::Error, msg);
    }

    pub fn progress(&self, done: u64, total: Option<u64>, msg: impl Into<String>) {
        self.send(&Event::Progress {
            done,
            total,
            msg: msg.into(),
        });
    }

    /// Announce a note at `relpath` (relative to the vault root) so core can index it. Prefer
    /// [`Emitter::write_note`], which writes the file *and* emits this for you.
    pub fn note(&self, relpath: impl Into<String>, action: NoteAction) {
        self.send(&Event::Note {
            path: relpath.into(),
            action,
        });
    }

    /// Terminal success event with a JSON summary.
    pub fn result_ok(&self, summary: serde_json::Value) {
        self.send(&Event::Result {
            ok: true,
            summary: Some(summary),
            data: None,
            no_index: false,
        });
    }

    /// Terminal success event that tells core **not** to index afterwards (e.g. `--dry-run`).
    pub fn result_ok_no_index(&self, summary: serde_json::Value) {
        self.send(&Event::Result {
            ok: true,
            summary: Some(summary),
            data: None,
            no_index: true,
        });
    }

    /// Terminal failure event.
    pub fn result_err(&self, summary: serde_json::Value) {
        self.send(&Event::Result {
            ok: false,
            summary: Some(summary),
            data: None,
            no_index: true,
        });
    }

    /// Write a Markdown note into the vault and emit the matching [`Event::Note`].
    ///
    /// Guards (contract): `relpath` must end in `.md` and must not escape the vault root. Parent
    /// directories are created. Returns the absolute path written.
    pub fn write_note(&self, relpath: &str, body: &str) -> std::io::Result<PathBuf> {
        let abs = self.resolve_note_path(relpath)?;
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // A fresh write or an overwrite both index the same way → NoteAction::Write.
        std::fs::write(&abs, body)?;
        self.note(relpath, NoteAction::Write);
        Ok(abs)
    }

    /// Append to (or create) a Markdown note and emit [`NoteAction::Append`].
    pub fn append_note(&self, relpath: &str, body: &str) -> std::io::Result<PathBuf> {
        let abs = self.resolve_note_path(relpath)?;
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&abs)?;
        f.write_all(body.as_bytes())?;
        self.note(relpath, NoteAction::Append);
        Ok(abs)
    }

    fn resolve_note_path(&self, relpath: &str) -> std::io::Result<PathBuf> {
        use std::io::{Error, ErrorKind};
        let root = vault().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                "no vault: $VAGUS_VAULT unset and ~/brain not found",
            )
        })?;
        if !relpath.ends_with(".md") {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("plugins may only write .md files into the vault: {relpath:?}"),
            ));
        }
        let rel = Path::new(relpath);
        // Reject absolute paths and any `..` component — the note must stay under the vault (G1/G16).
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("note path must be vault-relative with no `..`: {relpath:?}"),
            ));
        }
        Ok(root.join(rel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_md_and_escapes() {
        // Force a known vault via env so the test is hermetic.
        unsafe {
            std::env::set_var(protocol::ENV_VAULT, "/tmp/vagus-test-vault");
        }
        let e = Emitter { mode: Mode::Human };
        assert!(e.resolve_note_path("notes/x.txt").is_err()); // not .md
        assert!(e.resolve_note_path("../escape.md").is_err()); // escapes vault
        assert!(e.resolve_note_path("/abs/x.md").is_err()); // absolute
        let ok = e.resolve_note_path("30-Resources/slack/a.md").unwrap();
        assert!(ok.ends_with("30-Resources/slack/a.md"));
        assert!(ok.starts_with("/tmp/vagus-test-vault"));
    }
}
