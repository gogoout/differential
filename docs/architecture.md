# Architecture

How `differential` turns a large diff into a reviewable reading plan, and why it is built
the way it is. The normative behaviour lives in [`spec/`](../spec/); the decision records
with their evidence live in [`adr/`](../adr/). This page is the narrative tour.

## The problem

A 100-file merge request is not 100 files of work. Most of it is one decision echoing
through the codebase: a signature change cascading through call sites, a rename sweeping
across imports, a lockfile regenerating itself. A reviewer's real job is to find the
handful of changes that deserve focus (line-by-line) reading — and to *safely* skip the rest.

## The load-bearing idea: coverage is structural, judgement is delegated

1. **Mechanical partition.** Every hunk is assigned a *shape class* — a hash of its diff
   text with identifiers and literals normalised away, on both the removed and added sides.
   Hunks in one class are the same edit wearing different names. Coverage is 100% by
   construction: no model, no semantic parser, no file ever excluded.
2. **An LLM merges and labels class ids — never hunks.** Asking a model to partition hunks
   directly fails silently: on large refactors, measured coverage dropped as low as ~27%
   while reporting success, because an omitted hunk index is indistinguishable from one
   that never existed. Here the model can only group *class ids*; anything it omits is
   detected against the known id set and back-filled into a must-read group. It still earns
   its keep — merging twenty textually-different shape classes into one "path and import
   swaps" group is exactly what hashing can't do ([ADR 0001](../adr/0001-llm-merges-class-ids-never-hunks.md)).
3. **Deterministic ordering.** Symbol-definition → symbol-use edges between groups build a
   dependency DAG; the focus section is topologically sorted foundation-first, so the
   reviewer meets the abstraction before its consumers
   ([spec/ordering.md](../spec/ordering.md)).
4. **Structural audits.** Before any document is emitted: every changed file must
   reconstruct **byte-exactly** from base + hunks; the final tree is rebuilt *from applied
   hunks* (never copied) and must equal the real head tree; and an independent, deliberately
   dumb recount over git's own output must match. Each audit caught a real bug during
   validation ([`spec/invariants.md`](../spec/invariants.md)).

The report is honest about the saving: skim exemplars still get read, so documents track
*read hunks* and *skipped hunks* separately — only the latter is time saved.

## The pipeline

`enumerate → classify → group → order`, recorded in `generator.stages`
([spec/overview.md](../spec/overview.md)). The product is **one JSON document**
([spec/json-contract.md](../spec/json-contract.md)); renderers are views over it:

- **Shadow branch** ([spec/stack.md](../spec/stack.md)) — the diff rewritten as a synthetic
  commit stack; `git log --oneline` alone shows the shape of the change. Shipped, as
  `dfr stack`.
- **TUI** — a dedicated reviewer emitting structured findings keyed by hunk. Planned.
- **Forge review** — grouped comments posted to a GitLab MR / GitHub PR. Planned.

## Guarantees

- **Nothing is ever excluded from enumeration.** No extension filters, no path exclusions —
  not even via config. Manifest and lockfile edits are where refactor cascades live; path
  filtering was the single worst coverage bug found during validation (ADR 0005, 0012).
  Config and language plugins tune *classification*, never *what exists*.
- **A low-similarity rename can never masquerade as a verbatim move.** Renames carry git's
  similarity score on both halves; below ~95 the change is a modification, never
  skim-eligible, enforced by a deterministic gate after the model runs (ADR 0003).
- **Groupings are pinned.** Labels are non-deterministic across model calls; a content-hash
  cache keeps a review from reshuffling under the reviewer (ADR 0009).
- **Documents are pure functions of `base..head`.** Review state (comments, progress) lives
  in a sidecar store and re-anchors across regenerations by exact hunk digest — orphaned,
  listed, never silently dropped ([spec/persistence.md](../spec/persistence.md)).

## Crate layout

| path | what |
|---|---|
| `crates/engine` | the backend: git io, diff parsing, byte-exact applier, shape classes, language registry, grouping, ordering, invariants, review sessions — plus `engine::schema` (the frozen JSON contract, serde-only), `engine::plan` (shared domain policy over the schema), `engine::ports` (the traits domain owns) and `engine::store` (their filesystem adapters, alongside `gitio`) and `engine::llm` (the backend abstraction, tools denied) |
| `crates/stack` | the shadow-branch renderer — the diff as a synthetic commit stack |
| `crates/tui` | the terminal reviewer (vendored tuicr/lumen pieces live here) |
| `crates/cli` | the application layer: the `dfr` / `differential` binaries, argument parsing and dispatch only |
| `crates/testutil` | shared test fixtures, `publish = false` |

Dependency direction is strict: `cli → {tui, stack} → engine`. The `engine::schema`
module remains the product boundary — serde types only, no engine internals — as a
reviewed module discipline rather than a crate boundary (ADR 0008, 0018).
All git access shells out to real git, plumbing commands only — the byte-exactness
guarantees were validated against real git output and nothing else (ADR 0002, 0011).
Domain code reaches it through `engine::ports`, whose only implementation is `gitio::Repo`;
`Repo::run` is private, so a function's trait bounds are an honest statement of how much git
it can touch, and a fake git — which would make invariants 1–4 compare a fake with itself —
is impossible to introduce by accident (ADR 0020, enforced by `engine/tests/layering.rs`).

## Testing philosophy

Unit tests cover the byte-level traps (no-trailing-newline permutations worth exactly one
byte, deleted lines starting `--`, CRLF round-trips); synthetic-repo integration tests run
the full pipeline against hermetic temp repositories for every edge case (modes, symlinks,
binary, submodules, typechanges, renames at high and low similarity); grouping and stack
tests drive a programmable fake LLM backend; and an env-driven parity test asserts exact
class-count agreement with the validated prototype on a real private corpus — drift is a
port bug, never tolerance-adjusted away.
