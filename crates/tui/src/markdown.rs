//! Render a forge comment body as markdown.
//!
//! A comment is CommonMark on the forge, so it is CommonMark here. The body is
//! parsed once with `pulldown-cmark` (the boring, standard parser) and its
//! events are folded into lines of styled ratatui spans — one entry per line,
//! each a run of spans. The caller hangs each line off the thread's rail, and
//! the reviewer's existing span-aware soft wrap re-flows a long line.
//!
//! Emphasis is carried as `Modifier` (BOLD / ITALIC / …), matching the rest of
//! the reviewer; colour is taken from the theme's existing ink tokens, so no
//! new palette tokens are needed (ADR 0024). A fenced code block is run through
//! the diff's own syntect highlighter, keyed by the fence's language. Images,
//! raw HTML and tables are not rendered — their text shows or is dropped.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::theme::Theme;

/// A comment `body` as lines of styled spans, over the `base` prose style.
/// Never empty: an empty body yields one empty line so the comment still draws.
///
/// When `muted` (a resolved thread), the structure is still rendered — markers
/// gone, bullets, code as text — but every colour is the dim `base`: a resolved
/// thread stays settled to the eye, readable rather than raw.
pub fn render(body: &str, base: Style, theme: &Theme, muted: bool) -> Vec<Vec<Span<'static>>> {
    let mut r = Renderer::new(base, theme, muted);
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(body, opts) {
        r.event(event);
    }
    r.finish()
}

struct Renderer<'t> {
    base: Style,
    theme: &'t Theme,
    /// A resolved thread: keep the structure but never brighten it.
    muted: bool,
    lines: Vec<Vec<Span<'static>>>,
    cur: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    strike: u32,
    code: bool,
    link: bool,
    heading: bool,
    /// Blockquote nesting depth: each line begins with that many `> `.
    quote: usize,
    /// One entry per open list level; `Some(n)` is the next ordered number,
    /// `None` a bullet.
    lists: Vec<Option<u64>>,
    in_code_block: bool,
    /// A fenced block's language, if it named one; its raw text meanwhile.
    code_lang: Option<String>,
    code_buf: String,
}

impl<'t> Renderer<'t> {
    fn new(base: Style, theme: &'t Theme, muted: bool) -> Self {
        Renderer {
            base,
            theme,
            muted,
            lines: Vec::new(),
            cur: Vec::new(),
            bold: 0,
            italic: 0,
            strike: 0,
            code: false,
            link: false,
            heading: false,
            quote: 0,
            lists: Vec::new(),
            in_code_block: false,
            code_lang: None,
            code_buf: String::new(),
        }
    }

    /// The style for text emitted right now, from the base plus what is open.
    /// A muted thread keeps the dim base colour; only the modifiers apply.
    fn style(&self) -> Style {
        let mut s = if self.muted {
            self.base
        } else if self.heading {
            self.base.fg(self.theme.finding_fg)
        } else if self.code {
            self.base.fg(self.theme.hint_fg)
        } else if self.link {
            self.base.fg(self.theme.focus_fg)
        } else {
            self.base
        };
        if self.bold > 0 || self.heading {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.link {
            s = s.add_modifier(Modifier::UNDERLINED);
        }
        s
    }

    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let style = self.style();
        self.cur.push(Span::styled(text.to_string(), style));
    }

    /// The dim ink for a prefix or rule: the base itself when muted.
    fn faint(&self) -> Style {
        if self.muted {
            self.base
        } else {
            self.base.fg(self.theme.noise_fg)
        }
    }

    /// Seed a fresh line with its blockquote prefix, if any.
    fn open_line(&mut self) {
        for _ in 0..self.quote {
            self.cur.push(Span::styled("> ".to_string(), self.faint()));
        }
    }

    /// End the current line. A line carrying only its prefix is dropped, so an
    /// empty paragraph does not leave a blank row.
    fn close_line(&mut self) {
        if self.cur.iter().any(|s| !s.content.trim().is_empty()) {
            let line = std::mem::take(&mut self.cur);
            self.lines.push(line);
        }
        self.cur.clear();
    }

