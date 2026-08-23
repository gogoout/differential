# 0009 — Groupings are cached and pinned by content hash

Status: accepted

## Context

Three grouping runs over the same change produced 20, 23 and 25 groups with materially
different skim/close splits. Coverage and the structural invariants were identical every
time — they are structural; labels are model judgement and non-deterministic.

## Decision

A grouping is cached keyed by a content hash of its inputs (the class partition). A review
in progress reuses the pinned grouping; it never reshuffles under the reviewer.
Regeneration after new commits produces a new input hash and therefore a fresh grouping
(see spec/persistence.md for how review state carries over).

## Consequences

- Stable review experience; reproducible documents for a given pin.
- The cache lives in the per-repo state directory, not in the repo's tracked tree.
