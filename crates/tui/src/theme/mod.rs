//! Palettes: one seed each, everything else derived (ADR 0024).
//!
//! A theme decides seven things — which syntect theme paints the code, and six
//! accents that a syntect theme does not reliably carry. The other thirty-odd
//! colours are mixed from those by the rules in [`derive`], against the ground
//! the syntect theme itself declares. That is what keeps the chrome and the
//! code in one palette rather than two that drift.
//!
//! One seed per file, in the modules below. Field schema modelled on lumen's
//! `DiffColors`. There is no hand-tuned palette any more: a hand-tuned one is a
//! palette the rules were never tested against, and it is the rules every other
//! theme depends on.

mod dark;
mod gruvbox_dark;
mod gruvbox_light;
mod light;
mod monokai;
mod solarized_dark;
mod solarized_light;

use std::sync::Arc;

use differential_engine::config::ThemeName;
use palette::color_difference::Wcag21RelativeContrast;
use palette::{FromColor, IntoColor, Mix, Oklab, Srgb};
use ratatui::style::{Color, Modifier, Style};
use two_face::theme::EmbeddedThemeName;

use super::vendor::LineOrigin;
use super::vendor::syntax::SyntaxHighlighter;

/// A colour the derivation can actually work with.
///
/// `ratatui::style::Color` cannot be: most of its variants are ANSI names,
/// which have no value until a terminal supplies one, so there is nothing to
/// blend and nothing to contrast-check. That is exactly why the palettes
/// stopped using them.
type Rgb = Srgb<u8>;

pub(super) const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Srgb::new(r, g, b)
}

fn color(c: Rgb) -> Color {
    Color::Rgb(c.red, c.green, c.blue)
}

/// The colour `t` of the way from `a` to `b`, mixed **perceptually**.
///
/// Oklab rather than sRGB, so `t` is a share of the PERCEIVED distance rather
/// than of the encoded value — which is the judgement every derivation rule
/// below is actually making when it says "a whisper of colour over the
/// ground". The two differ a lot: half way from black to white is 99 here and
/// 128 in sRGB.
///
/// `palette` does the conversion and the mix. A hand-rolled `u8` lerp was the
/// wrong answer twice over — more code, and the wrong colour space.
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let a: Oklab = a.into_format::<f32>().into_color();
    let b: Oklab = b.into_format::<f32>().into_color();
    let mixed: Srgb<f32> = Srgb::from_color(a.mix(b, t.clamp(0.0, 1.0)));
    mixed.into_format()
}

/// WCAG 2.1 contrast ratio, 1.0 (identical) to 21.0 (black on white).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    a.into_format::<f32>()
        .relative_contrast(b.into_format::<f32>())
}

/// How colourful an ink is, independent of how light it is.
///
/// Oklab chroma, and the thing that makes a mark read AS a mark.
///
/// Perceptual DISTANCE from the foreground was the obvious measure and it is
/// the wrong one: Gruvbox Light's invisible reviewed green sat 0.160 from its
/// foreground while the dark theme's perfectly visible cyan header sits 0.124
/// from its own. What separated them was chroma — 0.066 against 0.110. A dark
/// desaturated ink beside dark desaturated prose is a shade, and a reader does
/// not see shades.
pub fn chroma(c: Rgb) -> f32 {
    let c: Oklab = c.into_format::<f32>().into_color();
    c.a.hypot(c.b)
}

/// The RGB behind a `Color`. `None` for anything a palette should no longer
/// contain, which is what the tests assert on.
pub fn rgb_of(c: Color) -> Option<Rgb> {
    match c {
        Color::Rgb(r, g, b) => Some(rgb(r, g, b)),
        _ => None,
    }
}

fn from_syntect(c: syntect::highlighting::Color) -> Rgb {
    rgb(c.r, c.g, c.b)
}

/// What a theme decides for itself.
///
/// Six accents, because a syntect theme carries a background and a foreground
/// but has no opinion about what an *addition* is, or a finding, or the skim
/// tier. Everything else on [`Theme`] is derived from these.
pub(super) struct Seed {
    /// The code's own colours, and the ground the chrome is mixed against.
    pub syntax: EmbeddedThemeName,
    pub add: Rgb,
    pub del: Rgb,
    /// The hunk you are reading, a header, the cursor's tint.
    pub accent: Rgb,
    /// The middle effort tier. Focus takes `del` and noise is derived, so this
    /// is the only tier with an ink of its own.
    pub skim: Rgb,
    pub finding: Rgb,
    pub reviewed: Rgb,
}

