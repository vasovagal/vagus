# ADR 0008 — Naming: `vagus` (tool) under the `vasovagal` org

- **Status:** Accepted (2026-05-29)

## Context

"Second brain" is too generic. The author's naming style favors a pun/twist on the underlying
domain or tech (e.g. *cheminée* = chem + the French-built tantivy it rides on; *knievel* ≈ Kevel),
something evocative, or a smooth neologism hinting at function.

## Decision

- **Tool / crate / binary: `vagus`** — the vagus nerve, which connects the gut (the literal "second
  brain", the enteric nervous system) to the head. A sly insider pun on the whole premise; short,
  pronounceable, and the crate name is free on crates.io.
- **GitHub org: `vasovagal`** — *vasovagal* (as in vasovagal syncope) derives from the **vagus** nerve,
  so the org is the parent term and `vagus` nests under it: `vasovagal/vagus`. The org handle was free.
- **Command typed as `vagus`** (5 letters; a shell alias is trivial if a shorter one is ever wanted).
- **Cloned to `~/code/vasovagal/vagus`** — org-dir convention mirroring `~/code/assaydepot/`.

## Availability (checked 2026-05-29)

- `github.com/vasovagal` org — free (now owned). `vasovagal/vagus` repo — created.
- `vagus` crate on crates.io — free. (`vagus` as a *global* GitHub user/org handle is taken, but
  irrelevant: repo names are org-scoped.)

## Consequences

- Personal org, separate from the work org `scientist-hq`.
- Other neuro/second-brain candidates considered and parked: *engram, mneme, cajal, pensieve, mentat,
  tulpa, drey, midden, grenier*.
