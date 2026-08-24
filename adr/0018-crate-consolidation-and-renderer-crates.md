# 0018 — Crate consolidation: engine owns schema + llm; renderers get crates

Status: accepted (supersedes the packaging half of 0008; amends 0014 and 0016)

## Context

The workspace shipped as `cli → engine → {schema, llm}`. Two years of arguments for that
shape didn't materialise in two milestones of practice:

- No consumer ever depended on `differential-schema` alone; every real consumer needs
  the engine anyway (pipelines, review sessions, git io). The "take the contract without
  the git plumbing" property had zero takers, while the extra crate cost a publish, a
  version bump line, and cross-crate import ceremony on every contract change.
- `differential-llm` held one trait, one impl and one error enum consumed by exactly one
  module (`grouping`) plus the pipeline's backend construction.

Meanwhile the renderers grew in the wrong places: the shadow-branch renderer lived
INSIDE the engine (`engine::stack`), and the TUI lived inside the application binary
crate — so "the core is a library, binaries belong to renderers" (ADR 0014) was true in
spirit but not in the crate graph.

## Decision

- **`engine::schema` and `engine::llm` are engine modules.** The guarantees survive as
  module discipline, reviewed rather than compiler-enforced: `schema` stays serde-only
  (no engine imports, no consumer conveniences — the frozen-contract rules of ADR 0008
  are unchanged), and nothing outside `llm` touches subprocess machinery (the
  tools-denied, one-shot contract of ADR 0016 is unchanged).
- **Each renderer is a crate**: `differential-stack` (shadow-branch commit stack, moved
  out of the engine together with `run_stack_pipeline`/`StackOutput`) and
  `differential-tui` (the terminal reviewer, moved out of the cli, vendored code and
  attribution included).
- **`crates/cli` is the application layer**: argument parsing and dispatch only, owning
  the `dfr`/`differential` binaries and consuming both renderer crates. (ADR 0014 said
  "the future TUI crate owns the `dfr` binary"; with two renderers that inverts — a thin
  app layer over renderer libraries scales to the forge consumer next.)
- **`differential-testutil`** (`publish = false`) carries the shared test fixtures
  (hermetic TestRepo, FakeBackend, prompt helpers) that were previously copy-pasted
  between engine and cli test suites.

Dependency direction stays strict and one-way: `cli → {tui, stack} → engine`.

## Consequences

- Published crates: `differential-engine`, `differential-stack`, `differential-tui`,
  `differential` (the bins). `differential-schema` and `differential-llm` remain on
  crates.io at 0.2.0, orphaned — no further versions.
- Contract changes are no longer visible as "touched the schema crate" in review; they
  are visible as "touched `engine/src/schema.rs`", which reviews identically.
- The engine gained a subprocess-spawning module; determinism claims now attach to the
  pipeline modules, not the crate as a whole.