fn seed(name: ThemeName) -> Seed {
    match name {
        ThemeName::Dark => dark::seed(),
        ThemeName::Light => light::seed(),
        ThemeName::GruvboxDark => gruvbox_dark::seed(),
        ThemeName::GruvboxLight => gruvbox_light::seed(),
        ThemeName::SolarizedDark => solarized_dark::seed(),
        ThemeName::SolarizedLight => solarized_light::seed(),
        ThemeName::Monokai => monokai::seed(),
    }
}

pub struct Theme {
    /// The pane's own ground, painted rather than inherited.
    ///
    /// The terminal's background used to show through, which is why the old
    /// palette could only ever be dark: a light theme over a dark terminal is
    /// pale ink on black. A theme that names a background owns its appearance.
    pub bg: Color,
    pub fg: Color,
    pub added_bg: Color,
    pub deleted_bg: Color,
    /// The line-number block. Stronger than the tint over the code, which is
    /// what makes the gutter read as an edge rather than as more of the line.
    pub added_gutter_bg: Color,
    pub deleted_gutter_bg: Color,
    /// The line-number block on the row the cursor is on. Brighter than the
    /// change block, so the cursor is the strongest cell in the gutter column
    /// — and still red on a deletion and green on an addition, so a row does
    /// not stop saying which it is just because you are standing on it.
    pub added_gutter_cursor_bg: Color,
    pub deleted_gutter_cursor_bg: Color,
    /// The line number itself on that row. One ink for all three blocks,
    /// whichever of the palette's own extremes reads on them.
    pub cursor_gutter_fg: Color,
    pub added_word_bg: Color,
    pub deleted_word_bg: Color,
    /// Diagonal fill for the side of a split row that has no line at all.
    pub hatch_fg: Color,
    /// A pill's own text — a hunk's class, a group's role, the footer's
    /// tallies. Deliberately quieter than the code it labels: a pill is a
    /// caption, and the counts and the accent on it are what carry.
    pub button_fg: Color,
    /// A pill's fill. Bright enough that the lit cell at its head reads as a
    /// COLOUR on it — the accent for the hunk you are in, a muted one for a
    /// hunk crossed in from another group.
    pub button_bg: Color,
    /// Behind a file header pinned to the top of the pane, so a stuck row is
    /// visibly stuck rather than looking like content that will not scroll.
    pub sticky_bg: Color,
    /// The context-boundary pill and its rule. Present, but not competing with
    /// the code — it marks where the file was cut, and the file is the point.
    pub hint_fg: Color,
    pub hint_bg: Color,
    /// The same band on the cursor's row. A boundary is a control, and a
    /// control the reader is standing on has to look like the one they are
    /// about to press — the band carries its own colour, so the row tint that
    /// marks the cursor everywhere else never showed through it.
    pub hint_cursor_fg: Color,
    pub hint_cursor_bg: Color,
    pub gutter_fg: Color,
    pub context_fg: Color,
    pub cursor_bg: Color,
    pub selected_bg: Color,
    pub header_fg: Color,
    /// A hunk crossed in from another group. The same accent the hunk you ARE
    /// reading wears, muted: it is real code you asked to see, so it belongs
    /// to the same family — but it is not on this reading list, and a full
    /// accent would say it was.
    pub foreign_fg: Color,
    pub focus_fg: Color,
    pub skim_fg: Color,
    pub noise_fg: Color,
    pub reviewed_fg: Color,
    /// Added / removed line counts in the left pane.
    pub add_fg: Color,
    pub del_fg: Color,
    pub finding_fg: Color,
    pub status_bg: Color,
    /// Built from the same syntect theme the colours above were derived from,
    /// so the code and the chrome can never be two palettes.
    ///
    /// Owned rather than memoised process-wide: the old `OnceLock` could not be
    /// re-seeded, so whichever theme rendered first won for the process — which
    /// a runtime-selected theme cannot live with.
    highlighter: Arc<SyntaxHighlighter>,
}

