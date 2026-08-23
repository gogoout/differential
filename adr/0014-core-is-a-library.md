# 0014 — The core is a library; binaries belong to renderers

Status: accepted

## Context

Milestone 1 shipped a `plan`/`check` CLI alongside the engine. In practice there is no
standalone-CLI use case: the JSON document flows in-process from the engine to its consumers
(TUI, shadow-branch builder, forge poster), none of which want to shell out to a subprocess
and re-parse their own product.

## Decision

The core ships as libraries only: `differential-schema` (the contract) and
`differential-engine` (the pipeline). The binary namespace is reserved for renderers — the
future TUI crate owns the `dfr` binary as its entry point, with other render surfaces as its
subcommands.

The invariant runner survives as a dev/CI example, not a product:
`cargo run -p differential-engine --example check -- <base>..<head>`.

## Consequences

- Consumers get typed access to `PlanDocument` and `InvariantReport`; JSON serialisation is
  for export and persistence, not for inter-process plumbing.
- Range resolution (`a..b`, `a...b` via merge-base) lives in the engine
  (`pipeline::resolve_range`) since every consumer needs it.
- Supersedes the milestone-1 `crates/cli`.
