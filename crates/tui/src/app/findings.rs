//! The mutators that reach the session: reviewed marks and findings.
//!
//! Everything here writes through `ReviewSession`, which persists before it
//! returns — so the model never holds a change the disk does not have.

use differential_engine::review_state::{Finding, Lines};

use crate::rows::{LineRef, RowKind};

use super::*;

impl App {
    /// Space marks reviewed. In the left pane that means the WHOLE selected
    /// entry (a group, or a file's hunks); in the diff pane it means the
    /// hunk under the cursor.
    pub(super) fn toggle_reviewed(&mut self) {
        let outcome = match self.focus {
            Focus::Groups => self.toggle_selected_entry(),
            Focus::Detail => match self.current_hunk() {
                Some(h) => self.session.toggle_reviewed(h).map(|_| ()),
                None => return,
            },
        };
        if let Err(e) = outcome {
            self.status = format!("save failed: {e:#}");
            return;
        }
        self.save_cursor();
        self.rebuild_rows();
    }

    /// Mark every hunk of the selected entry, in one write. Set semantics:
    /// a partly reviewed entry becomes fully reviewed rather than inverted.
    pub(super) fn toggle_selected_entry(&mut self) -> Result<(), differential_engine::EngineError> {
        let keys: Vec<String> = match self.view_mode {
            ViewMode::Groups => match self.groups().get(self.selected_group) {
                Some(g) => g
                    .hunks
                    .iter()
                    .map(|h| self.session.hunk_key(h.index()).to_string())
                    .collect(),
                None => return Ok(()),
            },
            ViewMode::Files => self
                .files_of_tree_row(self.selected_file)
                .iter()
                .flat_map(|i| self.files()[*i].hunks.iter())
                .map(|h| self.session.hunk_key(h.index()).to_string())
                .collect(),
        };
        if keys.is_empty() {
            self.status = "nothing to mark here".into();
            return Ok(());
        }
        let all_done = keys.iter().all(|k| self.session.is_reviewed(k));
        self.session.set_reviewed(&keys, !all_done)?;
        self.status = if all_done {
            "group unmarked".into()
        } else {
            "group marked reviewed".into()
        };
        Ok(())
    }

    /// The rows a note and the lines it annotates occupy, when the cursor is
    /// in one of them.
    ///
    /// A note is drawn under its LAST line, and the only sign that it and the
    /// run above it belong together was that they were adjacent — which, over
    /// a range, they mostly are not.
    pub(super) fn note_cluster(&self) -> Option<(usize, usize)> {
        if self.focus != Focus::Detail {
            return None;
        }
        // A forge thread is a note too: its rows, its reply drafts, and the
        // lines its anchor covers read as one thing.
        if let Some(t) = self.thread_at_cursor() {
            let anchor = t.anchor.as_ref();
            let mut file = "";
            let (mut lo, mut hi) = (usize::MAX, 0usize);
            for (i, row) in self.rows.iter().enumerate() {
                if let RowKind::FileHeader(p) = &row.kind {
                    file = p;
                }
                let mine = match (&row.kind, &row.line) {
                    (RowKind::Thread(tid, _), _) => tid == &t.id,
                    (RowKind::Finding(fid, _), _) => self
                        .session
                        .findings()
                        .iter()
                        .any(|f| &f.id == fid && f.reply_to.as_deref() == Some(t.id.as_str())),
                    (_, Some(l)) => anchor.is_some_and(|a| {
                        file == a.file
                            && (a.line..=a.end_line.max(a.line)).any(|n| l.holds(&a.side, n))
                    }),
                    _ => false,
                };
                if mine {
                    lo = lo.min(i);
                    hi = hi.max(i);
                }
            }
            return (lo <= hi).then_some((lo, hi));
        }
        let f = self.finding_at_cursor()?;
        let (id, path) = (f.id.as_str(), f.anchor.file.as_str());
        let mut file = "";
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        for (i, row) in self.rows.iter().enumerate() {
            if let RowKind::FileHeader(p) = &row.kind {
                file = p;
            }
            let mine = match (&row.kind, &row.line) {
                (RowKind::Finding(fid, _), _) => fid == id,
                (_, Some(l)) => file == path && self.anchor_covers(f, l),
                _ => false,
            };
            if mine {
                lo = lo.min(i);
                hi = hi.max(i);
            }
        }
        if lo > hi {
            return None;
        }
        // A note that hangs off its hunk's header covers no line at all; the
        // row above it is what it points at.
        if lo > 0 && matches!(self.rows[lo].kind, RowKind::Finding(..)) {
            lo -= 1;
        }
        Some((lo, hi))
    }

