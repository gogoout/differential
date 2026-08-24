# 0017 — Uncommitted review sources: staged and worktree snapshots as trees

Status: accepted

## Context

`dfr review` should work before anything is committed — on the staged index, and on the
worktree (the "review my changes so far" loop). The pipeline, the invariants and the
renderers all operate on two endpoints resolved by `rev-parse`; everything downstream of
that resolution — the three `diff-tree` calls, blob reads by `<rev>:<path>`, the tree
assertion's `<head>^{tree}`, the independent recount — already works on raw tree oids.
The only commit assumption in the whole pipeline was `rev_parse`'s `^{commit}` peeling.

ADR 0011's consequence line — "tree building never touches the user's worktree or index"
— was written about invariant 3's reconstruction. Reading the worktree to *snapshot* it is
a new capability and gets this record.

## Decision

Synthesize tree objects for uncommitted state with plumbing only, never touching the
user's index file (`engine::worktree`):

- Seed a temporary `GIT_INDEX_FILE` from `ls-files -s -z` piped into
  `update-index -z --index-info` (the record format matches byte-for-byte). Unmerged
  entries are an error — a conflicted index has no single tree.
- `index_tree`: `write-tree` of that seed — the staged state.
- `worktree_tree`: additionally feed the union of `ls-files -z` and
  `ls-files --others --exclude-standard -z` (untracked-but-not-ignored files included)
  to `update-index --add --remove -z --stdin`, then `write-tree`. This hashes current
  worktree content and writes the blobs into the odb — unreferenced and gc-able, exactly
  like invariant 3's reconstruction objects — which also pins the snapshot so later
  `cat-file` reads by `<tree>:<path>` always resolve.

Endpoint resolution becomes `rev_parse_commit_or_tree` (commit first, then `^{tree}` on a
raw tree oid). Nothing else in the pipeline changes; all four invariants run unmodified
over the synthesized endpoints.

Review identity for uncommitted reviews keys on the HEAD sha plus a stable literal
(`"INDEX"` / `"WORKTREE"`) rather than the synthesized tree — the tree churns with every
edit, and findings re-anchor by hunk digest exactly as they do on a moving branch.

The frozen schema's `SourceKind` gains `staged` and `worktree`. This is treated as
**additive**: `kind` is written, never read, by every existing consumer, and documents
produced from committed ranges are unchanged — only documents using the new capability
carry the new values (an old reader deserializing such a document fails on the unknown
variant, which is the correct outcome: it cannot interpret that source anyway). No
`schema_version` bump. `source.base`/`source.head` hold the tree oids in these documents.

## Consequences

- `dfr review` (no arguments) can offer staged/worktree review; the grouped pipeline —
  LLM grouping included — runs identically on the snapshots.
- Grouping-cache keys derive from class digests, so an unchanged diff still hits cache;
  any edit is a miss, which is inherent to reviewing a moving worktree.
- `dfr stack` still requires a commit base (`commit-tree -p`); it is not offered for
  uncommitted sources.
