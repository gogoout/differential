# The review TUI (`dfr review`)

A dedicated reviewer over the grouped, ordered document. Two panes: the reading plan and
the diff for the selected entry.

**Geometry is model state.** The event loop measures the terminal and pushes it into the
model before any key is handled, so scrolling is arithmetic over a known height and drawing
is a pure function of the model. A resize is folded in like any other event, in stream
order — keys before and after it each see the geometry that was true when they were
pressed. Row *contents* still compose their columns at draw time from the pane width, which
is why a resize never rebuilds rows.

The panes are a fixed split and **focus never changes a height**: the overviews below float
over a pane rather than taking room from one, which is what keeps the heights a function of
the terminal alone.

**Keys act on the pane you are in.** `z` shows what is being withheld — a context
boundary's hidden lines, a folded remainder, a directory — and which of those it means is
decided by the focused pane, not by where the diff's cursor happens to be parked. The
cursor is a diff row wherever the focus is, so without that rule a press in the file tree
opened part of a file the reader was not looking at.

**One key for files, acting on the pane it is pressed in.** In the left pane `f` chooses
which list of files you are reading — the plan or the tree. In the diff pane it chooses
which file you are looking at. It used to be two keys, and the one that switched the left
pane worked from either side, so a press in the diff pane silently rearranged the pane
behind it.

**Reading plan.** Groups in rank order, each a small block: the group **id**, effort tier
and label, then file count with added/removed line totals (in their own colours), the
role — as a pill, in the same muted colours a hunk's title pill wears, since it is a fact
about the group rather than a decoration on it, and the **same** pill the group's header
carries in the detail pane — and `after: <ids>` naming the groups it follows — the id column is what makes those
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
follows it is visible in the connector, which runs **down** from the selected group rather
than up. The `after:` line does not mark it: every id there reads the same, since a warning
on a row is worth its weight only when the reader can do something about it. Selecting a group draws a connector in the left gutter linking
it (`◆`) to every group it **follows**, so what has to be read first is visible without
reading ids. One direction only: the reverse edge is deliberately not drawn, so the gutter
says the same thing as the `after:` line beneath it rather than something different. Each
tick wears the **same arm** the file tree's guides wear (`├─`, `└─`, and `◆─` at the
selected group), in the pane's own border grey, so it reaches the title it points at: two guides a pane apart had no
reason to draw the same relation differently, and a tick that stopped a cell short read as
a mark floating beside the row rather than as a line into it. Counts derive from hunks, so binary/submodule
changes contribute zero and a rename counts as two files (the canonical view is
`--no-renames`). Skim groups show one exemplar per shape class with the remainder folded
behind a single line; noise groups are folded entirely.

**File view.** `f`, in the left pane, switches it to a **tree** of every file in the document
(including binary/submodule changes the group view cannot surface). Directories nest,
show their aggregate counts, and fold with `z`/`enter`; selecting a directory shows every
hunk beneath it, selecting a file shows that file's hunks in position order regardless of
grouping, each hunk header carrying its group's label. Reviewed marks are shared between
the views — they key on class content either way.

**Focus floats a map of the other pane.** Each side keeps its own job; what changes is what
is laid over it, so an unfocused pane earns its space without either pane losing any.

Reading the **plan**, the document's file tree floats over the detail pane with the selected
group's files lit — `files in g0 · 3 of 8` — so what a group spans is one look rather than a
walk through its hunks. It sits **below the group's header block**, leaving the full label
and description readable where the 40-column plan pane truncates them, and the diff carries
on underneath as a preview of what entering the group will show. The tree is drawn with
connector guides and marks a lit file beside its name rather than out in a column of its
own. Deliberately not interactive: it is a map, and a second cursor in a second pane is a
thing to explain and to get wrong.

