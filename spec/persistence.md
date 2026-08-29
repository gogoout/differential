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
│   └── state.json                  # review progress
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

Review progress (`state.json`) carries "reviewed" marks keyed by class/group **content
hash**: a group whose content is unchanged stays reviewed; anything that changed resets.
It also carries the resume cursor and the TUI's persisted layout choices (`split_diff`,
`file_view`) — all additive, defaulted fields. The cursor is `(id, row)` where `id` is a
group id in the semantic view and a file path in the flattened file view; the `file_view`
flag disambiguates on load.

## Status

Implemented in `engine::review_state` (primitives: store, types, re-anchoring) and
`engine::review_session` (the owning facade). **The engine owns all persistence**: a
renderer opens a `ReviewSession` and reads/mutates through it — every mutation
(reviewed mark, finding, cursor) is on disk before the call returns, and renderers hold
no review state of their own. The TUI ([tui.md](tui.md)) and `dfr findings` are both
session consumers; the forge consumer will publish from the same store.
