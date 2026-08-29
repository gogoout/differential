#!/usr/bin/env bash
# Regenerate assets/themes/*.png — one split-pane screenshot per palette.
#
# Builds a throwaway repository of fake Rust, warms the grouping cache with a
# single agent call, then hands the range to `assets/themes.vhs`, which opens
# the reviewer once per theme and screenshots it.
#
#   ./assets/themes.sh
#
# Needs `vhs` on PATH and an agent the grouping stage can reach. Everything but
# the PNGs lives in a temp directory and is deleted on exit.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
fixture="${KEEP_FIXTURE:-$(mktemp -d)}"
[ -n "${KEEP_FIXTURE:-}" ] || trap 'rm -rf "$fixture"' EXIT

cargo build -q --bin dfr
dfr="$root/target/debug/dfr"

cd "$fixture"
git init -q .
git config user.email themes@example.invalid
git config user.name Themes

# The fixture: a small tokeniser, long enough that the diff pane is full and
# the plan has focus, skim and noise material in it. Written by python rather
# than by heredoc — it is a lot of Rust, and bash quoting does it no favours.
python3 "$root/assets/fixture.py" base
git add -A
git commit -q -m "base"

python3 "$root/assets/fixture.py" head
git add -A
git commit -q -m "head"

base="$(git rev-parse HEAD~1)"
head="$(git rev-parse HEAD)"

# One agent call. Every recorded run below is a cache hit, so the splash is
# brief and all eleven screenshots show the same document.
echo "warming the grouping cache (one agent call)…"
"$dfr" findings "$base..$head" >/dev/null

# One config and one tape per theme. Each recording gets its own terminal —
# see the note at the top of assets/theme.vhs for why that is not incidental.
mkdir -p "$fixture/cfg" "$root/assets/themes"
cd "$root"
# Override to iterate on one: THEMES=dracula ./assets/themes.sh
for t in ${THEMES:-dark one-dark one-light gruvbox-dark gruvbox-light \
         solarized-dark solarized-light catppuccin-mocha catppuccin-latte \
         dracula monokai}; do
  printf '[review]\ntheme = "%s"\n' "$t" > "$fixture/cfg/$t.toml"
  sed -e "s|__THEME__|$t|g" -e "s|__OUT__|$root/assets/themes|g" \
    assets/theme.vhs > "$fixture/$t.vhs"
  # Every recording starts from the same state. The reviewer records its
  # layout and cursor per review, so without this each theme resumes wherever
  # the last one left off — eleven screenshots of eleven different screens.
  # The grouping cache is a sibling directory and is deliberately kept.
  rm -rf "$fixture/.git/differential/reviews"

  echo "recording $t..."
  DFR="$dfr" CFG="$fixture/cfg" FIXTURE="$fixture" RANGE="$base..$head" \
    vhs "$fixture/$t.vhs" >/dev/null
  rm -f "$root/assets/themes/.$t.gif"

  # A blank shot is a recording that went wrong, not a theme that renders
  # nothing. Catch it here rather than in review.
  bytes=$(wc -c < "$root/assets/themes/$t.png")
  if [ "$bytes" -lt 40000 ]; then
    echo "  $t.png is ${bytes}B — the reviewer did not draw" >&2
    exit 1
  fi
done

# VHS writes true-colour PNGs, and a terminal screenshot has perhaps a hundred
# distinct colours in it — so a palette re-encode is ~3x smaller with nothing
# visible lost. Without it eleven screenshots are 4 MB of repository.
if command -v ffmpeg >/dev/null; then
  echo "quantising..."
  for png in "$root"/assets/themes/*.png; do
    ffmpeg -y -loglevel error -i "$png" \
      -vf "palettegen=max_colors=128:stats_mode=full" "$fixture/pal.png"
    ffmpeg -y -loglevel error -i "$png" -i "$fixture/pal.png" \
      -lavfi "paletteuse=dither=none" "$fixture/out.png"
    mv "$fixture/out.png" "$png"
  done
else
  echo "note: ffmpeg not found - screenshots are ~3x larger than needed" >&2
fi

echo "wrote:"
ls -la "$root/assets/themes/"
