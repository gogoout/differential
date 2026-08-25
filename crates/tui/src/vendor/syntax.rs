// Adapted from agavra/tuicr (0dacb6b), src/syntax/mod.rs — the markdown
// (cmark) layer and the full-file-highlight heuristic removed, LineOrigin
// supplied by the vendor module. MIT License — Copyright (c) 2025 tuicr
// contributors. See LICENSE-MIT.
//
// `highlight_ranges` and `Highlighted` are OURS, not tuicr's: the whole-file
// entry point they replace was the reviewer's dominant cost (ADR 0021).

use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use two_face::theme::EmbeddedThemeName;

use super::LineOrigin;

/// A single line of highlighted spans (style + text pairs).
pub type HighlightedSpans = Vec<(Style, String)>;

/// How far above a window syntect is run before its results are kept.
///
/// syntect's parse state is carried line to line, so highlighting a window in
/// isolation would colour a line that opens inside a multi-line string or
/// comment as if it were code. Priming from a little way above fixes that for
/// every realistic construct while keeping the cost proportional to the window
/// rather than to the file.
pub const LOOKBACK: usize = 64;

/// The result of a windowed highlight: spans by zero-based line index, plus
/// how many lines syntect actually parsed.
///
/// `lines_scanned` exists so a test can assert the cost is proportional to the
/// window and not to the file — the property this whole approach buys, and one
/// that a wall-clock assertion could only measure flakily.
pub struct Highlighted {
    pub spans: HashMap<usize, HighlightedSpans>,
    pub lines_scanned: usize,
}

/// Helper to highlight lines of code from a diff
pub struct SyntaxHighlighter {
    pub syntax_set: syntect::parsing::SyntaxSet,
    pub theme: syntect::highlighting::Theme,
    /// Background color for added lines
    pub add_bg: Color,
    /// Background color for deleted lines
    pub del_bg: Color,
}

pub struct DiffHighlightSequences {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub old_line_indices: Vec<Option<usize>>,
    pub new_line_indices: Vec<Option<usize>>,
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new(
            EmbeddedThemeName::Base16EightiesDark,
            Color::Rgb(0, 35, 12),
            Color::Rgb(45, 0, 0),
        )
    }
}

impl SyntaxHighlighter {
    /// Create a new syntax highlighter with the given theme and diff background colors
    pub fn new(theme_name: EmbeddedThemeName, add_bg: Color, del_bg: Color) -> Self {
        let theme_set = two_face::theme::extra();
        let theme = theme_set[theme_name].clone();

        Self::with_theme(theme, add_bg, del_bg)
    }

    /// Create a new syntax highlighter with a preloaded syntect theme.
    pub fn with_theme(theme: syntect::highlighting::Theme, add_bg: Color, del_bg: Color) -> Self {
        let syntax_set = two_face::syntax::extra_newlines();
        Self {
            syntax_set,
            theme,
            add_bg,
            del_bg,
        }
    }

    /// A highlighter that resolves no syntax at all, so `highlight_file_lines`
    /// returns `None` for every path without doing any syntect work.
    ///
    /// The diff watcher uses this to parse a diff when it needs the content but not
    /// the colours. `DiffFile::compute_content_hash` runs during parsing over line
    /// text alone, and highlighting only ever assigns spans, so a diff parsed this
    /// way fingerprints identically to a highlighted one. Measured at 3.1ms against
    /// 197ms for the same 4,000-line diff.
    pub fn plain() -> Self {
        let theme = syntect::highlighting::Theme::default();
        Self {
            syntax_set: syntect::parsing::SyntaxSet::new(),
            theme,
            add_bg: Color::Reset,
            del_bg: Color::Reset,
        }
    }

