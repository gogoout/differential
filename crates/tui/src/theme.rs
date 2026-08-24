//! One dark palette. Field schema modelled on lumen's `DiffColors`; the syntax
//! highlighter is cached process-wide (tuicr's `OnceLock<Arc<..>>` pattern).

use std::sync::{Arc, OnceLock};

use ratatui::style::{Color, Modifier, Style};
use two_face::theme::EmbeddedThemeName;

use super::vendor::syntax::SyntaxHighlighter;

pub struct Theme {
    pub added_bg: Color,
    pub deleted_bg: Color,
    pub added_word_bg: Color,
    pub deleted_word_bg: Color,
    pub gutter_fg: Color,
    pub context_fg: Color,
    pub cursor_bg: Color,
    pub selected_bg: Color,
    pub header_fg: Color,
    pub close_fg: Color,
    pub skim_fg: Color,
    pub noise_fg: Color,
    pub reviewed_fg: Color,
    pub finding_fg: Color,
    pub status_bg: Color,
}

pub const THEME: Theme = Theme {
    added_bg: Color::Rgb(18, 48, 24),
    deleted_bg: Color::Rgb(58, 22, 22),
    added_word_bg: Color::Rgb(28, 92, 42),
    deleted_word_bg: Color::Rgb(110, 38, 38),
    gutter_fg: Color::DarkGray,
    context_fg: Color::Gray,
    cursor_bg: Color::Rgb(48, 52, 70),
    selected_bg: Color::Rgb(40, 44, 58),
    header_fg: Color::Cyan,
    close_fg: Color::LightRed,
    skim_fg: Color::Yellow,
    noise_fg: Color::DarkGray,
    reviewed_fg: Color::Green,
    finding_fg: Color::Magenta,
    status_bg: Color::Rgb(30, 32, 40),
};

impl Theme {
    pub fn effort_style(&self, effort: differential_engine::schema::Effort) -> Style {
        let fg = match effort {
            differential_engine::schema::Effort::Close => self.close_fg,
            differential_engine::schema::Effort::Skim => self.skim_fg,
            differential_engine::schema::Effort::Noise => self.noise_fg,
        };
        Style::default().fg(fg)
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
