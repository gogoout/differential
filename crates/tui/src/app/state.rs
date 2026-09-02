//! What the model recomputes when something changes.
//!
//! The tree, the rows, the cursor and the scroll. Every entry point here ends
//! with the model consistent: `rebuild_rows` is what the rest of the app calls
//! after any change that could move a row.

use differential_engine::plan::{Fold, PlanIndex};
use ratatui::text::Line;

use crate::rows::{Row, RowKind, RowsContext};
use crate::window::Side;

use super::draw::{Marker, compose_row_lines};
use super::*;

impl App {
    /// Rebuild the visible tree rows from the flat file list, honouring
    /// collapsed directories. Directory rows appear once, in path order.
    pub fn rebuild_tree(&mut self) {
        let mut paths: Vec<(usize, Vec<String>)> = self
            .files()
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.path.split('/').map(str::to_string).collect()))
            .collect();
        paths.sort_by(|a, b| a.1.cmp(&b.1));

        let mut tree = Vec::new();
        let mut open: Vec<String> = Vec::new(); // directory components in scope
        for (file_idx, parts) in paths {
            let dirs = &parts[..parts.len() - 1];
            // Close directories we have left.
            while open.len() > dirs.len() || (!open.is_empty() && open[..] != dirs[..open.len()]) {
                open.pop();
            }
            // Open the ones we entered.
            let mut hidden = false;
            for (d, name) in dirs.iter().enumerate() {
                if d < open.len() {
                    continue;
                }
                open.push(name.clone());
                let path = open.join("/");
                if !hidden {
                    tree.push(TreeEntry {
                        depth: d,
                        kind: TreeKind::Dir { path: path.clone() },
                    });
                }
                if self.collapsed.contains(&path) {
                    hidden = true;
                }
            }
            // A file under any collapsed ancestor is not a visible row.
            let under_collapsed =
                (1..=dirs.len()).any(|n| self.collapsed.contains(&dirs[..n].join("/")));
            if !under_collapsed {
                tree.push(TreeEntry {
                    depth: dirs.len(),
                    kind: TreeKind::File { file_idx },
                });
            }
        }
        self.tree = tree;
    }

    /// File indices covered by a tree row: one file, or every file under a
    /// directory (including collapsed ones).
    pub(super) fn files_of_tree_row(&self, row: usize) -> Vec<usize> {
        match self.tree.get(row).map(|e| &e.kind) {
            Some(TreeKind::File { file_idx }) => vec![*file_idx],
            Some(TreeKind::Dir { path }) => {
                let prefix = format!("{path}/");
                let mut under: Vec<usize> = self
                    .files()
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.path.starts_with(&prefix))
                    .map(|(i, _)| i)
                    .collect();
                // Path order, so the diff pane presents files in the order the
                // tree lists them rather than in document order.
                under.sort_by(|a, b| self.files()[*a].path.cmp(&self.files()[*b].path));
                under
            }
            None => Vec::new(),
        }
    }

    /// Path of the selected file-tree row (a file path, or a directory).
    pub fn selected_path(&self) -> Option<String> {
        self.tree_row_path(self.selected_file)
    }

    /// The path a tree row stands for — what the resume cursor persists.
    pub(super) fn tree_row_path(&self, row: usize) -> Option<String> {
        match self.tree.get(row).map(|e| &e.kind) {
            Some(TreeKind::File { file_idx }) => Some(self.files()[*file_idx].path.clone()),
            Some(TreeKind::Dir { path }) => Some(path.clone()),
            None => None,
        }
    }

    /// Locate the tree row showing `path`, expanding collapsed ancestors.
    pub(super) fn reveal_path(&mut self, path: &str) -> Option<usize> {
        let mut parts: Vec<&str> = path.split('/').collect();
        parts.pop();
        for n in 1..=parts.len() {
            self.collapsed.remove(&parts[..n].join("/"));
        }
        self.rebuild_tree();
        self.tree.iter().position(|e| match &e.kind {
            TreeKind::File { file_idx } => self.files()[*file_idx].path == path,
            TreeKind::Dir { path: p } => p == path,
        })
    }

    /// Fold or unfold the selected directory.
    pub(super) fn toggle_dir(&mut self) -> bool {
        let Some(TreeKind::Dir { path }) = self.tree.get(self.selected_file).map(|e| &e.kind)
        else {
            return false;
        };
        let path = path.clone();
        if !self.collapsed.insert(path.clone()) {
            self.collapsed.remove(&path);
        }
        self.rebuild_tree();
        self.selected_file = self
            .tree
            .iter()
            .position(|e| matches!(&e.kind, TreeKind::Dir { path: p } if *p == path))
            .unwrap_or(0);
        self.follow_plan_scroll();
        self.rebuild_rows();
        true
    }

    pub fn rebuild_rows(&mut self) {
        let reviewed = self.session.reviewed_hunks();
        match self.view_mode {
            ViewMode::Groups => {
                let Some(groups) = self.session.doc().groups.as_ref() else {
                    self.rows = Vec::new();
                    return;
                };
                if groups.is_empty() {
                    self.rows = vec![Row::full(
                        RowKind::Blank,
                        Line::from("nothing to review — empty diff"),
                    )];
                    return;
                }
                // A document whose ids contradict each other can only come from
                // a corrupt store; say so on screen rather than rendering a
                // silently short group.
                let index = match PlanIndex::build(self.session.doc()) {
                    Ok(index) => index,
                    Err(e) => {
                        self.rows = vec![Row::full(RowKind::Blank, Line::from(format!("{e}")))];
                        return;
                    }
                };
                let g = &groups[self.selected_group.min(groups.len() - 1)];
                let ctx = GroupContext {
                    core: RowsContext {
                        theme: &self.theme,
                        doc: self.session.doc(),
                        plan: self.session.plan(),
                        findings: self.session.findings(),
                        reviewed: &reviewed,
                        mode: self.diff_mode(),
                        show_group_labels: false,
                        context: self.opts.context,
                        context_step: self.opts.context_step,
                        expansion: &self.expanded,
                    },
                    index: &index,
                    group: g,
                    view: &self.session.plan().groups[self.selected_group.min(groups.len() - 1)],
                    fold: if self.folds_open.contains(&g.id) {
                        Fold::Unfolded
                    } else {
                        Fold::Folded
                    },
                };
                self.rows = build_group_rows(&mut self.factory, &ctx);
            }
            ViewMode::Files => {
                if self.tree.is_empty() {
                    self.rows = vec![Row::full(
                        RowKind::Blank,
                        Line::from("nothing to review — empty diff"),
                    )];
                    return;
                }
                let row = self.selected_file.min(self.tree.len() - 1);
                let targets = self.files_of_tree_row(row);
                let ctx = RowsContext {
                    theme: &self.theme,
                    doc: self.session.doc(),
                    plan: self.session.plan(),
                    findings: self.session.findings(),
                    reviewed: &reviewed,
                    mode: self.diff_mode(),
                    show_group_labels: true,
                    context: self.opts.context,
                    context_step: self.opts.context_step,
                    expansion: &self.expanded,
                };
                self.rows = match targets.as_slice() {
                    // A single file keeps its dedicated builder (it renders a
                    // placeholder for zero-hunk binary/submodule changes).
                    [only] => {
                        let f = &self.session.plan().files[*only];
                        let (path, hunks) =
                            (f.path.clone(), f.hunks.iter().map(|h| h.index()).collect());
                        build_file_rows(&mut self.factory, &ctx, &path, hunks)
                    }
                    // A directory: every hunk beneath it, file headers and all.
                    many => {
                        let hunks: Vec<usize> = many
                            .iter()
                            .flat_map(|i| self.files()[*i].hunks.iter().map(|h| h.index()))
                            .collect();
                        build_dir_rows(&mut self.factory, &ctx, hunks)
                    }
                };
            }
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        if !self
            .rows
            .get(self.cursor)
            .is_some_and(|r| r.kind.selectable())
        {
            self.cursor = self.next_selectable(0, 1).unwrap_or(0);
        }
        self.rebuild_overviews();
    }

    pub(super) fn next_selectable(&self, from: usize, dir: isize) -> Option<usize> {
        let mut i = from as isize;
        loop {
            if i < 0 || i as usize >= self.rows.len() {
                return None;
            }
            if self.rows[i as usize].kind.selectable() {
                return Some(i as usize);
            }
            i += dir;
        }
    }

    pub(super) fn move_cursor(&mut self, dir: isize) {
        let start = self.cursor as isize + dir;
        if let Some(next) = self.next_selectable(start.max(0) as usize, dir) {
            self.cursor = next;
        }
        self.follow_cursor();
    }

    /// Fold measured geometry into the model.
    ///
    /// A resize is an event like any other: both scroll offsets are re-clamped
    /// here, in update, rather than discovered while rendering.
    pub fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = viewport;
        self.follow_cursor();
        self.follow_plan_scroll();
    }

    /// Diff-pane scroll offset. Decided in update, never at draw time — which
    /// is why the field itself is private.
    /// The pane heights currently in force.
    ///
    /// Exposed so a test can assert the guarantee the geometry rework rests on
    /// — that they are re-derived when focus changes, not left stale.
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Rows one left-pane entry occupies.
    ///
    /// Arithmetic, not rendering, so the scroll math does not need a `Frame` —
    /// which is what lets it move out of `draw_groups`. A `debug_assert` there
    /// checks the two still agree.
    pub(super) fn plan_block_height(&self, idx: usize) -> usize {
        match self.view_mode {
            // group_lines: a title row, a counts row, and an `after:` row only
            // when the group has edges.
            ViewMode::Groups => self
                .groups()
                .get(idx)
                .map_or(0, |g| 2 + usize::from(!g.depends_on.is_empty())),
            ViewMode::Files => usize::from(idx < self.tree.len()),
        }
    }

    /// Keep the whole selected plan block in view. Lifted out of `draw_groups`.
    pub(super) fn follow_plan_scroll(&mut self) {
        let h = self.viewport.plan_rows.max(MIN_VIEWPORT);
        let selected = self.selected_entry();
        let start_row: usize = (0..selected).map(|i| self.plan_block_height(i)).sum();
        let end_row = start_row + self.plan_block_height(selected);
        if start_row < self.group_scroll {
            self.group_scroll = start_row;
        } else if end_row > self.group_scroll + h {
            self.group_scroll = end_row.saturating_sub(h);
        }
    }

    pub(super) fn follow_cursor(&mut self) {
        if self.cursor < self.scroll + SCROLL_MARGIN {
            self.scroll = self.cursor.saturating_sub(SCROLL_MARGIN);
        } else {
            self.scroll = self.scroll.max(self.highest_scroll());
        }
        // The rows above the first selectable one are the group header —
        // label, description, dependencies — and the cursor can never enter
        // them. Without this the scroll margin would pin the view one row
        // below the top and that header could never be read.
        if self
            .next_selectable(0, 1)
            .is_none_or(|first| self.cursor <= first)
        {
            self.scroll = 0;
        }
    }

    /// The furthest the view can be scrolled DOWN and still hold the cursor
    /// and its margin.
    ///
    /// The bottom edge is a budget of screen LINES, not a count of rows: a
    /// wrapped row is one row and several lines. Walking back from the last
    /// row that must stay visible, the first row the budget cannot afford is
    /// where the view has to start.
    pub(super) fn highest_scroll(&self) -> usize {
        let h = self.viewport.detail_rows.max(MIN_VIEWPORT);
        let last = (self.cursor + SCROLL_MARGIN).min(self.rows.len().saturating_sub(1));
        let mut used = 0;
        let mut top = last;
        for i in (0..=last).rev() {
            used += self.row_height(i);
            if used > h {
                break;
            }
            top = i;
        }
        // Never past the cursor: a row taller than the whole pane pins to the
        // top of the view and is cut off at the bottom, rather than scrolling
        // the row the reader is on out of sight.
        top.min(self.cursor)
    }

    /// The row half a pane away from `from`, walking `dir`.
    ///
    /// Half a pane of screen LINES. Counting rows would jump a screenful of
    /// wrapped prose in one press.
    pub(super) fn half_page(&self, from: usize, dir: isize) -> usize {
        let budget = self.viewport.detail_rows.max(MIN_VIEWPORT) / 2;
        let mut used = 0;
        let mut i = from;
        while used < budget {
            let next = i as isize + dir;
            if next < 0 || next as usize >= self.rows.len() {
                break;
            }
            i = next as usize;
            used += self.row_height(i);
        }
        i
    }

    /// Select the idx-th entry of the left pane (group or file).
    pub(super) fn select_entry(&mut self, idx: usize) {
        match self.view_mode {
            ViewMode::Groups => {
                if self.groups().is_empty() {
                    return;
                }
                self.selected_group = idx.min(self.groups().len() - 1);
            }
            ViewMode::Files => {
                if self.tree.is_empty() {
                    return;
                }
                self.selected_file = idx.min(self.tree.len() - 1);
            }
        }
        self.cursor = 0;
        self.scroll = 0;
        self.follow_plan_scroll();
        self.rebuild_rows();
    }

    pub(super) fn selected_entry(&self) -> usize {
        match self.view_mode {
            ViewMode::Groups => self.selected_group,
            ViewMode::Files => self.selected_file,
        }
    }

    /// Toggle semantic groups <-> flat file list; the choice persists.
    pub(super) fn toggle_file_view(&mut self) {
        let on = self.view_mode == ViewMode::Groups;
        if let Err(e) = self.session.set_file_view(on) {
            self.status = format!("save failed: {e:#}");
            return;
        }
        self.view_mode = if on {
            ViewMode::Files
        } else {
            ViewMode::Groups
        };
        self.cursor = 0;
        self.scroll = 0;
        self.follow_plan_scroll();
        self.rebuild_rows();
        self.status = if on { "file view" } else { "reading plan view" }.into();
    }

    /// Open the file-list modal over the current rows (reflects folds and the
    /// active view). Enter jumps to the chosen file's header.
    pub(super) fn open_file_list(&mut self) {
        let reviewed = self.session.reviewed_hunks();
        let entries: Vec<FileListEntry> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match &r.kind {
                RowKind::FileHeader(path) => Some((i, path.clone())),
                _ => None,
            })
            .map(|(row_idx, path)| {
                let info = self.files().iter().find(|f| f.path == path);
                let (adds, dels, done) = info
                    .map(|f| {
                        (
                            f.counts.adds,
                            f.counts.dels,
                            !f.hunks.is_empty()
                                && f.hunks.iter().all(|h| reviewed.contains(&h.index())),
                        )
                    })
                    .unwrap_or((0, 0, false));
                FileListEntry {
                    path,
                    row_idx,
                    adds,
                    dels,
                    reviewed: done,
                }
            })
            .collect();
        if entries.is_empty() {
            self.status = "no files listed here (unfold with z?)".into();
            return;
        }
        self.mode = Mode::FileList {
            entries,
            selected: 0,
            scroll: 0,
        };
    }

    /// Every finding in the review, open ones first and orphans after.
    ///
    /// Store order within each group, which is the order they were written.
    /// The rule between the two groups is drawn rather than stored, so
    /// `selected` indexes findings and nothing has to skip a row it cannot
    /// land on.
    pub(super) fn open_findings(&mut self) {
        // No row index is kept. A note's row exists only in the view that is
        // built, so one captured here would be stale the moment the reader
        // navigates — `jump_to_finding` re-finds it by id after selecting the
        // group or file that owns it.
        let mut entries: Vec<FindingEntry> = self
            .session
            .findings()
            .iter()
            .map(|f| FindingEntry {
                at: format!("{}:{}", f.anchor.file, f.anchor.line_span()),
                body: f.body.lines().next().unwrap_or("").to_string(),
                orphaned: f.status == FindingStatus::Orphaned,
                moved: f.moved,
                id: f.id.clone(),
            })
            .collect();
        if entries.is_empty() {
            self.status = "no findings yet — c writes one".into();
            return;
        }
        entries.sort_by_key(|e| e.orphaned);
        self.mode = Mode::Findings {
            entries,
            selected: 0,
            scroll: 0,
            confirming: false,
        };
    }

    /// Jump to the next/previous hunk header, so a reviewer can move by
    /// change instead of by line.
    pub(super) fn jump_hunk(&mut self, dir: isize) {
        let mut i = self.cursor as isize + dir;
        while i >= 0 && (i as usize) < self.rows.len() {
            // A foreign hunk is context the reviewer asked for, not an entry
            // on this group's reading list, so hunk-to-hunk navigation passes
            // over it.
            if matches!(
                self.rows[i as usize].kind,
                RowKind::HunkHeader { foreign: false, .. }
            ) {
                self.cursor = i as usize;
                self.focus = Focus::Detail;
                self.follow_cursor();
                return;
            }
            i += dir;
        }
        self.status = if dir > 0 {
            "last hunk in this view".into()
        } else {
            "first hunk in this view".into()
        };
    }

    /// Pull more of the file in at the boundary row under the cursor.
    ///
    /// Growing upward inserts rows ABOVE the cursor, so the index has to be
    /// re-found rather than kept: the boundary row moves, and when the window
    /// reaches the start of the file (or merges into its neighbour) it stops
    /// existing at all.
    pub(super) fn expand_at_cursor(&mut self) {
        let Some(RowKind::ContextEdge {
            hunk,
            side,
            crossing,
        }) = self.rows.get(self.cursor).map(|r| r.kind.clone())
        else {
            return;
        };
        let step = self.opts.context_step;
        let e = self.expanded.entry(hunk).or_default();
        match (side, crossing) {
            (Side::Up, false) => e.up += step,
            (Side::Down, false) => e.down += step,
            // Crossing resets that side's context counter: the gap it measured
            // is not the outermost one any more.
            (Side::Up, true) => {
                e.crossed_up += 1;
                e.up = 0;
            }
            (Side::Down, true) => {
                e.crossed_down += 1;
                e.down = 0;
            }
        }
        self.rebuild_rows();

        // Match on the edge, not on what it offers: crossing turns a "next:"
        // boundary back into a context one, and the cursor should follow it.
        match self.rows.iter().position(
            |r| matches!(r.kind, RowKind::ContextEdge { hunk: h, side: sd, .. } if h == hunk && sd == side),
        ) {
            Some(pos) => self.cursor = pos,
            None => {
                // Nothing left to unfold in that direction: land on the hunk
                // itself and say why the boundary vanished.
                if let Some(pos) = self
                    .rows
                    .iter()
                    .position(|r| matches!(r.kind, RowKind::HunkHeader { hunk: h, .. } if h == hunk))
                {
                    self.cursor = pos;
                }
                self.status = match side {
                    Side::Up => "top of what precedes this hunk".into(),
                    Side::Down => "end of what follows this hunk".into(),
                };
            }
        }
        self.follow_cursor();
    }

    /// The files the flat list under the plan would show: every file the
    /// current rows touch, in the order they appear.
    ///
    /// Its LENGTH decides how tall that pane is, so this is called from layout
    /// as well as from drawing — one answer, so the two cannot disagree about
    /// how much room the list needs.
    pub(super) fn file_list(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for row in &self.rows {
            if let RowKind::FileHeader(path) = &row.kind
                && let Some(i) = self.files().iter().position(|f| f.path == *path)
                && !out.contains(&i)
            {
                out.push(i);
            }
        }
        out
    }

    /// Recompute what the two overviews draw. Called with the rows, because
    /// that is what they describe — and never from `draw`, which runs on every
    /// keypress.
    pub(super) fn rebuild_overviews(&mut self) {
        self.listed_files = self.file_list();
        self.map_files = self.files_of_selected_group();
    }

    /// The row index of the file header the cursor is under.
    ///
    /// Walked backwards from the cursor, because a diff row does not name its
    /// file — the header above it does. Both the flat list's marker and the
    /// sticky header need this, so it is one function.
    pub(super) fn file_header_above(&self, from: usize) -> Option<usize> {
        // `..=0` on an empty slice panics, and rows ARE empty for a document
        // with no groups. Drawing was harmlessly a no-op there before this
        // helper existed; it stays one.
        let last = self.rows.len().checked_sub(1)?;
        self.rows[..=from.min(last)]
            .iter()
            .rposition(|r| matches!(r.kind, RowKind::FileHeader(_)))
    }

    /// The file the cursor is in, as an index into `files()`.
    pub fn file_at_cursor(&self) -> Option<usize> {
        let row = self.file_header_above(self.cursor)?;
        let RowKind::FileHeader(path) = &self.rows[row].kind else {
            return None;
        };
        self.files().iter().position(|f| f.path == *path)
    }

    pub(super) fn current_hunk(&self) -> Option<usize> {
        self.rows.get(self.cursor).and_then(|r| r.kind.hunk())
    }

    /// The reader's choice if they have made one, otherwise the configured
    /// default. A review that has recorded a choice keeps it, so changing the
    /// config never moves a layout under someone mid-read.
    pub(super) fn split_diff(&self) -> bool {
        self.session.split_diff().unwrap_or(self.opts.split_diff)
    }

    pub(super) fn diff_mode(&self) -> DiffMode {
        if self.split_diff() {
            DiffMode::Split
        } else {
            DiffMode::Unified
        }
    }

    /// Is soft wrap on? Off until the reader presses `w` on this review.
    ///
    /// No config default, unlike `split_diff`: a layout preference is worth
    /// setting once, but wrapping is something a reader wants for the file
    /// they are on.
    pub(super) fn wrap_on(&self) -> bool {
        self.session.wrap().unwrap_or(false)
    }

    /// Does this row wrap right now?
    ///
    /// Prose always does. A group's description and a reviewer's note are the
    /// reasons a plan and a finding exist, they are never code, and a reader
    /// who cannot see the end of one is missing the point of the pane. File
    /// content is the reader's call, because wrapping code is often unwanted.
    pub(super) fn wraps(&self, row: &Row) -> bool {
        match row.kind {
            RowKind::GroupHeader | RowKind::Finding(..) => true,
            RowKind::Diff(_) => self.wrap_on(),
            _ => false,
        }
    }

    /// Screen lines one row takes.
    ///
    /// The scroll budget and the drawing both read this, so they cannot
    /// disagree about where a row ends. Deliberately independent of the
    /// cursor: no row that wraps carries a marker or a hint, so nothing the
    /// cursor changes can change a height.
    ///
    /// Exposed for the same reason `viewport` is: a test asserting that the
    /// scroll budget counts screen LINES has to be able to count them, and
    /// counting rows instead is exactly the bug it guards against.
    pub fn row_height(&self, i: usize) -> usize {
        self.rows.get(i).map_or(1, |r| {
            compose_row_lines(
                &self.theme,
                &r.content,
                self.viewport.detail_cols,
                false,
                Marker::None,
                None,
                self.wraps(r),
            )
            .len()
        })
    }

    /// Toggle soft wrap. Row COUNTS do not change — a wrapped line is still one
    /// row — so nothing is rebuilt and the cursor stays where it was.
    pub(super) fn toggle_wrap(&mut self) {
        let on = !self.wrap_on();
        if let Err(e) = self.session.set_wrap(on) {
            self.status = format!("save failed: {e:#}");
            return;
        }
        self.follow_cursor();
    }

    /// Toggle unified/split. Row counts differ between the modes, so keep the
    /// reviewer's place by re-anchoring the cursor to the current hunk.
    pub(super) fn toggle_split(&mut self) {
        let hunk = self.current_hunk();
        let on = !self.split_diff();
        if let Err(e) = self.session.set_split_diff(on) {
            self.status = format!("save failed: {e:#}");
            return;
        }
        self.rebuild_rows();
        if let Some(h) = hunk
            && let Some(pos) = self.rows.iter().position(|r| r.kind.hunk() == Some(h))
        {
            self.cursor = pos;
            self.follow_cursor();
        }
    }

    /// Open or close the selected group's folded remainder — the skim group's
    /// hunks past its exemplars, or a noise group entire.
    pub(super) fn toggle_group_fold(&mut self) {
        if self.view_mode != ViewMode::Groups {
            return;
        }
        let Some(g) = self.groups().get(self.selected_group) else {
            return;
        };
        let gid = g.id.clone();
        if !self.folds_open.insert(gid.clone()) {
            self.folds_open.remove(&gid);
        }
        self.rebuild_rows();
    }

    /// Persist the resume position through the session; surface failures in
    /// the status line rather than tearing the TUI down.
    pub(super) fn save_cursor(&mut self) {
        let id = match self.view_mode {
            ViewMode::Groups => self
                .groups()
                .get(self.selected_group)
                .map(|g| g.id.clone())
                .unwrap_or_default(),
            ViewMode::Files => self.tree_row_path(self.selected_file).unwrap_or_default(),
        };
        if let Err(e) = self.session.save_cursor(id, self.cursor) {
            self.status = format!("save failed: {e:#}");
        }
    }
}
