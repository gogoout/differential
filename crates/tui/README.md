# differential-tui

The terminal reviewer behind `dfr review`, part of
[`differential`](https://crates.io/crates/differential).

Two panes over the grouped, ordered reading plan: the plan on the left, the diff on the
right. Hunks are marked reviewed and findings are written against lines. Every change is
written to disk as it happens.

Project home: <https://github.com/gogoout/differential>

![The dfr review reviewer, mid-review](https://raw.githubusercontent.com/gogoout/differential/main/assets/screenshot.png)

## The three screens

A session moves through them in order.

1. **The picker.** Only when you run `dfr review` with no range. Pick a base commit, and
   choose whether to include uncommitted work.
2. **The splash.** The pipeline runs on a worker thread. The screen shows the four stages —
   enumerate, classify, group, order — with the active one spinning and an elapsed timer.
   The grouping line names the agent it is waiting on, or says the cache spared the call.
   That stage shells out to an LLM on a cache miss, and it dominates the wait.
3. **The reviewer.** Two panes, described below.

## The reading plan pane

Groups in rank order. Each group is a small block: its id, its effort tier and label, then
a file count with added and removed line totals, then its role as a pill on the right edge.
Then an `after:` line naming the groups it follows.

Selecting a group draws a connector in the left gutter. The connector links the selected
group to every group it **follows**. So what has to be read first is visible without reading
ids.

The plan is a graph, not a tree. A group can follow several others. The graph can even
contain a cycle, when two groups each define symbols the other uses. The ordering stage
breaks a cycle deterministically, which means one edge cannot be honoured. A dependency
listed **later** than the group that follows it is what that looks like, and the connector
runs down rather than up so you can see it.

The trailing **back-filled** group is labelled `[unclassified]`, with a `?` tier glyph
rather than `[focus]`. It must be read either way, but for a different reason: nothing
judged it.

Skim groups show one exemplar per shape class, with the remainder folded behind a single
line. Noise groups are folded entirely.

## The file view

Press `f` in the left pane to switch it to a tree of every file in the document. That
includes binary and submodule changes, which the group view cannot show.

Directories nest and show aggregate counts. Selecting a directory shows every hunk beneath
it. Selecting a file shows that file's hunks in position order, whatever group they belong
to, each header carrying its group's label.

Reviewed marks are shared between the two views. They key on class content either way.

## The diff pane

Unified layout by default. Press `s` for a side-by-side split. The choice is saved per
review. Syntax highlighting and word-level change emphasis are on.

**Colour carries the change.** There are no `-` and `+` marker columns. A changed line's
background runs to the pane edge, and its line-number cell is a stronger block of the same
colour. In split mode, a row that exists on only one side has its other half filled with
`╱`, so an absent line looks absent rather than empty.

**The cursor is that block, brighter, plus a bar just inside the frame.** The bar sits on
every selectable row, including rows that have no line number: a hunk header, a fold, a
context boundary.

**A hunk is a pill and an edge.** The header is a band of hatch. A pill appears on it only
for the hunk the cursor is in, reading ` +25 −3 · C31 ` — the size of the change, then the
shape class. Below the pill, a vertical edge runs down the hunk's changed rows.

An idle header keeps only the marks: the group's id where the hunk is foreign, a tick where
its class is read, and a count of findings filed against it.

**Cyan is where you are.** The pane title, the cursor's bar, and the edge of the hunk you
are reading all wear it. A hunk you have already reviewed wears green instead. A hunk
crossed in from another group wears the same cyan, muted, and a dashed edge carrying its
owning group's id.

### Context is expandable

Canonical hunks carry no context, so the reviewer reads it out of the base and head blobs.
Three lines either side by default. Where more of the file exists, a boundary row says so
(`── ↑ 16 more above ──`). Put the cursor on it and press `z` to pull in another step.

Expand two hunks until their windows meet, and the boundary rows between them disappear.
The file then reads as one continuous stretch.

**A window stops at a neighbouring hunk, and says so.** One file routinely holds hunks from
several groups. When a window reaches one this view does not list, the boundary row names it
(`↓ next: C31 "Rename sweep"`). Another `z` pulls that hunk in whole. So a long expansion
can never silently swallow someone else's change.

A boundary row disappears at one place only: a real file edge.

How far a hunk is expanded is transient. It is a reading aid for this sitting, not a
finding, so nothing about it is saved.

## Keys

`?` opens a short list inside the app. The tables below are the complete list.

### Normal mode — moving

Only `j`, `k` and `enter` depend on which pane has focus. Every other movement key moves
the diff cursor, whichever pane you are in.

| key | pane | action |
|---|---|---|
| `j` / `↓` | left | Select the next group or tree row. |
| `j` / `↓` | diff | Move the diff cursor down one selectable row. |
| `k` / `↑` | left | Select the previous group or tree row. |
| `k` / `↑` | diff | Move the diff cursor up one selectable row. |
| `J` / `}` | either | Next group. |
| `K` / `{` | either | Previous group. |
| `n` | either | Next hunk. Hunks crossed in from other groups are skipped. |
| `N` | either | Previous hunk. |
| `ctrl-d` | either | Half a page down. |
| `ctrl-u` | either | Half a page up. |
| `g` | either | Jump to the first row. |
| `G` | either | Jump to the last row. |
| `tab` | either | Switch pane focus. |
| `enter` | left | In the file view, fold or unfold the directory. Otherwise move focus to the diff pane. |
| `enter` | diff | Nothing. |

### Normal mode — showing and switching

| key | pane | action |
|---|---|---|
| `z` | diff, on a `──` boundary row | Show more of the file, or cross into the hunk the row names. |
| `z` | left, file view | Fold or unfold the directory. |
| `z` | anywhere else | Unfold the skim remainder, or the noise group. |
| `s` | either | Toggle side-by-side and unified layout. Saved per review; `review.diff` sets what a review opens as. |
| `f` | left | Toggle the reading plan and the file tree. Saved per review. |
| `f` | diff | Open the file-list modal. |

`z` and `f` act on the pane you are in. The diff cursor exists whichever pane has focus, so
without that rule a press in the file tree would open part of a file you were not looking at.

### Normal mode — reviewing

`space` is the only key here that depends on the pane. `v`, `c` and `dd` always act on the
diff cursor.

| key | pane | action |
|---|---|---|
| `space` | left | Mark the whole selected group or file reviewed. |
| `space` | diff | Mark the hunk's **class** reviewed. One exemplar verifies the shape. |
| `v` | either | Start a line selection at the diff cursor. `j` and `k` then extend it. |
| `v` | either, while selecting | Drop the selection. |
| `esc` | either, while selecting | Drop the selection. |
| `c` | either | Write a finding. See below. |
| `dd` | either | Delete the finding under the diff cursor. |
| `F` | either | Open the findings list. |
| `y` | either | Copy the open-findings summary to the clipboard. |
| `?` | either | Open the help modal. |
| `q` | either | Quit. State is saved on every change, so quitting never loses anything. |

What `c` does depends on the row:

- On a line that already carries a finding, it reopens **that** finding for rewriting, with
  the cursor at the end.
- With a `v` selection active, it always files a **new** finding over the selected run.
- On a plain line, it files a finding on that line.
- On a row that is not a line — a hunk header, a fold — it files a finding against the whole
  hunk.
- With no hunk under the cursor, it says "move onto a hunk first".

The `y` summary is a markdown list of `file:lines: note`. It names no group. A group is how
this reviewer chose to read the branch, and the summary gets pasted somewhere that has no
idea what `g7` was.

### The help modal

| key | action |
|---|---|
| any key | Close it. |

### The file-list modal (`f` in the diff pane)

| key | action |
|---|---|
| `j` / `↓` | Next file. |
| `k` / `↑` | Previous file. |
| `enter` | Close, jump the diff cursor to that file, and focus the diff pane. |
| `esc` / `f` / `q` | Close. |

### The findings list (`F`)

| key | action |
|---|---|
| `j` / `↓` | Next finding. |
| `k` / `↑` | Previous finding. |
| `enter` | Close and jump to that finding, wherever in the review it lives. |
| `dd` | Delete the selected finding. The list stays open. |
| `D` | Ask before clearing every finding: `delete all N findings?  y / n`, or `delete this finding?  y / n` when there is only one. |
| `y` | Answer yes to that question. Only a bare `y` counts. Any other key cancels. |
| `esc` / `F` / `q` | Close. |

`D` is guarded because clearing every finding is the only irreversible thing in this
reviewer. `dd` is not, because a finding is one line and rewriting it is `c`.

The reviewer deliberately ignores `ctrl-y` for the confirmation. The one irreversible action
should not answer to a chord nobody aimed.

### The finding composer (`c`)

| key | action |
|---|---|
| `enter` | Save. |
| `ctrl-s` | Save. |
| `shift+enter` | Insert a newline, where the terminal reports the modifier. |
| `\` then `enter` | Insert a newline, where it does not. The `\` must sit just before the cursor. |
| `esc` | Discard. |
| anything else | Goes to the text box. |

`enter` saves because a finding is usually one line, and `enter` is the key that ends a line.
Most terminals send plain `enter` for `shift+enter` too, so the trailing `\` exists to give
every reader a way to write a second line.

Emptying the box does **not** delete an existing finding. That is `dd`, which is a
deliberate press.

A multi-line paste lands in the box whole.

### The picker (`dfr review` with no range)

| key | action |
|---|---|
| `j` / `↓` | Next commit. |
| `k` / `↑` | Previous commit. |
| `space` | Toggle "include uncommitted changes (worktree)". Dirty worktree only. |
| `enter` | Use this commit as the base. Start the review. |
| `esc` / `q` | Cancel. |

The checkbox is hidden on a clean worktree, because it could not change anything. With
nothing outstanding, the snapshot is `HEAD`'s own tree.

The range is `base..head`. It **excludes the base commit's own changes**. A bar marks every
row inside the range as the cursor moves.

### The splash (while the pipeline runs)

| key | action |
|---|---|
| `q` / `esc` | Cancel. This kills the agent subprocess, not just the screen watching it. |

Raw mode has already disabled `Ctrl-C`, so nothing else would reap that subprocess.

## Findings

**A finding is about lines.** `c` on a diff row annotates that line. `v` first starts a
selection, and `c` then annotates the run.

A selection stops at a context boundary and at a file header. Those two rows stand for a
stretch of file you are not looking at. A gap you never opened is a gap you never read, and
a note claiming those lines would claim something nobody said. Nothing else breaks a run.

A finding is drawn under the line it annotates, as a quoted panel: muted italics behind a
rail. Standing anywhere in a note lights all of it — every line it covers, and the note
itself.

A finding whose line is not on screen falls back to its hunk's header.

### How findings survive a regeneration

The plan document is a pure function of `base..head`. Regenerate it and the ids move. So a
finding anchors on the **hunk's exact content digest**, plus a signed offset into that hunk.
Not a line number: a hunk that moved in the file still holds the same line at the same
offset, while its absolute number did not survive the move.

On every open, each finding re-anchors in this order:

1. Exact hunk digest. The normal case.
2. A content match somewhere else. The finding is flagged *moved*.
3. Orphaned. No hunk matches.

**An orphan is never dropped.** It gets its own section in the `F` list, under a rule. For
an orphan that list is not a convenience but the only door: it matches no line and no hunk,
so no row is emitted for it anywhere. The plan pane's title counts orphans, which is the
signpost that sends you to `F`.

An orphan revives when its content comes back.

### The JSON contract

`dfr findings <range>` prints the findings, re-anchored. Each record carries
`{id, created, body, status, moved, plan_hash, anchor}`. The anchor carries
`{file, side, line, end_line, offset, span, hunk_digest, line_text, end_line_text}`.

`line` and `end_line` are the resolved numbers, for a consumer that only reads.
`offset` and `span` are what survive a regeneration. `hunk_digest` keys back into the plan
document's `hunks[].digest`, and from there to `forge_position`.

## Review state

Everything persists through the engine's `ReviewSession`. This crate is a stateless
frontend: it reads and mutates review state only through that session, which writes the
sidecar store under `<git-common-dir>/differential/reviews/<review-id>/`.

The review id derives from the resolved base sha plus the head **as typed**. So reviewing
`main..feature` keeps one review while `feature` moves.

Reviewing uncommitted work keys on the base sha plus the literal `WORKTREE`, so marks and
findings survive while the snapshot tree churns with every edit.

One consequence worth knowing: commit your outstanding work mid-review, and the next
`dfr review` opens the `HEAD`-keyed review rather than the `WORKTREE`-keyed one you were in.
Nothing is lost. The old review is still on disk under its own id. But its marks are not the
ones you see.

## Config

From the user file, `~/.config/differential/config.toml`:

```toml
[review]
theme = "dark"
context = 3
context_step = 10
# Diff layout a review opens in: "split" or "unified".
diff = "split"
```

| key | default | meaning |
|---|---|---|
| `review.theme` | `dark` | Which palette to wear, by name. See below. |
| `review.context` | `3` | Context lines shown either side of a hunk before any expansion. |
| `review.context_step` | `10` | Lines one `z` pulls in at a context boundary row. |
| `review.diff` | `split` | Layout a review OPENS in. `s` still toggles, and a review that has recorded a choice keeps it. |

These are presentation only. They widen what is **displayed** around a hunk, and what
colour it is. They can never change which hunks exist.

### Themes

Eleven, each pairing the reviewer's own colours with the syntax theme the code is painted
in — so the chrome and the code come from one source and cannot drift apart. Every shot is
the same change in the same reviewer; only the palette differs. `./assets/themes.sh`
regenerates them.

<details open>
<summary><code>dark</code> — the default</summary>

![dark](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/dark.png)

</details>

<details>
<summary><code>one-dark</code></summary>

![one-dark](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/one-dark.png)

</details>

<details>
<summary><code>one-light</code></summary>

![one-light](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/one-light.png)

</details>

<details>
<summary><code>gruvbox-dark</code></summary>

![gruvbox-dark](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/gruvbox-dark.png)

</details>

<details>
<summary><code>gruvbox-light</code></summary>

![gruvbox-light](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/gruvbox-light.png)

</details>

<details>
<summary><code>solarized-dark</code></summary>

![solarized-dark](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/solarized-dark.png)

</details>

<details>
<summary><code>solarized-light</code></summary>

![solarized-light](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/solarized-light.png)

</details>

<details>
<summary><code>catppuccin-mocha</code></summary>

![catppuccin-mocha](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/catppuccin-mocha.png)

</details>

<details>
<summary><code>catppuccin-latte</code></summary>

![catppuccin-latte](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/catppuccin-latte.png)

</details>

<details>
<summary><code>dracula</code></summary>

![dracula](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/dracula.png)

</details>

<details>
<summary><code>monokai</code></summary>

![monokai](https://raw.githubusercontent.com/gogoout/differential/main/assets/themes/monokai.png)

</details>

A theme **paints its own background** rather than letting the terminal's show through, so a
light palette works on a dark terminal and the other way round.

Each is *derived* rather than hand-written: a seed names the syntax theme and six accents —
an addition, a deletion, the accent, the skim tier, a finding, a reviewed mark — and the
other thirty-odd colours are mixed from those against the ground the syntax theme declares
(ADR 0024). Seeds live one per file in `src/theme/`.

Adding one is a variant on `ThemeName` and a seed file. Tests then run the legibility,
chroma and distinctness checks over it along with the rest, which is what makes it cheap:
a palette that does not hold up fails the build rather than someone's eyes.

## Using it as a library

```rust
use differential_tui::{review, ReviewOptions, Prepared};

review(
    &repo,
    pick,          // true opens the picker first
    ReviewOptions { context: 3, context_step: 10, split_diff: true, range: None },
    |picked, progress, cancel| {
        // Run the pipeline on a worker thread. Send Progress values down the
        // channel to drive the splash. Watch `cancel` and kill the subprocess.
        Ok(Prepared { out, review_base, head_spec })
    },
)?;
```

The pipeline is a closure you supply, so this crate never composes a backend or a cache.
That is the application's job.

## Performance

Only the lines actually drawn are diffed and highlighted. Per hunk, `similar` runs over the
changed lines alone, and `syntect` over the window plus a fixed lookback. So a keypress costs
what is on screen, not the size of the files the group touches.

## Licence

MIT or Apache-2.0, at your option. Parts of this crate are adapted from `agavra/tuicr` and
`jnsahaj/lumen`, both MIT. See
<https://github.com/gogoout/differential/blob/main/CREDITS.md>.