impl Clone for Theme {
    fn clone(&self) -> Self {
        // Every field is `Copy` but the highlighter, which is an `Arc` — so a
        // clone is a pointer bump, not a second syntax-set parse.
        Theme {
            highlighter: Arc::clone(&self.highlighter),
            ..*self
        }
    }
}

impl Theme {
    /// Build the named palette. The expensive part — parsing the embedded
    /// theme dump and the syntax set — happens once, here.
    pub fn named(name: ThemeName) -> Theme {
        let seed = seed(name);
        let syntect = two_face::theme::extra()[seed.syntax].clone();
        derive(&seed, syntect)
    }

    pub fn highlighter(&self) -> &SyntaxHighlighter {
        &self.highlighter
    }

    /// The ground, as one style — painted under the whole frame before
    /// anything else, so every pane inherits it.
    pub const fn ground(&self) -> Style {
        Style::new().bg(self.bg).fg(self.fg)
    }

    /// The tint behind a line's code, `None` for unchanged context.
    ///
    /// Same source as the highlighter's own background, so the padding that
    /// carries a change to the pane edge can never be a different green from
    /// the code it follows.
    pub const fn line_bg(&self, origin: LineOrigin) -> Option<Color> {
        match origin {
            LineOrigin::Addition => Some(self.added_bg),
            LineOrigin::Deletion => Some(self.deleted_bg),
            LineOrigin::Context => None,
        }
    }

    /// The block behind a line NUMBER, `None` for unchanged context.
    pub const fn gutter_bg(&self, origin: LineOrigin) -> Option<Color> {
        match origin {
            LineOrigin::Addition => Some(self.added_gutter_bg),
            LineOrigin::Deletion => Some(self.deleted_gutter_bg),
            LineOrigin::Context => None,
        }
    }

    /// The same cell on the cursor's row, from the block it wears otherwise.
    ///
    /// Keyed by colour: this is the one place that knows a gutter's blocks, so
    /// a fourth one means adding it here rather than at the call site. An
    /// unchanged line has no block of its own and takes the plain cursor tint,
    /// which is what makes the cursor readable on every row.
    ///
    /// Because the dispatch compares values, two blocks that derived to the
    /// same colour would silently collapse into one — which is what
    /// `every_theme_keeps_the_colours_it_dispatches_on_apart` guards.
    pub fn gutter_cursor(&self, block: Option<Color>) -> Style {
        let bg = match block {
            Some(c) if c == self.added_gutter_bg => self.added_gutter_cursor_bg,
            Some(c) if c == self.deleted_gutter_bg => self.deleted_gutter_cursor_bg,
            _ => self.cursor_bg,
        };
        Style::default()
            .fg(self.cursor_gutter_fg)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    }

    /// Re-ink one span of a boundary band for the cursor's row.
    ///
    /// Keyed by colour, like `gutter_cursor`: this is the one place that knows
    /// the band's inks. Anything else — a line's change colour, a syntax span
    /// — passes through untouched, which is what lets one pass over a whole
    /// row lighten the band and nothing else.
    pub fn lit_band(&self, style: Style) -> Style {
        let mut out = style;
        if style.bg == Some(self.hint_bg) {
            out = out.bg(self.hint_cursor_bg);
        }
        if style.fg == Some(self.hint_fg) {
            out = out.fg(self.hint_cursor_fg);
        }
        out
    }

    pub fn effort_style(&self, effort: differential_engine::schema::Effort) -> Style {
        let fg = match effort {
            differential_engine::schema::Effort::Focus => self.focus_fg,
            differential_engine::schema::Effort::Skim => self.skim_fg,
            differential_engine::schema::Effort::Noise => self.noise_fg,
        };
        Style::default().fg(fg)
    }

