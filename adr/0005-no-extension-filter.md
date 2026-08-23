# 0005 — Every file is processed; no extension filter

Status: accepted

## Context

An early probe restricted analysis to source extensions (`.rs`, `.ts`, `.tsx`, `.py`) and
excluded path segments (`node_modules`, `dist`, `vendor`, …). On a crate-split refactor this
discarded 119 of 303 hunks — *including the single most collapsible block in the change*: a
dependency swap repeated across ~15 manifest files. For a refactor, the manifest edits ARE
the derived edits; extension filtering is structurally biased against exactly the
cascade-shaped changes this tool exists for.

## Decision

Canonical enumeration processes **every file** — no extension filter, no path exclusions,
no size cutoffs. Manifests, lockfiles, docs, CI config: everything is enumerated, classified
and carried.

## Consequences

- 100% coverage claims are real; the invariants depend on this.
- Files that deserve less attention are handled by *classification* (generated hints, the
  noise tier), never by exclusion. See 0012.
