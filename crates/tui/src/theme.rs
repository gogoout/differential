//! One dark palette. Field schema modelled on lumen's `DiffColors`; the syntax
//! highlighter is cached process-wide (tuicr's `OnceLock<Arc<..>>` pattern).

use std::sync::{Arc, OnceLock};

use ratatui::style::{Color, Modifier, Style};
use two_face::theme::EmbeddedThemeName;

use super::vendor::LineOrigin;
use super::vendor::syntax::SyntaxHighlighter;

pub struct Theme {
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
    /// The line number itself on that row. One ink for all three blocks, light
    /// enough to read on bright green, bright red and the plain cursor grey.
    pub cursor_gutter_fg: Color,
    pub added_word_bg: Color,
    pub deleted_word_bg: Color,
    /// Diagonal fill for the side of a split row that has no line at all.
    pub hatch_fg: Color,
    /// A hunk pill that is not lit: a control, but not the one in hand.
    pub button_fg: Color,
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
    /// A hunk crossed in from another group. The same cyan the hunk you ARE
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
}

pub const THEME: Theme = Theme {
    added_bg: Color::Rgb(18, 48, 24),
    deleted_bg: Color::Rgb(58, 22, 22),
    added_gutter_bg: Color::Rgb(24, 68, 33),
    deleted_gutter_bg: Color::Rgb(82, 29, 29),
    added_gutter_cursor_bg: Color::Rgb(46, 128, 62),
    deleted_gutter_cursor_bg: Color::Rgb(150, 52, 52),
    cursor_gutter_fg: Color::Rgb(240, 242, 248),
    added_word_bg: Color::Rgb(28, 92, 42),
    deleted_word_bg: Color::Rgb(110, 38, 38),
    hatch_fg: Color::Rgb(48, 50, 58),
    button_fg: Color::Rgb(198, 204, 220),
    button_bg: Color::Rgb(58, 63, 83),
    sticky_bg: Color::Rgb(34, 37, 44),
    hint_fg: Color::Rgb(108, 114, 126),
    hint_bg: Color::Rgb(38, 41, 48),
    hint_cursor_fg: Color::Rgb(198, 206, 222),
    hint_cursor_bg: Color::Rgb(64, 69, 82),
    gutter_fg: Color::DarkGray,
    context_fg: Color::Gray,
    cursor_bg: Color::Rgb(48, 52, 70),
    selected_bg: Color::Rgb(40, 44, 58),
    header_fg: Color::Cyan,
    foreign_fg: Color::Rgb(84, 132, 146),
    focus_fg: Color::LightRed,
    skim_fg: Color::Yellow,
    noise_fg: Color::DarkGray,
    reviewed_fg: Color::Green,
    add_fg: Color::LightGreen,
    del_fg: Color::LightRed,
    finding_fg: Color::Magenta,
    status_bg: Color::Rgb(30, 32, 40),
};

impl Theme {
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
    /// By colour, like `lit_ink`: this is the one place that knows a gutter's
    /// blocks, so a fourth one means adding it here rather than at the call
    /// site. An unchanged line has no block of its own and takes the plain
    /// cursor grey, which is what makes the cursor readable on every row.
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
    /// By colour, like `lit_ink` and `gutter_cursor`: this is the one place
    /// that knows the band's inks. Anything else — a line's change colour, a
    /// syntax span — passes through untouched, which is what lets one pass
    /// over a whole row lighten the band and nothing else.
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

/// The syntax set/theme build is the expensive part — one per process.
pub fn highlighter() -> Arc<SyntaxHighlighter> {
    static CELL: OnceLock<Arc<SyntaxHighlighter>> = OnceLock::new();
    CELL.get_or_init(|| {
        Arc::new(SyntaxHighlighter::new(
            EmbeddedThemeName::Base16EightiesDark,
            THEME.added_bg,
            THEME.deleted_bg,
        ))
    })
    .clone()
}
