# 0013 — Incremental review: total regeneration + persistent sidecar state

Status: accepted

## Context

The reviewed branch moves. Patching a plan document in place would make it no longer a pure
function of the diff, and positional ids (`h0…`, `C0…`) shift whenever hunks are inserted —
so anything persistent must not key on them.

## Decision

- Plan documents are immutable and content-addressed; a new head means full regeneration.
- Comments and review progress live in a sidecar store under
  `<git-common-dir>/differential/`, keyed by review identity (base + branch/MR ref, not
  head). Which review a spelling opens is refined by
  [ADR 0026](0026-a-review-adopts-an-ancestor.md): the spelling is part of the name, so a
  new spelling adopts a review on the same line of history.
- Comments anchor to `hunks[].digest` (exact content hash, stable across regenerations).
  On regeneration: exact digest match → reattach; fuzzy context match → reattach flagged
  moved; otherwise `orphaned` — listed, never deleted.
- Reviewed-marks key on class/group content hashes so unchanged work stays done.
  Superseded by [ADR 0025](0025-reviewed-marks-key-per-hunk.md): the gate was right and the
  unit was not, so marks key on the hunk digest instead.

Details in spec/persistence.md.

## Consequences

- The generator stays simple and auditable; statefulness is isolated in one store.
- `hunks[].digest` is part of the frozen schema from v1 so consumers can rely on it.