**The map folds on the group.** A document of any size otherwise runs past the bottom of
the float. A directory the group never enters is **one row** with a `▸` and the number of
files under it, and a chain of such directories is joined into that row (`▸ a/b/c/`), so a
deep path the reader is not going into costs one line rather than four. Inside a directory
the group does enter, the files it does not touch fold to a count (`… 6 more`). What
remains is exactly the group's own files, each lit, in the tree that holds them — which is
the question the float is asked. The fold is the map's own: it never touches the file
view's folds, whose state belongs to the reader's `z` and to that pane's cursor.

Reading the **detail**, a flat list of the files in view floats over the foot of the plan
pane, the current one lit edge to edge and the title counting `file 2 of 7`. Lit, not
marked with a glyph: the row the reader is on is the one place they are already looking,
and a marker column costs every other row two cells to say nothing.

Neither float appears in the **file view**, where the left pane is already a file tree: a
map of one group would name a group nothing is selecting, and a file list would be the pane
behind it. Both trees are drawn with the same connector guides.

**A file header sticks.** It is the path and nothing else — no leading glyph, since the
one bar in this column belongs to a hunk's edge and a second one a row above it said
nothing the bold cyan path had not already said. Scrolled past it, the filename pins to
the pane's top row, which costs a row only while it would otherwise be invisible. The hunk pill does not stick with
it: two pinned rows is most of a small pane, and the pill's information is in the plan pane
anyway.

**Diff pane.** Unified layout by default, `s` toggles a side-by-side split (the layout
choice persists per review); syntax highlighting and word-level change emphasis.

**Colour carries the change.** There are no `-`/`+` marker columns: a changed line's
background runs to the pane edge, and its line-number cell is a stronger block of the same
colour, which is what makes the gutter read as an edge. In split mode a row that exists on
only one side has its other half filled with `╱` — an absent line is visibly absent rather
than looking like an empty one.

**The cursor is that block, brighter — and a bar beside the frame.** A row-wide colour
cannot carry it: a changed line already has one, and a line style sits under span styles.
So the cursor lights the line-number cell — the brighter twin of the block it wears
anyway, which keeps a deleted line red and an added one green while making the cursor the
strongest cell in the column. An unchanged line has no block of its own and takes the
plain cursor grey. The cell never changes width, so moving the cursor never shifts the
pane sideways.

Only a diff row has a line number, though, and `space`, `c` and `z` all act on rows that
do not: a hunk header, a fold, a context boundary. On those the cursor was a faint tint
and nothing else. So a **bar sits in the cell just inside the pane's frame** on every
selectable row, keeping that cell's own background — over a lit block it stands on the
change colour rather than punching a hole in it. One thing to look for, on every row.

**Both gutters light.** A split row is one row, so a cursor drawn on the left half alone
read as a cursor on that side's line. The absent side keeps a blank line-number cell of
the same width, so the block lands in the same column on a row that exists on one side
only — where a marker glyph used to vanish into the `╱` fill.

**A hunk is a pill and an edge.** Its header is a band of `╱` hatch, and a pill appears on
it only for the hunk the cursor is in — ` +25 −3 · C31 `, the size of the change and then
the shape class — rather than a `@@ -479,0 +480,25 @@` line: every row
carries both line numbers in its gutter, so the coordinates repeated what was already on
screen in a notation you had to decode. What the header uniquely says stays on it: the
counts, the class, the reviewed mark, the finding count, and the group's id where that is
not already obvious. The counts lead: how much changed is what a reader sizes a hunk up by,
and putting a class token they cannot read at a glance in front of two numbers they can
made the size the second thing on the row. It remains a selectable row, so `n`/`N` jump to it and `space`
and `c` act on it.

Below the pill, a vertical **edge** runs down the hunk's changed rows. Deliberately not a
box: closing one top and bottom with horizontal rules cut the file into slabs and broke the
flow of reading down it. An edge says where a hunk begins and ends without chopping up the
page.

**The edge is the pane's own border.** It sits in that column rather than a cell inside it,
so it costs the content no width and there are never two vertical lines a cell apart. The
pill **starts against that border**, with no cell of gap: the pill caps the edge that runs
down the hunk beneath it, and a gap read as two marks that happened to line up rather than
as one mark and the run it opens.

