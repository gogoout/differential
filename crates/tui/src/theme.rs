//! Palettes: one seed each, everything else derived (ADR 0024).
//!
//! A theme decides seven things — which syntect theme paints the code, and six
//! accents that a syntect theme does not reliably carry. The other thirty-odd
//! colours are mixed from those by the rules in [`derive`], against the ground
//! the syntect theme itself declares. That is what keeps the chrome and the
//! code in one palette rather than two that drift.
//!
//! Field schema modelled on lumen's `DiffColors`. There is no hand-tuned
//! palette any more: a hand-tuned one is a palette the rules were never tested
//! against, and it is the rules every other theme depends on.

use std::sync::Arc;

use differential_engine::config::ThemeName;
use ratatui::style::{Color, Modifier, Style};
use two_face::theme::EmbeddedThemeName;

use super::vendor::LineOrigin;
use super::vendor::syntax::SyntaxHighlighter;

/// Eight bits a channel — the only form the derivation can mix.
///
/// `ratatui::style::Color` cannot be: most of its variants are ANSI names,
/// which have no value until a terminal supplies one, so there is nothing to
/// blend and nothing to contrast-check. That is exactly why the palettes
/// stopped using them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    const fn color(self) -> Color {
        Color::Rgb(self.0, self.1, self.2)
    }

    /// The colour `t` of the way from `self` to `other`, `t` in 0..=1.
    ///
    /// Plain sRGB interpolation. Weighed against the `palette` crate, which
    /// would do this in a perceptual space: that is a colour-science library
    /// for what is here two functions over `u8` triples. If a derived palette
    /// ever reads as muddy, this is the line to reach for Oklab in.
    fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let c = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgb(c(self.0, other.0), c(self.1, other.1), c(self.2, other.2))
    }

    /// Relative luminance, per WCAG 2.1 — the input to [`Rgb::contrast`].
    pub fn luminance(self) -> f32 {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }

    /// WCAG contrast ratio, 1.0 (identical) to 21.0 (black on white).
    pub fn contrast(self, other: Rgb) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The RGB behind a `Color`. `None` for anything a palette should no
    /// longer contain, which is what the tests assert on.
    pub fn of(c: Color) -> Option<Rgb> {
        match c {
            Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
            _ => None,
        }
    }

    fn from_syntect(c: syntect::highlighting::Color) -> Rgb {
        Rgb(c.r, c.g, c.b)
    }
}

/// What a theme decides for itself.
///
/// Six accents, because a syntect theme carries a background and a foreground
/// but has no opinion about what an *addition* is, or a finding, or the skim
/// tier. Everything else on [`Theme`] is derived from these.
struct Seed {
    /// The code's own colours, and the ground the chrome is mixed against.
    syntax: EmbeddedThemeName,
    add: Rgb,
    del: Rgb,
    /// The hunk you are reading, a header, the cursor's tint.
    accent: Rgb,
    /// The middle effort tier. Focus takes `del` and noise is derived, so this
    /// is the only tier with an ink of its own.
    skim: Rgb,
    finding: Rgb,
    reviewed: Rgb,
}