    /// One-letter tier for the plan pane's narrow column.
    ///
    /// The pane is 40 columns wide and the tier shares a line with counts, so
    /// this is the TUI's own vocabulary — the wire token lives in
    /// `plan::effort_name` and the row header spells it out in full.
    pub const fn effort_glyph(effort: differential_engine::schema::Effort) -> &'static str {
        match effort {
            differential_engine::schema::Effort::Focus => "F",
            differential_engine::schema::Effort::Skim => "S",
            differential_engine::schema::Effort::Noise => "N",
        }
    }

    /// The two colours a pill wears — one pair, always. A hunk's pill says the
    /// cursor is in it with a lit bar at its head rather than by filling, so
    /// no ink on a pill ever needs a second twin for a bright background.
    pub const fn pill(&self) -> (Color, Color) {
        (self.button_fg, self.button_bg)
    }

    pub fn word_emphasis(&self, addition: bool) -> Style {
        Style::default()
            .bg(if addition {
                self.added_word_bg
            } else {
                self.deleted_word_bg
            })
            .add_modifier(Modifier::BOLD)
    }
}

/// Every colour on the screen, from six accents and the syntect theme's ground.
///
/// The mix fractions all run *towards the background*, so each rule reads the
/// same way in a light theme as in a dark one: 0.86 is a whisper of colour over
/// the ground, 0.35 is most of the way to the accent itself. That relativity is
/// the whole reason a light palette needs no rules of its own.
const WHITE: Rgb = rgb(0xFF, 0xFF, 0xFF);
const BLACK: Rgb = rgb(0x00, 0x00, 0x00);

