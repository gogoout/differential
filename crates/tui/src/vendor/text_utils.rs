// Adapted from agavra/tuicr (0dacb6b), src/ui/text_utils.rs — the span
// wrapping and search-highlight helpers removed (this crate never called
// them), leaving the truncation/padding pair it does use.
// MIT License — Copyright (c) 2025 tuicr contributors. See LICENSE-MIT.
//
// `wrap_pairs` and `slice_pairs` below are OURS, not vendored: soft wrap needs
// them, and they live here because this is where a row's pairs are measured
// and cut.
use ratatui::{style::Style, text::Span};
use textwrap::WordSeparator;
use textwrap::core::Word;
use textwrap::wrap_algorithms::wrap_first_fit;
use unicode_width::UnicodeWidthStr;

/// Truncate or pad highlighted spans to a specific display width
/// Uses unicode width to properly handle wide characters (CJK, emoji, etc.)
/// Returns a vector of spans that fits exactly within the width
pub fn truncate_or_pad_spans(
    spans: &[(Style, String)],
    width: usize,
    base_style: Style,
) -> Vec<Span<'static>> {
    // Count total display width
    let total_width: usize = spans.iter().map(|(_, text)| text.width()).sum();

    if total_width > width {
        // Need to truncate
        let mut result = Vec::new();
        let mut remaining = width.saturating_sub(3); // Reserve space for "..."

        for (style, text) in spans {
            if remaining == 0 {
                break;
            }

            let text_width = text.width();
            if text_width <= remaining {
                result.push(Span::styled(text.clone(), *style));
                remaining -= text_width;
            } else {
                // Truncate this span character by character to fit remaining width
                let mut truncated = String::new();
                let mut current_width = 0;
                for c in text.chars() {
                    let char_width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                    if current_width + char_width > remaining {
                        break;
                    }
                    truncated.push(c);
                    current_width += char_width;
                }
                if !truncated.is_empty() {
                    result.push(Span::styled(truncated, *style));
                }
                remaining = 0;
            }
        }

        // Add ellipsis
        result.push(Span::styled("...".to_string(), base_style));
        result
    } else if total_width < width {
        // Need to pad
        let mut result: Vec<Span> = spans
            .iter()
            .map(|(style, text)| Span::styled(text.clone(), *style))
            .collect();

        // Add padding
        let padding = " ".repeat(width - total_width);
        result.push(Span::styled(padding, base_style));
        result
    } else {
        // Perfect fit
        spans
            .iter()
            .map(|(style, text)| Span::styled(text.clone(), *style))
            .collect()
    }
}

/// Break styled pairs into the screen lines they occupy at `width`.
///
/// Always returns at least one line, and returns exactly one when the pairs
/// already fit — so a caller that wraps and one that does not have the same
/// shape.
///
/// Word boundaries, display width and breaking an over-long token are
/// `textwrap`'s job, not ours (working rule 5). It answers in words over the
/// row's plain text; this cuts the STYLED pairs at the byte offsets those
/// words fall on, so a break carries every style across untouched.
pub fn wrap_pairs(pairs: &[(Style, String)], width: usize) -> Vec<Vec<(Style, String)>> {
    let plain: String = pairs.iter().map(|(_, t)| t.as_str()).collect();
    if width == 0 || plain.width() <= width {
        return vec![pairs.to_vec()];
    }
    let words: Vec<Word> = WordSeparator::new()
        .find_words(&plain)
        // A token wider than the pane has no break point of its own, and a
        // line the reader cannot see the end of is the bug being fixed. Only
        // such a token, though: `break_apart` returns NOTHING for a word that
        // is pure whitespace, and a line's indent is exactly that word.
        .flat_map(|w| {
            if w.word.width() > width {
                w.break_apart(width).collect::<Vec<_>>()
            } else {
                vec![w]
            }
        })
        .collect();

    let mut cut = Vec::new();
    let mut at = 0;
    for line in wrap_first_fit(&words, &[width as f64]) {
        let start = at;
        for w in line {
            at += w.word.len() + w.whitespace.len();
        }
        // The whitespace a line breaks ON is not drawn: it would pad the row
        // with spaces that carry the line's own background past its last
        // character.
        let end = at - line.last().map_or(0, |w| w.whitespace.len());
        cut.push(slice_pairs(pairs, start, end));
    }
    if cut.is_empty() {
        cut.push(pairs.to_vec());
    }
    cut
}

/// The pairs covering `start..end` of the concatenated text, styles kept.
pub fn slice_pairs(pairs: &[(Style, String)], start: usize, end: usize) -> Vec<(Style, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (style, text) in pairs {
        let span = at..at + text.len();
        at = span.end;
        let lo = span.start.max(start);
        let hi = span.end.min(end);
        if lo < hi {
            out.push((*style, text[lo - span.start..hi - span.start].to_string()));
        }
    }
    out
}

