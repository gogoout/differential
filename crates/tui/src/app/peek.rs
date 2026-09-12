//! `z` on a code row: light the symbol, and show what declares it.
//!
//! The engine records which token on which line resolves to which declaration
//! (ADR 0032). This is the half that reads it.
//!
//! Three things decide what a press does, and all three are model state:
//!
//! - **Which symbols the row has.** Only the ones the change can resolve, in
//!   column order, so stepping reads left to right the way the line does.
//! - **Where the token is.** The engine's columns are byte offsets into the RAW
//!   line; the pane draws the line with its tabs expanded. Translating between
//!   the two is this module's job and nobody else's.
//! - **What the declaration says.** Read and highlighted HERE, when the key is
//!   pressed, because drawing is a pure function of the model and reading a
//!   blob is not.
//!
//! A row with nothing to resolve is not this feature's row: the key falls
//! through to what `z` already did there.

use differential_engine::schema;

use super::{App, Peek};
use crate::rows::TAB_WIDTH;
use crate::vendor::diff_types::expand_tabs;

/// How many lines of a declaration are worth reading before the pane is asked
/// to hold them. The float caps again against the space it actually has; this
/// only bounds what is read and kept.
const MOST_LINES: usize = 60;

impl App {
    /// The uses on the cursor's row that resolve to a declaration, left to
    /// right.
    ///
    /// Empty when the row has no new-side line — a pure deletion, a hunk
    /// header, a boundary — because the index is keyed by new-side line and a
    /// row without one cannot be asked.
    pub(super) fn peekable(&self) -> Vec<&schema::SymbolUse> {
        let Some(index) = self.session.doc().symbols.as_ref() else {
            return Vec::new();
        };
        let Some((path, line)) = self.new_side_of(self.cursor) else {
            return Vec::new();
        };
        let mut found: Vec<&schema::SymbolUse> = index
            .uses
            .iter()
            .filter(|u| u.line == line && u.file == path)
            .collect();
        found.sort_by_key(|u| u.start);
        found
    }

    /// The row's path and new-side line, where it has one.
    ///
    /// A split row shows both sides at once, so the new-side number may be the
    /// row's `other` rather than its own.
    fn new_side_of(&self, row: usize) -> Option<(String, u32)> {
        let r = self.rows.get(row)?;
        let l = r.line.as_ref()?;
        let line = if l.side == "new" {
            l.line
        } else {
            l.other.filter(|(s, _)| *s == "new").map(|(_, n)| n)?
        };
        Some((self.file_path_above(row)?.to_string(), line))
    }

    /// Advance the float: open it, step to the next symbol, or close it.
    ///
    /// **After the last symbol it closes**, rather than wrapping to the first.
    /// Stepping is how the reader asks "and what else is on this line"; a wrap
    /// answers a question they have already had answered and gives them no way
    /// out through the key they are already pressing.
    pub(super) fn step_peek(&mut self) {
        let next = match &self.peek {
            Some(p) if p.row == self.cursor => p.nth + 1,
            _ => 0,
        };
        let count = self.peekable().len();
        if next >= count {
            self.peek = None;
            return;
        }
        self.peek = self.build_peek(next);
        if self.peek.is_none() {
            self.status = "nothing to show for that symbol".to_string();
        }
    }

    /// Resolve symbol `nth` on the cursor's row into a drawable float.
    fn build_peek(&mut self, nth: usize) -> Option<Peek> {
        let index = self.session.doc().symbols.as_ref()?;
        let use_at = self.peekable().get(nth).copied()?;
        let (on, start, end) = (use_at.on.clone(), use_at.start, use_at.end);
        let def = index.definitions.iter().find(|d| d.id == on)?;
        let (name, file, line, through, class) = (
            def.name.clone(),
            def.file.clone(),
            def.line,
            def.through,
            def.class.clone(),
        );

        // The group the declaring class ended up in. The reader's next move is
        // often "go and read that group first", and the id is what the plan
        // pane's rows and their `after:` lines are keyed by.
        let group = self
            .session
            .doc()
            .groups
            .as_ref()
            .and_then(|gs| gs.iter().find(|g| g.class_ids.contains(&class)))
            .map(|g| g.id.clone());
        let title = match group {
            Some(g) => format!("{name} · {file}:{line} · {class} · {g}"),
            None => format!("{name} · {file}:{line} · {class}"),
        };

        let (body, more) = self
            .factory
            .declaration(&self.theme, &file, line, through, MOST_LINES);
        if body.is_empty() {
            return None;
        }

        let (row_path, row_line) = self.new_side_of(self.cursor)?;
        let raw = self.factory.raw_head_line(&row_path, row_line);
        let at = drawn_columns(raw.as_deref(), start, end);

        Some(Peek {
            row: self.cursor,
            nth,
            at,
            title,
            body,
            more,
        })
    }
}

/// Turn raw-line byte offsets into offsets in the text the pane draws.
///
/// The pane expands tabs before drawing (`rows::source_lines`), so a raw offset
/// indexes a different string from the one on screen. Expanding the PREFIX is
/// exact and needs no table: whatever `expand_tabs` did to the bytes before the
/// token is exactly how far the token moved.
///
/// Without the raw line there is nothing to translate against, so the offsets
/// pass through — right for a line with no tabs, which is nearly all of them,
/// and the only answer available for the rest.
fn drawn_columns(raw: Option<&str>, start: u32, end: u32) -> (usize, usize) {
    let (s, e) = (start as usize, end as usize);
    let Some(raw) = raw else { return (s, e) };
    let at = |byte: usize| match raw.get(..byte) {
        Some(prefix) => expand_tabs(prefix, TAB_WIDTH).len(),
        // A byte offset that is not a character boundary, or runs past the
        // line. The reader moved on or the blob did; a highlight nobody asked
        // for is worse than one that lands where the engine said.
        None => byte,
    };
    (at(s), at(e))
}

#[cfg(test)]
mod tests {
    use super::drawn_columns;

    #[test]
    fn a_line_without_tabs_needs_no_translation() {
        let raw = "export function lookUpName() {";
        assert_eq!(drawn_columns(Some(raw), 16, 26), (16, 26));
    }

    #[test]
    fn a_tab_moves_the_token_by_what_it_draws_as() {
        // One tab, then `plain()`. Raw byte 1; drawn at column 4, because
        // `TAB_WIDTH` is 4 and the tab stands at column 0.
        let raw = "\tplain()";
        assert_eq!(drawn_columns(Some(raw), 1, 6), (4, 9));
    }

    #[test]
    fn two_tabs_and_a_word_still_land_on_the_word() {
        let raw = "\t\tw.Meth()";
        let (start, end) = drawn_columns(Some(raw), 4, 8);
        let drawn = crate::vendor::diff_types::expand_tabs(raw, crate::rows::TAB_WIDTH);
        assert_eq!(&drawn[start..end], "Meth");
    }

    #[test]
    fn no_raw_line_passes_the_offsets_through() {
        assert_eq!(drawn_columns(None, 3, 9), (3, 9));
    }
}
