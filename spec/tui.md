# The review TUI (`dfr review`)

A dedicated reviewer over the grouped, ordered document. Two panes: the reading plan and
the diff for the selected entry.

**Reading plan.** Groups in rank order, each a small block: effort tier and label, then
file count with added/removed line totals (in their own colours), the role, and
`after: <labels>` naming the groups it follows — dependencies read as labels, never as
ids the reviewer would have to resolve. Counts derive from hunks, so binary/submodule
changes contribute zero and a rename counts as two files (the canonical view is
`--no-renames`). Skim groups show one exemplar per shape class with the remainder folded
behind a single line; noise groups are folded entirely.

**File view.** `v` switches the left pane to a **tree** of every file in the document
(including binary/submodule changes the group view cannot surface). Directories nest,
show their aggregate counts, and fold with `z`/`enter`; selecting a directory shows every
hunk beneath it, selecting a file shows that file's hunks in position order regardless of
grouping, each hunk header carrying its group's label. Reviewed marks are shared between
the views — they key on class content either way.

**Diff pane.** Unified layout by default, `s` toggles a side-by-side split (the layout
choice persists per review); syntax highlighting, word-level change emphasis, ±3 context
lines recomputed from the base/head blobs (canonical `-U0` hunks carry none).

## Keys

| key | action |
|---|---|
| `j`/`k` | move (groups pane: switch group · diff pane: move over rows) |
| `J`/`K`, `{`/`}` | previous / next group |
| `tab`, `enter` | switch pane focus |
| `ctrl-d`/`ctrl-u` | half page |
| `g`/`G` | top / bottom |
| `n`/`N` | next / previous hunk |
| `z` | fold: the skim remainder or noise group (plan view) · a directory (file view) |
| `s` | toggle unified / side-by-side diff layout (persisted) |
| `v` | toggle the left pane: reading plan ↔ file tree (persisted) |
| `f` | file-list modal over the current view (`enter` jumps to the file) |
| `space` | mark reviewed — the whole selected group/file in the left pane, the hunk's **class** in the diff pane (one exemplar verifies the shape) |
| `c` | add a finding on the current hunk (Ctrl-s save, Esc cancel) |
| `dd` | delete the finding under the cursor |
| `y` | copy the open-findings summary to the clipboard (markdown list) |
| `?` | help |
| `q` | quit — state is saved on every change, quitting never loses anything |

## No range: the picker

`dfr review` with no arguments opens a picker instead of failing. It has one checkbox —
**include uncommitted changes (worktree)** — and a list of recent commits from which you
pick the **base**. The review then runs from that commit to the worktree snapshot (box
ticked) or to `HEAD` (unticked), which is how "everything on my branch since `main`,
including what I haven't committed" is expressed. Commits show the branch and tag names
pointing at them (read with `for-each-ref`, plumbing, so no dependence on
`log.decorate` config), and a leading bar marks every row inside the range as the cursor
moves, so what is covered is visible while choosing. `HEAD` itself is a valid base: with
the box ticked it means "just my uncommitted work".

Uncommitted sources run the full grouped pipeline like any range (ADR 0017); their review
identity keys on the base sha plus the stable literal `WORKTREE`, so marks and findings
survive while the snapshot tree churns with every edit. A committed pick keys on `HEAD`
as typed, so the review survives new commits landing.

## While the pipeline runs

The reviewer opens immediately and shows a splash until the document is ready:
enumerate → classify → group → order, the active stage spinning, with an elapsed timer.
The grouping line names the agent it is waiting on, or says the cache spared the call —
that stage shells out to an LLM on a cache miss and dominates the wait. `q` cancels.

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