    /// The note the cursor is standing in.
    ///
    /// "In", not "on": a note over a RANGE covers every line of it, and it is
    /// drawn under the last of them. Standing on the first line of a run is
    /// standing in the note about that run.
    pub(super) fn finding_at_cursor(&self) -> Option<&Finding> {
        let by_id = |id: &str| self.session.findings().iter().find(|f| f.id == id);
        // On the note itself.
        if let Some(RowKind::Finding(id, _)) = self.rows.get(self.cursor).map(|r| &r.kind) {
            return by_id(id);
        }
        // On a line the note covers.
        if let Some(l) = self.rows.get(self.cursor).and_then(|r| r.line.as_ref())
            && let Some(path) = self.file_path_above(self.cursor)
            && let Some(f) = self
                .session
                .findings()
                .iter()
                .find(|f| f.anchor.file == path && self.anchor_covers(f, l))
        {
            return Some(f);
        }
        // Directly above a note that could only be anchored to its hunk, and
        // so hangs off the header rather than off any line.
        if let Some(RowKind::Finding(id, _)) = self.rows.get(self.cursor + 1).map(|r| &r.kind) {
            return by_id(id);
        }
        None
    }

    /// Does this finding's anchor cover the line this row shows?
    pub(super) fn anchor_covers(&self, f: &Finding, l: &LineRef) -> bool {
        (f.anchor.line..=f.anchor.end_line.max(f.anchor.line)).any(|n| l.holds(&f.anchor.side, n))
    }

    /// The path of the file header above `row`.
    pub(super) fn file_path_above(&self, row: usize) -> Option<&str> {
        match self.rows.get(self.file_header_above(row)?).map(|r| &r.kind) {
            Some(RowKind::FileHeader(path)) => Some(path),
            _ => None,
        }
    }

    pub(super) fn rewrite_finding(&mut self, id: &str, body: String) {
        match self.session.edit_finding(id, body) {
            Ok(true) => self.status = "finding rewritten".into(),
            Ok(false) => self.status = "that finding is gone".into(),
            Err(e) => self.status = format!("save failed: {e:#}"),
        }
        self.rebuild_rows();
    }

    /// The rows a selection actually covers.
    ///
    /// From the anchor toward the cursor, stopping at a **context boundary**
    /// or a **file header** — the two rows that stand for a stretch of file
    /// the reader is not looking at. Dragging from line 23 across `13 lines
    /// hidden` to line 37 would otherwise file a note claiming fifteen lines,
    /// thirteen of which were never on screen.
    ///
    /// Nothing else breaks a run. A hunk's header, its removed and added rows,
    /// a note already filed on one of them — all of that is one continuous
    /// stretch of one file, and a selection has to cross it.
    pub(super) fn selected_run(&self) -> Vec<(usize, u32)> {
        let anchor = self.visual.unwrap_or(self.cursor);
        let at = |i: usize| self.rows.get(i).and_then(|r| r.line.as_ref());
        // One side, so a run is a run in ONE file's numbering. The anchor's,
        // since that is the end the reader chose deliberately. A row that
        // exists in both files answers for either.
        let Some(side) = at(anchor).or_else(|| at(self.cursor)).map(|l| l.side) else {
            return Vec::new();
        };

        let step: isize = if self.cursor >= anchor { 1 } else { -1 };
        let (mut i, stop) = (anchor as isize, self.cursor as isize);
        let mut run = Vec::new();
        loop {
            let Some(row) = self.rows.get(i as usize) else {
                break;
            };
            if matches!(
                row.kind,
                RowKind::ContextEdge { .. } | RowKind::FileHeader(_)
            ) {
                break;
            }
            if let Some(n) = row.line.as_ref().and_then(|l| l.line_on(side)) {
                run.push((i as usize, n));
            }
            if i == stop {
                break;
            }
            i += step;
        }
        run
    }

    /// The lines the selection covers, as the engine wants them: lowest first,
    /// both ends' text, one side.
    ///
    /// `None` when the cursor is on a row that is not a line of a file — a
    /// hunk header, a fold — and the finding then anchors the whole hunk.
    pub(super) fn selected_lines(&self) -> Option<Lines> {
        let run = self.selected_run();
        let side = self
            .rows
            .get(self.visual.unwrap_or(self.cursor))
            .or_else(|| self.rows.get(self.cursor))
            .and_then(|r| r.line.as_ref())
            .map(|l| l.side)?;
        let lo = run.iter().min_by_key(|(_, n)| *n)?;
        let hi = run.iter().max_by_key(|(_, n)| *n)?;
        let text = |row: usize| {
            self.rows[row]
                .line
                .as_ref()
                .map(|l| l.text.clone())
                .unwrap_or_default()
        };
        Some(Lines {
            side: side.to_string(),
            start: lo.1,
            end: hi.1,
            start_text: text(lo.0),
            end_text: text(hi.0),
        })
    }

    pub(super) fn add_finding(&mut self, hunk_idx: usize, lines: Option<Lines>, body: String) {
        match self.session.add_finding(hunk_idx, lines, body) {
            Ok(_) => self.status = "finding saved".into(),
            Err(e) => self.status = format!("save failed: {e:#}"),
        }
        self.rebuild_rows();
    }

