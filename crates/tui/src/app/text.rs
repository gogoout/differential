//! Measuring and cutting text to a column budget.
//!
//! A leaf module: it knows nothing about `App`. Both `keys` and `draw` read
//! from it, which is the point — a modal's scroll height and its drawn height
//! come from one function, and they used to be two different numbers.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::*;

/// The two count columns, and the width every path therefore starts after.
///
/// Each column is as wide as the widest number IN IT, so the paths line up
/// down the list. Four digits is the floor, which is where both used to be
/// fixed — one file over 9999 lines then pushed its OWN path right while every
/// other row's stayed put, and the column the eye scans stopped being one.
pub(super) fn counts_columns(entries: &[FileListEntry]) -> (usize, usize, usize) {
    let widest = |f: fn(&FileListEntry) -> usize| {
        entries
            .iter()
            .map(|e| f(e).to_string().len())
            .max()
            .unwrap_or(0)
            .max(4)
    };
    let add_w = widest(|e| e.adds);
    let del_w = widest(|e| e.dels);
    // The mark and its space, then `+adds`, then `−dels` and one space.
    (add_w, del_w, 2 + (1 + add_w) + (1 + del_w + 1))
}

/// Cut a location down to `max` columns from its HEAD, not its tail.
///
/// `a/b/c/deeply/nested/module.rs:13` becomes `…/module.rs:13`. The file name
/// and the line number are what identify a finding; the leading directories are
/// not, and cutting the tail throws away the only part worth reading.
///
/// Hand-written rather than reached for. The vendored `truncate_or_pad_spans`
/// next door cuts the tail, and WHICH END to keep is a policy no crate can hold
/// for us.
pub(super) fn elide_head(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".repeat(max);
    }
    // Prefer a cut at a separator: the widest run of WHOLE segments that fits
    // behind `…/`. Separators come left to right, so the first suffix that fits
    // is the widest one. A cut inside a directory name reads as a typo.
    for (i, _) in s.match_indices('/') {
        let seg = &s[i + 1..];
        if UnicodeWidthStr::width(seg) + 2 <= max {
            return format!("…/{seg}");
        }
    }
    // Not even the last segment fits, so cut the name itself. One column goes
    // to the ellipsis; the rest buys tail characters.
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        used += w;
        kept.push(c);
    }
    let tail: String = kept.into_iter().rev().collect();
    format!("…{tail}")
}

/// Cut a note down to `max` columns from its tail, with an ellipsis.
///
/// The pair to `elide_head`, and by display width for the same reason: the note
/// used to be cut by char count while the column beside it was measured by
/// width, so one wide character put the row over the border.
pub(super) fn truncate_width(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Extend `line` with blank, styled cells so a selection background covers
/// the full row width. Trailing padding only — the leading connector column
/// keeps its own styling.
pub(super) fn pad_to_width(line: &mut Line<'static>, width: usize, bg: ratatui::style::Color) {
    let used: usize = line
        .spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    if used < width {
        line.spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(bg),
        ));
    }
}

/// How many rows a modal list actually shows, from its content and the body's
/// height.
///
/// One function per modal, called by BOTH the key handler and the render, so
/// the height a list scrolls against is the height it is drawn at. They used to
/// be two different numbers — `viewport.detail_rows` for the scroll, the box's
/// own inner height for the draw — and a long findings list scrolled against a
/// window it was never drawn in.
pub(super) fn findings_rows(entries: usize, ruled: bool, body_rows: usize) -> usize {
    // The box is the list plus the orphan rule, a border pair, a title row and
    // the key footer; the paragraph then gets everything but the border pair
    // and that footer.
    (entries + usize::from(ruled) + 4)
        .min(body_rows)
        .saturating_sub(3)
}

/// The same, for the file list — a plain bordered box with no footer row.
pub(super) fn file_list_rows(entries: usize, body_rows: usize) -> usize {
    (entries + 2).min(body_rows).saturating_sub(2)
}

