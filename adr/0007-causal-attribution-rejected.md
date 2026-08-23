# 0007 — Causal root/derived attribution: rejected for collapse, reserved for ordering

Status: accepted

## Context

The original hypothesis: a few hunks change an interface (roots) and most others exist only
because a root moved (derived), so a reviewer reads roots and collapses the rest. Measured
over 79 MRs it failed: median 8 roots per MR (target 1–3), mean 29% of hunks attributed
(target ≥60%). The precision is too low to *hide* anything behind.

## Decision

Root/derived attribution is not a collapse mechanism and must not be rebuilt as one.
It IS the right idea for **ordering**: symbol-definition → symbol-use is a good partial
order between groups, and ordering tolerates the ~30% precision that killed attribution
(a wrong edge reorders; it never hides content). The ordering stage builds a group-level
DAG on this signal and topologically sorts foundation-first.

## Consequences

- Collapse comes only from shape classes (0004) and generated hints (0006).
- The ordering stage (later milestone) emits `depends_on` edges and `rank`, so consumers can
  render the causal chain, not just a sequence.
