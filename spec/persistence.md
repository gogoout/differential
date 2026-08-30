# Incremental review and persistence

`differential` is primarily a local review tool, and the branch under review moves. The
model is: **regeneration is total, state is a sidecar.**

## Plan documents are immutable

A plan document is a pure function of `base..head` (plus config). When new commits land, the
document is regenerated completely — never patched. Each generated document is
content-addressed and immutable; a review in progress never reshuffles under the reviewer
(groupings are additionally pinned via the grouping cache, since labels are
non-deterministic while coverage is not).

## State directory

Per-repo state lives inside the target repo's git directory — like `rebase-merge` state:
private, per-clone, worktree-safe, never committable.

```
<git-common-dir>/differential/
├── reviews/<review-id>/
│   ├── plans/<content-hash>.json   # every generated document, immutable
│   ├── current                     # the active plan's content hash
│   ├── findings.jsonl              # the comment store
│   ├── identity.json               # a name, or the endpoints it was opened as
│   └── state.json                  # review progress
├── reviews/<review-id>/
│   └── alias                       # a redirect: read that review instead
└── cache/
    ├── grouping/<classes-hash>.json   # the raw model response (ADR 0009)
    └── document/<content-hash>.json   # the pre-group document (ADR 0022)
```

`cache/` and `reviews/` are **siblings, and that is load-bearing**. Everything under
`cache/` can be recomputed — the groupings at the cost of a model call, the documents for
free. Findings cannot be recomputed at all. Rooting the two apart means nothing that
clears one can reach the other by construction, rather than by remembering to be careful.

`dfr clean` removes `cache/` and nothing else, reporting what it removed; `--dry-run`
reports without removing. Clearing is a deliberate act because the next grouped run pays
for a fresh model call.

## Review identity

A review is keyed by **base + the branch/MR ref under review, not by head** — so the same
review survives the head moving. Regeneration adds a new immutable plan and advances
`current`.

The key is the head endpoint **as typed**, which makes the spelling part of the review's
name. On its own that split one range into two reviews: mark `<base>..<sha>`, reopen
`<base>..HEAD` where `HEAD` *is* that sha, and the progress was filed under a name you did
not type again (ADR 0026).

So a spelling with no review of its own **adopts** one. A filed review is adoptable when
its base sha is the same and one head is reachable from the other:

- Two spellings of one commit — a short sha, a full sha, `HEAD`, a branch name — adopt each
  other, because the heads are equal.
- A review opened after new commits adopts the one you were already in, because the old
  head is an ancestor of the new one. That is the carry-forward: unchanged hunks stay
  marked and the new commits arrive unread.
- Two branches off one base do **not** adopt each other. Neither head reaches the other, so
  their findings can never collide.

Adoption is silent and permanent. It is recorded as an `alias` file under the new
spelling's id, so every later open costs one file read and `dfr findings` reaches the same
review the reviewer is looking at. `identity.json` records what a review was opened as,
which is what a later scan reads; a review of uncommitted work writes none, because its
head is a synthesized tree and ancestry says nothing about it (ADR 0017).

The join never expires. Switch branches afterwards and the two spellings stay one review:
marks key on hunk content and findings re-anchor by digest, so a diff that no longer
matches costs orphans, never loss.

### A rebase, and the named session

Adoption rests on ancestry, and **a rebase defeats ancestry by construction**: rewriting
commits gives a head that is not a descendant of the old one, and rebasing onto a moved base
changes the base sha too. Neither endpoint reaches its old self, so nothing is adoptable.

The loss is the directory, not the content — a hunk digest hashes only the removed and added
lines, so a clean rebase leaves every digest unchanged.

So a reader who wants a session that outlives a rebase **names** it (ADR 0027). The name is
then the whole identity: no endpoint is in the key, it works from the picker and with any
range, and it neither adopts nor is adopted.

```sh
# the branch name as the session name — the shell already knows it, so the
# tool does not have to guess it from a commit
dfr review   --name "$(git branch --show-current)" main..HEAD
dfr findings --name "$(git branch --show-current)" main..HEAD
```

