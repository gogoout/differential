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
│   ├── comments.jsonl              # append-only comment store
│   └── state.json                  # review progress
└── cache/grouping/<classes-hash>.json
```

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
  "anchor": { "file": "...", "side": "old" | "new", "line": 47,
              "hunk_digest": "<hunks[].digest>",
              "context_before": ["..."], "context_after": ["..."] } }
```

Comments anchor to a hunk's **digest** (exact content hash, stable across regenerations),
never to its positional id.

## Re-anchoring on regeneration

When `current` advances, a migration pass visits every comment:

1. **Exact**: a hunk with the same digest exists in the new plan → reattach.
2. **Fuzzy**: no digest match, but the anchor's context lines match at some position in the
   file → reattach, flagged as moved.
3. **Orphaned**: neither → `status: orphaned`, surfaced in a dedicated list.

Orphaned comments are **never deleted** — the same philosophy as the grouping coverage
back-fill: detection and preservation, not silent loss.

Review progress (`state.json`) carries "reviewed" marks keyed by class/group **content
hash**: a group whose content is unchanged stays reviewed; anything that changed resets.

## Milestone status

Milestone 1 ships the enablers: this spec, `hunks[].digest` in the schema, and the state-dir
convention. The `review-state` crate (comment store + re-anchoring) lands with the TUI; the
forge consumer publishes from the same store.