pub fn split_pairs_at_ranges(
    pairs: &[(Style, String)],
    ranges: Vec<(usize, usize)>,
    highlight: Style,
) -> Vec<(Style, String)> {
    let mut out: Vec<(Style, String)> = Vec::new();
    let mut ranges = ranges.into_iter().peekable();
    let mut span_start = 0;
    for (style, text) in pairs {
        if text.is_empty() {
            out.push((*style, String::new()));
            continue;
        }
        let span_end = span_start + text.len();
        let mut cursor = span_start;
        while cursor < span_end {
            while ranges.peek().is_some_and(|&(_, end)| end <= cursor) {
                ranges.next();
            }
            match ranges.peek().copied() {
                Some((start, end)) if start < span_end => {
                    if start > cursor {
                        out.push((
                            *style,
                            text[cursor - span_start..start - span_start].to_string(),
                        ));
                        cursor = start;
                    }
                    let segment_end = end.min(span_end);
                    out.push((
                        style.patch(highlight),
                        text[cursor - span_start..segment_end - span_start].to_string(),
                    ));
                    cursor = segment_end;
                }
                _ => {
                    out.push((*style, text[cursor - span_start..].to_string()));
                    cursor = span_end;
                }
            }
        }
        span_start = span_end;
    }
    out
}

// A slice holding one `Range` is exactly what `highlight_ranges` takes; the
// lint is about `vec![0..n]` written where a list of numbers was meant.
#[allow(clippy::single_range_in_vec_init)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_pad_highlighted_spans_to_exact_width() {
        // given - highlighted spans from the syntax highlighter (which strips
        // the trailing \n that syntect includes). Short content gets padded
        // by truncate_or_pad_spans; the result must have exactly `width`
        // characters so the side-by-side separator stays aligned.
        let highlighter = crate::vendor::syntax::SyntaxHighlighter::default();
        let lines = vec!["let x = 1;".to_string()];
        let highlighted = highlighter
            .highlight_ranges(std::path::Path::new("test.rs"), &lines, &[0..1])
            .unwrap();
        let spans = &highlighted.spans[&0];

        let width = 80;

        // when
        let result = truncate_or_pad_spans(spans, width, Style::default());

        // then - total char count must equal the target width so each
        // side-by-side column is the same size
        let total_chars: usize = result.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(
            total_chars, width,
            "padded spans should have exactly {width} chars, got {total_chars}"
        );
    }

    fn text(line: &[(Style, String)]) -> String {
        line.iter().map(|(_, t)| t.as_str()).collect()
    }

    #[test]
    fn should_return_one_line_when_the_pairs_already_fit() {
        // given
        let pairs = vec![(Style::default(), "fits".to_string())];

        // when
        let lines = wrap_pairs(&pairs, 10);

        // then
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "fits");
    }

    #[test]
    fn should_break_at_a_word_boundary_and_drop_the_space_it_broke_on() {
        // given
        let pairs = vec![(Style::default(), "one two three".to_string())];

        // when
        let lines = wrap_pairs(&pairs, 7);

        // then
        let out: Vec<String> = lines.iter().map(|l| text(l)).collect();
        assert_eq!(out, vec!["one two", "three"]);
    }

    #[test]
    fn should_carry_every_style_across_a_break() {
        // given - two differently styled runs, broken mid-way
        let red = Style::default().fg(ratatui::style::Color::Red);
        let blue = Style::default().fg(ratatui::style::Color::Blue);
        let pairs = vec![(red, "aaa bbb ".to_string()), (blue, "ccc ddd".to_string())];

        // when
        let lines = wrap_pairs(&pairs, 7);

        // then - the styles survive, and each line keeps only its own runs
        let out: Vec<String> = lines.iter().map(|l| text(l)).collect();
        assert_eq!(out, vec!["aaa bbb", "ccc ddd"]);
        assert_eq!(lines[0][0].0, red);
        assert_eq!(lines[1][0].0, blue);
    }

    #[test]
    fn should_break_a_token_wider_than_the_pane() {
        // given - one word with no break point of its own
        let pairs = vec![(Style::default(), "abcdefghij".to_string())];

        // when
        let lines = wrap_pairs(&pairs, 4);

        // then
        let out: Vec<String> = lines.iter().map(|l| text(l)).collect();
        assert_eq!(out, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn should_keep_leading_indentation_on_the_first_line_only() {
        // given - a code line, indented
        let pairs = vec![(Style::default(), "    let x = compute(a, b);".to_string())];

        // when
        let lines = wrap_pairs(&pairs, 14);

        // then - the indent is text like any other, and never repeats
        let out: Vec<String> = lines.iter().map(|l| text(l)).collect();
        assert_eq!(out[0], "    let x =");
        assert!(!out[1].starts_with(' '), "got {:?}", out[1]);
    }

    #[test]
    fn should_return_one_empty_line_for_no_content() {
        // given
        let pairs: Vec<(Style, String)> = Vec::new();

        // when
        let lines = wrap_pairs(&pairs, 10);

        // then
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "");
    }
}