/// Keep `selected` inside a window `height` tall, moving `scroll` as little as
/// it takes. The diff pane's own `follow_cursor` keeps a margin; a list this
/// short does not need one.
pub(super) fn follow(selected: usize, scroll: usize, height: usize) -> usize {
    let height = height.max(1);
    if selected < scroll {
        selected
    } else if selected >= scroll + height {
        selected + 1 - height
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub(super) fn a_location_is_cut_at_its_head_and_snaps_to_a_separator() {
        // Fits: untouched.
        assert_eq!(elide_head("src/a.rs:13", 20), "src/a.rs:13");
        assert_eq!(elide_head("src/a.rs:13", 11), "src/a.rs:13");

        // Too long: the name and the number survive, the directories go, and
        // the cut lands between segments rather than inside one.
        let deep = "src/one/two/three/four/module.rs:13";
        let out = elide_head(deep, 24);
        assert_eq!(out, "…/four/module.rs:13");
        assert!(UnicodeWidthStr::width(out.as_str()) <= 24);
        // A wider budget buys another whole segment, never half of one.
        assert_eq!(elide_head(deep, 25), "…/three/four/module.rs:13");

        // No separator in reach: cut where the budget runs out. The END of
        // the name is what survives, which is the guarantee a file list needs
        // — a path is identified by its tail, never by its head.
        assert_eq!(elide_head("averylongsinglename.rs", 8), "…name.rs");
        for max in 2..40 {
            let out = elide_head("a/b/c/verylongmodulename.rs", max);
            assert!(
                "a/b/c/verylongmodulename.rs".ends_with(out.trim_start_matches(['…', '/'])),
                "at {max} columns the result must be a suffix: {out:?}"
            );
            assert!(
                UnicodeWidthStr::width(out.as_str()) <= max,
                "over budget at {max}"
            );
        }

        // Degenerate budgets never panic.
        assert_eq!(elide_head("src/a.rs", 1), "…");
        assert_eq!(elide_head("src/a.rs", 0), "");
    }

    /// One file over 9999 lines must widen the column for EVERY row, or the
    /// paths stop lining up and the list stops being scannable.
    #[test]
    pub(super) fn a_wide_count_widens_the_column_for_every_row() {
        let entry = |adds, dels| FileListEntry {
            path: "src/f.rs".into(),
            row_idx: 0,
            adds,
            dels,
            reviewed: false,
        };
        // Small counts still get the four-digit floor, so the common case is
        // unchanged.
        let (a, d, lead) = counts_columns(&[entry(3, 1), entry(12, 40)]);
        assert_eq!((a, d), (4, 4));
        assert_eq!(lead, 2 + 5 + 6);

        // A five-digit file widens the ADD column only, and for every row.
        let (a, d, lead) = counts_columns(&[entry(12_000, 1), entry(3, 2)]);
        assert_eq!((a, d), (5, 4));
        assert_eq!(lead, 2 + 6 + 6);

        // An empty list still yields a usable lead rather than zero.
        let (a, d, lead) = counts_columns(&[]);
        assert_eq!((a, d), (4, 4));
        assert_eq!(lead, 2 + 5 + 6);
    }

    #[test]
    pub(super) fn a_note_is_cut_by_display_width_not_by_characters() {
        assert_eq!(truncate_width("short", 10), "short");
        assert_eq!(truncate_width("abcdefgh", 4), "abc…");
        // Two columns each: three of them fit a five-column budget, not four.
        assert_eq!(truncate_width("ありがとう", 5), "あり…");
        assert_eq!(truncate_width("abc", 0), "");
    }

    /// The height a modal list scrolls against has to be the height it is
    /// drawn at. They were two different numbers.
    #[test]
    pub(super) fn a_modal_scrolls_against_the_window_it_is_drawn_in() {
        // Room to spare: every entry shows, so nothing scrolls.
        assert_eq!(findings_rows(3, false, 40), 4);
        assert_eq!(file_list_rows(3, 40), 3);
        // Capped by the body: the box stops growing and the window is what is
        // left inside its chrome.
        assert_eq!(findings_rows(100, false, 20), 17);
        assert_eq!(file_list_rows(100, 20), 18);
        // A body too short for any chrome must not underflow.
        assert_eq!(findings_rows(100, true, 2), 0);
        assert_eq!(file_list_rows(100, 1), 0);
    }
}
