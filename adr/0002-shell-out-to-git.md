# 0002 — Shell out to the git binary; no libgit2/gix

Status: accepted

## Context

The engine's guarantees are byte-level: applier fidelity to the last no-newline byte, hunk
enumeration matching `git diff -U0` exactly, tree construction via plumbing. All of this was
validated against real git output. libgit2's diff engine has its own hunk-splitting
behaviour; gix would require re-validating every invariant.

## Decision

All git access spawns the `git` binary: `diff-tree -U0 --no-renames`, `diff-tree -M`,
`cat-file`, `hash-object`, `update-index` (temp index), `write-tree`, `commit-tree`.
Output is handled as bytes end to end; UTF-8 decoding happens only at display boundaries.

## Consequences

- Semantics stay pinned to git itself; the parser targets one stable, documented format.
- Requires git on PATH; subprocess overhead is negligible next to the LLM stage.
- See 0011 for why plumbing commands specifically.
