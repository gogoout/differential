// Adapted from agavra/tuicr (0dacb6b), src/ui/text_utils.rs — the span
// wrapping and search-highlight helpers removed (this crate never called
// them), leaving the truncation/padding pair it does use.
// MIT License — Copyright (c) 2025 tuicr contributors. See LICENSE-MIT.
use ratatui::{style::Style, text::Span};
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
}
