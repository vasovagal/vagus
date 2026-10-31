# vagus skills

Install the three bundled [Agent Skills](https://agentskills.io/) to capture, retrieve, and file
Vagus notes in Claude Code or pi. They call `vagus` on `PATH`; no separate scripts implement the CLI.
[Install](#install), [migrate old names](#upgrading-the-old-names), or [edit a skill](#contributing-to-a-skill).

| Skill | Claude Code | pi | Action |
|---|---|---|---|
| `vagus-create-note` | `/vagus-create-note "title"` | `/skill:vagus-create-note title` | Capture into the inbox |
| `vagus-search` | `/vagus-search <query>` | `/skill:vagus-search <query>` | Retrieve notes; pass requested time windows to `--since` |
| `vagus-process-inbox` | `/vagus-process-inbox` | `/skill:vagus-process-inbox` | Propose PARA filing; confirm each move |

These names describe current source. Release **v0.14.1** bundles `create-note`, `search`, and
`process-inbox`; installing that binary again does not rename them. Use `vagus skills list` to check
your installed inventory.

Generic “make a note” or “save this” requests capture into Vagus; “find that idea in my notes”
retrieves without creating a note. Explicit repo files, release notes, or another notes app override
this default. “Organize my notes” may prompt an offer to invoke `vagus-process-inbox`, which requires
manual invocation (`disable-model-invocation: true`) and confirmation of each move. These are routing
instructions, not measured guarantees of model activation.

Search passes matching chunk bodies to the host agent and permits reading a full note to resolve
ambiguity. Vagus's local retrieval does not make the host agent or its model provider offline.

## Install

Skills are embedded in the binary with `include_str!`; no clone is needed:

```sh
vagus skills install                 # Claude Code (default): ~/.claude/skills
vagus skills install --agent pi      # pi: ~/.pi/agent/skills
vagus skills list --agent pi         # bundled skills + pi install status
```

`CLAUDE_CONFIG_DIR` and `PI_CODING_AGENT_DIR` override the respective config roots; `--dir` selects
an installation directory directly. After upgrading Vagus, install again for each agent. Pi loads
skills at session startup; use `/reload` in an existing session.

For the current names, installation leaves identical files alone, backs up hand-edited `SKILL.md`
to `SKILL.md.bak`, and skips symlinks. `--force` replaces symlinks and divergent files without backup.
Preserve any existing `.bak` you need before another update: it is not a versioned backup history.

### Upgrading the old names

In current source, `create-note`, `search`, and `process-inbox` become `vagus-create-note`,
`vagus-search`, and `vagus-process-inbox`. After installing each new copy, the installer retires only
byte-exact, recognized recent bundled legacy files. It first saves a non-overwriting backup as
`../.vagus-skill-backups/<old-name>.SKILL.md.bak` beside the skills directory, outside discovery.
Companion files stay in place. A backup collision or failure leaves the legacy file intact and
reports an error; reconcile that backup before retrying.

Custom/unknown legacy files and symlinks (including symlinked parent directories) are preserved even
with `--force`, with a warning to reconcile them manually; they can still cause duplicate activation.
Older unrecognized releases follow that same conservative rule. Transfer personal edits before
retiring a custom legacy skill.

### Contributing to a skill

Edit `skills/<name>/SKILL.md` here and rebuild to update the embedded copy. Each file has a
canonical-source comment pointing here. To test without rebuilding, run the following from the
repository root, using the default agent directories. Reconcile any existing real skill directory
before creating its symlink:

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

`vagus skills install` skips these symlinks unless `--force` is used. Shared frontmatter follows
the Agent Skills standard; pi ignores unsupported Claude Code fields. Explicit pi invocations append
arguments as plain text after the expanded skill block; Claude Code substitutes the 0-based `$0`
placeholder.
