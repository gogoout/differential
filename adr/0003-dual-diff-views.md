# 0003 — Dual diff views: --no-renames canonical, -M for classification

Status: accepted

## Context

`--no-renames` makes reconstruction tractable: added and deleted files are one atomic hunk
each, which is what lets the applier and tree assertion work. But it also turns a
62%-similar rename into whole-file-delete + whole-file-add, and a classifier that only sees
that view cannot tell a verbatim move from a rewrite. During validation this caused the
worst mislabel found: a file that was 38% rewritten — the key file of the whole change — was
marked "skim: confirm the relocation is verbatim".

## Decision

Two views, two jobs:

- `--no-renames` is the **canonical** view: hunk enumeration, ids, reconstruction,
  invariants.
- `-M` (rename detection with similarity scores) feeds **classification**:
  `rename_similarity`, `old_path`/`new_path` cross-links on both sides of a rename.

`rename_similarity` is carried in the JSON. Anything below ~95 is a modification, not a
relocation, and must never be skim-eligible. The gate itself is grouping-layer policy; the
core records the number.

## Consequences

- A low-similarity rename can never be presented as a verbatim move.
- Consumers can express "moved and modified" across the D and A halves via the cross-links.
