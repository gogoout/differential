# The demo video linked from the README.
#
#   cd assets && vhs record.vhs
#
# Run it FROM THIS DIRECTORY. `Output` and `Screenshot` below are relative
# paths, and vhs resolves them against the process directory, not the tape's.
#
# The range is fixed on purpose. This tape used to type a bare `dfr review`
# and walk the commit picker, so every recording was a different diff and no
# two runs could be compared. `ecc9400..cfea95f` is 11 commits and 79 files,
# and its plan has all three tiers in it: eight focus groups, six skim, one
# folded noise group. Every beat below has real material to land on.
#
# Needs `dfr` on PATH and `vim` for the last beat.

Output demo.webm
Set Shell zsh
Set FontSize 12
Set Height 800
Set Width 1600

# Every run starts from the same screen. The reviewer records its cursor, its
# layout and its reviewed marks per review, so without this the recording
# resumes wherever the last one stopped. Delete ONLY the sessions for this
# range: this is a real repository with real reviews in it, and the blanket
# `rm -rf .../reviews` that assets/themes.sh uses is safe only there, on a
# throwaway fixture.
#
# The grouping cache is a SIBLING of reviews/ and is deliberately kept, so the
# splash below is a cache hit rather than a live agent call. `dfr findings`
# warms it if something has evicted it. Do not reach for `dfr clean`: it
# deletes the cache and leaves the sessions, which is the wrong way round.
Hide
Type "cd $(git rev-parse --show-toplevel)"
Enter
Type "grep -rl cfea95f .git/differential/reviews/*/identity.json 2>/dev/null | xargs -n1 dirname | xargs rm -rf"
Enter
Type "dfr findings ecc9400..cfea95f >/dev/null 2>&1"
Enter
Type "cd assets && clear"
Enter
Show

Sleep 1.2s

# The picker: `dfr review` with no range offers the last 30 commits as a base.
# Walk it, then leave. `q` cancels the whole command.
Type "dfr review"
Enter
Wait+Screen@30s /q cancel/
Sleep 500ms
Down@180ms 5
Up@180ms 2
Sleep 900ms
Type "q"
Sleep 800ms

# Now the real thing, on the fixed range.
Type "dfr review ecc9400..cfea95f"
Enter

# Wait on what is on screen, never on a fixed delay. The pipeline is a cache
# hit here but still enumerates, and a sleep long enough to be safe on a slow
# machine is a sleep wasted on every other one.
Wait+Screen@60s /reading plan/

# A beat after the first paint. `Wait+Screen` returns as soon as the reviewer
# has DRAWN, which is a moment before it is reading keys — a key sent on the
# frame it appeared was dropped.
Sleep 1s

# The dependency graph. There is no key for it: the relation always draws in
# the plan pane, as a gutter connector from the selected group up to each
# group it follows, plus an `after:` line under the group and a `depends on:`
# line in the detail header. So the beat is a cursor move and a pause. Six
# down from the top is g4, "The diff pane soft-wraps", which follows four
# other groups — the widest fan in this document.
Down@200ms 6
Sleep 1.7s

# Eight further down is g14, the noise group, which the plan opens folded.
# `z` means "show me what this pane is withholding", and in the plan pane
# that is the folded group.
Down@140ms 8
Sleep 600ms
Type "z"
Sleep 1.3s
Type "z"
Sleep 600ms

# The file tree. `f` in the LEFT pane swaps the reading plan for the tree;
# `f` in the diff pane is a different key entirely, and opens a file list.
# `z` folds the directory under the cursor.
Type "f"
Sleep 900ms
Down@140ms 5
Sleep 400ms
Type "z"
Sleep 900ms
Type "z"
Sleep 600ms
Type "f"
Sleep 800ms

# Back up the plan to the first group, then into the diff. Walk it with `k`
# rather than `g`: `g` is the diff pane's top, and in the plan pane it leaves
# the selection where it is — the whole second half of an earlier take ran on
# a lockfile because of that. Extra presses clamp at the top, so 16 is safe
# for a 15-group plan.
#
# `enter` rather than `tab` to cross into the diff: it says move to the diff
# rather than toggle, so it cannot land back on the plan.
Up@90ms 16
Sleep 800ms
Enter
Sleep 600ms
Down@70ms 12
Sleep 500ms

# Split and unified. Split is what a review opens in.
Type "s"
Sleep 1.5s
Type "s"
Sleep 1.2s

# What the diff is withholding. `f` here is the diff pane's key: a file list,
# and `enter` jumps to that file's first context boundary — the row that says
# how many lines are hidden and what `z` will show. So `z` lands every time,
# with no counting. Two presses: 50 lines, then the last 18.
Type "f"
Sleep 700ms
Down@140ms 4
Enter
Sleep 900ms
Type "z"
Sleep 1.4s
Type "z"
Sleep 1.4s

# A finding on one line.
Down@80ms 6
Sleep 300ms
Type "c"
Sleep 500ms
Type "explain this"
Enter
Sleep 900ms

# A finding over a range. `v` starts the selection, `j`/`k` extend it, `c`
# writes over it. The trailing `\` is the continuation marker: it makes the
# `enter` after it a newline instead of a save.
Down@60ms 14
Sleep 400ms
Type "v"
Down@170ms 7
Sleep 500ms
Type "c"
Sleep 700ms
Type "range comment\"
Enter
Type "and multiple lines comments"
Enter
Sleep 800ms
Screenshot screenshot.png
Sleep 1.2s

# Mark a group reviewed, then move on.
Tab
Sleep 500ms
Type " "
Sleep 800ms
Down@180ms 2
Sleep 600ms

# Leave for another file entirely, so the jump back has somewhere to come
# from.
Tab
Type "f"
Sleep 700ms
Down@180ms 6
Enter
Sleep 1.2s

# Every finding in one list, wherever it lives. `enter` jumps to one — across
# files, across groups, opening a folded remainder if it has to.
Type "F"
Sleep 1.2s
Down@200ms 1
Sleep 600ms
Enter
Sleep 1.8s

# Copy the findings. One key, and it copies the whole open-findings summary as
# markdown — file, lines and note per bullet.
Type "y"
Sleep 1.3s

# Clean up: clear the findings before quitting. `D` asks, and only a bare `y`
# confirms. The list closes itself once nothing is left in it.
Type "F"
Sleep 900ms
Type "D"
Sleep 1.1s
Type "y"
Sleep 900ms
Type "q"
Sleep 700ms

# What the summary is for. Open insert mode BEFORE pasting: the summary's
# first line starts with `- `, and in normal mode vim reads `- c r a` as
# commands and swallows them — an earlier take pasted a first line missing
# its first five characters.
Type "vim"
Enter
Sleep 700ms
Escape
Type "i"
Paste
Sleep 500ms
Enter@150ms 2
Sleep 500ms
Type "paste to your agent!"
Sleep 1.8s
Escape
Type ":q!"
Enter
Sleep 600ms
