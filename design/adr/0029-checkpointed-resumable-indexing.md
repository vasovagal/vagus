# ADR 0029 — Checkpointed, resumable indexing

- **Status:** Accepted (2026-09-12)
- **Amends:** ADRs 0019 and 0022; amends G5 and G6

## Context

`index::run_timed` wrote SQLite as it went (`upsert_file`, `replace_chunks`, and `set_embedding` each
autocommit), but committed tantivy exactly once, after the loop, and saved the usearch sidecar once at
the end. A killed run could therefore leave files whose mtime/sha were blessed in SQLite, with complete
embeddings, and no BM25 docs. The next incremental run took the mtime fast path and skipped them for
good.

On 2026-09-10 a `vagus reindex` was killed about ten minutes in. SQLite kept 48 fully embedded files;
none of their tantivy docs had been committed. A later incremental `vagus index` rebuilt the sidecar
from BLOBs (size drift), indexed the other 666 files, and skipped the 48. End state: 714 files, 6,759
chunks all embedded, 6,759 usearch vectors, 6,241 tantivy docs — exactly 518 chunks missing from BM25,
while `vagus doctor` printed `[ok]` on every line because it never compared tantivy with SQLite. The
same hole applied to any killed incremental run that touched new or changed files, including the
implicit refresh in `vagus search`, `add-note`, and `file`. ADR 0022's NULL-embedding repair covered
only the vector store.

A killed reindex also left a gutted index that the next `vagus search` would silently finish, turning a
search into hours of embedding.

## Options considered

1. **One SQLite transaction per run, committed after tantivy.** The stores agree, but a kill throws
   away all progress (a multi-hour reindex restarts from zero) and the run holds one write transaction
   throughout.
2. **Write the `files` row only after the tantivy commit.** Chunks FK-reference `files(path)`, so the
   row must exist first; deferring it means buffering every chunk write across a batch.
3. **A checkpoint watermark** (last committed snapshot position). Meaningful only for one walk order
   over an unchanged vault, and says nothing about ordinary incremental runs.
4. **A `pending` marker on the file row, cleared after each checkpoint commit** (chosen). The row is
   written as before but flagged not-yet-durable; a checkpoint commits tantivy and only then clears the
   flags. The next run treats flagged rows exactly like ADR 0022's NULL-embedding rows.

## Decision

- **Checkpoints.** The indexer commits tantivy every 64 indexed files or 30 seconds, whichever comes
  first, and at the end of the run. A new, changed, or forced file's row is upserted with
  `files.pending = 1` before its chunks; a checkpoint commits tantivy and then sets `pending = 0` for
  exactly the paths in its batch (rows an earlier killed run left pending stay pending until a run
  redoes them). Pending rows join the implicit repair set, so the next run revisits exactly them and
  never touches a file a checkpoint already covered. The cadence is a constant, not configuration.
- **Reuse, not re-embed** (amended 2026-09-12). Most of a killed batch is already fully embedded in
  SQLite; only its tantivy docs never became durable. On the redo of a pending row the indexer
  re-chunks the current text (cheap) and keeps the stored embeddings only if the stored rows match
  that chunk set exactly — same count, and per position the same id, ord, kind, heading, and body —
  with every embedding present at full dimension. It then re-adds the tantivy docs, rewrites the
  note-level `created_at`/`source` columns, and (if the sidecar is being mutated in place) re-adds the
  vectors; otherwise it falls through to the full redo. This is sound because an embedding is a
  function of the chunk body under the pinned identity, and a non-NULL embedding is only ever written
  for the body stored beside it (`replace_chunks` inserts NULL rows in one transaction;
  `set_embedding` fills them from the same chunk list). A matching sha/mtime would not be:
  `upsert_file_pending` stamps the new hash before `replace_chunks`, so a crash between the two
  leaves the new hash over the old, fully embedded rows — the body comparison rejects exactly that.
  Reuse cannot cross an identity change: G4 refuses an incremental run across an embedding change,
  and a chunk-version change forces the wipe. NULL-embedding repairs fail the match and re-embed, and
  an explicit `reindex --since` selection always does the full redo.
