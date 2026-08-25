# The review TUI (`dfr review`)

A dedicated reviewer over the grouped, ordered document. Two panes: the reading plan and
the diff for the selected entry.

**Geometry is model state.** The event loop measures the terminal and pushes the pane
heights into the model before any key is handled, so scrolling is arithmetic over a known
height and drawing is a pure function of the model. A resize is folded in like any other
event, in stream order — keys before and after it each see the geometry that was true when
they were pressed. Row *contents* still compose their columns at draw time from the pane
width, which is why a resize never rebuilds rows.

**Reading plan.** Groups in rank order, each a small block: the group **id**, effort tier
and label, then file count with added/removed line totals (in their own colours), the
role, and `after: <ids>` naming the groups it follows — the id column is what makes those
references resolvable.

The trailing **back-filled** group — classes the model omitted, recovered by the coverage
audit (ADR 0001, invariant 5) — is labelled `[unclassified]` with a `?` tier glyph rather
than `[focus]`/`F`. It is must-read either way, but for a different reason: nothing
judged it. The shadow-branch renderer has always said so on its commit subject, and the
two renderers read the flag from the same projection so they cannot disagree.

The plan is a **DAG, not a tree**: a group can follow several others, and the graph can
even contain cycles (two groups that each define symbols the other uses). The ordering
stage breaks a cycle deterministically, which means some edge cannot be honoured; the
plan says so rather than hiding it — a dependency listed **later** than the group that
follows it is marked `↓`. Selecting a group draws a connector in the left gutter linking
it (`◆`) to every group it **follows**, so what has to be read first is visible without
reading ids. One direction only: the reverse edge is deliberately not drawn, so the gutter
says the same thing as the `after:` line beneath it rather than something different. Counts derive from hunks, so binary/submodule
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
choice persists per review); syntax highlighting and word-level change emphasis.

**Colour carries the change.** There are no `-`/`+` marker columns: a changed line's
background runs to the pane edge, and its line-number cell is a stronger block of the same
colour, which is what makes the gutter read as an edge. In split mode a row that exists on
only one side has its other half filled with `╱` — an absent line is visibly absent rather
than looking like an empty one. Because a background is what marks a change, the cursor
cannot be one: it is a `▸` in the leading gutter cell, which reads on any background, and
that cell is reserved so moving the cursor never shifts the pane sideways.

**A row about the file runs across the file.** A hunk header is a band —
`── C31 · +25 ─────`, the shape class and the size of the change — rather than a
`@@ -479,0 +480,25 @@` line: every row carries both line numbers in its gutter, so the
coordinates repeated what was already on screen in a notation you had to decode. What the
header uniquely says stays on it: the class, the counts, the reviewed mark, the finding
count, and in the file view the group's label. It remains a selectable row, so `n`/`N`
jump to it and `space` and `c` act on it. Headers and boundary rows rule out to the pane
edge and cross the split separator, because what they describe is not one side of the
file.

**Context is expandable.** Canonical `-U0` hunks carry no context, so it is read out of the
base and head blobs — three lines either side by default. Where more of the file is
available, the pane says so on a **boundary row** at each end of what is shown
(`── ↑ 16 more above — z shows 10 ──`); put the cursor on it and `z` pulls in another step.
Both numbers come from `[review]` in the user config (`context`, `context_step`). Expand
two hunks until their windows meet and the boundary rows between them disappear: the file
reads as one continuous stretch, each hunk keeping its own header band so `n`/`N` and
findings still work. A gap between two blocks keeps a boundary at each end rather than
collapsing to one — a step only reveals part of it, so both ends stay live. A boundary row is deliberately **not** a hunk — `space` and `c` ask
for one rather than acting on a row that is only about how much of the file is visible.

A window never crosses a neighbouring hunk, shown or not. Between two hunks the old/new
line offset is constant, which is what lets one context stretch carry both sides' numbers;
across a hunk it is not. Stopping at the neighbour keeps every rendered line number honest
and means expanding can never present someone else's change as untouched context. Reaching
one is the same as reaching the file's edge: the boundary row has nothing left to offer, so
it is not drawn.

Only the lines actually drawn are diffed and highlighted — per hunk, `similar` runs over
the changed lines alone and syntect over the window plus a fixed lookback, so a keypress
costs what is on screen rather than the size of the files the group touches (ADR 0021).
How far each hunk is expanded is **transient**, like an open fold: a reading aid for this
sitting, not a finding, so nothing about it reaches the sidecar store.

## Keys

| key | action |
|---|---|
| `j`/`k` | move (groups pane: switch group · diff pane: move over rows) |
| `J`/`K`, `{`/`}` | previous / next group |
| `tab`, `enter` | switch pane focus |
| `ctrl-d`/`ctrl-u` | half page |
| `g`/`G` | top / bottom |
| `n`/`N` | next / previous hunk |
| `z` | on a `──` context boundary row: show more of the file · elsewhere, fold the skim remainder or noise group (plan view) or a directory (file view) |
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

`dfr review` with no arguments opens a picker instead of failing. It has a list of recent
commits from which you pick the **base**, and — **only when the worktree has uncommitted
changes** — a checkbox, **include uncommitted changes (worktree)**, ticked by default. The
review then runs from that commit to the worktree snapshot (box ticked) or to `HEAD`
(unticked or absent), which is how "everything on my branch since `main`, including what I
haven't committed" is expressed.

The checkbox is hidden on a clean worktree because it could not change anything: with
nothing outstanding the snapshot is `HEAD`'s own tree, so ticking it would re-hash every
tracked file to produce an identical review, filed under a different identity. Cleanliness
is detected with plumbing — `diff-index` for tracked changes, `ls-files --others` for
untracked ones — and errs toward showing the box, since a wrong "clean" would hide an
option you need while a wrong "dirty" costs one harmless row. The range is `base..head`, so it
**excludes the base commit's own changes**: the bar covers the commits above the cursor,
and the selected row is marked as the boundary. Picking the newest commit with the box
unticked therefore reviews nothing, and the title says so — which on a clean worktree is
the only thing picking the newest commit can mean. Commits show the branch and tag names
pointing at them (read with `for-each-ref`, plumbing, so no dependence on
`log.decorate` config), and a leading bar marks every row inside the range as the cursor
moves, so what is covered is visible while choosing. `HEAD` itself is a valid base: with
the box ticked it means "just my uncommitted work".

Uncommitted sources run the full grouped pipeline like any range (ADR 0017); their review
identity keys on the base sha plus the stable literal `WORKTREE`, so marks and findings
survive while the snapshot tree churns with every edit. A committed pick keys on `HEAD`
as typed, so the review survives new commits landing.

A clean worktree is therefore a committed pick, filed under `HEAD`. One consequence worth
knowing: commit your outstanding work mid-review and the next `dfr review` opens the
`HEAD`-keyed review, not the `WORKTREE`-keyed one you were in. Nothing is lost — the old
review is still on disk under its own id — but its marks are not the ones you see.

## While the pipeline runs

The reviewer opens immediately and shows a splash until the document is ready:
enumerate → classify → group → order, the active stage spinning, with an elapsed timer.
The grouping line names the agent it is waiting on, or says the cache spared the call —
that stage shells out to an LLM on a cache miss and dominates the wait. `q` cancels, and
cancelling kills the agent subprocess rather than merely stopping the screen from
watching it: raw mode has already disabled `Ctrl-C`, so nothing else would reap it.

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
