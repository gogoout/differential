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
    pub added_word_bg: Color,
    pub deleted_word_bg: Color,
    /// Diagonal fill for the side of a split row that has no line at all.
    pub hatch_fg: Color,
    /// A hunk pill that is not lit: a control, but not the one in hand.
    pub button_fg: Color,
    pub button_bg: Color,
    /// The context-boundary pill and its rule. Present, but not competing with
    /// the code — it marks where the file was cut, and the file is the point.
    pub hint_fg: Color,
    pub hint_bg: Color,
    /// Text on a LIT pill, whose fill is the hunk's accent — so it has to be
    /// dark enough to read on yellow, green or cyan alike.
    pub pill_fg: Color,
    /// The counts on a lit pill. `add_fg`/`del_fg` are chosen to glow on a dark
    /// background and are illegible on a bright one, so a lit pill needs its
    /// own pair — dark enough to read on the accent, saturated enough to still
    /// say added and removed.
    pub add_on_pill: Color,
    pub del_on_pill: Color,
    pub gutter_fg: Color,
    pub context_fg: Color,
    pub cursor_bg: Color,
    pub selected_bg: Color,
    pub header_fg: Color,
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
    added_word_bg: Color::Rgb(28, 92, 42),
    deleted_word_bg: Color::Rgb(110, 38, 38),
    hatch_fg: Color::Rgb(48, 50, 58),
    button_fg: Color::Rgb(198, 204, 220),
    button_bg: Color::Rgb(58, 63, 83),
    hint_fg: Color::Rgb(108, 114, 126),
    hint_bg: Color::Rgb(38, 41, 48),
    pill_fg: Color::Rgb(20, 22, 28),
    add_on_pill: Color::Rgb(14, 72, 28),
    del_on_pill: Color::Rgb(112, 20, 20),
    gutter_fg: Color::DarkGray,
    context_fg: Color::Gray,
    cursor_bg: Color::Rgb(48, 52, 70),
    selected_bg: Color::Rgb(40, 44, 58),
    header_fg: Color::Cyan,
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

    /// The role decoration, empty when the ordering stage assigned none.
    pub fn role_suffix(role: Option<differential_engine::schema::Role>) -> String {
        role.map(|r| format!(" · {}", differential_engine::plan::role_name(r)))
            .unwrap_or_default()
    }

    /// The two colours a pill wears. `accent` is `Some` when this is the hunk
    /// under the cursor, and the fill then matches its edge so the marker and
    /// the run below it read as one thing.
    pub fn pill(&self, accent: Option<Color>) -> (Color, Color) {
        match accent {
            Some(bg) => (self.pill_fg, bg),
            None => (self.button_fg, self.button_bg),
        }
    }

    /// Re-ink one span of a pill for a LIT fill.
    ///
    /// A pill is built in its muted palette because whether it is lit is a
    /// cursor question; this maps each ink to its bright-background twin. The
    /// mapping is by colour, so it is the one place that knows a pill's inks —
    /// adding a third means adding it here, not just at the call site.
    pub fn lit_ink(&self, muted: Option<Color>) -> Color {
        match muted {
            Some(c) if c == self.add_fg => self.add_on_pill,
            Some(c) if c == self.del_fg => self.del_on_pill,
            _ => self.pill_fg,
        }
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
