# 0026 — A review adopts an ancestor

Status: accepted

Refines the review-identity bullet of [ADR 0013](0013-incremental-review-sidecar-state.md).
Closes issue 40.

## Context

`plan::review_id` is `sha1(base_sha ‖ NUL ‖ head_spec)`, where `head_spec` is the head
endpoint **as typed**. That is deliberate and load-bearing: a branch name keeps one review
alive as its tip moves, where the resolved sha would file every commit as a new review and
strand the reviewer's progress behind it.

The cost is that the **spelling is part of the review's name**. Review `<base>..<sha>`, mark
work, quit, reopen `<base>..HEAD` where `HEAD` *is* that sha, and the progress is gone.
Same base, same commit, same diff, two reviews. An abbreviated sha against a full one, a
branch name against the sha it points at, `HEAD` against either — any difference splits it.

Keying on the resolved sha instead would fix the spelling and break the moving-ref
property, and `HEAD` cannot simply be resolved either: `spec/tui.md` files a clean-worktree
pick under `HEAD` precisely so the review survives new commits landing.

The two behaviours conflict on the same input, so this is a decision rather than a patch.

## Decision

**Identity stays a pure function of the two strings. Which review a spelling *opens* is a
separate question, answered by `review_identity::resolve`.**

A spelling with no review directory of its own **adopts** a filed review when the two are
the same work:

- the base shas are equal, **and**
- one head is reachable from the other (`merge-base --is-ancestor`, either direction).

That covers both halves of the report. Two spellings of one commit adopt each other because
the heads are equal. A review opened after new commits adopts the one already in progress
because the old head is an ancestor of the new one — the carry-forward, at the cost of a
redirect rather than a copy, because marks key on hunk content (ADR 0025) and findings
re-anchor by digest.

Two branches off one base adopt nothing from each other: neither head reaches the other.

Two files carry it, both in the review's own directory:

- `identity.json` — `{ base, head_spec }`, what this review was opened as. The id is a hash,
  so the id alone cannot say what its head spec means today. A review with no identity file
  can be recognised but never adopted.
- `alias` — the id whose progress this spelling reads. Written once, read first.

**Adoption is silent and permanent.** No prompt and no new screen: an exact base and a line
of history is not an ambiguity to put to the reader, and `dfr findings` cannot prompt, so a
prompt would let the two commands disagree about which review they mean. The TUI's status
line names it on the open that adopts.

The scan is bounded, cheapest test first: compare the stored base (a string compare, no git
call), then resolve the survivors' specs, then one `is_ancestor` per survivor. It runs once
per spelling; the `alias` file answers every later open with one file read.

**Uncommitted reviews are outside all of this.** A `WORKTREE` head is a synthesized tree,
not a commit, so ancestry says nothing about it (ADR 0017). Such a review writes no
identity, which keeps it out of every scan by construction rather than by a check.

Two ports, each named for the need and each implemented once:

```rust
pub trait Ancestry {
    fn commit_of(&self, spec: &str) -> Result<Option<String>, EngineError>;
    fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool, EngineError>;
}

pub trait ReviewCatalogue {
    fn filed_reviews(&self) -> Result<Vec<FiledReview>, EngineError>;
    fn alias_of(&self, id: &str) -> Result<Option<String>, EngineError>;
    fn file_alias(&self, from: &str, to: &str) -> Result<(), EngineError>;
    fn file_identity(&self, id: &str, opened_as: &ReviewIdentity) -> Result<(), EngineError>;
}
```

`commit_of` returns `None` for a spec that names nothing — a branch deleted since the review
was filed is a candidate that cannot be placed, not an error that refuses to open.

## Consequences

- The reported split closes in both directions, and the common loop — review, commit,
  review again — keeps its marks.
- **The join never expires.** Switch branches afterwards and the two spellings stay one
  review. Nothing is lost: marks key on hunk content and findings re-anchor by digest, so a
  diff that no longer matches costs orphans, which the pane title counts and the `F` list
  reads. Re-checking on every open would be worse — it would split a review whose progress
  is already in the shared directory, which is the reported bug again.
- A branch cut from this one **after** the filed head has the same base and satisfies
  ancestry, so it adopts. Its shared hunks stay marked, which is right; the other branch's
  findings orphan while it is open, and revive on the way back.
- Reviews filed before this change carry no `identity.json`, so they cannot be adopted on
  their first reopen. Opening one by its own spelling records the identity, and it is
  adoptable from then on.
- One directory per spelling used, holding a single `alias` file. `dfr clean` does not touch
  `reviews/` and must not start (ADR 0013).
- **A rebase is not covered, and cannot be.** Rewriting commits breaks ancestry on both
  endpoints at once, so nothing is adoptable. A reader who wants a session that outlives a
  rebase names it ([ADR 0027](0027-a-named-review-session.md)).

## Alternatives rejected

**Normalise only what cannot move** — resolve a head spec that is a plain sha, leave branch
names and `HEAD` alone. Half a fix: it joins `..<abbrev>` to `..<full-sha>` and leaves
`..<sha>` against `..HEAD` split, which is the case reported.

**Key on the resolved sha always.** One review per diff, and the moving-ref property gone.
Restoring it needs progress carried forward per tip — a catalogue, an ancestry check, an
identity file and a copy per commit. That is this decision with the copy added, and the copy
is what makes it worse: two directories that must be kept in step instead of one.

**Take the head out of identity entirely** — key on the base alone. It closes the split with
no scan and no ancestry, and keeps the moving-ref property for free. It also merges two
branches off one base into one review, which is the collision ancestry exists to prevent.

**Prompt before adopting.** The offer was the shape the issue proposed. It costs a new
visible surface, re-asks on every open unless the answer is recorded anyway, and cannot be
shown by `dfr findings` — so the non-interactive path would have to adopt silently
regardless, and the two commands could disagree.

**Leave it and document it.** Cheapest, and the reported surprise stands.
