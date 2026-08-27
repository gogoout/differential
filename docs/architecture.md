# Architecture

How `differential` turns a large diff into a reading plan, and why it is built this way.

The normative behaviour lives in [`spec/`](../spec/). The decision records, with their
evidence, live in [`adr/`](../adr/). This page is the tour.

## The problem

A 100-file merge request is not 100 files of work. Most of it is one decision echoing
through the codebase. A signature change cascades through call sites. A rename sweeps
across imports. A lockfile regenerates itself.

A reviewer's real job is to find the few changes that deserve careful reading, and to skip
the rest **safely**.

## The load-bearing idea

Coverage is structural. Judgement is delegated.

### 1. Mechanical partition

Every hunk gets a **shape class**. That is a hash of its diff text with identifiers and
literals normalised away, on both the removed and the added side. Two hunks in one class
are the same edit wearing different names.

Coverage is 100% by construction. No model is involved. No semantic parser is involved. No
file is ever excluded.

### 2. An LLM merges and labels class ids, never hunks

Asking a model to partition hunks directly fails silently. We measured it. On large
refactors, coverage dropped as low as 27% while the model reported success. An omitted hunk
index is indistinguishable from one that never existed.

Here the model can only group **class ids**. Anything it omits is detected against the known
id set and back-filled into a must-read group.

It still earns its keep. Merging twenty textually different shape classes into one "path and
import swaps" group is exactly what hashing cannot do. See
[ADR 0001](../adr/0001-llm-merges-class-ids-never-hunks.md).

### 3. Deterministic ordering

Edges run from a symbol's definition to its uses. Those edges build a dependency graph
between groups. The focus section is sorted foundation-first, so the reviewer meets an
abstraction before its consumers. See [spec/ordering.md](../spec/ordering.md).

### 4. Structural audits

These run before any document is emitted.

- Every changed file must reconstruct **byte-exactly** from its base plus its hunks.
- The final tree is rebuilt **from applied hunks**, never copied, and must equal the real
  head tree.
- An independent, deliberately dumb recount over git's own output must match.

Each audit caught a real bug during validation. See
[`spec/invariants.md`](../spec/invariants.md).

### The report is honest about the saving

Skim exemplars still get read. So documents track **read hunks** and **skipped hunks**
separately. Only the second number is time saved.

## The pipeline

`enumerate → classify → group → order`. The document records which stages ran, in
`generator.stages`. See [spec/overview.md](../spec/overview.md).

The product is **one JSON document** ([spec/json-contract.md](../spec/json-contract.md)).
Renderers are views over it:

- **Shadow branch** ([spec/stack.md](../spec/stack.md)) — the diff rewritten as a synthetic
  commit stack. `git log --oneline` alone shows the shape of the change. Shipped, as
  `dfr stack`.
- **TUI** ([spec/tui.md](../spec/tui.md)) — a dedicated reviewer that emits structured
  findings keyed by hunk. Shipped, as `dfr review`. `dfr findings` prints those findings as
  JSON.
- **Forge review** — grouped comments posted to a GitLab merge request or a GitHub pull
  request. Planned.

## Guarantees

**Nothing is ever excluded from enumeration.** No extension filters. No path exclusions.
Not even through config. Manifest and lockfile edits are where refactor cascades live, and
path filtering was the single worst coverage bug found during validation (ADR 0005, 0012).
Config and language plugins tune *classification*, never *what exists*.

**A low-similarity rename can never masquerade as a verbatim move.** Renames carry git's
similarity score on both halves. Below 95, the change is a modification and is never
skim-eligible. A deterministic gate enforces this after the model runs (ADR 0003).

**Groupings are pinned.** Labels are not deterministic across model calls. A content-hash
cache keeps a review from reshuffling under the reviewer (ADR 0009).

**Documents are pure functions of `base..head`.** Review state — findings and progress —
lives in a sidecar store. It re-anchors across regenerations by exact hunk digest. An
orphan is listed, never silently dropped. See
[spec/persistence.md](../spec/persistence.md).

## Crate layout

| crate | what it holds |
|---|---|
| [`crates/engine`](../crates/engine/README.md) | The backend: git io, diff parsing, the byte-exact applier, shape classes, the language registry, grouping, ordering, invariants and review sessions. |
| [`crates/stack`](../crates/stack/README.md) | The shadow-branch renderer. The diff as a synthetic commit stack. |
| [`crates/tui`](../crates/tui/README.md) | The terminal reviewer. Vendored `tuicr` and `lumen` pieces live here. |
| [`crates/cli`](../crates/cli/README.md) | The application layer: the `dfr` and `differential` binaries. Argument parsing and dispatch only. |
| `crates/testutil` | Shared test fixtures. `publish = false`. |

Inside the engine, four module boundaries carry weight:

| module | what it is |
|---|---|
| `engine::schema` | The frozen JSON contract. Serde types only, no engine internals. |
| `engine::plan` | Shared domain policy over the schema. Both renderers read tiers and reading splits from here, so they cannot disagree. |
| `engine::ports` | The traits the domain owns. |
| `engine::store` and `engine::gitio` | The adapters that implement those ports. |

Dependency direction is strict: `cli → {tui, stack} → engine`.

`engine::schema` stays the product boundary. It is a reviewed module discipline rather than
a crate boundary (ADR 0008, 0018).

All git access shells out to real git, plumbing commands only. The byte-exactness guarantees
were validated against real git output and nothing else (ADR 0002, 0011).

Domain code reaches git through `engine::ports`, and `gitio::Repo` is their only
implementation. `Repo::run` is private, so a function's trait bounds are an honest statement
of how much git it can touch. A fake git would make invariants 1 to 4 compare a fake with
itself, so it is impossible to introduce by accident (ADR 0020, enforced by
`engine/tests/layering.rs`).

## Testing philosophy

**Unit tests cover the byte-level traps.** No-trailing-newline permutations worth exactly
one byte. Deleted lines starting with `--`. CRLF round-trips.

**Integration tests run the full pipeline** against hermetic temporary repositories, for
every edge case: modes, symlinks, binary files, submodules, typechanges, and renames at
high and low similarity.

**Grouping and stack tests drive a programmable fake LLM backend.** The LLM is the one seam
where a fake is correct: it is genuinely chosen at run time.

**A parity test asserts exact class-count agreement** with the validated prototype, on a
real private corpus. It runs from an environment variable, so it is local-only. Drift is a
port bug. It is never tolerated by adjusting the number.