/// Accents are the theme family's own, so a Gruvbox review is Gruvbox all the
/// way down rather than one palette wearing another's green.
fn seed(name: ThemeName) -> Seed {
    match name {
        ThemeName::Dark => Seed {
            syntax: EmbeddedThemeName::Base16EightiesDark,
            add: Rgb(0x7C, 0xC7, 0x7F),
            del: Rgb(0xEF, 0x8A, 0x8A),
            accent: Rgb(0x5D, 0xD5, 0xE8),
            skim: Rgb(0xF2, 0xC9, 0x60),
            finding: Rgb(0xCB, 0x8A, 0xD6),
            reviewed: Rgb(0x7C, 0xC7, 0x7F),
        },
        ThemeName::Light => Seed {
            syntax: EmbeddedThemeName::Github,
            add: Rgb(0x1A, 0x7F, 0x37),
            del: Rgb(0xCF, 0x22, 0x2E),
            accent: Rgb(0x0A, 0x5C, 0xC2),
            skim: Rgb(0x8A, 0x5A, 0x00),
            finding: Rgb(0x7A, 0x3D, 0xD0),
            reviewed: Rgb(0x1A, 0x7F, 0x37),
        },
        ThemeName::GruvboxDark => Seed {
            syntax: EmbeddedThemeName::GruvboxDark,
            add: Rgb(0xB8, 0xBB, 0x26),
            del: Rgb(0xFB, 0x69, 0x54),
            accent: Rgb(0x8E, 0xC0, 0x7C),
            skim: Rgb(0xFA, 0xBD, 0x2F),
            finding: Rgb(0xD3, 0x86, 0x9B),
            reviewed: Rgb(0xB8, 0xBB, 0x26),
        },
        ThemeName::GruvboxLight => Seed {
            syntax: EmbeddedThemeName::GruvboxLight,
            add: Rgb(0x69, 0x64, 0x0C),
            del: Rgb(0x9D, 0x00, 0x06),
            accent: Rgb(0x07, 0x5F, 0x70),
            skim: Rgb(0x8F, 0x5D, 0x10),
            finding: Rgb(0x8F, 0x3F, 0x71),
            reviewed: Rgb(0x42, 0x6B, 0x4E),
        },
        ThemeName::SolarizedDark => Seed {
            syntax: EmbeddedThemeName::SolarizedDark,
            add: Rgb(0x9E, 0xB5, 0x00),
            del: Rgb(0xFF, 0x6E, 0x6B),
            accent: Rgb(0x35, 0xB9, 0xAF),
            skim: Rgb(0xCA, 0x9A, 0x00),
            finding: Rgb(0xEE, 0x74, 0xAA),
            reviewed: Rgb(0x9E, 0xB5, 0x00),
        },
        ThemeName::SolarizedLight => Seed {
            syntax: EmbeddedThemeName::SolarizedLight,
            add: Rgb(0x5B, 0x69, 0x00),
            del: Rgb(0xC2, 0x2B, 0x28),
            accent: Rgb(0x1E, 0x6F, 0xA8),
            skim: Rgb(0x8A, 0x68, 0x00),
            finding: Rgb(0xB0, 0x2B, 0x6C),
            reviewed: Rgb(0x5B, 0x69, 0x00),
        },
        ThemeName::Monokai => Seed {
            syntax: EmbeddedThemeName::MonokaiExtended,
            add: Rgb(0xA6, 0xE2, 0x2E),
            del: Rgb(0xF9, 0x5C, 0x8E),
            accent: Rgb(0x66, 0xD9, 0xEF),
            skim: Rgb(0xE6, 0xDB, 0x74),
            finding: Rgb(0xAE, 0x81, 0xFF),
            reviewed: Rgb(0xA6, 0xE2, 0x2E),
        },
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
const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);
const BLACK: Rgb = Rgb(0x00, 0x00, 0x00);

fn derive(seed: &Seed, syntect: syntect::highlighting::Theme) -> Theme {
    let settings = &syntect.settings;
    // A syntect theme is not obliged to state either, though every embedded one
    // does. The fallbacks keep a missing setting from being an invisible screen.
    let bg = settings
        .background
        .map_or(Rgb(0x1E, 0x20, 0x28), Rgb::from_syntect);
    let fg = settings
        .foreground
        .map_or(Rgb(0xE0, 0xE2, 0xE8), Rgb::from_syntect);

    // How much room the theme gives before anything is muted at all. Solarized
    // puts its own foreground 4.1:1 from its own ground; Monokai puts it at
    // 15:1. Muting both by the same fraction makes the quiet inks unreadable in
    // the first and merely quiet in the second — so the ramp is a share of the
    // headroom rather than a constant.
    let headroom = (fg.contrast(bg) / 8.0).clamp(0.45, 1.0);

    // Towards the ground, always. `q` for an accent, `m` for a muted ink, which
    // is the one that has to yield on a low-contrast palette.
    let q = |c: Rgb, t: f32| c.mix(bg, t).color();
    let m = |c: Rgb, t: f32| c.mix(bg, t * headroom).color();
    let (add, del, accent) = (seed.add, seed.del, seed.accent);

    // One ink for all three cursor blocks, so the line number does not change
    // colour as the cursor moves between an addition, a deletion and a plain
    // line. Chosen by whichever of the palette's own extremes reads WORST on
    // its hardest block least badly — picking against one block was how an ink
    // that is perfect on a bright green ended up at 1.4:1 on the plain tint.
    //
    // From the palette rather than a flat white, so a Solarized cursor stays
    // Solarized.
    let blocks = [add.mix(bg, 0.52), del.mix(bg, 0.52), accent.mix(bg, 0.72)];
    let worst = |ink: Rgb| {
        blocks
            .iter()
            .map(|b| ink.contrast(*b))
            .fold(f32::INFINITY, f32::min)
    };
    // The palette's own extremes first. They are not always enough: a mid
    // olive-green block against Solarized's mid-grey foreground and its very
    // dark ground leaves neither at 2:1, so the candidates also include a
    // near-white and a near-black, pulled a little towards the ground so they
    // still belong to the theme. A line number has to be readable before it
    // has to be tasteful.
    let cursor_gutter_fg = [fg, bg, bg.mix(WHITE, 0.94), bg.mix(BLACK, 0.94)]
        .into_iter()
        .max_by(|a, b| worst(*a).total_cmp(&worst(*b)))
        .expect("four candidates");

    Theme {
        bg: bg.color(),
        fg: fg.color(),
        // A tint you read code through, and the stronger block beside it.
        added_bg: q(add, 0.90),
        deleted_bg: q(del, 0.90),
        added_gutter_bg: q(add, 0.78),
        deleted_gutter_bg: q(del, 0.78),
        added_gutter_cursor_bg: q(add, 0.52),
        deleted_gutter_cursor_bg: q(del, 0.52),
        cursor_gutter_fg: cursor_gutter_fg.color(),
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
        context_fg: fg.color(),
        cursor_bg: q(accent, 0.72),
        selected_bg: q(accent, 0.90),
        header_fg: accent.color(),
        foreign_fg: m(accent, 0.34),
        // Focus is the must-read tier and wears the deletion red; noise is the
        // quietest thing on screen, one step past the gutter.
        focus_fg: del.color(),
        skim_fg: seed.skim.color(),
        noise_fg: m(fg, 0.42),
        reviewed_fg: seed.reviewed.color(),
        add_fg: add.color(),
        del_fg: del.color(),
        finding_fg: seed.finding.color(),
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

    fn rgb(c: Color) -> Rgb {
        Rgb::of(c).unwrap_or_else(|| panic!("{c:?} is not an Rgb — a palette may not use ANSI"))
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
            assert!(Rgb::of(t.bg).is_some(), "{name:?}");
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
                assert!(Rgb::of(c).is_some(), "{name:?}.{what} is {c:?}, not Rgb");
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
            let bg = rgb(t.bg);
            // A theme cannot be held to a bar it does not clear on bare
            // ground: Solarized is low-contrast on purpose. What is being
            // tested is that the DERIVATION never makes a theme less legible
            // than the theme already is.
            let base = rgb(t.fg).contrast(bg);
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
                let got = rgb(c).contrast(bg);
                if got < want {
                    bad.push(format!("{name:?}.{what}: {got:.2}:1, want {want:.2}:1"));
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
            let ink = rgb(t.cursor_gutter_fg);
            for (what, block) in [
                ("added", t.added_gutter_cursor_bg),
                ("deleted", t.deleted_gutter_cursor_bg),
                ("plain", t.cursor_bg),
            ] {
                let ratio = ink.contrast(rgb(block));
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
            let (bg, fg) = (rgb(t.bg), rgb(t.fg));
            let base = fg.contrast(bg);
            for (what, tint) in [("added", t.added_bg), ("deleted", t.deleted_bg)] {
                let tint = rgb(tint);
                assert_ne!(tint, bg, "{name:?}: the {what} tint is the ground");
                // Relative to what the theme itself offers, plus a floor. An
                // absolute 4.5:1 would be unmeetable for a base palette that
                // only reaches 5.6:1 on its own ground — the question is
                // whether the tint DROWNS the code, not whether the theme is
                // high-contrast.
                let on_tint = fg.contrast(tint);
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
        let (a, b) = (Rgb(0, 0, 0), Rgb(100, 200, 40));
        assert_eq!(a.mix(b, 0.0), a);
        assert_eq!(a.mix(b, 1.0), b);
        assert_eq!(a.mix(b, 0.5), Rgb(50, 100, 20));
        // Out of range is clamped rather than extrapolated into nonsense.
        assert_eq!(a.mix(b, 2.0), b);
        assert_eq!(a.mix(b, -1.0), a);
    }

    /// The two anchors of the WCAG scale, so a wrong constant in `luminance`
    /// cannot pass by making every ratio equally wrong.
    #[test]
    fn contrast_runs_from_one_to_twenty_one() {
        let (black, white) = (Rgb(0, 0, 0), Rgb(255, 255, 255));
        assert!((black.contrast(white) - 21.0).abs() < 0.01);
        assert!((white.contrast(white) - 1.0).abs() < 0.01);
        // Symmetric: the ratio does not depend on which is the ground.
        assert_eq!(black.contrast(white), white.contrast(black));
    }
}
