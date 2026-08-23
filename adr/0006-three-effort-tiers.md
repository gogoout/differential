# 0006 — Three effort tiers, and the honest saving

Status: accepted

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
