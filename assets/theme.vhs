# One split-pane screenshot of the reviewer, for a single palette.
#
# A TEMPLATE, not a runnable tape: `assets/themes.sh` substitutes `__THEME__`
# and runs one copy of this per theme. That is deliberate. Recording all eleven
# in one tape shares a terminal between them, and a shared terminal shares
# scrollback, leftover keystrokes and a shell prompt — `Wait+Screen` matched a
# previous run's reviewer still in the buffer and fired the shot at a bare
# prompt, twice out of eleven. One terminal per screenshot has none of that,
# and the whole run is no slower for it.
#
#   ./assets/themes.sh
#
# $DFR, $FIXTURE, $CFG and $RANGE come from that script.

Output "__OUT__/.__THEME__.gif"
Set Shell zsh
Set FontSize 13
Set Width 1500
Set Height 780
Set Padding 0

Hide
Sleep 800ms
Type "$DFR review --repo $FIXTURE --user-config $CFG/__THEME__.toml $RANGE"
Enter

# Wait on what is on screen, never on a fixed delay: the pipeline is a cache
# hit here but still enumerates, and a sleep long enough to be safe on a slow
# machine is a sleep wasted on every other one.
Wait+Screen@60s /reading plan/

# A beat after the first paint. `Wait+Screen` returns as soon as the reviewer
# has DRAWN, which is a moment before it is reading keys — a `tab` sent on the
# frame it appeared was dropped, and the arrow keys that followed walked the
# plan instead of the diff.
Sleep 700ms

# Into the diff pane. With the plan focused the file tree floats over the diff,
# and the diff is what a palette is judged on. `enter` rather than `tab`: it
# says move to the diff rather than toggle, so it cannot land back on the plan.
#
# The layout needs no keypress: split is what a review opens in.
Enter
Sleep 400ms
Down@60ms 6
Sleep 500ms

Show
Screenshot "__OUT__/__THEME__.png"
Sleep 300ms
