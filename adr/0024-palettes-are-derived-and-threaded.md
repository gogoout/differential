# 0024 — Palettes are derived from a seed, and threaded

Status: accepted.

## Context

The reviewer had one palette: a `pub const THEME`, thirty-one colour fields, hand-tuned,
described in its own module doc as "one dark palette". There was no way to change it.

Eight of those thirty-one fields were ANSI named colours — `DarkGray`, `Gray`, `Cyan`,
`Green`, `Yellow` — and those eight carried **74% of all field reads** in the crate. An ANSI
colour has no value until a terminal supplies one, so the reviewer's appearance was partly
outside its own control, and on a light background it was close to unreadable. There was no
light palette because there could not be one: nothing painted a background, so the
terminal's showed through, and pale ink over black is what a light theme would have been.

## Decision

### One derived system, no special case

A theme declares a **seed**: which syntect theme paints the code, and six accents that a
syntect theme has no opinion about — an addition, a deletion, the accent, the skim tier, a
finding, a reviewed mark. The other thirty-odd colours are mixed from those against the
ground the syntect theme itself declares.

That pairing is the point. The chrome is derived from the same theme the code is painted
in, so the two are one palette rather than two that drift. Adding a theme is a
`config::ThemeName` variant and a seed file.

**Today's dark palette was not preserved.** It is derived like every other, its ANSI
colours are now RGB, and its exact values changed. A hand-tuned default kept as a literal
would be the one palette the derivation rules were never tested against — and it is those
rules every other theme depends on.

**A theme paints its own background.** Without that a light palette cannot exist, and every
float must clear to that ground rather than to the terminal's.

### Threaded, not global

`THEME` is gone. `RowsContext` carries the palette, because rows bake their colours in at
**build** time and are cached — so a theme has to reach `rebuild_rows`, not only `draw`. The
picker and the splash take it as a parameter, because both draw before `App` exists.

A `&Theme` on fifty signatures is more code than a global would be. It is also honest:
there is exactly one palette per process, chosen once, and a static that anything can read
invites the run-time swap this design does not support. It removed a global rather than
adding one — `Theme` owns its highlighter, which retires the process-wide `OnceLock` that
could not be re-seeded and so let whichever theme rendered first win for the process.

### Named, not configurable field by field

`[review].theme` takes a name, like `[grouping].agent` does, and for the same reason: a
palette is a coherent set the renderer builds, not a value a caller supplies. Per-field
overrides would freeze thirty field names as public API and let a half-overridden palette
be unreadable in ways no test could catch.

## Consequences

- **The colour maths is `palette`'s**, and mixing happens in **Oklab**. `t` is therefore a
  share of the *perceived* distance rather than of the encoded value, which is what every
  derivation rule was already claiming to mean. The two differ a lot — half way from black
  to white is 99 in Oklab and 128 in sRGB — so a silent revert to sRGB would shift every
  palette at once. A test pins it.

- **Muting scales with the headroom a theme has.** Solarized puts its own foreground 4.1:1
  from its own ground; Monokai puts it at 15:1. A fixed mute fraction leaves the first
  unreadable. The ramp is a share of what the theme offers.

- **Legibility is tested against what the theme can reach, not an absolute.** Solarized is
  low-contrast by design and cannot meet WCAG AA on its own ground before this crate
  touches it. Holding it to 4.5:1 would mean either dropping it or lying; the test instead
  asserts the *derivation* never makes a theme less legible than the theme already is.

- **Contrast is not sufficient.** An ink can clear AA against the background and still be
  invisible: Gruvbox Light's reviewed ✓ sat 5:1 from its cream ground and read as ordinary
  brown prose. Perceptual distance from the foreground does not catch it either — that ink
  was *further* from its foreground (0.160) than the dark theme's perfectly visible cyan
  header is from its own (0.124). **Chroma** is the discriminator, at 0.066 against 0.110,
  and a chroma floor is now part of the test set. A desaturated ink beside desaturated
  prose is a shade, and readers do not see shades.

- **A published palette is not a usable one.** Syntax colours are tuned for code on a page,
  where a keyword at 3:1 is fine; the plan pane uses them as interface text, where it is
  not. Every light theme's greens, blues and yellows had to come down in lightness, and the
  pastel sets fail the opposite bar — Catppuccin Mocha's yellow and Dracula's cyan read as
  off-white. Each deviation is recorded in that theme's own file with the number that
  forced it.

- Two colour-keyed dispatches (`Theme::gutter_cursor`, `Theme::lit_band`) compare colour
  *values*, so two fields deriving to the same colour would silently collapse into one. A
  distinctness test guards the pairs.

- Eleven themes ship, and the marginal cost of the twelfth is a seed file — because the
  tests run over all of them, a palette that does not hold up fails the build rather than
  someone's eyes.