Which is why a **frame never lights** — a pane's or a float's. Focus is carried by the
**title**, in the same colour the border used to take. A lit frame drew a box around half the screen to
say a thing about the cursor, and it competed with the hunk edge — the one border in this
view that means something. The title is where a reader looks to know which pane they are
in anyway. For the same reason the plan's connector wears **one** colour, the border grey:
it says which rows are tied together, and the rows themselves say what they are.

**Only the hunk the cursor is in wears a colour**; every other edge is muted to the gutter,
because a screenful of accents is no accent at all. Which box is lit is a cursor question
and the cursor moves without rebuilding rows, so a row carries the colour it *would* take
and drawing chooses. What it changes is the header's **whole content**: idle, the row is
hatch and nothing else; the cursor moving in is what puts the pill there, its leading cell
lit in the accent so the marker and the run below it read as one thing rather than as a
label that happens to sit above a line.

A pill on every header was a column of labels down the page competing with the code they
label, and the one worth reading is the hunk you are in. Entering a hunk is one keypress,
and it is the same press that makes its header worth reading.

What an idle header keeps is the **marks**: the group's id where the hunk is foreign, `✓`
where its class is read, and `◆ N` for the findings filed against it. Those are facts about
the hunk and they are what a reader scans a file for; the class and the counts describe it,
and describing every hunk at once is the column that was in the way. The hatch carries the
rest of the row, so a hunk still begins somewhere visible without a word on it.

Filling the whole pill said this far more loudly than it needed to — a block of colour the
eye went to before the code — and it cost the palette a second, darker ink for every span
that could sit on a pill, since `add_fg`/`del_fg` glow on a dark background and vanish on a
bright one. One cell needs no twins, so the `+N`/`−M` counts are one pair everywhere. The
fill stays bright: that lit cell is cyan for your hunk and a muted cyan for a foreign one,
and a darker fill put the two too close to tell apart.

**Cyan is where you are.** The pane title wears it, the cursor's bar wears it, and so does
the edge of the hunk you are reading — one colour for one idea, rather than a third accent
to learn. A hunk already **reviewed** wears green instead: that is the one fact worth
seeing at a glance on a hunk you have been through. A **foreign** hunk wears the same cyan,
**muted** — it is real code you asked to see, so it belongs to the same family, but it is
not on this reading list and a full accent would say it was.

Headers and boundary rows rule out to the pane edge and cross the split separator, because
what they describe is not one side of the file. A boundary **divides**, so its rule runs on
both sides of a centred label; a header **labels** what follows it, so it starts at the left
and stays there — a label that drifted with the pane width would be harder to scan down a
column.

A **context boundary** is a control, not a caption: a tinted band across the pane with its
arrow in the border column, **lighter on the cursor's row** — a control the reader is
standing on has to look like the one they are about to press, and the band carries its own
colour the whole way across, so the tint that marks the cursor everywhere else never
showed through it. It says `29 lines hidden` or, once the gap is spent,
`next: C42 "Group 42"` — and, on the cursor's row, `z shows 10` or `z shows it`, straight
after the label rather than out at the pane's edge, where a key is a key you have to go and
look for — a control that does not say how to work it is a label. But a screenful of bands each naming
the same key is a wall the reader stops reading, so the key appears on the **cursor's row
only**. Whether a row is the cursor's is a cursor question, so the row carries the text and
drawing chooses — the same way a hunk's accent works, one column over.
Where two boundaries are the two ends of one gap they carry the **same** count, so opening
one end drops the other's figure too. Deliberately not `@@ …` — that is the notation the hunk headers
dropped, and the gutters either side already carry the numbers.

Where two blocks meet, the two boundary rows describing that one gap sit **adjacent with no
blank between them**, so the seam reads as one band. They stay two rows while there is a
direction to choose — each keeps its own `z`, so no key has to mean two things — and both
carry the **same** count, since they are two ends of one gap and opening either shortens it.

