# 0006 — Three effort tiers, and the honest saving

Status: accepted (the `close` tier was renamed `focus` in schema v2 — ADR 0019;
the rationale below is unchanged and uses the original name)

## Context

Two-tier (`close`/`skim`) ratings overstate the win twice over. First, skim *exemplars*
still get read — on the reference change, of 283 hunks: 77 close, 126 exemplars, 80
remainder, so the genuine saving was 28%, not the ~73% a skim total suggests. Second, a
40+-hunk lockfile group is not genuinely skimmable at all — it is fold-don't-read.

## Decision

- `effort` is `close | skim | noise`. `noise` = generated content (lockfiles, snapshots,
  build artefacts), folded entirely with no exemplars.
- Documents report `read_hunks` (close + exemplars) and `skipped_hunks` (skim remainders +
  folded noise) separately. Consumers must never present skim totals as the saving.

## Consequences

- The tool's claimed value matches what a reviewer actually experiences.
- Noise assignment is driven by computed `generated` hints (builtin list, gitattributes,
  repo config), not model judgement alone.

## Clarification: deferring is an opinion, not a prohibition

A skim group shows one exemplar per shape and defers the remainder; a noise group is folded
entirely. That is a claim about what is worth reading **by default**, and the accounting
above is what keeps it honest.

It is not a claim that deferred hunks must stay unreachable. `z` has always unfolded a
remainder, and the reviewer can now also expand a window across a hunk this group does not
list (ADR 0021) — which may be a deferred one. Both are the reviewer overriding the default
deliberately, on a specific hunk, having been told what it is. Nothing about the tiers, the
counts, or the coverage audit changes: what was deferred is still deferred, and
`read_hunks`/`skipped_hunks` still describe the plan rather than the reviewer's route
through it.
