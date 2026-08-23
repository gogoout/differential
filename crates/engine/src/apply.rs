//! Byte-exact hunk applier.
//!
//! CRLF asymmetry, by design: this module is byte-faithful — `\r` stays attached
//! to line content and round-trips exactly. Only the shape normaliser
//! (`shape.rs`) is CRLF-agnostic. The two modules disagree on purpose.

use crate::model::Hunk;

/// Apply a subset of one file's hunks to its base content. Hunks are disjoint
/// under `-U0`.
///
/// An absent file's base is ONE EMPTY LINE, not zero lines: `b"".split(b'\n')`
/// yields `[b""]`, and that trailing empty element is what encodes "ends with a
/// newline". Dropping it loses the final byte of every created file.
pub fn apply_hunks(base: Option<&[u8]>, hunks: &[&Hunk]) -> Vec<u8> {
    let lines: Vec<&[u8]> = match base {
        Some(b) => b.split(|&c| c == b'\n').collect(),
        None => vec![b"".as_slice()],
    };

    let mut order: Vec<&Hunk> = hunks.to_vec();
    order.sort_by_key(|h| (h.old_start, h.old_count));

    let mut out: Vec<&[u8]> = Vec::with_capacity(lines.len());
    let mut pos = 0usize;
    // Set when the applied hunk set includes the EOF-covering hunk (only that
    // hunk can carry a no-newline marker); the value is the NEW side's state.
    let mut eof_nonl_new: Option<bool> = None;
    for h in order {
        // old_count == 0 means "insert after line old_start"; otherwise the hunk
        // replaces starting at old_start (1-based).
        let start = if h.old_count == 0 {
            h.old_start as usize
        } else {
            (h.old_start as usize) - 1
        };
        out.extend_from_slice(&lines[pos..start]);
        out.extend(h.added.iter().map(Vec::as_slice));
        pos = start + h.old_count as usize;
        if h.nonl_old || h.nonl_new {
            eof_nonl_new = Some(h.nonl_new);
        }
    }
    out.extend_from_slice(&lines[pos..]);

    match eof_nonl_new {
        // New content ends without a newline: drop the empty tail element so
        // the join does not reintroduce one.
        Some(true) => {
            if out.last().is_some_and(|l| l.is_empty()) {
                out.pop();
            }
        }
        // The edit ADDED the final newline (old side had the marker, new side
        // does not): the base's line list has no empty tail element, so the
        // join needs one appended to produce it.
        Some(false) => out.push(b""),
        None => {}
    }
    out.join(b"\n".as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(os: u32, oc: u32, ns: u32, nc: u32, add: &[&[u8]], nonl_new: bool) -> Hunk {
        Hunk {
            file: 0,
            old_start: os,
            old_count: oc,
            new_start: ns,
            new_count: nc,
            removed: vec![],
            added: add.iter().map(|l| l.to_vec()).collect(),
            nonl_old: false,
            nonl_new,
        }
    }

    #[test]
    fn created_file_with_trailing_newline() {
        let h = hunk(0, 0, 1, 2, &[b"a", b"b"], false);
        assert_eq!(apply_hunks(None, &[&h]), b"a\nb\n");
    }

    #[test]
    fn created_file_without_trailing_newline() {
        let h = hunk(0, 0, 1, 2, &[b"a", b"b"], true);
        assert_eq!(apply_hunks(None, &[&h]), b"a\nb");
    }

    #[test]
    fn replace_middle_line() {
        let h = hunk(2, 1, 2, 1, &[b"B"], false);
        assert_eq!(apply_hunks(Some(b"a\nb\nc\n"), &[&h]), b"a\nB\nc\n");
    }

    #[test]
    fn insert_at_top_of_file() {
        // @@ -0,0 +1,1 @@ — insertion before line 1.
        let h = hunk(0, 0, 1, 1, &[b"first"], false);
        assert_eq!(apply_hunks(Some(b"a\n"), &[&h]), b"first\na\n");
    }

    #[test]
    fn delete_to_empty_file() {
        let h = hunk(1, 2, 0, 0, &[], false);
        assert_eq!(apply_hunks(Some(b"a\nb\n"), &[&h]), b"");
    }

    #[test]
    fn whole_file_delete_of_nonl_file() {
        // Base has no trailing newline; deleting everything must still be b"".
        // Real git marks the old side: `-only` + `\ No newline at end of file`.
        let mut h = hunk(1, 1, 0, 0, &[], false);
        h.nonl_old = true;
        assert_eq!(apply_hunks(Some(b"only"), &[&h]), b"");
    }

    #[test]
    fn delete_final_nonl_line_restores_trailing_newline() {
        // Base "a\nlast" (no final newline); deleting `last` leaves "a\n".
        let mut h = hunk(2, 1, 1, 0, &[], false);
        h.nonl_old = true;
        assert_eq!(apply_hunks(Some(b"a\nlast"), &[&h]), b"a\n");
    }

    #[test]
    fn newline_removed_from_final_line() {
        // -last / +last with nonl_new: content identical, exactly 1 byte shorter.
        let mut h = hunk(2, 1, 2, 1, &[b"last"], true);
        h.removed = vec![b"last".to_vec()];
        assert_eq!(apply_hunks(Some(b"a\nlast\n"), &[&h]), b"a\nlast");
    }

    #[test]
    fn newline_added_to_final_line() {
        // `-last` + marker, `+last` without: the edit adds exactly one byte.
        let mut h = hunk(2, 1, 2, 1, &[b"last"], false);
        h.nonl_old = true;
        assert_eq!(apply_hunks(Some(b"a\nlast"), &[&h]), b"a\nlast\n");
    }

    #[test]
    fn multiple_disjoint_hunks_apply_in_order_regardless_of_input_order() {
        let h1 = hunk(1, 1, 1, 1, &[b"A"], false);
        let h2 = hunk(3, 1, 3, 1, &[b"C"], false);
        let base = b"a\nb\nc\n";
        assert_eq!(apply_hunks(Some(base), &[&h2, &h1]), b"A\nb\nC\n");
    }

    #[test]
    fn crlf_is_byte_faithful() {
        let h = hunk(1, 1, 1, 1, &[b"new\r"], false);
        assert_eq!(
            apply_hunks(Some(b"old\r\nkeep\r\n"), &[&h]),
            b"new\r\nkeep\r\n"
        );
    }

    #[test]
    fn typechange_delete_plus_create_on_same_base() {
        let del = hunk(1, 2, 0, 0, &[], false);
        let mut create = hunk(0, 0, 1, 1, &[b"target"], true);
        create.nonl_new = true;
        assert_eq!(
            apply_hunks(Some(b"real\ncontent\n"), &[&del, &create]),
            b"target"
        );
    }
}