- **Vectors.** The usearch sidecar is not saved at checkpoints. The f32 BLOBs are the durable vectors
  (G5), and rewriting a whole sidecar every 30 seconds would dominate a large vault. Instead
  `meta.vec_dirty` is set before a run's first SQLite vector change and cleared after the end-of-run
  save. A run that starts with the flag set repacks the sidecar from the BLOBs (no re-embed). Size-drift
  detection stays as a backstop.
- **Rebuild marker and who resumes.** `meta.rebuild_pending` is set before a full wipe (explicit
  `reindex` or the chunk-version auto-rebuild) and whenever a run starts from an empty index; a
  completed run clears it. While it is set:
  - `vagus index` resumes: plain incremental reconciliation over the partial index.
  - `vagus reindex` (and `reindex --since`) also resumes instead of wiping, provided the stored
    embedding identity and chunk version still match and the tantivy directory opens. Otherwise it
    restarts. The committed files are exactly what a restart would re-embed.
  - The implicit refresh in `vagus search`, `add-note`, `file`, and plugin captures
    (`IndexMode::AutoRefresh`) does **not** resume. It prints one stderr line pointing at `vagus index`,
    touches nothing, and search runs over the partial index. Resuming means embedding the rest of the
    vault; that has to be something the user asked for, not a side effect of looking something up.
- **Graceful Ctrl-C.** During an index run the first SIGINT prints `vagus: finishing current task and
  exiting. hit ctrl-c again to exit immediately` and sets a flag checked between files. The run
  finishes the current file (on a huge note that can take minutes; the message says what is happening
  rather than making an embed interruptible), commits a checkpoint, skips deletions (its walk is incomplete) and the
  vector save, and exits 130 with a resume hint. A second SIGINT exits immediately. Outside an index run
  the handler restores the default disposition and re-raises, so every other command still dies on
  Ctrl-C. SIGTERM and SIGKILL are not intercepted; checkpoints bound what they lose.
- **One writer.** An advisory `flock` on `<data_dir>/index.lock` is taken before any derived-store
  mutation. Tantivy's own writer lock was acquired only after a full run's `clear_all`, so a second
  `reindex` could wipe SQLite under a live run. The kernel releases the lock on exit or kill, and doctor
  uses it to tell "in progress" from "interrupted".
- **BM25 self-heal.** Every non-full run compares tantivy's live doc count with SQLite's chunk count.
  Only a disagreement pays for the census: live docs per `path` term against `chunks GROUP BY path`.
  Paths only tantivy knows are deleted. Current files whose counts differ get their docs re-added from
  their stored chunk rows — the exact text tantivy indexes — with no read, chunking, or embedding.
  Files already slated for a full redo are left to that path. This repairs indexes stranded by
  pre-checkpoint binaries.
- **Doctor.** `vagus doctor` reports `full-text docs` (tantivy live docs vs chunks) and
  `index checkpoint` (pending rows, unfinished rebuild, stale sidecar), each `[!!]` with a `vagus index`
  hint.
- **Progress.** Each mid-run checkpoint prints one stderr line: files checked of total, files indexed,
  chunks embedded, elapsed. Stdout summaries are unchanged.

## Consequences

- A kill throws away at most the embedding of the note in flight (plus any note whose embeddings were
  only partly written); the rest of an uncommitted batch is reused, and a killed reindex resumes from
  its last checkpoint. An index in the 2026-09-10 state repairs itself on its next `vagus index` or search
  refresh: the 518 missing docs are re-added from SQLite without touching the model.
- G6's single `commit()` becomes one commit per checkpoint. Long runs produce more small segments;
  tantivy's merge policy and the final `wait_merging_threads` bound them, and `vagus compact` remains
  the explicit fix.
- The census triggers on a total mismatch. Offsetting errors (one note missing N docs while another
  carries N extra) keep the totals equal and go unnoticed; `vagus reindex` remains the complete answer.
- After an interrupted run, the next run (possibly a search refresh) repacks the whole sidecar from the
  BLOBs. Seconds at current scale, minutes on the >500k trajectory, and only after a crash.
- The first search after a chunk-version upgrade still starts the one-time rebuild, because G4 wants it
  automatic. Only its resumption after an interrupt is reserved for explicit commands.
- `files.pending` is an additive migration defaulting to 0. Pre-existing rows are treated as current;
  the census covers any that a pre-checkpoint run stranded in BM25.
