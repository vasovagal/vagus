//! Wire schema for the **vagus plugin contract** — the language-agnostic protocol a `vagus-<name>`
//! plugin speaks back to `vagus` core over stdout.
//!
//! This crate is intentionally tiny and dependency-light (serde only). It is shared by **both**
//! `vagus` core (which parses the events) and the `vagus-plugin` SDK (which emits them) so the
//! on-the-wire shape can never drift between the two. Non-Rust plugins ignore this crate and just
//! emit the documented JSON; see `docs/plugin-contract.md`.
//!
//! ## The contract in one paragraph
//! Core spawns `vagus-<name>` as a child with the subcommand name stripped from argv, stdin/stderr
//! inherited, stdout piped, and the [env vars](#constants) set. The plugin streams newline-delimited
//! JSON [`Event`]s on **stdout** (machine channel) and writes human logs to **stderr**. Any stdout
//! line that does not parse as a known [`Event`] is echoed verbatim by core (so trivial text-only
//! plugins still work). On a clean exit, core indexes every [`Event::Note`] path it saw.

use serde::{Deserialize, Serialize};

/// Contract version core advertises via [`ENV_CONTRACT`]. Bumped only on breaking changes; the event
/// schema is otherwise extended additively (new optional fields, new `#[serde(other)]`-tolerated
/// variants), so a plugin built against v1 keeps working.
pub const CONTRACT_VERSION: u32 = 1;

/// Absolute path to the `vagus` binary, so a plugin can call back (e.g. `$VAGUS index`) without
/// guessing where core lives. Set by core for every plugin invocation.
pub const ENV_VAGUS_BIN: &str = "VAGUS";
/// Absolute path to the resolved vault root (the `~/brain` symlink target). Plugins write Markdown
/// here and **must not** write anything else (guardrail G1/G16).
pub const ENV_VAULT: &str = "VAGUS_VAULT";
/// vagus data dir (`~/.local/share/vagus`) — informational; plugins keep their *own* state under
/// their own XDG dir, never here and never in the vault.
pub const ENV_DATA_DIR: &str = "VAGUS_DATA_DIR";
/// vagus config dir — informational.
pub const ENV_CONFIG_DIR: &str = "VAGUS_CONFIG_DIR";
/// Core's version string.
pub const ENV_VERSION: &str = "VAGUS_VERSION";
/// Set to [`PROTOCOL_NDJSON`] when core is the parent and wants the NDJSON event stream. When unset,
/// the plugin was run standalone (directly, not via `vagus`) and should print human output and
/// self-index via `$VAGUS index`.
pub const ENV_PROTOCOL: &str = "VAGUS_PLUGIN_PROTOCOL";
/// Decimal [`CONTRACT_VERSION`] core supports, for additive compat checks by the plugin.
pub const ENV_CONTRACT: &str = "VAGUS_PLUGIN_CONTRACT";

/// The only protocol value currently defined for [`ENV_PROTOCOL`].
pub const PROTOCOL_NDJSON: &str = "ndjson";

/// Reserved discovery subcommand: `vagus-<name> __describe` prints a one-line summary on stdout for
/// `vagus plugins`. Used by both core (caller) and the SDK (callee).
pub const DESCRIBE_SUBCOMMAND: &str = "__describe";

/// Severity for a [`Event::Log`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// What happened to a note file, so core knows whether to (re)index or drop it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoteAction {
    Write,
    Append,
    Delete,
}

/// One newline-delimited JSON record on a plugin's stdout. Externally tagged on `type`.
///
/// **Streaming** = emit `Progress`/`Note` as work happens, then a final `Result`. **Batch** = emit
/// only the final `Result`. Same schema; the plugin chooses.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A structured log line core may render uniformly (human-readable logs also go to stderr).
    Log { level: LogLevel, msg: String },
    /// Progress for a uniform progress indicator. `total` omitted ⇒ indeterminate.
    Progress {
        done: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        msg: String,
    },
    /// A vault note the plugin created/changed. `path` is **relative to the vault root**. Core
    /// collects these and runs one incremental index after the plugin exits cleanly.
    Note {
        path: String,
        #[serde(default = "default_note_action")]
        action: NoteAction,
    },
    /// Terminal event. `summary`/`data` are plugin-defined JSON. `no_index: true` tells core to skip
    /// the post-run index pass (e.g. a `--dry-run`).
    Result {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "is_false")]
        no_index: bool,
    },
}

fn default_note_action() -> NoteAction {
    NoteAction::Write
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Event {
    /// Parse one stdout line as an [`Event`]. Returns `None` when the line is not a known event —
    /// the caller (core) then echoes that line verbatim. Blank/whitespace lines are `None`.
    pub fn parse_line(line: &str) -> Option<Event> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        serde_json::from_str::<Event>(line).ok()
    }

    /// Serialize to a single NDJSON line (no trailing newline).
    pub fn to_line(&self) -> String {
        // Events are flat and always serialize; unwrap is safe.
        serde_json::to_string(self).expect("Event serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_each_variant() {
        for ev in [
            Event::Log {
                level: LogLevel::Warn,
                msg: "hi".into(),
            },
            Event::Progress {
                done: 3,
                total: Some(10),
                msg: "fetching".into(),
            },
            Event::Note {
                path: "30-Resources/slack/x.md".into(),
                action: NoteAction::Append,
            },
            Event::Result {
                ok: true,
                summary: Some(serde_json::json!({"notes": 4})),
                data: None,
                no_index: false,
            },
        ] {
            let line = ev.to_line();
            let back = Event::parse_line(&line).expect("parses");
            assert_eq!(format!("{ev:?}"), format!("{back:?}"));
        }
    }

    #[test]
    fn progress_total_optional() {
        let ev = Event::parse_line(r#"{"type":"progress","done":1}"#).unwrap();
        match ev {
            Event::Progress { done, total, msg } => {
                assert_eq!(done, 1);
                assert!(total.is_none());
                assert!(msg.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn note_action_defaults_to_write() {
        let ev = Event::parse_line(r#"{"type":"note","path":"a.md"}"#).unwrap();
        assert!(matches!(
            ev,
            Event::Note {
                action: NoteAction::Write,
                ..
            }
        ));
    }

    #[test]
    fn non_events_are_none() {
        assert!(Event::parse_line("just some text").is_none());
        assert!(Event::parse_line(r#"["a","b"]"#).is_none()); // a plugin's own --json payload
        assert!(Event::parse_line(r#"{"type":"bogus"}"#).is_none());
        assert!(Event::parse_line("   ").is_none());
    }
}
