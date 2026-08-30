# 0027 — A named review session

Status: accepted

Answers the gap [ADR 0026](0026-a-review-adopts-an-ancestor.md) leaves open.

## Context

ADR 0026 joins two spellings of one range, and joins a review reopened after new commits.
Both rest on ancestry. **A rebase defeats ancestry by construction**: rewriting commits
produces a head that is not a descendant of the old one, and rebasing onto a moved base
changes the base sha too. Neither endpoint is reachable from its old self, so nothing can be
adopted and the reader starts empty.

The loss is the directory, not the content. `hunk_digest` hashes the removed lines, the
added lines and the two `nonl` flags — no path, no line numbers, no commit. Measured on a
hermetic repo: rebase a feature branch onto a moved `main` and **every hunk digest is
unchanged**. The marks would still match. Nothing reaches them.

GitHub and GitLab do not solve this either; they avoid it. A pull request is a server-side
object with its own id, and the base and head are attributes of that object that are allowed
to change. A force-push files a new diff version under the same review. Their identity was
never derived from a range.

Deriving a stable name from a commit does not work here. Asking git which refs point at the
base gives zero names, or several, and a different answer next week — an identity that
changes when nobody touched the review is worse than the sha. The forge does not infer it
either: you name the source and target branch when you open the pull request.

## Decision

**The reader may name a review session, and the name is then the whole identity.**

```sh
dfr review --name "$(git branch --show-current)" main..HEAD
dfr findings --name "$(git branch --show-current)" main..HEAD
```

- `review_id_named(name)` hashes a tag byte and the name. Neither endpoint is in the key, so
  rebasing either cannot strand the session. The tag byte keeps names out of
  `review_id`'s space: that hash starts with a resolved sha, which is hex ASCII and can
  never begin with `0x01`.
- The name is hashed rather than used as a directory component. Branch names carry slashes.
- **A named session neither adopts nor is adopted.** The reader has said which review this
  is, so a scan has nothing left to decide, and an unnamed range must never capture a
  session someone addressed by name. `resolve` therefore returns before the catalogue scan
  and makes no git call at all.
- It works from the picker, which has no spelling to key on, and with any range. The range
  still decides what is diffed; the name only decides where progress lives. That is the
  separation `ReviewSource` already draws between endpoints and identity (ADR 0017).
- `ReviewIdentity` becomes an enum — `Range { base, head_spec }` or `Named(name)`. "A named
  session is not a range" is then a fact the compiler checks, not a convention `adopt` has
  to remember.

**The name is not inferred.** `--name "$(git branch --show-current)"` is documented because
the shell already answers that question, and the answer belongs to the reader rather than to
a heuristic that reads differently each week.

## Consequences

- A named session survives a rebase of the base, of the head, or of both. It is the only
  thing here that does.
- Naming is opt-in and per-invocation. Forget the flag and you open the range's own review,
  which is a different one — the flag is the identity, so it is not a decoration.
- Moving to a name does not carry an unnamed review's progress across. Adoption would have
  to bridge them, and that is exactly the capture a name exists to prevent.
- `identity.json` records either a name or a pair of endpoints, never both. A half-written
  record is read as neither: recognisable, not adoptable.
- Nothing else changes. Marks still key on hunk digests (ADR 0025) and findings still
  re-anchor by digest, so a named session reopened over a different range keeps what still
  matches and orphans what does not.

## Alternatives rejected

**Key the base on its spelling instead of its resolved sha**, mirroring the head. It closes
the rebase hole for `dfr review main..HEAD` and leaves it open everywhere else: type a sha
and it keys on the sha, use the picker and there is no spelling at all. It also changes what
an unnamed review means for every reader who never asked for it.

**Infer the name from the branch that points at the base.** Zero names, several names, and a
different answer as branches move.

**Match on content overlap** — compare the new plan's hunk digests against a candidate's
current plan document, and adopt on a strong overlap. It would cover a rebase without a
flag. It also joins two branches that share most of their work, which is the collision
ancestry exists to prevent, and it makes identity depend on how much of a diff two reviews
happen to have in common. Not ruled out forever; ruled out as an implicit default.
