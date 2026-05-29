# vagus skills

Three Claude Code skills that drive the `vagus` CLI. They shell out to `vagus`, which must be on
`PATH` (`cargo install --path .`). No bundled scripts — the CLI is the one implementation.

- **`create-note`** — capture a note from a session into the inbox: `/create-note "title"`.
- **`search`** — hybrid search the vault: `/search <query>`.
- **`process-inbox`** — assisted PARA filing of inbox notes: `/process-inbox` (manual-trigger only —
  `disable-model-invocation: true`, since it moves files).

## Install

These files are **embedded in the `vagus` binary** (`include_str!`), so the supported install is:

```sh
vagus skills install        # writes them into ~/.claude/skills (idempotent; safe to re-run)
vagus skills list           # bundled skills + install status
```

### Contributing to a skill

Edit `skills/<name>/SKILL.md` here and rebuild — that updates the embedded copy. To live-test your
edits without rebuilding/installing each time, symlink the source into your skills dir instead:

```sh
mkdir -p ~/.claude/skills
for s in create-note search process-inbox; do
  ln -sfn "$PWD/skills/$s" ~/.claude/skills/"$s"
done
```

(`vagus skills install` deliberately **skips symlinks**, so this dev setup and the installed copies
don't fight.) Frontmatter follows the current Claude Code conventions
(`code.claude.com/docs/en/skills.md`): `description` drives auto-invocation, args are 0-based (`$0`).
