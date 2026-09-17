# vagus skills

Three [Agent Skills](https://agentskills.io/) that drive the `vagus` CLI in Claude Code or pi. They
shell out to `vagus`, which must be on `PATH`. No bundled scripts — the CLI is the one implementation.

- **`vagus-create-note`** — capture a note from a session into the inbox (`/vagus-create-note "title"` in Claude
  Code; `/skill:vagus-create-note title` in pi).
- **`vagus-search`** — hybrid search the vault, translating requests such as “from the last 3 months” into
  a native `--since 3m` retrieval filter (`/vagus-search <query>`; `/skill:vagus-search <query>` in pi).
- **`vagus-process-inbox`** — assisted PARA filing, including time-bounded passes such as the last five days
  (`/vagus-process-inbox`; `/skill:vagus-process-inbox` in pi), manual-trigger only because it moves files
  (`disable-model-invocation: true`).

Generic note intent defaults to Vagus: “make a note of this finding” / “save this for later” capture
conversation content; “find that idea in my notes” / “what did I write about X?” retrieve existing
notes. No Vagus/vault wording is required, but a question about notes is not permission to create one.
Explicit destinations win: “write docs/notes.md in this repo”, “add release notes”, or another notes
app do not route to Vagus. “Organize my notes” may prompt an offer to invoke `vagus-process-inbox`,
never automatic invocation or moves; confirm each move. These are intended routing contracts, not
measured model activation guarantees.

## Install

These files are **embedded in the `vagus` binary** (`include_str!`), so the supported install is:

```sh
vagus skills install                 # Claude Code (default): ~/.claude/skills
vagus skills install --agent pi      # pi: ~/.pi/agent/skills
vagus skills list --agent pi         # bundled skills + pi install status
```

The defaults honor `CLAUDE_CONFIG_DIR` and `PI_CODING_AGENT_DIR`; `--dir` overrides either one.
Install is idempotent and safe to re-run. Pi loads the installed skills in new sessions; use
`/reload` in a running session.

### Upgrading the old names

`create-note`, `search`, and `process-inbox` are now `vagus-create-note`, `vagus-search`, and
`vagus-process-inbox`. After installing each new copy, the installer retires only byte-exact,
recognized recent bundled legacy files. It first saves a non-overwriting backup as
`../.vagus-skill-backups/<old-name>.SKILL.md.bak` beside the skills directory, outside discovery.
Companion files stay in place. A backup collision or failure leaves the legacy file intact and
reports an error; reconcile that backup before retrying.

Custom/unknown legacy files and symlinks (including symlinked parent directories) are preserved even
with `--force`, with a warning to reconcile them manually; they can still cause duplicate activation.
Older unrecognized releases follow that same conservative rule. Transfer personal edits deliberately
before retiring a custom legacy skill. The usual new-name install behavior is unchanged: hand edits
are backed up to `SKILL.md.bak` unless `--force`, and symlinks are skipped unless `--force`.

### Contributing to a skill

Edit `skills/<name>/SKILL.md` here and rebuild — that updates the embedded copy. Every `SKILL.md`
opens with a canonical-source comment pointing back here: agents audit the installed copies in place,
and the next `vagus skills install` replaces a hand edit there and moves it to `SKILL.md.bak`. To
live-test your edits without rebuilding/installing each time, symlink the source into your skills dir
instead:

```sh
# Claude Code
mkdir -p ~/.claude/skills
for s in vagus-create-note vagus-search vagus-process-inbox; do
  ln -sfn "$PWD/skills/$s" ~/.claude/skills/"$s"
done

# pi (or use $PI_CODING_AGENT_DIR/skills when that variable is set)
mkdir -p ~/.pi/agent/skills
for s in vagus-create-note vagus-search vagus-process-inbox; do
  ln -sfn "$PWD/skills/$s" ~/.pi/agent/skills/"$s"
done
```

(`vagus skills install` deliberately **skips symlinks**, so this dev setup and the installed copies
don't fight.) The shared frontmatter follows the Agent Skills standard; pi ignores additional
Claude Code fields it does not use. Explicit pi invocations append arguments as plain text after the
expanded skill block rather than substituting Claude Code's 0-based `$0` placeholder.