fn derive(seed: &Seed, syntect: syntect::highlighting::Theme) -> Theme {
    let settings = &syntect.settings;
    // A syntect theme is not obliged to state either, though every embedded one
    // does. The fallbacks keep a missing setting from being an invisible screen.
    let bg = settings
        .background
        .map_or(rgb(0x1E, 0x20, 0x28), from_syntect);
    let fg = settings
        .foreground
        .map_or(rgb(0xE0, 0xE2, 0xE8), from_syntect);

    // How much room the theme gives before anything is muted at all. Solarized
    // puts its own foreground 4.1:1 from its own ground; Monokai puts it at
    // 15:1. Muting both by the same fraction makes the quiet inks unreadable in
    // the first and merely quiet in the second — so the ramp is a share of the
    // headroom rather than a constant.
    let headroom = (contrast(fg, bg) / 8.0).clamp(0.45, 1.0);

    // Towards the ground, always. `q` for an accent, `m` for a muted ink, which
    // is the one that has to yield on a low-contrast palette.
    let q = |c: Rgb, t: f32| color(mix(c, bg, t));
    let m = |c: Rgb, t: f32| color(mix(c, bg, t * headroom));
    let (add, del, accent) = (seed.add, seed.del, seed.accent);

    // One ink for all three cursor blocks, so the line number does not change
    // colour as the cursor moves between an addition, a deletion and a plain
    // line. Chosen by whichever of the palette's own extremes reads WORST on
    // its hardest block least badly — picking against one block was how an ink
    // that is perfect on a bright green ended up at 1.4:1 on the plain tint.
    //
    // From the palette rather than a flat white, so a Solarized cursor stays
    // Solarized.
    let blocks = [
        mix(add, bg, 0.52),
        mix(del, bg, 0.52),
        mix(accent, bg, 0.72),
    ];
    let worst = |ink: Rgb| {
        blocks
            .iter()
            .map(|b| contrast(ink, *b))
            .fold(f32::INFINITY, f32::min)
    };
    // The palette's own extremes first. They are not always enough: a mid
    // olive-green block against Solarized's mid-grey foreground and its very
    // dark ground leaves neither at 2:1, so the candidates also include a
    // near-white and a near-black, pulled a little towards the ground so they
    // still belong to the theme. A line number has to be readable before it
    // has to be tasteful.
    let cursor_gutter_fg = [fg, bg, mix(bg, WHITE, 0.94), mix(bg, BLACK, 0.94)]
        .into_iter()
        .max_by(|a, b| worst(*a).total_cmp(&worst(*b)))
        .expect("four candidates");

    Theme {
        bg: color(bg),
        fg: color(fg),
        // A tint you read code through, and the stronger block beside it.
        added_bg: q(add, 0.90),
        deleted_bg: q(del, 0.90),
        added_gutter_bg: q(add, 0.78),
        deleted_gutter_bg: q(del, 0.78),
        added_gutter_cursor_bg: q(add, 0.52),
        deleted_gutter_cursor_bg: q(del, 0.52),
        cursor_gutter_fg: color(cursor_gutter_fg),
        // Word emphasis sits on top of the line tint, so it has to be a clear
        // step past it without becoming the gutter block.
        added_word_bg: q(add, 0.58),
        deleted_word_bg: q(del, 0.58),
        hatch_fg: m(fg, 0.80),
        button_fg: m(fg, 0.26),
        button_bg: q(fg, 0.82),
        sticky_bg: q(fg, 0.90),
        hint_fg: m(fg, 0.44),
        hint_bg: q(fg, 0.89),
        hint_cursor_fg: m(fg, 0.18),
        hint_cursor_bg: q(fg, 0.74),
        gutter_fg: m(fg, 0.41),
        // The code's own ink, not a mix of it: an unchanged line is the
        // theme's foreground, and quieting it by a tenth cost a low-contrast
        // palette like Solarized a quarter of the contrast it had.
        context_fg: color(fg),
        cursor_bg: q(accent, 0.72),
        selected_bg: q(accent, 0.90),
        header_fg: color(accent),
        foreign_fg: m(accent, 0.30),
        // Focus is the must-read tier and wears the deletion red; noise is the
        // quietest thing on screen, one step past the gutter.
        focus_fg: color(del),
        skim_fg: color(seed.skim),
        noise_fg: m(fg, 0.42),
        reviewed_fg: color(seed.reviewed),
        add_fg: color(add),
        del_fg: color(del),
        finding_fg: color(seed.finding),
        status_bg: q(fg, 0.93),
        highlighter: Arc::new(SyntaxHighlighter::with_theme(
            syntect,
            q(add, 0.90),
            q(del, 0.90),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ThemeName; 7] = [
        ThemeName::Dark,
        ThemeName::Light,
        ThemeName::GruvboxDark,
        ThemeName::GruvboxLight,
        ThemeName::SolarizedDark,
        ThemeName::SolarizedLight,
        ThemeName::Monokai,
    ];

    fn must_rgb(c: Color) -> Rgb {
        rgb_of(c).unwrap_or_else(|| panic!("{c:?} is not an Rgb — a palette may not use ANSI"))
    }

    fn fields(t: &Theme) -> Vec<(&'static str, Color)> {
        vec![
            ("bg", t.bg),
            ("fg", t.fg),
            ("added_bg", t.added_bg),
            ("deleted_bg", t.deleted_bg),
            ("added_gutter_bg", t.added_gutter_bg),
            ("deleted_gutter_bg", t.deleted_gutter_bg),
            ("added_gutter_cursor_bg", t.added_gutter_cursor_bg),
            ("deleted_gutter_cursor_bg", t.deleted_gutter_cursor_bg),
            ("cursor_gutter_fg", t.cursor_gutter_fg),
            ("added_word_bg", t.added_word_bg),
            ("deleted_word_bg", t.deleted_word_bg),
            ("hatch_fg", t.hatch_fg),
            ("button_fg", t.button_fg),
            ("button_bg", t.button_bg),
            ("sticky_bg", t.sticky_bg),
            ("hint_fg", t.hint_fg),
            ("hint_bg", t.hint_bg),
            ("hint_cursor_fg", t.hint_cursor_fg),
            ("hint_cursor_bg", t.hint_cursor_bg),
            ("gutter_fg", t.gutter_fg),
            ("context_fg", t.context_fg),
            ("cursor_bg", t.cursor_bg),
            ("selected_bg", t.selected_bg),
            ("header_fg", t.header_fg),
            ("foreign_fg", t.foreign_fg),
            ("focus_fg", t.focus_fg),
            ("skim_fg", t.skim_fg),
            ("noise_fg", t.noise_fg),
            ("reviewed_fg", t.reviewed_fg),
            ("add_fg", t.add_fg),
            ("del_fg", t.del_fg),
            ("finding_fg", t.finding_fg),
            ("status_bg", t.status_bg),
        ]
    }

    /// Every variant has a seed and builds. A variant added to the config enum
    /// without one fails to compile in `seed`; this catches everything after.
    #[test]
    fn every_named_theme_builds() {
        for name in ALL {
            let t = Theme::named(name);
            assert!(rgb_of(t.bg).is_some(), "{name:?}");
        }
    }

    /// No ANSI anywhere. An ANSI colour has no value until a terminal supplies
    /// one, so it cannot be mixed and cannot be contrast-checked — which is
    /// the whole reason the old palette could not go light.
    #[test]
    fn no_palette_uses_a_terminal_defined_colour() {
        for name in ALL {
            let t = Theme::named(name);
            for (what, c) in fields(&t) {
                assert!(rgb_of(c).is_some(), "{name:?}.{what} is {c:?}, not Rgb");
            }
        }
    }

    /// `gutter_cursor` and `lit_band` dispatch by COMPARING colour values, so
    /// two fields that derive to the same colour silently collapse into one: a
    /// deletion's cursor block would light green, or a boundary band would stop
    /// lighting at all. Neither shows up as a failure anywhere else.
    #[test]
    fn every_theme_keeps_the_colours_it_dispatches_on_apart() {
        for name in ALL {
            let t = Theme::named(name);
            let pairs = [
                ("gutter blocks", t.added_gutter_bg, t.deleted_gutter_bg),
                (
                    "gutter cursors",
                    t.added_gutter_cursor_bg,
                    t.deleted_gutter_cursor_bg,
                ),
                ("band bg", t.hint_bg, t.hint_cursor_bg),
                ("band fg", t.hint_fg, t.hint_cursor_fg),
                // Not a dispatch, but the same class of bug: a foreign hunk
                // wearing the full accent stops saying it is foreign.
                ("foreign vs header", t.foreign_fg, t.header_fg),
            ];
            for (what, a, b) in pairs {
                assert_ne!(a, b, "{name:?}: {what} derived to one colour");
            }
        }
    }

    /// A derived palette nobody looked at is still readable. Text carries the
    /// 4.5:1 of WCAG AA; chrome that only has to be *seen* rather than read
    /// carries 3:1.
    #[test]
    fn every_theme_is_legible_on_its_own_ground() {
        // Every failure at once. One assert per field would report the first
        // and hide the other six, and these are tuned as a set.
        let mut bad = Vec::new();
        for name in ALL {
            let t = Theme::named(name);
            let bg = must_rgb(t.bg);
            // A theme cannot be held to a bar it does not clear on bare
            // ground: Solarized is low-contrast on purpose. What is being
            // tested is that the DERIVATION never makes a theme less legible
            // than the theme already is.
            let base = contrast(must_rgb(t.fg), bg);
            let text = 4.5f32.min(base);
            let muted = 3.0f32.min(base * 0.65);
            let checks = [
                // Text: WCAG AA, or the theme's own ceiling.
                (text, "fg", t.fg),
                (text, "context_fg", t.context_fg),
                (text, "header_fg", t.header_fg),
                (text, "add_fg", t.add_fg),
                (text, "del_fg", t.del_fg),
                (text, "skim_fg", t.skim_fg),
                (text, "finding_fg", t.finding_fg),
                (text, "reviewed_fg", t.reviewed_fg),
                (text, "focus_fg", t.focus_fg),
                // Quieter by design — they mark rather than say.
                (muted, "gutter_fg", t.gutter_fg),
                (muted, "noise_fg", t.noise_fg),
                (muted, "hint_fg", t.hint_fg),
                (muted, "foreign_fg", t.foreign_fg),
                (muted, "button_fg", t.button_fg),
            ];
            for (want, what, c) in checks {
                let got = contrast(must_rgb(c), bg);
                if got < want {
                    bad.push(format!("{name:?}.{what}: {got:.2}:1, want {want:.2}:1"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// An ink that MEANS something has to be tellable from ordinary text.
    ///
    /// Contrast against the ground does not give you this, and checking only
    /// that was how Gruvbox Light shipped a reviewed ✓ nobody could see: a dark
    /// desaturated green, comfortably legible on cream, and all but identical
    /// to the dark brown prose beside it.
    #[test]
    fn a_semantic_ink_is_tellable_from_ordinary_text() {
        let mut bad = Vec::new();
        for name in ALL {
            let t = Theme::named(name);
            for (what, c) in [
                ("reviewed_fg", t.reviewed_fg),
                ("add_fg", t.add_fg),
                ("del_fg", t.del_fg),
                ("skim_fg", t.skim_fg),
                ("finding_fg", t.finding_fg),
                ("header_fg", t.header_fg),
                ("focus_fg", t.focus_fg),
            ] {
                // Chroma, not distance from the foreground: the green that
                // vanished was FURTHER from its foreground than the dark
                // theme's cyan header is from its own.
                let ch = chroma(must_rgb(c));
                if ch < 0.10 {
                    bad.push(format!("{name:?}.{what}: chroma {ch:.3}, want 0.10"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The line number on the cursor's row is one ink over three blocks, so it
    /// has to read on all three rather than on the one it was chosen against.
    #[test]
    fn the_cursor_line_number_reads_on_every_block_it_sits_on() {
        for name in ALL {
            let t = Theme::named(name);
            let ink = must_rgb(t.cursor_gutter_fg);
            for (what, block) in [
                ("added", t.added_gutter_cursor_bg),
                ("deleted", t.deleted_gutter_cursor_bg),
                ("plain", t.cursor_bg),
            ] {
                let ratio = contrast(ink, must_rgb(block));
                assert!(ratio >= 3.0, "{name:?}: on the {what} block {ratio:.2}:1");
            }
        }
    }

    /// A change's tint has to be visible against the ground, or a diff stops
    /// looking like a diff — but not so strong that code cannot be read on it.
    #[test]
    fn a_change_tint_is_visible_without_drowning_the_code() {
        for name in ALL {
            let t = Theme::named(name);
            let (bg, fg) = (must_rgb(t.bg), must_rgb(t.fg));
            let base = contrast(fg, bg);
            for (what, tint) in [("added", t.added_bg), ("deleted", t.deleted_bg)] {
                let tint = must_rgb(tint);
                assert_ne!(tint, bg, "{name:?}: the {what} tint is the ground");
                // Relative to what the theme itself offers, plus a floor. An
                // absolute 4.5:1 would be unmeetable for a base palette that
                // only reaches 5.6:1 on its own ground — the question is
                // whether the tint DROWNS the code, not whether the theme is
                // high-contrast.
                let on_tint = contrast(fg, tint);
                assert!(
                    on_tint >= base * 0.75,
                    "{name:?}: code on {what} is {on_tint:.2}:1, \
                     {:.0}% of the {base:.2}:1 this theme offers on bare ground",
                    on_tint / base * 100.0
                );
                assert!(on_tint >= 3.5, "{name:?}: code on {what}: {on_tint:.2}:1");
            }
        }
    }

    #[test]
    fn mixing_moves_between_the_two_ends_and_stops_there() {
        let (a, b) = (rgb(0, 0, 0), rgb(100, 200, 40));
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        // Out of range is clamped rather than extrapolated into nonsense.
        assert_eq!(mix(a, b, 2.0), b);
        assert_eq!(mix(a, b, -1.0), a);
    }

    /// The mix is PERCEPTUAL, which is the reason for the `palette` dependency
    /// rather than a detail.
    ///
    /// Halfway from black to white is about 99, not the 128 a straight sRGB
    /// interpolation gives: Oklab's lightness is roughly the cube root of
    /// luminance, so half the perceived distance is well below half the
    /// encoded value. Every "t of the way to the background" rule below is
    /// stated in those terms — if this ever reads 128, the mix has silently
    /// reverted to sRGB and every derived palette has shifted with it.
    #[test]
    fn mixing_is_perceptual_rather_than_a_straight_srgb_lerp() {
        let grey = mix(rgb(0, 0, 0), rgb(255, 255, 255), 0.5);
        assert_ne!(
            grey.red, 128,
            "that is the sRGB midpoint, not a perceptual one"
        );
        assert!(
            (90..=110).contains(&grey.red),
            "midpoint is {}, expected the Oklab midpoint near 99",
            grey.red
        );
        assert_eq!(grey.red, grey.green);
        assert_eq!(grey.green, grey.blue);
    }

    /// The two anchors of the WCAG scale, so a wrong conversion cannot pass by
    /// making every ratio equally wrong.
    #[test]
    fn contrast_runs_from_one_to_twenty_one() {
        let (black, white) = (rgb(0, 0, 0), rgb(255, 255, 255));
        assert!((contrast(black, white) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.01);
        // Symmetric: the ratio does not depend on which is the ground.
        assert_eq!(contrast(black, white), contrast(white, black));
    }
}
