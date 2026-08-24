// Adapted from jnsahaj/lumen (f600389), src/command/diff/types.rs (subset).
// MIT License — Copyright (c) 2024 Sahaj Jain. See LICENSE-MIT.

pub fn expand_tabs(s: &str, tab_width: usize) -> String {
    if tab_width == 0 {
        return s.replace('\t', "");
    }
    let mut result = String::with_capacity(s.len());
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let spaces = tab_width - (col % tab_width);
            for _ in 0..spaces {
                result.push(' ');
            }
            col += spaces;
        } else {
            result.push(c);
            col += 1;
        }
    }
    result
}

/// Represents a segment of text with optional emphasis for word-level highlighting
#[derive(Clone, Debug)]
pub struct InlineSegment {
    pub text: String,
    /// If true, this segment represents a changed word that should be emphasized
    pub emphasized: bool,
}

pub struct DiffLine {
    pub old_line: Option<(usize, String)>,
    pub new_line: Option<(usize, String)>,
    pub change_type: ChangeType,
    /// Word-level segments for the old line (only populated for Modified lines)
    pub old_segments: Option<Vec<InlineSegment>>,
    /// Word-level segments for the new line (only populated for Modified lines)
    pub new_segments: Option<Vec<InlineSegment>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ChangeType {
    Equal,
    Delete,
    Insert,
    /// A paired delete+insert, shown on the same row (GitHub-style)
    Modified,
}