Two rows exist to offer a **direction**. Where both ends would do the same thing there is
none to offer and the second row only repeats the first, so one `↕` band speaks for both.
That happens two ways: one press would close the gap — which point depends on
`context_step`, so a wider step collapses the seam sooner — or both ends are spent and name
the *same* hunk beyond, which is what an unlisted hunk sitting between two blocks looks
like from either side.

Pills are square. The half-circle caps that would round them are drawn at inconsistent
widths across terminals and fonts, and a pill a cell wider in one terminal than another is
worse than a pill with corners.

**Context is expandable.** Canonical `-U0` hunks carry no context, so it is read out of the
base and head blobs — three lines either side by default. Where more of the file is
available, the pane says so on a **boundary row** at each end of what is shown
(`── ↑ 16 more above ──`); put the cursor on it and `z` pulls in another step.
Both numbers come from `[review]` in the user config (`context`, `context_step`). Expand
two hunks until their windows meet and the boundary rows between them disappear: the file
reads as one continuous stretch, each hunk keeping its own header band so `n`/`N` and
findings still work. A gap between two blocks keeps a boundary at each end rather than
collapsing to one — a step only reveals part of it, so both ends stay live. A boundary row is deliberately **not** a hunk — `space` and `c` ask
for one rather than acting on a row that is only about how much of the file is visible.

**A window stops at a neighbouring hunk, and says so.** Grouping is by shape class, so one
file routinely holds hunks belonging to several groups. When a window reaches one this view
does not list, the boundary row does not vanish — it **names** it
(`↓ next: C31 "Rename sweep"`), and another `z` pulls that hunk in. So a long
expansion can never silently swallow someone else's change, and a wall can never be
mistaken for the end of the file. A boundary row disappears at one place only: a real file
edge.

A crossed hunk carries a **dashed** edge and its owning group's **id**
(`╌ +25 −3 · C31 · g7 ╌`) — the id alone, since a label is a sentence and the header would
then be longer than the code under it — real code the reviewer asked to see, plainly not on
this group's reading list. The id is what the plan pane's rows and their `after:` lines are
keyed by, so it is what turns "some other group" into a row you can go and look at. It is absorbed whole
and costs no context budget, because showing half a change would be worse than showing
none. `n`/`N` pass over it, since it is not on this reading list. `space` and `c` treat it like
any other hunk: a reviewed mark keys on class content and a finding anchors on the hunk's
digest, and both are group-independent — so reading it here is reading it everywhere, and
a finding filed here is filed against the hunk itself rather than against this view of it.

That a crossed hunk is a **change** segment, never flattened into context, is what keeps
the numbers honest. Between two hunks the old/new line offset is constant, which is what
lets one context stretch carry both sides' numbers from a single length; across a hunk it
is not, and a change segment carries each side explicitly.

Only the lines actually drawn are diffed and highlighted — per hunk, `similar` runs over
the changed lines alone and syntect over the window plus a fixed lookback, so a keypress
costs what is on screen rather than the size of the files the group touches (ADR 0021).
How far each hunk is expanded is **transient**, like an open fold: a reading aid for this
sitting, not a finding, so nothing about it reaches the sidecar store.

**The footer is two pills and two keys.** What the review stands at goes on the left, as
pills — `0/88 classes reviewed` and `3 findings` — because those are facts about the
review, the same as a group's role and a hunk's class, and they wore a run of grey words
that read as chrome. Each takes its own colour once it has something to say: green when
every class is read, magenta when anything is filed. A transient message follows them.

Against the right edge sit `? help` and `q quit`, and nothing else. The footer named ten
keys, in a different order and a different wording from the modal that also named them —
a wall the reader stops seeing, and two lists to keep in step. `?` is the one place a full
list belongs; the footer's job is to point at it.

## Keys

