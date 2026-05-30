# ADR 0014 — Identity: a self-contained Rust universe, "no versioned runtime"

- **Status:** Accepted (2026-05-30)

## Context

v1 framed vagus as "a single Rust binary." Two pressures revealed that the *literal* single-binary
framing was the wrong invariant:

1. The plugin system ([ADR 0010](./0010-plugin-subcommands.md)) already makes vagus a *family* of
   executables (`vagus` + `vagus-<name>` on `$PATH`).
2. To make `vagus search` genuinely good *without* Claude (the tier-1 shell experience — see
   [ADR 0012](./0012-three-tier-retrieval.md)), we want local ML models, including a small generative
   rewriter. The instinct was to reject that as "too heavy for a single binary."

But the thing the author actually wants to protect is **not** "one file." It is **"no versioned
runtime to manage"** — no Python, no Node, no TypeScript, no interpreter whose version you have to
reconcile with the OS. CLI tools that *click together* with zero runtime ceremony.

A decisive observation: **vagus already statically links a C++ inference library** — onnxruntime, via
`ort` ([guardrail G13](../guardrails.md), verified with `otool -L`: the installed binary references
only OS dylibs/frameworks). So a statically-linked C++ lib is *already in-character*; it carries no
managed runtime.

## Decision

Reframe the identity from **"single binary"** to **"a self-contained Rust universe with no versioned
runtime."** Concretely:

- **No managed/foreign runtime, ever.** No Python/Node/TS/JVM/interpreter dependency in any vagus
  tool. This is the binding rule.
- **Statically-linked native libraries are in-character.** onnxruntime (today, via `ort`) and, where
  justified, other static C++ inference libs (e.g. `ggml`/`llama.cpp`, or a pure-Rust engine like
  `candle`) are allowed — they ship *inside* a self-contained executable, with only OS frameworks
  dynamically linked. Re-verify each with `otool -L` (G13).
- **vagus may be more than one executable.** The core `vagus` binary plus optional `vagus-<name>`
  companions/plugins form the "universe." Each is independently self-contained.
- **Models are a lazily-downloaded cache, not part of the binary.** They live outside iCloud
  (`~/Library/Caches/vagus/`, G1/G6) and download on first use. Binary size ≠ model footprint.

## Consequences

- The "single statically-linked binary by default" non-goal in [`requirements.md`](../requirements.md)
  is **superseded** by this ADR: the goal is *self-contained + no managed runtime*, which one or
  several Rust binaries satisfy.
- Adopting a second native inference engine (e.g. for a local generative rewriter,
  [ADR 0016](./0016-local-generative-rewriter.md)) is *consistent with* the identity — but still
  warrants its own ADR (it's a real dependency decision, G11).
- The choice of *where* a capability lives (in `vagus` vs. a companion binary vs. a plugin) is a
  case-by-case engineering call, not an identity constraint. Scoring models that ride the existing
  `ort` stack default into core; heavy or networked code stays out (G18).