    /// The row a note was laid into, if this view holds one.
    pub(super) fn row_of_finding(&self, id: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(&r.kind, RowKind::Finding(fid, _) if fid == id))
    }

    /// Put the cursor on a note, wherever in the review it lives.
    ///
    /// A note's row exists only in the view that is BUILT: the plan view
    /// builds the selected group's rows, the file view the selected file's. So
    /// reaching one is a navigation, not a row index — select the thing that
    /// owns it, let the rows rebuild, then find the row again by the note's id.
    ///
    /// Folded context is not in the way: `place_findings` hangs a note whose
    /// line is hidden off its hunk's header instead. A folded skim remainder
    /// is, since the hunk itself is not there, so that fold is opened.
    pub(super) fn jump_to_finding(&mut self, id: &str) -> bool {
        if let Some(row) = self.row_of_finding(id) {
            self.land_on(row);
            return true;
        }
        // Copied out before anything takes `&mut self`.
        let Some((digest, path)) = self
            .session
            .findings()
            .iter()
            .find(|f| f.id == id)
            .map(|f| (f.anchor.hunk_digest.clone(), f.anchor.file.clone()))
        else {
            return false;
        };

        let owner = match self.view_mode {
            ViewMode::Groups => {
                let plan = self.session.plan();
                plan.hunk_by_digest(&digest)
                    .and_then(|h| plan.group_of_hunk(h))
                    .map(|g| g.id.clone())
                    .and_then(|gid| self.session.plan().group_position(&gid))
            }
            ViewMode::Files => self.reveal_path(&path),
        };
        let Some(owner) = owner else { return false };
        self.select_entry(owner);

        // Still nothing: the hunk is in the group's folded remainder.
        if self.row_of_finding(id).is_none() {
            self.toggle_group_fold();
        }
        match self.row_of_finding(id) {
            Some(row) => {
                self.land_on(row);
                true
            }
            None => false,
        }
    }

    /// Park the cursor on a row and bring it into view.
    pub(super) fn land_on(&mut self, row: usize) {
        self.cursor = self.next_selectable(row, 1).unwrap_or(row);
        self.focus = Focus::Detail;
        self.follow_cursor();
    }

    /// Delete one note by id, from wherever it was named.
    ///
    /// The findings modal rebuilds its own list afterwards: every `row_idx`
    /// below the deleted note has shifted, and a stale one would jump the
    /// cursor to the wrong place.
    pub(super) fn delete_finding(&mut self, id: &str) {
        match self.session.delete_finding(id) {
            Ok(true) => self.status = "finding deleted".into(),
            Ok(false) => self.status = "that finding is already gone".into(),
            Err(e) => self.status = format!("save failed: {e:#}"),
        }
        self.rebuild_rows();
        self.reopen_findings();
    }

    pub(super) fn clear_findings(&mut self) {
        match self.session.clear_findings() {
            Ok(n) => self.status = format!("{n} finding(s) deleted"),
            Err(e) => self.status = format!("save failed: {e:#}"),
        }
        self.rebuild_rows();
        self.reopen_findings();
    }

    /// Rebuild the findings modal's list in place, keeping the cursor as near
    /// where it was as the new list allows. Closes it when nothing is left.
    pub(super) fn reopen_findings(&mut self) {
        let Mode::Findings {
            selected, scroll, ..
        } = &self.mode
        else {
            return;
        };
        let (was, scrolled) = (*selected, *scroll);
        self.mode = Mode::Normal;
        self.open_findings();
        if let Mode::Findings {
            entries,
            selected,
            scroll,
            ..
        } = &mut self.mode
        {
            *selected = was.min(entries.len().saturating_sub(1));
            *scroll = scrolled.min(*selected);
        }
    }

    pub(super) fn delete_finding_at_cursor(&mut self) {
        // A thread is the forge's. The two things a reader can do to it are
        // both on other keys, and the footer names them (ADR 0029).
        if matches!(
            self.rows.get(self.cursor).map(|r| &r.kind),
            Some(RowKind::Thread(..))
        ) {
            self.status = "a review thread is the forge's · c replies · x resolves".into();
            return;
        }
        if let Some(RowKind::Finding(id, _)) = self.rows.get(self.cursor).map(|r| r.kind.clone()) {
            match self.session.delete_finding(&id) {
                Ok(_) => self.status = "finding deleted".into(),
                Err(e) => self.status = format!("save failed: {e:#}"),
            }
            self.rebuild_rows();
        } else {
            self.status = "dd works on a finding line".into();
        }
    }

    /// Markdown summary of open findings, for pasting into an agent or PR.
    pub fn findings_summary(&self) -> String {
        // The engine's, not this crate's: `dfr findings --summary` prints the
        // same text, and a projection owned by a renderer is a projection the
        // other consumer will reimplement slightly differently.
        self.session.findings_summary()
    }
}