| key | action |
|---|---|
| `j`/`k` | move (groups pane: switch group · diff pane: move over rows) |
| `J`/`K`, `{`/`}` | previous / next group |
| `tab`, `enter` | switch pane focus |
| `ctrl-d`/`ctrl-u` | half page |
| `g`/`G` | top / bottom |
| `n`/`N` | next / previous hunk (skipping hunks crossed in from other groups) |
| `z` | show what is being withheld, in the pane you are in — diff pane: on a `──` context boundary row, more of the file, or the hunk it names · elsewhere, the skim remainder or noise group · plan pane, file view: a directory |
| `s` | toggle unified / side-by-side diff layout (persisted) |
| `f` | files, in the pane you are in — plan pane: toggle reading plan ↔ file tree (persisted) · diff pane: the file-list modal (`enter` jumps to the file) |
| `space` | mark reviewed — the whole selected group/file in the left pane, the hunk's **class** in the diff pane (one exemplar verifies the shape) |
| `v` | start a line selection at the cursor · `j`/`k` extend it · `esc` drops it · `c` writes a finding over it |
| `c` | add a finding on the current hunk — a float over the diff, titled with the file and lines it annotates (`enter` saves · `shift+enter` or a trailing `\` before `enter` makes a newline · `esc` cancels) |
| `dd` | delete the finding under the cursor |
| `y` | copy the open-findings summary to the clipboard — a markdown list of `file:lines: note`, and nothing about groups: a group is how this reviewer chose to READ the branch, and the summary is pasted somewhere that has no idea what `g7` was |
| `?` | help — the keys, as one uninterrupted table |
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

**A finding is about lines.** `c` on a diff row annotates **that line**; `V` first starts a
selection the cursor extends, and `c` then annotates the run. On a row that is not a line of
a file — a hunk header, a fold — `c` annotates the whole hunk, which is what every finding
used to do.

The anchor is stored as an **offset into the hunk**, not as a line number. The digest fixes
the hunk's content, so a hunk that moved in the file still holds the same line at the same
offset — while its absolute number did not survive the move. ADR 0013 is unchanged by this:
the anchor is still the digest, with a position inside it. A record written before offsets
existed reads `0`, which lands it on the hunk's first line — exactly where it was.

A finding is **drawn under the line it annotates**, so a note and its subject are read
together. One whose line is not on screen — the context around it still folded, or a
regeneration that could only re-anchor it to the hunk — falls back to its hunk's header,
where they all used to sit.

It is drawn as a **quoted panel**: every line of the note behind a muted rail, in muted
italics. It is prose the reviewer wrote about the code above it, so it has to read as a
different kind of thing from the code without competing with it — which one truncated line
under a bright marker glyph did not. Every line of the panel is a finding row, so `dd`
deletes the note from any of them and the cursor never lands on a line belonging to
nothing.

**Writing a finding** opens a float over the diff rather than a strip pinned to its foot:
a note is about lines you should still be able to see. Its border carries the file and line
range it will anchor to, and its footer the keys.

**`enter` saves.** A finding is usually one line, and the key that ends a line is the key a
reader reaches for to be done with it. A newline is `shift+enter` where the terminal
reports it, and a **trailing `\` before `enter`** where it does not — most terminals send
plain `enter` for both without the keyboard enhancements this reviewer deliberately does
not ask for, so `shift+enter` alone would leave some readers no way to write a second line.
`ctrl-s` saves as well: it costs one arm, and some terminals swallow it before the app ever
sees it, which is why it cannot be the only way.

A **paste** lands in the box whole. Bracketed paste is on precisely so a multi-line paste
arrives as one event instead of a run of keys each driving a normal-mode action; the event
was being dropped, which read as the box being broken.

## Findings contract

`dfr findings <range>` re-anchors and prints the findings as JSON — each record carries
`{id, created, body, status, moved, plan_hash, anchor: {file, side, line, end_line, offset,
span, hunk_digest, line_text, end_line_text}}`. `line`/`end_line` are the resolved numbers
for a consumer that only reads; `offset`/`span` are what survive a regeneration. All five
are additive with defaults, so an older `findings.jsonl` loads unchanged. `hunk_digest` keys back into the plan document's `hunks[].digest` and from
there to `forge_position`, which is how agent tooling and the future forge consumer act on
them. The `y` clipboard summary is the human-readable projection: one markdown bullet per
open finding, `file:line (group label): body`.