    /// One blank line between top-level blocks, never doubled.
    fn separate(&mut self) {
        let at_top = self.quote == 0 && self.lists.is_empty();
        let last_blank = self.lines.last().is_none_or(Vec::is_empty);
        if at_top && !self.lines.is_empty() && !last_blank {
            self.lines.push(Vec::new());
        }
    }

    /// Emit the buffered code block: each line syntect-highlighted by the
    /// fence's language when one is known and resolves, else plain in the code
    /// ink. Runs the same highlighter the diff pane uses.
    fn flush_code_block(&mut self) {
        let buf = std::mem::take(&mut self.code_buf);
        let lang = self.code_lang.take();
        let lines: Vec<&str> = buf.strip_suffix('\n').unwrap_or(&buf).split('\n').collect();
        // A muted thread keeps code dim too: no syntect colour on it.
        let highlighted = (!self.muted)
            .then_some(lang)
            .flatten()
            .filter(|l| !l.trim().is_empty())
            .and_then(|l| self.theme.highlighter().highlight_fenced(&l, &lines));
        match highlighted {
            Some(rows) => {
                for row in rows {
                    self.lines
                        .push(row.into_iter().map(|(s, t)| Span::styled(t, s)).collect());
                }
            }
            None => {
                let code = if self.muted {
                    self.base
                } else {
                    self.base.fg(self.theme.hint_fg)
                };
                for line in lines {
                    self.lines.push(vec![Span::styled(line.to_string(), code)]);
                }
            }
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            // A code block's text is held whole, then highlighted at its end:
            // syntect carries parse state line to line, so it needs the run.
            Event::Text(t) if self.in_code_block => self.code_buf.push_str(&t),
            Event::Text(t) => self.push(&t),
            Event::Code(t) => {
                self.code = true;
                self.push(&t);
                self.code = false;
            }
            Event::SoftBreak | Event::HardBreak => {
                self.close_line();
                self.open_line();
            }
            Event::Rule => {
                self.close_line();
                self.separate();
                let rule = self.faint();
                self.lines.push(vec![Span::styled("───".to_string(), rule)]);
            }
            Event::TaskListMarker(done) => {
                self.push(if done { "[x] " } else { "[ ] " });
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.close_line();
                self.separate();
                self.open_line();
            }
            Tag::Heading { .. } => {
                self.close_line();
                self.separate();
                self.open_line();
                self.heading = true;
            }
            Tag::BlockQuote(_) => {
                self.close_line();
                self.separate();
                // The prefix is seeded when the inner paragraph opens its
                // line, not here, or an empty prefixed line would slip in.
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.close_line();
                self.separate();
                self.in_code_block = true;
                self.code_buf.clear();
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(info) => Some(info.into_string()),
                    CodeBlockKind::Indented => None,
                };
            }
            Tag::List(first) => {
                self.close_line();
                self.separate();
                self.lists.push(first);
            }
            Tag::Item => {
                self.close_line();
                self.open_line();
                let depth = self.lists.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{indent}{n}. ");
                        *n += 1;
                        m
                    }
                    _ => format!("{indent}• "),
                };
                self.cur.push(Span::styled(marker, self.base));
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { .. } => self.link = true,
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.close_line(),
            TagEnd::Heading(_) => {
                self.heading = false;
                self.close_line();
            }
            TagEnd::BlockQuote(_) => {
                self.close_line();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.flush_code_block();
                self.in_code_block = false;
            }
            TagEnd::List(_) => {
                self.close_line();
                self.lists.pop();
            }
            TagEnd::Item => self.close_line(),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = false,
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Vec<Span<'static>>> {
        self.close_line();
        // A trailing blank between blocks earns no row on its own.
        while self.lines.last().is_some_and(Vec::is_empty) {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn theme() -> Theme {
        Theme::named(differential_engine::config::ThemeName::Dark)
    }

    /// The plain text of a rendered line.
    fn text(line: &[Span]) -> String {
        line.iter().map(|s| s.content.as_ref()).collect()
    }

    fn has_mod(line: &[Span], needle: &str, m: Modifier) -> bool {
        line.iter()
            .any(|s| s.content.contains(needle) && s.style.add_modifier.contains(m))
    }

    #[test]
    fn a_heading_is_bold() {
        let t = theme();
        let lines = render("# Title", Style::default(), &t, false);
        assert_eq!(text(&lines[0]), "Title");
        assert!(has_mod(&lines[0], "Title", Modifier::BOLD));
    }

    #[test]
    fn bold_and_italic_and_code_carry_their_styles() {
        let t = theme();
        let lines = render("a **b** c *d* `e`", Style::default(), &t, false);
        let line = &lines[0];
        assert_eq!(text(line), "a b c d e");
        assert!(has_mod(line, "b", Modifier::BOLD));
        assert!(has_mod(line, "d", Modifier::ITALIC));
        assert!(
            line.iter()
                .any(|s| s.content == "e" && s.style.fg == Some(t.hint_fg))
        );
    }

    #[test]
    fn a_bullet_list_gets_markers() {
        let t = theme();
        let lines = render("- one\n- two", Style::default(), &t, false);
        assert_eq!(text(&lines[0]), "• one");
        assert_eq!(text(&lines[1]), "• two");
    }

    #[test]
    fn an_ordered_list_numbers_its_items() {
        let t = theme();
        let lines = render("1. one\n2. two", Style::default(), &t, false);
        assert_eq!(text(&lines[0]), "1. one");
        assert_eq!(text(&lines[1]), "2. two");
    }

    #[test]
    fn a_link_shows_its_text_underlined() {
        let t = theme();
        let lines = render(
            "see [the docs](https://example.invalid)",
            Style::default(),
            &t,
            false,
        );
        assert_eq!(text(&lines[0]), "see the docs");
        assert!(has_mod(&lines[0], "the docs", Modifier::UNDERLINED));
    }

    #[test]
    fn a_fenced_code_block_keeps_each_line() {
        let t = theme();
        let lines = render(
            "```\nfn main() {}\nlet x = 1;\n```",
            Style::default(),
            &t,
            false,
        );
        let texts: Vec<String> = lines.iter().map(|l| text(l)).collect();
        assert!(texts.iter().any(|l| l == "fn main() {}"), "{texts:?}");
        assert!(texts.iter().any(|l| l == "let x = 1;"), "{texts:?}");
    }

    #[test]
    fn a_rust_fence_is_syntax_highlighted_into_tokens() {
        let t = theme();
        let lines = render("```rust\nfn main() {}\n```", Style::default(), &t, false);
        let code = lines
            .iter()
            .find(|l| text(l).contains("fn main"))
            .expect("the code line");
        // Highlighted code splits into several coloured spans; plain text is
        // one span. More than one means syntect ran on it.
        assert!(code.len() > 1, "{:?}", code);
    }

    #[test]
    fn an_unknown_fence_stays_plain_code() {
        let t = theme();
        let lines = render("```\nnot a language\n```", Style::default(), &t, false);
        let code = lines.iter().find(|l| text(l).contains("not a language"));
        assert_eq!(code.map(Vec::len), Some(1), "plain code is one span");
    }

    #[test]
    fn a_blockquote_is_prefixed() {
        let t = theme();
        let lines = render("> quoted", Style::default(), &t, false);
        assert_eq!(text(&lines[0]), "> quoted");
    }

    #[test]
    fn a_muted_thread_renders_structure_but_stays_dim() {
        let t = theme();
        let base = Style::default().fg(t.noise_fg);
        let lines = render("# H\n\n**bold** `code`\n\n- one", base, &t, true);
        // Markers still gone: a heading, a bullet, no backticks or stars.
        let all: String = lines
            .iter()
            .flat_map(|l| l.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(all.contains("• one"), "{all}");
        assert!(
            !all.contains('#') && !all.contains('`') && !all.contains('*'),
            "{all}"
        );
        // But every span keeps the dim base colour — no bright accents.
        for line in &lines {
            for s in line {
                assert_eq!(s.style.fg, Some(t.noise_fg), "{:?}", s);
            }
        }
    }

    #[test]
    fn an_empty_body_is_one_empty_line() {
        let t = theme();
        let lines = render("", Style::default(), &t, false);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].is_empty());
    }

    #[test]
    fn two_paragraphs_are_separated_by_a_blank_line() {
        let t = theme();
        let lines = render("first\n\nsecond", Style::default(), &t, false);
        assert_eq!(text(&lines[0]), "first");
        assert!(lines[1].is_empty());
        assert_eq!(text(&lines[2]), "second");
    }
}
