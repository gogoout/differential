# 0001 — The LLM merges and labels class ids, never hunks

Status: accepted

## Context

The obvious design — ask a model to partition a diff's hunks into groups — was evaluated
(including the open-source `semantic-diff` tool, whose prompt asks for `{file, hunks: [i]}`
tuples). Any hunk index the model fails to emit is simply absent, indistinguishable from a
hunk that does not exist. Measured on four real MRs, hunk coverage degraded with size:
100% → 57% → 39% → **27%** on a ~100-file refactor, always reporting success. The failure is
representational, not a reasoning flaw: omission is invisible.

## Decision

Coverage is structural. A mechanical pass partitions every hunk into shape classes (100%
coverage by construction). The LLM only **merges and labels class ids** — it can never name,
and therefore never drop, a hunk. An omitted class id is detected against the known id set
and back-filled into a trailing must-read group.

## Consequences

- Measured result: 0 missing / 0 duplicated / 0 hallucinated class ids, 100% coverage on
  every validation MR; one earlier run dropped exactly 1 class of 197 and the audit caught it.
- The model still earns its place: merging classes that differ in text but not intent
  (e.g. 20 distinct shapes folded into one "path and import swaps" group) is what hashing
  cannot do.
