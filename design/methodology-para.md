# PARA / CODE — domain-model reference

Why the vault is shaped the way it is. Source: Tiago Forte, *Building a Second Brain*
([PARA](https://fortelabs.com/blog/para/), [CODE/BASB](https://fortelabs.com/blog/basb/));
MOCs from Nick Milo (Linking Your Thinking); atomic notes from Zettelkasten.

## PARA: organize by actionability, not topic

The central claim: sort information by **how soon/actively you need it**, not by subject. Four buckets,
decreasing actionability:

- **Projects** — a goal with an end state and (ideally) a deadline. *"Can I picture it done?"* → Project.
- **Areas** — an ongoing responsibility with a standard to maintain, no end (Health, Finances, a role).
- **Resources** — topics of ongoing interest, not tied to a current obligation (reference material).
- **Archive** — inactive items from the other three. **Archive, never delete** — the reuse-on-revival
  payoff is the point.

**Filing rule (first match wins):** which *project* → else which *area* → else which *resource* → else
*archive*. Bias toward the most actionable home so notes surface where work happens.

**Lifecycle is a flow, not a filing cabinet.** Notes migrate as actionability changes (Project→Archive
on completion, Resource→Project when it becomes actionable, Archive→Project on revival). In `vagus` a
lifecycle move is a **whole-folder `mv`**, so per-project folders matter.

## CODE: the processing pipeline

- **Capture** — save anything that resonates, immediately, to **one place: the inbox**. Bar = "does this
  resonate?", not "will I use it?".
- **Organize** — move inbox items into PARA by actionability. This is `vagus`'s assisted `/process-inbox`.
- **Distill** — progressively summarize (bold → highlight → one-line summary) so future-you skims fast.
- **Express** — turn notes into output. The point of the whole system.

**The inbox is temporary staging.** Its job is to decouple fast, emotional capture from deliberate
filing. Success = periodically processing it toward empty (a weekly review cadence). `vagus`'s
`00-Inbox/` is exactly this; `/process-inbox` is the ritual, made low-effort by having Claude propose
destinations.

## Vault conventions adopted

- **Numeric-prefixed top-level folders** so they sort in workflow order, not alphabetically:
  `00-Inbox / 10-Projects / 20-Areas / 30-Resources / 40-Archive` (+ optional `50-Meta`).
- **Per-project folders**, each with a same-named hub/MOC note → lifecycle `mv` drags all related notes.
- **Folders carry actionability; a small flat tag set carries topic.** Don't encode topic in the folder
  tree (that's the over-categorization trap PARA avoids).
- **Capture-time frontmatter is minimal**: `created`, `status: inbox`, `source` (the one
  irrecoverable-later field). Everything else (title cleanup, tags, type, final status) is added during
  the Organize/Distill pass. A bare note with *no* frontmatter is still valid.
- **Link by `[[title]]`/alias, not by path**, so PARA moves don't break the recall graph.

## Retrieval implications

- **Full-text (BM25)** = known-item retrieval ("the note where I wrote `useLayoutEffect`").
- **Semantic (embeddings)** = discovery ("what do I know about render performance").
  They're complementary, which is why `vagus` fuses both (RRF). Full-text is the always-available floor;
  the semantic layer is rebuildable and never a single point of failure for your own notes.
- **Atomic notes** (one idea per note) make both lexical and semantic retrieval sharper, and give
  embeddings a single clean concept to vectorize.