The name is never inferred. Asking git which refs point at the base gives zero names, or
several, and a different answer as branches move. A pull request is not inferred either: you
name the source and target branch when you open it.

`identity.json` records **either** a name **or** a pair of endpoints, never both.

## Comments

Comment record:

```jsonc
{ "id": "...", "created": "...", "body": "...",
  "status": "open" | "resolved" | "orphaned",
  "plan_hash": "<plan this was written against>",
  "anchor": { "file": "...", "side": "old" | "new",
              "line": 47, "end_line": 52,
              "offset": 3, "span": 5,
              "hunk_digest": "<hunks[].digest>",
              "line_text": "...", "end_line_text": "..." } }
```

Comments anchor to a hunk's **digest** (exact content hash, stable across regenerations),
never to its positional id — and to an **offset inside it**, never to a line number. The
digest fixes the hunk's content, so a hunk that moved in the file still holds the same line
at the same offset, while its absolute number did not survive the move. `line`/`end_line`
are the resolved numbers, recomputed on every re-anchor for consumers that only read;
`offset`/`span` are the durable pair. `offset` is **signed**: a reader can annotate a
context line, and context sits on both sides of a hunk. `end_line_text` does for the range's far end what
`line_text` does for its start.

Every field but `file`, `side`, `line` and `hunk_digest` is additive with a default, so a
`findings.jsonl` written before ranges loads unchanged: `offset: 0`, `span: 0` puts it on
the hunk's first line, which is where it already was.

## Re-anchoring on regeneration

When `current` advances, a migration pass visits every comment:

1. **Exact**: a hunk with the same digest exists in the new plan → reattach.
2. **Fuzzy**: no digest match, but the anchor's context lines match at some position in the
   file → reattach, flagged as moved.
3. **Orphaned**: neither → `status: orphaned`, surfaced in the TUI's findings list (`F`),
   which is the only place an orphan can be read or deleted — it has no line and no hunk,
   so nothing in the diff pane can reach it.

Orphaned comments are **never deleted** — the same philosophy as the grouping coverage
back-fill: detection and preservation, not silent loss.

Review progress (`state.json`) carries "reviewed" marks keyed by **hunk digest**: a hunk
whose content is unchanged stays reviewed; a hunk that changed resets, and only that hunk
(ADR 0025). Marking a group marks every hunk in it. Two byte-identical hunks carry one
digest, so a mark on either covers both — the mark names content, exactly as an anchor
does.
It also carries the resume cursor and the TUI's persisted layout choices (`split_diff`,
`wrap`, `file_view`) — all additive fields. The cursor is `(id, row)` where `id` is a group id in
the semantic view and a file path in the flattened file view; the `file_view` flag
disambiguates on load.

`split_diff` is an **option, and the absent case is load-bearing**. `None` means the reader
has never pressed `s` on this review, so the reviewer falls back to `review.diff` from the
user config; `Some` is a choice they made, and it wins whatever the config later says. That
is what stops a config edit moving the layout under someone midway through a read.

`wrap` is an option for the same shape of reason, though it has no config key to fall back
to: soft wrap is off until the reader presses `w`. A layout preference is worth setting
once; wrapping is something a reader wants for the file in front of them.

The migration needed no code. A `state.json` written before the field became an option
records a bare `false` or `true`, which deserialises to `Some(false)` or `Some(true)` — so
every review already on disk keeps the layout it had, and only a review with no state file
at all takes the configured default.

## Status

Implemented in `engine::review_state` (primitives: store, types, re-anchoring) and
`engine::review_session` (the owning facade). **The engine owns all persistence**: a
renderer opens a `ReviewSession` and reads/mutates through it — every mutation
(reviewed mark, finding, cursor) is on disk before the call returns, and renderers hold
no review state of their own. The TUI ([tui.md](tui.md)) and `dfr findings` are both
session consumers; the forge consumer will publish from the same store.
