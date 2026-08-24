# The review TUI (`dfr review`)

A dedicated reviewer over the grouped, ordered document. Two panes: the reading plan
(groups in rank order, effort/role tags, per-group progress) and the diff for the selected
group — unified layout by default, `s` toggles a side-by-side split (the layout choice
persists per review); syntax highlighting, word-level change emphasis, ±3 context lines
recomputed from the base/head blobs (canonical `-U0` hunks carry none). Skim groups show
one exemplar per shape class with the remainder folded behind a single line; noise groups
are folded entirely. Group headers render `depends_on` with labels — the causal chain, not
just the sequence.

## Keys

| key | action |
|---|---|
| `j`/`k` | move (groups pane: switch group · diff pane: move over rows) |
| `J`/`K`, `{`/`}` | previous / next group |
| `tab`, `enter` | switch pane focus |
| `ctrl-d`/`ctrl-u` | half page |
| `g`/`G` | top / bottom |
| `z` | unfold / fold the skim remainder or noise group |
| `s` | toggle unified / side-by-side diff layout (persisted) |
| `space` | toggle the hunk's **class** reviewed (one exemplar verifies the shape) |
| `c` | add a finding on the current hunk (Ctrl-s save, Esc cancel) |
| `dd` | delete the finding under the cursor |
| `y` | copy the open-findings summary to the clipboard (markdown list) |
| `?` | help |
| `q` | quit — state is saved on every change, quitting never loses anything |

## State

Everything persists through the engine's `ReviewSession` — the TUI is a stateless
frontend that reads and mutates review state only via the session, which writes the
sidecar store (spec/persistence.md) under
`<git-common-dir>/differential/reviews/<review-id>/`, where the review id derives from the
resolved base sha plus the head **as typed** — reviewing `main..feature` keeps one review
while `feature` moves. Reviewed marks key on class content (sorted member digests);
findings anchor on exact hunk digests and re-anchor on every open (exact digest →
content match flagged *moved* → orphaned, never dropped; orphans revive when content
returns). The pane title shows an orphan count when any exist.

## Findings contract

`dfr findings <range>` re-anchors and prints the findings as JSON — each record carries
`{id, created, body, status, moved, plan_hash, anchor: {file, side, line, hunk_digest,
line_text}}`. `hunk_digest` keys back into the plan document's `hunks[].digest` and from
there to `forge_position`, which is how agent tooling and the future forge consumer act on
them. The `y` clipboard summary is the human-readable projection: one markdown bullet per
open finding, `file:line (group label): body`.
