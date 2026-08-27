# differential-stack

The shadow-branch renderer for [`differential`](https://crates.io/crates/differential). It
rewrites a grouped diff as a **synthetic commit stack**, so you can read the plan in your
IDE, in `tig`, or with plain `git log`.

`git log --oneline` over the stack **is** the reading plan. Focus groups come first, in
foundation-first order. Skim exemplars are split from their skippable remainders. Generated
noise is folded into one commit. The audit back-fill trails at the end.

Project home: <https://github.com/gogoout/differential>

## What you see

```
$ dfr stack main..feature
refs/review/1a2b3c4-5d6e7f8/stack  (14 commits, 187 hunks, recount 187)
  ...
review with: git log --oneline 1a2b3c4d5e6f..refs/review/1a2b3c4-5d6e7f8/stack

$ git log --oneline 1a2b3c4d5e6f..refs/review/1a2b3c4-5d6e7f8/stack
f00dfee  [unclassified] 1 hunks carried by no group
0ddba11  [noise] Lockfiles and generated artefacts — folded, 21 hunks
cafe007  [skim 2/2] Import swaps for the renamed module — 38 further hunks, same shapes
beefed5  [skim 1/2] Import swaps for the renamed module — 28 exemplars
add1c7e  [focus] Rework retry handling in the client
decade0  [focus] Introduce the storage backend trait and its implementations
```

Read from the bottom up. Read the `[focus]` commits first: definitions come before their
callers. Then read one exemplar per shape in `[skim 1/2]`. Then skip `[skim 2/2]` and
`[noise]` on their subject lines alone. Every hunk in them repeats a shape you already
checked.

## The commit plan

One commit per group, in rank order.

| subject | what the commit contains |
|---|---|
| `[focus] {label}` | Every hunk of the group. |
| `[skim 1/2] {label} — k exemplars` | One hunk per shape class. |
| `[skim 2/2] {label} — n−k further hunks, same shapes` | The remainder. Skippable on this subject line alone. |
| `[skim] {label} — k exemplars` | A skim group whose classes are all single hunks. |
| `[noise] {label} — folded, n hunks` | Generated content. |
| `[unclassified] n hunks carried by no group` | The audit back-fill. Nothing judged it, so read it. |
| `[meta] n binary, mode or empty-file changes` | Files with zero hunks. No class owns them, so a trailing commit must, or the tree assertion could not hold. |

Each commit body carries the group's description and its reason. Every body ends with the
trailer `Review-Synthetic: <base12>..<head12>`. That marks the commit as synthetic and
reconstructible.

Commits are authored as `differential <differential@localhost>`.

## The ref

The stack lands on `refs/review/<base7>-<head7>/stack` by default. Pass
`StackOptions.ref_name`, or `dfr stack --ref <name>`, to choose another. Re-running moves
the ref.

**Nothing else is touched.** No checkout. No branch switch. No contact with your worktree or
your index. The builder uses git plumbing only: a temporary `GIT_INDEX_FILE`,
`hash-object`, a bulk `update-index --index-info`, `write-tree`, `commit-tree`,
`update-ref`.

## Three assertions, and no ref on failure

1. **Accounting.** Every canonical hunk appears in exactly one commit.
2. **Tree assertion.** Commit content is computed by cumulatively **applying hunks**, never
   by copying head blobs. So `tip^{tree} == head^{tree}` proves every hunk was carried.
   Copying would make the equality hold by construction and prove nothing. Binary files
   staged from a recorded object id are the one documented exception.
3. **Independent recount.** A dumb `@@` counter, summed over each parent-to-child
   `diff-tree -U0 --no-renames`, must equal the canonical hunk count.

If any assertion fails, no ref is updated.

The recount is safe from hunk coalescing. With `-U0`, the split points are unchanged gap
lines, and those remain present in every intermediate tree.

## Using it

Two entry points.

```rust
use differential_stack::{StackOptions, run_stack_pipeline};

// Core stages, then group, then order, then stack — over one shared diff view.
// `source` is the plan::ReviewSource that resolve_range returned.
let out = run_stack_pipeline(&repo, &source, &config, &langs,
                             &grouping_opts, &StackOptions { ref_name: None })?;

// out.pipeline — the PipelineOutput, including the invariant report.
// out.stack    — Some(StackResult), or None if the pipeline produced no document.
```

```rust
use differential_stack::build_stack;

// You already have a grouped document. Build the stack from it.
let result = build_stack(&repo, &document, &view, &StackOptions { ref_name: None })?;
```

`StackResult` carries `{ ref_name, tip, commits, hunks_carried, recount }`. Each
`StackCommit` carries `{ sha, subject, hunks }`.

The git access is generic over the engine's ports, so a function's bound list states
exactly how much git it can touch.

## Notes for reviewers

- **Deletions materialise gradually.** A file whose deletion hunks span several groups
  shrinks commit by commit. It disappears when its last hunk lands.
- **The stack is stable.** It is content-addressed downstream of the grouping cache. With a
  cache hit, the same input range re-renders an identical plan. Only the commit timestamps
  differ.

## Licence

MIT or Apache-2.0, at your option.
