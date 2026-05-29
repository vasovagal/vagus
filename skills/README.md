# vagus skills

Three Claude Code skills that drive the `vagus` CLI. They shell out to `vagus`, which must be on
`PATH` (`cargo install --path .`). No bundled scripts — the CLI is the one implementation.

- **`create-note`** — capture a note from a session into the inbox: `/create-note "title"`.
- **`search`** — hybrid search the vault: `/search <query>`.
- **`process-inbox`** — assisted PARA filing of inbox notes: `/process-inbox` (manual-trigger only —
  `disable-model-invocation: true`, since it moves files).

## Install

Symlink them into your user skills dir so edits here stay live:

```sh
mkdir -p ~/.claude/skills
for s in create-note search process-inbox; do
  ln -sfn "$PWD/skills/$s" ~/.claude/skills/"$s"
done
```

(Or copy the directories if you'd rather not symlink.) Frontmatter follows the current Claude Code
skill conventions (`code.claude.com/docs/en/skills.md`): `description` drives auto-invocation, args are
0-based (`$0`).