    /// Highlight only the line ranges asked for.
    ///
    /// `wanted` must be sorted and non-overlapping (zero-based, half-open).
    /// One `HighlightLines` walks forward over all of them: a short gap between
    /// two ranges is simply run through — cheaper than a fresh prime, and more
    /// accurate — while a longer jump restarts with a `LOOKBACK` prime. So the
    /// cost is proportional to the lines rendered, not to the file's size.
    ///
    /// Returns `None` when no syntax can be resolved for the file (by path or
    /// shebang); a line that syntect itself fails on is simply absent from the
    /// map, so one bad line never costs the rest of the window its colour.
    pub fn highlight_ranges(
        &self,
        file_path: &Path,
        lines: &[String],
        wanted: &[Range<usize>],
    ) -> Option<Highlighted> {
        use syntect::easy::HighlightLines;

        let syntax = self.get_syntax(file_path).or_else(|| {
            lines
                .first()
                .and_then(|line| self.syntax_set.find_syntax_by_first_line(line))
        })?;

        let mut out = Highlighted {
            spans: HashMap::new(),
            lines_scanned: 0,
        };
        let mut hl = HighlightLines::new(syntax, &self.theme);
        // Where `hl`'s parse state currently stands, or `None` before the first
        // range primes it.
        let mut at: Option<usize> = None;

        for range in wanted {
            let (start, end) = (range.start, range.end.min(lines.len()));
            if start >= end {
                continue;
            }
            let prime_from = match at {
                Some(a) if a <= start && start - a <= LOOKBACK => a,
                _ => {
                    hl = HighlightLines::new(syntax, &self.theme);
                    start.saturating_sub(LOOKBACK)
                }
            };
            for line in &lines[prime_from..start] {
                let _ = self.spans_for(&mut hl, line);
                out.lines_scanned += 1;
            }
            for (i, line) in lines[start..end].iter().enumerate() {
                if let Some(spans) = self.spans_for(&mut hl, line) {
                    out.spans.insert(start + i, spans);
                }
                out.lines_scanned += 1;
            }
            at = Some(end);
        }
        Some(out)
    }

    /// One line through syntect, converted to ratatui spans.
    fn spans_for(
        &self,
        hl: &mut syntect::easy::HighlightLines,
        line: &str,
    ) -> Option<HighlightedSpans> {
        // Highlight failures are scoped to the single line; other lines still keep highlighting.
        hl.highlight_line(&format!("{line}\n"), &self.syntax_set)
            .ok()
            .map(|ranges| {
                let mut spans: Vec<(Style, String)> = ranges
                    .into_iter()
                    .map(|(style, text)| (Self::syntect_to_ratatui_style(style), text.to_string()))
                    .collect();
                // Strip trailing \n that syntect includes from the input.
                // Leaving it causes ratatui to allocate an extra buffer cell,
                // misaligning side-by-side diff columns on short (padded) lines.
                if let Some(last) = spans.last_mut()
                    && last.1.ends_with('\n')
                {
                    last.1.truncate(last.1.len() - 1);
                    if last.1.is_empty() {
                        spans.pop();
                    }
                }
                spans
            })
    }

    fn highlighted_line_at(
        highlighted_lines: Option<&[Option<HighlightedSpans>]>,
        line_idx: Option<usize>,
    ) -> Option<HighlightedSpans> {
        line_idx
            .and_then(|idx| highlighted_lines.and_then(|all| all.get(idx)))
            .and_then(|line_highlight| line_highlight.as_ref().cloned())
    }

    pub fn split_diff_lines_for_highlighting(
        line_contents: &[String],
        line_origins: &[LineOrigin],
    ) -> DiffHighlightSequences {
        debug_assert_eq!(line_contents.len(), line_origins.len());

        let mut old_lines = Vec::new();
        let mut new_lines = Vec::new();
        let mut old_line_indices = Vec::with_capacity(line_origins.len());
        let mut new_line_indices = Vec::with_capacity(line_origins.len());

        for (content, origin) in line_contents.iter().zip(line_origins.iter()) {
            match origin {
                LineOrigin::Context => {
                    let old_idx = old_lines.len();
                    old_lines.push(content.clone());
                    old_line_indices.push(Some(old_idx));

                    let new_idx = new_lines.len();
                    new_lines.push(content.clone());
                    new_line_indices.push(Some(new_idx));
                }
                LineOrigin::Addition => {
                    let new_idx = new_lines.len();
                    new_lines.push(content.clone());
                    old_line_indices.push(None);
                    new_line_indices.push(Some(new_idx));
                }
                LineOrigin::Deletion => {
                    let old_idx = old_lines.len();
                    old_lines.push(content.clone());
                    old_line_indices.push(Some(old_idx));
                    new_line_indices.push(None);
                }
            }
        }

        DiffHighlightSequences {
            old_lines,
            new_lines,
            old_line_indices,
            new_line_indices,
        }
    }

