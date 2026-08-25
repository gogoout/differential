# 0021 — The reviewer reconstructs hunks from line ranges, not from whole-file diffs

Status: accepted, implemented (refines 0003; constrains 0002, 0011)

**Amended.** The "a window stops at the neighbouring hunk" rule below was stated as though
it were forced by the arithmetic. It is not — it is a policy, and a later change replaced
it. See *Amendment: crossing* at the end.

## Context

The terminal reviewer showed ±3 context lines around each hunk, recomputed from the base
and head blobs (ADR 0003 — the canonical `-U0` hunks carry no context of their own, so
context has to come from somewhere). The way it got them was to diff and highlight the
whole file:

- `git cat-file` both blobs — and `Repo::blob` spawned **two** processes each, an
  existence probe and a read, so four spawns per file;
- `similar::TextDiff::from_lines` over both blobs **entire**;
- syntect over **the whole old file and the whole new file**, measured in this repo's own
  vendored notes at ~197ms per 4,000 lines;
- then, once per hunk, a **linear scan of the resulting row vector** to find where that
  hunk had landed.

Selecting a group is one keypress, and it pays all of the above for every file the group
touches, on the UI thread. A focus group spanning twenty files of two thousand lines cost
seconds. Measured on this repository's own history as one range — 118 files — the old path
was around four seconds before the first frame.

Nearly all of it was wasted. Only `[hunk_start - 3, hunk_end + 3]` was ever drawn, and the
engine already recorded exactly where every hunk is: `old_start`, `old_count`, `new_start`,
`new_count` on `schema::HunkEntry`.

The scan was not merely wasteful, it was a second source of truth. It located a hunk by
taking the first and last change row whose line number fell inside the hunk's span — asking
`similar`'s whole-file pairing to reproduce a boundary git had already stated. Two diff
algorithms need not agree about where a change begins, and nothing made them.

The reviewer also had no way to see *more* than three lines, which is the feature that
forced the question: pulling more context in means fetching arbitrary line ranges out of
the blobs, and once you can do that, diffing the whole file to get three lines is obviously
the wrong shape.

## Decision

**The reviewer computes line ranges and renders only those.** `crates/tui/src/window.rs`
turns a file's hunks plus a per-hunk expansion into blocks of `Context` and `Change`
segments; `rows.rs` reads those ranges out of the cached blob lines. Per hunk, `similar`
runs over the changed lines **only** — the slice `old_start..old_start+old_count` against
`new_start..new_start+new_count` — which keeps the GitHub-style pairing and the word-level
emphasis while scoping the work to the hunk. The full-file scan is gone, and with it the
disagreement it invited.

**Highlighting is windowed, primed by a fixed lookback.** syntect carries parse state from
line to line, so a window highlighted in isolation colours a line that opens inside a
multi-line string or comment as if it were code. `SyntaxHighlighter::highlight_ranges` runs
one forward walk per file per side: it primes from `LOOKBACK = 64` lines above a window,
and where two windows are within that distance it simply runs the gap through — cheaper
than re-priming, and more accurate. Cost is proportional to the lines drawn.

**A window stops at the neighbouring hunk**, shown or not. Between two hunks the old/new
line offset is constant, which is what lets one context stretch carry both sides' numbers
from a single length; across a hunk it is not. Stopping at the neighbour keeps every
rendered line number honest and means expanding can never quietly present someone else's
change as untouched context. (Superseded — see the amendment.)

**`Repo::blob` is one process, not two.** `cat-file --batch` states absence as the word
`missing`, so the existence probe is not merely saved but replaced by something more
explicit than an exit code. Still plumbing, still bytes in and bytes out (ADR 0002, 0011).

## Consequences

The reviewer's per-keypress cost stops tracking the size of the files a group touches. On
this repository's history as one range, the first group build went from seconds to ~0.6s;
on a realistic multi-commit range, to ~0.16s. `RowFactory::highlighted_lines` exposes the
lines syntect parsed so the bound is a test rather than a claim — a wall clock could only
have measured it flakily.

**The trade-off is highlight fidelity.** A window that opens more than 64 lines inside a
single multi-line construct can mis-colour its first lines. This is deliberate and is the
whole price of the change: the alternative is O(file) parse state for every file drawn.
Raising `LOOKBACK` raises the floor cost for every window; the fix for a genuinely
pathological file is not to go back to whole-file highlighting.

**What is left is process spawns.** With the diff and the highlight windowed, `blob`'s
remaining single spawn is the dominant term — around 4ms per call on macOS, two calls per
file drawn. Going further means either a persistent `cat-file --batch` child in the adapter
or a port that reads many paths at once. Both are real design decisions about the port
surface and about holding a subprocess across calls, so neither is taken here.

The blob-line cache is per path and unbounded, as its predecessor was, but strictly
smaller: two vectors of lines instead of a full `DiffLine` vector plus two complete
highlight passes.

## Amendment: crossing

The stopping rule was reasoned about backwards. What the constant-offset argument actually
forbids is rendering a crossed hunk's region as **context** — there, one side's numbers
would be wrong. It says nothing about rendering that hunk as a **change** segment, where
each side carries its own range explicitly, and `plan` already emitted blocks holding
several change segments with joining context between them. The mechanism was there; the
bound was a choice.

The choice had a cost. Grouping is by shape class, so one file routinely holds hunks from
several groups. A window that stopped at one simply lost its boundary row, which is exactly
what reaching the end of the file looks like — a wall the reviewer could not see and was
never told about.

So a window still stops, but the boundary **names** what stopped it, and a further `z`
crosses. Two properties are deliberate:

- **Crossing is never implicit.** The boundary keeps its old meaning while the gap has
  lines in it; only when the gap is spent does it offer the hunk. A long expansion cannot
  swallow someone else's change on the way past.
- **A hunk is atomic.** It is absorbed whole and costs no context budget. Half a change is
  worse than none.

A crossed hunk is drawn in a dashed box naming its owning group. `n`/`N` skip it — it is
context the reviewer asked for, not an entry on this group's reading list — while `space`
marks it like any other, because marks key on class content and are already shared across
groups.

ADR 0006's skim contract is unaffected and clarified there: deferring a remainder is an
opinion about what is worth reading by default, not a rule that it stay unreachable.
