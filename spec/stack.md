# The shadow-branch renderer

The grouped, ordered document rewritten as a **synthetic commit stack**, reviewed natively
in an IDE or `tig`. `git log --oneline` over the stack IS the reading plan: focus groups
first (foundation-first), skim exemplars split from their skippable remainders, generated
noise folded, the audit back-fill trailing. Arguably the primary artefact of the whole
tool.

Built via `run_stack_pipeline` (core → group → order → stack over one shared diff view),
or `stack::build_stack` given a grouped document. Plumbing only (ADR 0011): temporary
`GIT_INDEX_FILE`, `hash-object`, bulk `update-index --index-info`, `write-tree`,
`commit-tree`, `update-ref`. No checkout, no branch switch, no contact with the worktree.

## Commit plan

One commit per group in rank order:

| subject | contents |
|---|---|
| `[focus] {label}` | every hunk of the group |
| `[skim 1/2] {label} — k exemplars` | one hunk per shape class (the class exemplar) |
| `[skim 2/2] {label} — n−k further hunks, same shapes` | the remainder — skippable on this subject line alone |
| `[skim] {label} — k exemplars` | skim group whose classes are all singletons |
| `[noise] {label} — folded, n hunks` | generated content |
| `[unclassified] n hunks carried by no group` | the audit back-fill (invariant 5) |
| `[meta] n binary, mode or empty-file changes` | zero-hunk files — no class owns them, so a trailing commit must, or the tree assertion could not hold |

Bodies carry the group description + reason; every commit ends with the trailer
`Review-Synthetic: <base12>..<head12>`, marking it as synthetic and reconstructible.

The stack lands on `refs/review/<base7>-<head7>/stack` by default
(`StackOptions.ref_name` overrides). Re-running moves the ref; commits are authored as
`differential <differential@localhost>`.

## Assertions (no ref update on failure)

1. **Accounting** — every canonical hunk in exactly one commit.
2. **Tree assertion** — commit content is computed by cumulatively APPLYING HUNKS (never
   copying head blobs), so `tip^{tree} == head^{tree}` proves every hunk was carried. The
   `[meta]` commit's recorded-oid staging is the same documented exception as the core
   tree builder.
3. **Independent recount** — the dumb `@@` counter summed over each parent→child
   `diff-tree -U0 --no-renames` must equal the canonical hunk count. Safe from hunk
   coalescing: `-U0` split points are unchanged gap lines that remain present in every
   intermediate tree.

## Notes for reviewers

- Deletions materialise gradually: a file whose deletion hunks span groups shrinks commit
  by commit and disappears when its last hunk lands.
- The stack is content-addressed downstream of the grouping cache: with a cache hit the
  same input range re-renders an identical plan (fresh commit timestamps aside).