    pub fn highlighted_line_for_diff_with_background(
        &self,
        old_highlighted_lines: Option<&[Option<HighlightedSpans>]>,
        new_highlighted_lines: Option<&[Option<HighlightedSpans>]>,
        old_line_idx: Option<usize>,
        new_line_idx: Option<usize>,
        origin: LineOrigin,
    ) -> Option<HighlightedSpans> {
        let spans = match origin {
            LineOrigin::Addition => Self::highlighted_line_at(new_highlighted_lines, new_line_idx),
            LineOrigin::Deletion => Self::highlighted_line_at(old_highlighted_lines, old_line_idx),
            LineOrigin::Context => Self::highlighted_line_at(new_highlighted_lines, new_line_idx),
        }?;

        Some(self.apply_diff_background(spans, origin))
    }

    fn syntect_to_ratatui_style(style: syntect::highlighting::Style) -> Style {
        let fg_color = Self::syntect_color_to_ratatui(style.foreground);
        let mut ratatui_style = Style::default().fg(fg_color);

        if style
            .font_style
            .contains(syntect::highlighting::FontStyle::BOLD)
        {
            ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
        }
        if style
            .font_style
            .contains(syntect::highlighting::FontStyle::ITALIC)
        {
            ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
        }
        if style
            .font_style
            .contains(syntect::highlighting::FontStyle::UNDERLINE)
        {
            ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
        }

        ratatui_style
    }

    /// Translate syntect colors into ratatui colors.
    ///
    /// Some bat-compatible Base16 `.tmTheme` files encode ANSI palette slots as
    /// placeholder colors of the form `#0N000000`. syntect preserves those
    /// bytes literally, so we translate them here at the render boundary.
    fn syntect_color_to_ratatui(color: syntect::highlighting::Color) -> Color {
        if color.g == 0 && color.b == 0 && color.a == 0 {
            return match color.r {
                0 => Color::Black,
                1 => Color::Red,
                2 => Color::Green,
                3 => Color::Yellow,
                4 => Color::Blue,
                5 => Color::Magenta,
                6 => Color::Cyan,
                7 => Color::Gray,
                8 => Color::DarkGray,
                9 => Color::LightRed,
                10 => Color::LightGreen,
                11 => Color::LightYellow,
                12 => Color::LightBlue,
                13 => Color::LightMagenta,
                14 => Color::LightCyan,
                15 => Color::White,
                _ => Color::Rgb(color.r, color.g, color.b),
            };
        }

        Color::Rgb(color.r, color.g, color.b)
    }

    /// Map extensions not in two-face's syntax set to a known equivalent.
    fn fallback_extension(ext: &str) -> Option<&'static str> {
        match ext {
            "jsx" | "mjs" | "cjs" => Some("js"),
            "hbs" | "handlebars" | "mustache" | "ejs" | "pug" | "jade" | "njk" => Some("html"),
            "mdx" => Some("md"),
            "jsonc" | "json5" | "prisma" => Some("json"),
            "heex" => Some("rb"),
            _ => None,
        }
    }

    /// Map extension-less filenames to a known syntax extension.
    fn fallback_filename(name: &str) -> Option<&'static str> {
        match name {
            "Containerfile" => Some("sh"),
            "Justfile" | "justfile" => Some("sh"),
            _ => None,
        }
    }

    /// Resolve syntax from a file path using this lookup order:
    /// extension -> lowercase extension (when different) -> fallback extension ->
    /// filename token -> filename name -> fallback filename.
    fn get_syntax(&self, file_path: &Path) -> Option<&syntect::parsing::SyntaxReference> {
        // Try by extension first
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            if let Some(syntax) = self.syntax_set.find_syntax_by_extension(ext) {
                return Some(syntax);
            }

            let normalized = ext.to_ascii_lowercase();
            if normalized != ext
                && let Some(syntax) = self.syntax_set.find_syntax_by_extension(&normalized)
            {
                return Some(syntax);
            }

            // Try fallback mapping for extensions not in syntect's defaults
            if let Some(fallback) = Self::fallback_extension(&normalized)
                && let Some(syntax) = self.syntax_set.find_syntax_by_extension(fallback)
            {
                return Some(syntax);
            }
        }

        // Try token/name matches for extension-less files (e.g. Makefile, BUILD).
        if let Some(filename) = file_path.file_name().and_then(|f| f.to_str()) {
            if let Some(syntax) = self.syntax_set.find_syntax_by_token(filename) {
                return Some(syntax);
            }

            if let Some(syntax) = self.syntax_set.find_syntax_by_name(filename) {
                return Some(syntax);
            }

            if let Some(fallback) = Self::fallback_filename(filename)
                && let Some(syntax) = self.syntax_set.find_syntax_by_extension(fallback)
            {
                return Some(syntax);
            }
        }

        None
    }

    /// Apply diff background colors to highlighted spans based on line origin
    pub fn apply_diff_background(
        &self,
        spans: Vec<(Style, String)>,
        origin: LineOrigin,
    ) -> Vec<(Style, String)> {
        let bg_color = match origin {
            LineOrigin::Addition => self.add_bg,
            LineOrigin::Deletion => self.del_bg,
            LineOrigin::Context => return spans, // No background for context
        };

        spans
            .into_iter()
            .map(|(style, text)| (style.bg(bg_color), text))
            .collect()
    }
}

