# vagus skills

Three [Agent Skills](https://agentskills.io/) that drive the `vagus` CLI in Claude Code or pi. They
shell out to `vagus`, which must be on `PATH`. No bundled scripts — the CLI is the one implementation.

- **`create-note`** — capture a note from a session into the inbox (`/create-note "title"` in Claude
  Code; `/skill:create-note title` in pi).
- **`search`** — hybrid search the vault, translating requests such as “from the last 3 months” into
  a native `--since 3m` retrieval filter (`/search <query>`; `/skill:search <query>` in pi).
- **`process-inbox`** — assisted PARA filing, including time-bounded passes such as the last five days
  (`/process-inbox`; `/skill:process-inbox` in pi), manual-trigger only because it moves files
  (`disable-model-invocation: true`).

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

### Contributing to a skill

Edit `skills/<name>/SKILL.md` here and rebuild — that updates the embedded copy. To live-test your
edits without rebuilding/installing each time, symlink the source into your skills dir instead:

```sh
# Claude Code
mkdir -p ~/.claude/skills
for s in create-note search process-inbox; do
  ln -sfn "$PWD/skills/$s" ~/.claude/skills/"$s"
done

# pi (or use $PI_CODING_AGENT_DIR/skills when that variable is set)
mkdir -p ~/.pi/agent/skills
for s in create-note search process-inbox; do
  ln -sfn "$PWD/skills/$s" ~/.pi/agent/skills/"$s"
done
```

(`vagus skills install` deliberately **skips symlinks**, so this dev setup and the installed copies
don't fight.) The shared frontmatter follows the Agent Skills standard; pi ignores additional
Claude Code fields it does not use. Explicit pi invocations append arguments as `User:` text rather
than substituting Claude Code's 0-based `$0` placeholder.