// A slice holding one `Range` is exactly what `highlight_ranges` takes; the
// lint is about `vec![0..n]` written where a list of numbers was meant.
#[allow(clippy::single_range_in_vec_init)]
#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of `lines`, in order, as the old whole-file entry point
    /// returned them — so the ported tests still read as assertions about
    /// per-line results rather than about ranges.
    fn all_lines(
        h: &SyntaxHighlighter,
        path: &str,
        lines: &[String],
    ) -> Option<Vec<Option<HighlightedSpans>>> {
        let got = h.highlight_ranges(Path::new(path), lines, &[0..lines.len()])?;
        Some(
            (0..lines.len())
                .map(|i| got.spans.get(&i).cloned())
                .collect(),
        )
    }

    #[test]
    fn should_resolve_no_syntax_for_any_path() {
        let plain = SyntaxHighlighter::plain();
        assert!(
            all_lines(&plain, "a.rs", &["fn main() {}".to_string()]).is_none(),
            "plain highlighter must not resolve a syntax, or the probe is not cheap"
        );
    }

    #[test]
    fn should_find_syntax_for_uppercase_extension() {
        let highlighter = SyntaxHighlighter::default();
        let syntax = highlighter.get_syntax(Path::new("SRC/MAIN.RS"));
        assert!(syntax.is_some());
    }

    #[test]
    fn should_find_syntax_for_build_filename_token() {
        let highlighter = SyntaxHighlighter::default();
        let syntax = highlighter.get_syntax(Path::new("BUILD"));
        assert!(syntax.is_some());
    }

    #[test]
    fn should_highlight_each_line_independently() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "fn main() {".to_string(),
            "    let x = 42;".to_string(),
            "}".to_string(),
        ];
        let highlighted = all_lines(&highlighter, "main.rs", &lines);

        assert!(highlighted.is_some());
        let highlighted = highlighted.unwrap();
        assert_eq!(highlighted.len(), lines.len());
        assert!(highlighted.iter().all(|line| line.is_some()));
    }

    /// The cost of a windowed highlight must track the window, not the file —
    /// the whole reason the whole-file entry point is gone.
    #[test]
    fn should_scan_only_the_window_and_its_lookback() {
        let highlighter = SyntaxHighlighter::default();
        let lines: Vec<String> = (0..5_000).map(|i| format!("let x{i} = {i};")).collect();

        let got = highlighter
            .highlight_ranges(Path::new("big.rs"), &lines, &[4_900..4_905])
            .unwrap();
        assert_eq!(got.spans.len(), 5, "only the window's lines are returned");
        assert_eq!(got.lines_scanned, LOOKBACK + 5);
        assert!(got.spans.contains_key(&4_900) && !got.spans.contains_key(&4_899));
    }

    /// Two windows a short distance apart share one forward walk: running the
    /// gap through is both cheaper than a fresh prime and more accurate.
    #[test]
    fn should_walk_forward_across_a_short_gap_but_reprime_after_a_long_one() {
        let highlighter = SyntaxHighlighter::default();
        let lines: Vec<String> = (0..1_000).map(|i| format!("let x{i} = {i};")).collect();

        let near = highlighter
            .highlight_ranges(Path::new("f.rs"), &lines, &[500..502, 510..512])
            .unwrap();
        // One prime, then straight through: 64 + 2 + gap(8) + 2.
        assert_eq!(near.lines_scanned, LOOKBACK + 2 + 8 + 2);

        let far = highlighter
            .highlight_ranges(Path::new("f.rs"), &lines, &[100..102, 900..902])
            .unwrap();
        assert_eq!(
            far.lines_scanned,
            2 * (LOOKBACK + 2),
            "a long jump reprimes"
        );
    }

    /// Why the lookback exists: a window opening inside a multi-line string
    /// would otherwise be coloured as if it were code.
    #[test]
    fn should_prime_parse_state_so_a_window_inside_a_string_stays_a_string() {
        let highlighter = SyntaxHighlighter::default();
        let mut lines = vec!["let s = r#\"".to_string()];
        lines.extend((0..10).map(|i| format!("still inside the string {i}")));
        lines.push("\"#;".to_string());

        let inside = |from: usize| -> Vec<(Style, String)> {
            highlighter
                .highlight_ranges(Path::new("s.rs"), &lines, &[from..from + 1])
                .unwrap()
                .spans
                .remove(&from)
                .unwrap()
        };
        // Line 5 is inside the raw string; primed from the top, syntect knows.
        assert_eq!(
            inside(5).iter().map(|(s, _)| s.fg).collect::<Vec<_>>(),
            inside(6).iter().map(|(s, _)| s.fg).collect::<Vec<_>>(),
            "consecutive lines inside one string must colour alike"
        );
    }

    #[test]
    fn should_find_syntax_for_typescript() {
        let highlighter = SyntaxHighlighter::default();
        for ext in &["ts", "tsx", "mts", "cts", "jsx", "mjs", "cjs"] {
            let path = format!("file.{ext}");
            assert!(
                highlighter.get_syntax(Path::new(&path)).is_some(),
                "should find syntax for .{ext}"
            );
        }
    }

    #[test]
    fn should_find_syntax_for_fallback_extensions() {
        let highlighter = SyntaxHighlighter::default();
        let extensions = [
            "jsx", "mjs", "cjs", "hbs", "mustache", "ejs", "pug", "njk", "mdx", "jsonc", "json5",
            "prisma", "heex",
        ];
        for ext in &extensions {
            let path = format!("file.{ext}");
            assert!(
                highlighter.get_syntax(Path::new(&path)).is_some(),
                "should find syntax for .{ext}"
            );
        }
    }

    #[test]
    fn should_find_syntax_for_fallback_filenames() {
        let highlighter = SyntaxHighlighter::default();
        for name in &["Containerfile", "Justfile", "justfile"] {
            assert!(
                highlighter.get_syntax(Path::new(name)).is_some(),
                "should find syntax for {name}"
            );
        }
    }

    #[test]
    fn highlighted_spans_should_have_color() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "fn main() {".to_string(),
            "    let x = 42;".to_string(),
            "}".to_string(),
        ];
        let highlighted = all_lines(&highlighter, "test.rs", &lines).unwrap();
        for (i, line) in highlighted.iter().enumerate() {
            let spans = line
                .as_ref()
                .unwrap_or_else(|| panic!("line {i} should be Some"));
            assert!(!spans.is_empty(), "line {i} should have spans");
            // At least one span should have a non-default foreground color
            let has_fg = spans.iter().any(|(style, _)| style.fg.is_some());
            assert!(has_fg, "line {i} should have foreground color: {spans:?}");
        }
    }

    #[test]
    fn should_translate_base16_placeholder_colors_to_ansi_palette() {
        let style = SyntaxHighlighter::syntect_to_ratatui_style(syntect::highlighting::Style {
            foreground: syntect::highlighting::Color {
                r: 7,
                g: 0,
                b: 0,
                a: 0,
            },
            background: syntect::highlighting::Color::BLACK,
            font_style: syntect::highlighting::FontStyle::empty(),
        });
        assert_eq!(style.fg, Some(Color::Gray));

        let bright = SyntaxHighlighter::syntect_to_ratatui_style(syntect::highlighting::Style {
            foreground: syntect::highlighting::Color {
                r: 12,
                g: 0,
                b: 0,
                a: 0,
            },
            background: syntect::highlighting::Color::BLACK,
            font_style: syntect::highlighting::FontStyle::empty(),
        });
        assert_eq!(bright.fg, Some(Color::LightBlue));
    }

    #[test]
    fn should_detect_syntax_from_shebang_when_extensionless() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "#!/usr/bin/env python".to_string(),
            "print('hello')".to_string(),
        ];

        let highlighted = all_lines(&highlighter, "script", &lines);
        assert!(highlighted.is_some());
        assert_eq!(highlighted.unwrap().len(), lines.len());
    }

    #[test]
    fn should_preserve_empty_line_highlight_results() {
        let highlighter = SyntaxHighlighter::default();
        let lines = vec!["let value = 1;".to_string(), String::new()];
        let highlighted = all_lines(&highlighter, "e.rs", &lines).unwrap();
        assert!(
            matches!(&highlighted[1], Some(spans) if spans.iter().all(|(_, t)| t.trim().is_empty())),
            "an empty line is highlighted, just to nothing: {:?}",
            highlighted[1]
        );
    }

    #[test]
    fn should_not_use_weak_fallback_mappings() {
        for ext in &["toml", "hcl", "tf", "tfvars", "nix", "swift", "zig", "v"] {
            assert_eq!(SyntaxHighlighter::fallback_extension(ext), None);
        }
    }

    #[test]
    fn split_diff_lines_for_highlighting_should_build_old_and_new_sequences() {
        let contents = vec![
            "ctx".to_string(),
            "del".to_string(),
            "add".to_string(),
            "ctx2".to_string(),
        ];
        let origins = vec![
            LineOrigin::Context,
            LineOrigin::Deletion,
            LineOrigin::Addition,
            LineOrigin::Context,
        ];

        let seq = SyntaxHighlighter::split_diff_lines_for_highlighting(&contents, &origins);
        assert_eq!(seq.old_lines, vec!["ctx", "del", "ctx2"]);
        assert_eq!(seq.new_lines, vec!["ctx", "add", "ctx2"]);
        assert_eq!(seq.old_line_indices, vec![Some(0), Some(1), None, Some(2)]);
        assert_eq!(seq.new_line_indices, vec![Some(0), None, Some(1), Some(2)]);
    }

    #[test]
    fn highlighted_line_for_diff_with_background_should_handle_none_per_line() {
        let highlighter = SyntaxHighlighter::default();
        let old_lines = vec![None];
        let new_lines = vec![None];
        let highlighted = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Addition,
        );
        assert!(highlighted.is_none());
    }

    #[test]
    fn highlighted_line_for_diff_with_background_should_apply_background_on_success() {
        let highlighter = SyntaxHighlighter::default();
        let old_lines = vec![Some(vec![(Style::default(), "old".to_string())])];
        let new_lines = vec![Some(vec![(Style::default(), "new".to_string())])];

        let deletion = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Deletion,
        );
        let addition = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Addition,
        );
        let context = highlighter.highlighted_line_for_diff_with_background(
            Some(&old_lines),
            Some(&new_lines),
            Some(0),
            Some(0),
            LineOrigin::Context,
        );

        let deletion = deletion.unwrap();
        assert_eq!(deletion.len(), 1);
        assert_eq!(deletion[0].0.bg, Some(highlighter.del_bg));
        assert_eq!(deletion[0].1, "old");

        let addition = addition.unwrap();
        assert_eq!(addition.len(), 1);
        assert_eq!(addition[0].0.bg, Some(highlighter.add_bg));
        assert_eq!(addition[0].1, "new");

        let context = context.unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].0.bg, None);
        assert_eq!(context[0].1, "new");
    }

    #[test]
    fn should_not_include_trailing_newline_in_highlighted_spans() {
        // given - syntect requires a trailing \n for highlight_line, but the
        // resulting spans must not include it. A leaked \n occupies an extra
        // buffer cell in ratatui, misaligning side-by-side diff columns on
        // short (padded) lines while truncated lines stay correct.
        let highlighter = SyntaxHighlighter::default();
        let lines = vec![
            "fn main() {".to_string(),
            "    let x = 42;".to_string(),
            "}".to_string(),
        ];

        // when
        let highlighted = all_lines(&highlighter, "test.rs", &lines).unwrap();

        // then
        for (i, line) in highlighted.iter().enumerate() {
            let spans = line.as_ref().unwrap();
            let full_text: String = spans.iter().map(|(_, t)| t.as_str()).collect();
            assert!(
                !full_text.contains('\n'),
                "line {i} spans should not contain newline, got: {full_text:?}"
            );
        }
    }
}
