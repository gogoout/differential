//! The reviewer's model, key handling and drawing. `handle_key` is a plain
//! method on the model returning effects — testable without a terminal.
//!
//! All review state (reviewed marks, findings, resume cursor) lives in the
//! engine's `ReviewSession`; this model holds presentation state only.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use differential_engine::FsReviewSession;
use differential_engine::plan::{Fold, PlanIndex};
use differential_engine::review_state::FindingStatus;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use super::rows::{
    DiffMode, Fill, GroupContext, Half, Row, RowContent, RowFactory, RowKind, RowsContext,
    build_dir_rows, build_file_rows, build_group_rows,
};
use super::theme::{THEME, Theme};
use super::vendor::text_utils::truncate_or_pad_spans;
use super::window::{Expansion, Side};

const SCROLL_MARGIN: usize = 3;

/// The cursor's own cell, at the head of every diff row's gutter.
///
/// A background is what marks a change now, and `Line::style` sits UNDER span
/// styles — so a row-wide cursor colour is invisible on exactly the rows a
/// reviewer is most likely to be standing on. A glyph reads on any background,
/// and living inside the reserved gutter cell means moving the cursor never
/// shifts the pane sideways.
const CURSOR_MARK: char = '▸';

/// Presentation settings the application layer reads from config and hands to
/// the renderer. Not review state: nothing here is persisted in the sidecar.
#[derive(Debug, Clone, Copy)]
pub struct ReviewOptions {
    /// Context lines either side of a hunk before any expansion.
    pub context: usize,
    /// Lines one `z` on a context boundary row pulls in.
    pub context_step: usize,
}

impl Default for ReviewOptions {
    fn default() -> Self {
        ReviewOptions {
            context: 3,
            context_step: 10,
        }
    }
}

/// Floor for the scroll arithmetic.
///
/// Not a guess about the terminal — geometry is measured — but a clamp so a
/// three-row window cannot produce nonsense.
const MIN_VIEWPORT: usize = 8;

/// The reviewer's panes: a fixed-width plan pane, the diff, a status row.
pub struct Panes {
    pub body: Rect,
    pub plan: Rect,
    pub diff: Rect,
    pub status: Rect,
}

/// The one layout. `draw` places widgets with it and the event loop measures
/// with it, so the two can never disagree about how tall the diff pane is.
pub fn layout(area: Rect) -> Panes {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(40), Constraint::Min(0)])
        .split(outer[0]);
    Panes {
        body: outer[0],
        plan: panes[0],
        diff: panes[1],
        status: outer[1],
    }
}

/// Measured terminal geometry, pushed into the model BEFORE any key is
/// handled — so scroll math is arithmetic over a known height rather than a
/// guess corrected one frame later.
///
/// Deliberately carries no WIDTH. Row building must never depend on width
/// (`RowContent::Split` defers its columns to draw time precisely so a resize
/// never rebuilds rows), and a width here would be an invitation to break
/// that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub diff_rows: usize,
    pub plan_rows: usize,
}

impl Viewport {
    pub fn measure(area: Rect) -> Self {
        let panes = layout(area);
        Viewport {
            // Both panes are bordered.
            diff_rows: panes.diff.height.saturating_sub(2) as usize,
            plan_rows: panes.plan.height.saturating_sub(2) as usize,
        }
    }
}

impl Default for Viewport {
    /// Before the first measurement.
    fn default() -> Self {
        Viewport {
            diff_rows: 24,
            plan_rows: 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Groups,
    Diff,
}

pub enum Mode {
    Normal,
    /// Editing a finding for the given canonical hunk index.
    Editing(usize, Box<TextArea<'static>>),
    Help,
    /// File-list modal over the current rows: jump to a file header.
    FileList {
        entries: Vec<FileListEntry>,
        selected: usize,
    },
}

pub struct FileListEntry {
    pub path: String,
    /// Row index of the file's header in the current rows.
    pub row_idx: usize,
    pub adds: usize,
    pub dels: usize,
    pub reviewed: bool,
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Quit,
    CopySummary(String),
}

/// A plan row's relation to the selected group — what the gutter connector
/// draws. The plan is a DAG (a group can follow several others), not a tree.
///
/// One direction only: what the selected group *follows*. The reverse edge was
/// drawn too, in a second colour of the same glyph, which meant the gutter said
/// something different from the `after:` line directly beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Selected,
    /// The selected group follows this one.
    Dependency,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Semantic groups — the reading plan.
    Groups,
    /// Flattened per-file list, every hunk in file order.
    Files,
}

/// One visible row of the file tree: a directory, or a file (indexing into
/// `files`, which stays flat — it anchors reviewed state and the persisted
/// cursor).
pub struct TreeEntry {
    pub depth: usize,
    pub kind: TreeKind,
}

pub enum TreeKind {
    Dir { path: String },
    File { file_idx: usize },
}

pub struct App {
    pub session: FsReviewSession,
    factory: RowFactory,

    /// Visible rows of the file tree (rebuilt when a directory folds).
    pub tree: Vec<TreeEntry>,
    /// Directory paths currently collapsed.
    collapsed: HashSet<String>,

    pub focus: Focus,
    pub mode: Mode,
    pub view_mode: ViewMode,
    pub selected_group: usize,
    pub selected_file: usize,
    pub rows: Vec<Row>,
    pub cursor: usize,
    scroll: usize,
    group_scroll: usize,
    /// Group ids whose fold is open.
    pub folds_open: HashSet<String>,
    /// How far each hunk's context has been pulled open, by canonical index.
    ///
    /// Transient, like `folds_open`: how much of a file you are looking at is a
    /// reading aid for this sitting, not a finding, so nothing here reaches the
    /// sidecar store.
    expanded: HashMap<usize, Expansion>,
    opts: ReviewOptions,
    pub status: String,
    /// Measured geometry. An input to update, never a draw-time output.
    viewport: Viewport,
    pending_d: bool,
}

impl App {
    pub fn new(session: FsReviewSession, factory: RowFactory, opts: ReviewOptions) -> Self {
        // Resume position: the cursor id is a group id in the semantic view,
        // a file path in the file view (session.file_view() disambiguates).
        let view_mode = if session.file_view() {
            ViewMode::Files
        } else {
            ViewMode::Groups
        };
        let resume: Option<(String, usize)> = session.cursor().cloned();
        let selected_group = match (&resume, view_mode) {
            (Some((id, _)), ViewMode::Groups) => session.plan().group_position(id).unwrap_or(0),
            _ => 0,
        };

        let mut app = App {
            session,
            factory,
            tree: Vec::new(),
            collapsed: HashSet::new(),
            focus: Focus::Groups,
            mode: Mode::Normal,
            view_mode,
            selected_group,
            selected_file: 0,
            rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            group_scroll: 0,
            folds_open: HashSet::new(),
            expanded: HashMap::new(),
            opts,
            status: String::new(),
            viewport: Viewport::default(),
            pending_d: false,
        };
        app.rebuild_tree();
        // The persisted cursor names a path; reveal it in the tree.
        if app.view_mode == ViewMode::Files
            && let Some((path, _)) = resume.as_ref()
            && let Some(row) = app.reveal_path(path)
        {
            app.selected_file = row;
        }
        app.rebuild_rows();
        if let Some((_, row)) = resume {
            app.cursor = row.min(app.rows.len().saturating_sub(1));
        }
        app
    }

    /// Lines syntect has parsed since the reviewer opened.
    ///
    /// Exposed so the windowed rebuild's whole point — cost proportional to
    /// what is drawn, not to the files touched — is a testable property rather
    /// than a claim in a comment.
    pub fn highlighted_lines(&self) -> usize {
        self.factory.highlighted_lines()
    }

    /// The document's groups, projected by the engine.
    pub fn groups(&self) -> &[differential_engine::plan::GroupView] {
        &self.session.plan().groups
    }

    /// Every file in the document, document order — including the zero-hunk
    /// binary/submodule changes the group view cannot surface.
    pub fn files(&self) -> &[differential_engine::plan::FileView] {
        &self.session.plan().files
    }

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
    fn files_of_tree_row(&self, row: usize) -> Vec<usize> {
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
    fn tree_row_path(&self, row: usize) -> Option<String> {
        match self.tree.get(row).map(|e| &e.kind) {
            Some(TreeKind::File { file_idx }) => Some(self.files()[*file_idx].path.clone()),
            Some(TreeKind::Dir { path }) => Some(path.clone()),
            None => None,
        }
    }

    /// Locate the tree row showing `path`, expanding collapsed ancestors.
    fn reveal_path(&mut self, path: &str) -> Option<usize> {
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
    fn toggle_dir(&mut self) -> bool {
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
    }

    fn next_selectable(&self, from: usize, dir: isize) -> Option<usize> {
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

    fn move_cursor(&mut self, dir: isize) {
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
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Rows one left-pane entry occupies.
    ///
    /// Arithmetic, not rendering, so the scroll math does not need a `Frame` —
    /// which is what lets it move out of `draw_groups`. A `debug_assert` there
    /// checks the two still agree.
    fn plan_block_height(&self, idx: usize) -> usize {
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
    fn follow_plan_scroll(&mut self) {
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

    fn follow_cursor(&mut self) {
        let h = self.viewport.diff_rows.max(MIN_VIEWPORT);
        if self.cursor < self.scroll + SCROLL_MARGIN {
            self.scroll = self.cursor.saturating_sub(SCROLL_MARGIN);
        } else if self.cursor + SCROLL_MARGIN + 1 > self.scroll + h {
            self.scroll = self.cursor + SCROLL_MARGIN + 1 - h;
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

    /// Select the idx-th entry of the left pane (group or file).
    fn select_entry(&mut self, idx: usize) {
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

    fn selected_entry(&self) -> usize {
        match self.view_mode {
            ViewMode::Groups => self.selected_group,
            ViewMode::Files => self.selected_file,
        }
    }

    /// Toggle semantic groups <-> flat file list; the choice persists.
    fn toggle_file_view(&mut self) {
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
    fn open_file_list(&mut self) {
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
        };
    }

    /// Jump to the next/previous hunk header, so a reviewer can move by
    /// change instead of by line.
    fn jump_hunk(&mut self, dir: isize) {
        let mut i = self.cursor as isize + dir;
        while i >= 0 && (i as usize) < self.rows.len() {
            if matches!(self.rows[i as usize].kind, RowKind::HunkHeader(_)) {
                self.cursor = i as usize;
                self.focus = Focus::Diff;
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
    fn expand_at_cursor(&mut self) {
        let Some(RowKind::ContextEdge(hunk, side)) =
            self.rows.get(self.cursor).map(|r| r.kind.clone())
        else {
            return;
        };
        let step = self.opts.context_step;
        let e = self.expanded.entry(hunk).or_default();
        match side {
            Side::Up => e.up += step,
            Side::Down => e.down += step,
        }
        self.rebuild_rows();

        match self
            .rows
            .iter()
            .position(|r| r.kind == RowKind::ContextEdge(hunk, side))
        {
            Some(pos) => self.cursor = pos,
            None => {
                // Nothing left to unfold in that direction: land on the hunk
                // itself and say why the boundary vanished.
                if let Some(pos) = self
                    .rows
                    .iter()
                    .position(|r| r.kind == RowKind::HunkHeader(hunk))
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

    fn current_hunk(&self) -> Option<usize> {
        self.rows.get(self.cursor).and_then(|r| r.kind.hunk())
    }

    fn diff_mode(&self) -> DiffMode {
        if self.session.split_diff() {
            DiffMode::Split
        } else {
            DiffMode::Unified
        }
    }

    /// Toggle unified/split. Row counts differ between the modes, so keep the
    /// reviewer's place by re-anchoring the cursor to the current hunk.
    fn toggle_split(&mut self) {
        let hunk = self.current_hunk();
        let on = !self.session.split_diff();
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
        self.status = if on { "split diff" } else { "unified diff" }.into();
    }

    /// Persist the resume position through the session; surface failures in
    /// the status line rather than tearing the TUI down.
    fn save_cursor(&mut self) {
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

    /// Key handling. Returns effects for the loop to execute.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        match &mut self.mode {
            Mode::Help => {
                self.mode = Mode::Normal;
                return Vec::new();
            }
            Mode::FileList { entries, selected } => {
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        *selected = (*selected + 1).min(entries.len().saturating_sub(1));
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        *selected = selected.saturating_sub(1);
                    }
                    KeyCode::Enter => {
                        let row = entries[*selected].row_idx;
                        self.mode = Mode::Normal;
                        self.cursor = self.next_selectable(row, 1).unwrap_or(row);
                        self.focus = Focus::Diff;
                        self.follow_cursor();
                    }
                    KeyCode::Esc | KeyCode::Char('f') | KeyCode::Char('q') => {
                        self.mode = Mode::Normal;
                    }
                    _ => {}
                }
                return Vec::new();
            }
            Mode::Editing(hunk, textarea) => {
                let hunk = *hunk;
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => {
                        self.mode = Mode::Normal;
                        self.status = "finding discarded".into();
                        return Vec::new();
                    }
                    (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                        let body = textarea.lines().join("\n").trim().to_string();
                        self.mode = Mode::Normal;
                        if body.is_empty() {
                            self.status = "empty finding discarded".into();
                            return Vec::new();
                        }
                        self.add_finding(hunk, body);
                        return Vec::new();
                    }
                    _ => {
                        textarea.input(key);
                        return Vec::new();
                    }
                }
            }
            Mode::Normal => {}
        }

        let pending_d = std::mem::take(&mut self.pending_d);
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) => {
                self.save_cursor();
                return vec![Effect::Quit];
            }
            (KeyCode::Char('?'), _) => self.mode = Mode::Help,
            (KeyCode::Tab, _) => {
                self.focus = match self.focus {
                    Focus::Groups => Focus::Diff,
                    Focus::Diff => Focus::Groups,
                }
            }
            (KeyCode::Enter, _) if self.focus == Focus::Groups => {
                // Enter opens a directory rather than jumping to the diff.
                if !(self.view_mode == ViewMode::Files && self.toggle_dir()) {
                    self.focus = Focus::Diff;
                }
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => match self.focus {
                Focus::Groups => self.select_entry(self.selected_entry() + 1),
                Focus::Diff => self.move_cursor(1),
            },
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => match self.focus {
                Focus::Groups => self.select_entry(self.selected_entry().saturating_sub(1)),
                Focus::Diff => self.move_cursor(-1),
            },
            (KeyCode::Char('J'), _) | (KeyCode::Char('}'), _) => {
                self.select_entry(self.selected_entry() + 1)
            }
            (KeyCode::Char('K'), _) | (KeyCode::Char('{'), _) => {
                self.select_entry(self.selected_entry().saturating_sub(1))
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                let h = self.viewport.diff_rows.max(MIN_VIEWPORT) / 2;
                self.cursor = (self.cursor + h).min(self.rows.len().saturating_sub(1));
                self.cursor = self.next_selectable(self.cursor, -1).unwrap_or(self.cursor);
                self.follow_cursor();
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let h = self.viewport.diff_rows.max(MIN_VIEWPORT) / 2;
                self.cursor = self.cursor.saturating_sub(h);
                self.cursor = self.next_selectable(self.cursor, 1).unwrap_or(self.cursor);
                self.follow_cursor();
            }
            (KeyCode::Char('g'), _) => {
                self.cursor = self.next_selectable(0, 1).unwrap_or(0);
                self.follow_cursor();
            }
            (KeyCode::Char('G'), _) => {
                self.cursor = self
                    .next_selectable(self.rows.len().saturating_sub(1), -1)
                    .unwrap_or(0);
                self.follow_cursor();
            }
            // On a context boundary `z` opens the file up; everywhere else it
            // keeps its existing meaning. One key for "show me what is being
            // withheld", whatever is withholding it.
            (KeyCode::Char('z'), _)
                if matches!(
                    self.rows.get(self.cursor).map(|r| &r.kind),
                    Some(RowKind::ContextEdge(_, _))
                ) =>
            {
                self.expand_at_cursor();
            }
            (KeyCode::Char('z'), _) if self.view_mode == ViewMode::Files => {
                self.toggle_dir();
            }
            (KeyCode::Char('z'), _) => {
                if self.view_mode == ViewMode::Groups
                    && let Some(g) = self.groups().get(self.selected_group)
                {
                    let gid = g.id.clone();
                    if !self.folds_open.insert(gid.clone()) {
                        self.folds_open.remove(&gid);
                    }
                    self.rebuild_rows();
                }
            }
            (KeyCode::Char('n'), KeyModifiers::NONE) => self.jump_hunk(1),
            (KeyCode::Char('N'), _) => self.jump_hunk(-1),
            (KeyCode::Char('s'), KeyModifiers::NONE) => self.toggle_split(),
            (KeyCode::Char('v'), KeyModifiers::NONE) => self.toggle_file_view(),
            (KeyCode::Char('f'), KeyModifiers::NONE) => self.open_file_list(),
            (KeyCode::Char(' '), _) => self.toggle_reviewed(),
            (KeyCode::Char('c'), KeyModifiers::NONE) => {
                if let Some(h) = self.current_hunk() {
                    let mut ta = TextArea::default();
                    ta.set_block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" finding — Ctrl-s save · Esc cancel "),
                    );
                    self.mode = Mode::Editing(h, Box::new(ta));
                } else {
                    self.status = "move onto a hunk first".into();
                }
            }
            (KeyCode::Char('d'), KeyModifiers::NONE) => {
                if pending_d {
                    self.delete_finding_at_cursor();
                } else {
                    self.pending_d = true;
                }
            }
            (KeyCode::Char('y'), _) => {
                return vec![Effect::CopySummary(self.findings_summary())];
            }
            _ => {}
        }
        Vec::new()
    }

    /// Space marks reviewed. In the left pane that means the WHOLE selected
    /// entry (a group, or a file's classes); in the diff pane it means the
    /// class under the cursor.
    fn toggle_reviewed(&mut self) {
        let outcome = match self.focus {
            Focus::Groups => self.toggle_selected_entry(),
            Focus::Diff => match self.current_hunk() {
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

    /// Mark every class of the selected entry, in one write. Set semantics:
    /// a partly reviewed entry becomes fully reviewed rather than inverted.
    fn toggle_selected_entry(&mut self) -> Result<(), differential_engine::EngineError> {
        let keys: Vec<String> = match self.view_mode {
            ViewMode::Groups => match self.groups().get(self.selected_group) {
                Some(g) => g.class_keys.clone(),
                None => return Ok(()),
            },
            ViewMode::Files => self
                .files_of_tree_row(self.selected_file)
                .iter()
                .flat_map(|i| self.files()[*i].hunks.iter())
                .map(|h| self.session.hunk_class_key(h.index()).to_string())
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

    fn add_finding(&mut self, hunk_idx: usize, body: String) {
        match self.session.add_finding(hunk_idx, body) {
            Ok(_) => self.status = "finding saved".into(),
            Err(e) => self.status = format!("save failed: {e:#}"),
        }
        self.rebuild_rows();
    }

    fn delete_finding_at_cursor(&mut self) {
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
        let plan = self.session.plan();
        let mut out = String::new();
        for f in self
            .session
            .findings()
            .iter()
            .filter(|f| f.status == FindingStatus::Open)
        {
            // Findings anchor on digests, which survive regeneration; the
            // projection resolves one to its owning group.
            let label = plan
                .hunk_by_digest(&f.anchor.hunk_digest)
                .and_then(|h| plan.group_of_hunk(h))
                .map(|g| format!(" ({})", g.label))
                .unwrap_or_default();
            out.push_str(&format!(
                "- {}:{}{label}: {}\n",
                f.anchor.file, f.anchor.line, f.body
            ));
        }
        if out.is_empty() {
            out.push_str("(no open findings)\n");
        }
        out
    }

    // ------------------------------------------------------------- drawing

    pub fn draw(&self, frame: &mut Frame) {
        let panes = layout(frame.area());
        self.draw_groups(frame, panes.plan);
        self.draw_diff(frame, panes.diff);
        self.draw_status(frame, panes.status);

        match &self.mode {
            Mode::Editing(_, textarea) => {
                let area = bottom_rect(panes.body, 8);
                frame.render_widget(Clear, area);
                frame.render_widget(&**textarea, area);
            }
            Mode::Help => {
                let area = centered_rect(panes.body, 66, 30);
                frame.render_widget(Clear, area);
                frame.render_widget(help_paragraph(), area);
            }
            Mode::FileList { entries, selected } => {
                let height = (entries.len() as u16 + 2).min(panes.body.height);
                let area = centered_rect(panes.body, 70, height);
                let lines: Vec<Line> = entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| {
                        let mark = if e.reviewed { "✓" } else { " " };
                        let mut style = Style::default().fg(THEME.context_fg);
                        if i == *selected {
                            style = style.bg(THEME.selected_bg).add_modifier(Modifier::BOLD);
                        }
                        Line::from(Span::styled(
                            format!("{mark} +{:<4}-{:<4} {}", e.adds, e.dels, e.path),
                            style,
                        ))
                    })
                    .collect();
                frame.render_widget(Clear, area);
                frame.render_widget(
                    Paragraph::new(lines).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" files — enter jump · esc close "),
                    ),
                    area,
                );
            }
            Mode::Normal => {}
        }
    }

    fn draw_groups(&self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        let selected = self.selected_entry();

        // Entries render as blocks of lines, so scrolling counts ROWS, not
        // entries; keep the whole selected block in view.
        let mut blocks: Vec<Vec<Line>> = match self.view_mode {
            ViewMode::Groups => (0..self.groups().len())
                .map(|i| self.group_lines(i, i == selected))
                .collect(),
            ViewMode::Files => {
                let reviewed = self.session.reviewed_hunks();
                (0..self.tree.len())
                    .map(|i| self.tree_lines(i, i == selected, &reviewed))
                    .collect()
            }
        };
        // The selection reads as a row, not as highlighted text: pad its lines
        // out to the pane so the background runs to the right edge.
        let inner_w = area.width.saturating_sub(2) as usize;
        if let Some(block) = blocks.get_mut(selected) {
            for line in block.iter_mut() {
                pad_to_width(line, inner_w, THEME.selected_bg);
            }
        }
        // Scroll was decided in update; drawing only reads it. The heights
        // this pane renders must match what `plan_block_height` predicted, or
        // the two would disagree about where the selection is.
        debug_assert!(
            blocks
                .iter()
                .enumerate()
                .all(|(i, b)| b.len() == self.plan_block_height(i)),
            "plan block height disagrees with the rendered block"
        );
        let items: Vec<Line> = blocks
            .into_iter()
            .flatten()
            .skip(self.group_scroll)
            .take(inner_h)
            .collect();

        let orphans = self
            .session
            .findings()
            .iter()
            .filter(|f| f.status == FindingStatus::Orphaned)
            .count();
        let pane_name = match self.view_mode {
            ViewMode::Groups => "reading plan",
            ViewMode::Files => "files",
        };
        let title = if orphans > 0 {
            format!(" {pane_name} · ⚠ {orphans} orphaned finding(s) ")
        } else {
            format!(" {pane_name} ")
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if self.focus == Focus::Groups {
                Style::default().fg(THEME.header_fg)
            } else {
                Style::default().fg(THEME.gutter_fg)
            });
        frame.render_widget(Paragraph::new(items).block(block), area);
    }

    /// How a plan row relates to the selected one — what the connector line
    /// in the left gutter is drawing.
    pub fn relation_to_selected(&self, idx: usize) -> Relation {
        if idx == self.selected_group {
            return Relation::Selected;
        }
        // Both indices are guarded: `idx` comes from callers that iterate the
        // rendered blocks, but `relation_to_selected` is public and a stale
        // index should not be a panic.
        let (Some(sel), Some(row)) = (
            self.groups().get(self.selected_group),
            self.groups().get(idx),
        ) else {
            return Relation::None;
        };
        if sel.depends_on.iter().any(|d| d.id == row.id) {
            return Relation::Dependency;
        }
        Relation::None
    }

    /// Rows spanned by the selected group and everything it follows, so the
    /// connector is one continuous line.
    ///
    /// Usually that runs upward — foundation-first ordering puts a dependency
    /// above its consumer — but a broken cycle can put one below, and the span
    /// covers that too.
    fn edge_span(&self) -> (usize, usize) {
        let mut lo = self.selected_group;
        let mut hi = self.selected_group;
        for i in 0..self.groups().len() {
            if !matches!(self.relation_to_selected(i), Relation::None) {
                lo = lo.min(i);
                hi = hi.max(i);
            }
        }
        (lo, hi)
    }

    /// One group as 2–3 lines: title, counts, and what it follows.
    fn group_lines(&self, idx: usize, selected: bool) -> Vec<Line<'static>> {
        let g = &self.groups()[idx];
        let relation = self.relation_to_selected(idx);
        let (lo, hi) = self.edge_span();
        // The connector: a line from the selected group to each group it
        // follows, so what must be read first is visible without reading ids.
        let (head_glyph, head_style) = match relation {
            Relation::Selected => ("◆", Style::default().fg(THEME.header_fg)),
            Relation::Dependency => ("├", Style::default().fg(THEME.reviewed_fg)),
            Relation::None if idx > lo && idx < hi => ("│", Style::default().fg(THEME.gutter_fg)),
            Relation::None => (" ", Style::default().fg(THEME.gutter_fg)),
        };
        let tail_glyph = if idx >= lo && idx < hi { "│" } else { " " };
        let done =
            g.class_keys.iter().all(|k| self.session.is_reviewed(k)) && !g.class_keys.is_empty();
        // "?" rather than a tier letter: the back-fill was never classified.
        let tier = if g.unclassified {
            "?"
        } else {
            Theme::effort_glyph(g.effort)
        };
        let bg = |st: Style| {
            if selected {
                st.bg(THEME.selected_bg).add_modifier(Modifier::BOLD)
            } else {
                st
            }
        };
        let dim = bg(Style::default().fg(THEME.gutter_fg));

        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{head_glyph} "), head_style),
            Span::styled(
                // The id is what `after:` references, so it has to be visible.
                format!("{:>3} ", g.id),
                bg(Style::default().fg(THEME.gutter_fg)),
            ),
            Span::styled(
                format!("{tier} "),
                bg(THEME.effort_style(g.effort).add_modifier(Modifier::BOLD)),
            ),
            Span::styled(
                g.label.clone(),
                bg(Style::default().fg(if done {
                    THEME.reviewed_fg
                } else {
                    THEME.context_fg
                })),
            ),
            Span::styled(
                if done { "  ✓" } else { "" }.to_string(),
                bg(Style::default().fg(THEME.reviewed_fg)),
            ),
        ])];

        let role = Theme::role_suffix(g.role);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{tail_glyph} "),
                Style::default().fg(THEME.gutter_fg),
            ),
            Span::styled(format!("   {} files  ", g.n_files), dim),
            Span::styled(
                format!("+{}", g.counts.adds),
                bg(Style::default().fg(THEME.add_fg)),
            ),
            Span::styled(" ", dim),
            Span::styled(
                format!("−{}", g.counts.dels),
                bg(Style::default().fg(THEME.del_fg)),
            ),
            Span::styled(role.to_string(), dim),
        ]));
        if !g.depends_on.is_empty() {
            // "↓" marks a dependency that appears LATER in the plan: the two
            // groups depend on each other, so no order can satisfy both.
            let mut spans = vec![
                Span::styled(
                    format!("{tail_glyph} "),
                    Style::default().fg(THEME.gutter_fg),
                ),
                Span::styled("   after: ".to_string(), dim),
            ];
            for d in &g.depends_on {
                spans.push(Span::styled(
                    format!("{}{} ", d.id, if d.unsatisfied { "↓" } else { "" }),
                    if d.unsatisfied {
                        bg(Style::default().fg(THEME.skim_fg))
                    } else {
                        dim
                    },
                ));
            }
            lines.push(Line::from(spans));
        }
        lines
    }

    /// One tree row: a directory (with aggregate counts and a fold marker)
    /// or a file.
    fn tree_lines(
        &self,
        row: usize,
        selected: bool,
        reviewed: &HashSet<usize>,
    ) -> Vec<Line<'static>> {
        let entry = &self.tree[row];
        let bg = |st: Style| {
            if selected {
                st.bg(THEME.selected_bg).add_modifier(Modifier::BOLD)
            } else {
                st
            }
        };
        let indent = "  ".repeat(entry.depth);
        let files = self.files_of_tree_row(row);
        let (adds, dels): (usize, usize) = files
            .iter()
            .map(|i| (self.files()[*i].counts.adds, self.files()[*i].counts.dels))
            .fold((0, 0), |(a, d), (x, y)| (a + x, d + y));
        let hunks: Vec<usize> = files
            .iter()
            .flat_map(|i| self.files()[*i].hunks.iter().map(|h| h.index()))
            .collect();
        let done = !hunks.is_empty() && hunks.iter().all(|h| reviewed.contains(h));
        let mark = if done { "✓" } else { " " };
        let name_style = bg(Style::default().fg(if done {
            THEME.reviewed_fg
        } else {
            THEME.context_fg
        }));
        let dim = bg(Style::default().fg(THEME.gutter_fg));

        match &entry.kind {
            TreeKind::Dir { path } => {
                let glyph = if self.collapsed.contains(path) {
                    "▸"
                } else {
                    "▾"
                };
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                vec![Line::from(vec![
                    Span::styled(format!("{mark}{indent}{glyph} "), dim),
                    Span::styled(
                        format!("{name}/"),
                        bg(Style::default()
                            .fg(THEME.header_fg)
                            .add_modifier(Modifier::BOLD)),
                    ),
                    Span::styled("  ", dim),
                    Span::styled(format!("+{adds}"), bg(Style::default().fg(THEME.add_fg))),
                    Span::styled(" ", dim),
                    Span::styled(format!("−{dels}"), bg(Style::default().fg(THEME.del_fg))),
                ])]
            }
            TreeKind::File { file_idx } => {
                let f = &self.files()[*file_idx];
                let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
                let mut spans = vec![
                    Span::styled(format!("{mark}{indent}  "), dim),
                    Span::styled(name, name_style),
                    Span::styled("  ", dim),
                ];
                if f.hunks.is_empty() {
                    spans.push(Span::styled("(no text hunks)".to_string(), dim));
                } else {
                    spans.push(Span::styled(
                        format!("+{}", f.counts.adds),
                        bg(Style::default().fg(THEME.add_fg)),
                    ));
                    spans.push(Span::styled(" ", dim));
                    spans.push(Span::styled(
                        format!("−{}", f.counts.dels),
                        bg(Style::default().fg(THEME.del_fg)),
                    ));
                }
                vec![Line::from(spans)]
            }
        }
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        let inner_w = area.width.saturating_sub(2) as usize;
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(inner_h)
            .map(|(i, r)| {
                let on = i == self.cursor && self.focus == Focus::Diff && r.kind.selectable();
                let mut line = compose_row(&r.content, inner_w, on);
                if on {
                    // Span backgrounds win over a line style, so this colours
                    // exactly the rows that have no change colour of their own
                    // — on the rest, CURSOR_MARK in the gutter carries it.
                    line = line.style(Style::default().bg(THEME.cursor_bg));
                }
                line
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" diff ")
            .border_style(if self.focus == Focus::Diff {
                Style::default().fg(THEME.header_fg)
            } else {
                Style::default().fg(THEME.gutter_fg)
            });
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let total: usize = self.groups().iter().map(|g| g.class_keys.len()).sum();
        let done = self.session.reviewed_count().min(total);
        let open = self
            .session
            .findings()
            .iter()
            .filter(|f| f.status == FindingStatus::Open)
            .count();
        let text = format!(
            " {done}/{total} classes reviewed · {open} finding(s) · {} · j/k J/K nav · n/N hunk · space reviewed · c finding · s split · v files · z fold/expand · y copy summary · ? help · q quit",
            self.status
        );
        frame.render_widget(
            Paragraph::new(text).style(Style::default().bg(THEME.status_bg)),
            area,
        );
    }
}

/// Render a row at the given pane width.
///
/// Every diff row pads HERE rather than at build time: a background that runs
/// to the pane edge is a width question, and row counts must stay independent
/// of width or each resize would rebuild them.
fn compose_row(content: &RowContent, width: usize, cursor: bool) -> Line<'static> {
    match content {
        RowContent::Full(line) => line.clone(),
        RowContent::Unified(half) => Line::from(compose_half(half, width, cursor)),
        RowContent::Split { old, new } => {
            let lw = width.saturating_sub(1) / 2;
            let rw = width.saturating_sub(1).saturating_sub(lw);
            // The marker belongs on the leftmost gutter only.
            let mut spans = compose_half(old, lw, cursor);
            spans.push(Span::styled("│", Style::default().fg(THEME.gutter_fg)));
            spans.extend(compose_half(new, rw, false));
            Line::from(spans)
        }
    }
}

/// One side of a diff row at a known column width: the gutter, the content,
/// and padding out to the edge in whatever the row is filled with.
fn compose_half(half: &Half, width: usize, cursor: bool) -> Vec<Span<'static>> {
    let mut gutter = half.gutter.1.clone();
    if cursor && !gutter.is_empty() {
        // The leading cell is reserved for exactly this, so the substitution is
        // width-preserving and the pane never shifts as the cursor moves.
        gutter = format!("{CURSOR_MARK}{}", &gutter[1..]);
    }
    let used = UnicodeWidthStr::width(gutter.as_str());
    let rest = width.saturating_sub(used);
    let style = if cursor {
        half.gutter
            .0
            .fg(THEME.header_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        half.gutter.0
    };

    let mut spans = Vec::new();
    if !gutter.is_empty() {
        spans.push(Span::styled(gutter, style));
    }
    match half.fill {
        Fill::Bg(bg) => spans.extend(truncate_or_pad_spans(&half.pairs, rest, bg)),
        // An absent side is hatched rather than blank, so a line that does not
        // exist here cannot be mistaken for one that is empty.
        Fill::Hatch => spans.push(Span::styled(
            "╱".repeat(rest),
            Style::default().fg(THEME.hatch_fg),
        )),
        // A rule carries the row across the whole pane, separator column and
        // all — the row is about the file, not about one side of it.
        Fill::Rule(style) => {
            let used: usize = half.pairs.iter().map(|(_, t)| t.width()).sum();
            if used >= rest {
                spans.extend(truncate_or_pad_spans(&half.pairs, rest, style));
            } else {
                spans.extend(half.pairs.iter().map(|(s, t)| Span::styled(t.clone(), *s)));
                spans.push(Span::styled("─".repeat(rest - used), style));
            }
        }
    }
    spans
}

/// Extend `line` with blank, styled cells so a selection background covers
/// the full row width. Trailing padding only — the leading connector column
/// keeps its own styling.
fn pad_to_width(line: &mut Line<'static>, width: usize, bg: ratatui::style::Color) {
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

fn bottom_rect(area: Rect, height: u16) -> Rect {
    let h = height.min(area.height);
    Rect {
        x: area.x,
        y: area.y + area.height - h,
        width: area.width,
        height: h,
    }
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn help_paragraph() -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::from("differential review"),
        Line::from(""),
        Line::from("  j/k        move (groups pane: switch group)"),
        Line::from("  J/K { }    previous / next group"),
        Line::from("  tab/enter  switch pane focus"),
        Line::from("  ctrl-d/u   half page"),
        Line::from("  g/G        top / bottom"),
        Line::from("  z          on a ── boundary row: show more of the file"),
        Line::from("             elsewhere: unfold skim remainder / noise"),
        Line::from("  n/N        next / previous hunk"),
        Line::from(""),
        Line::from("  plan rows: <id> <tier> label · after: what it follows"),
        Line::from("  the line links the selected group to what it follows;"),
        Line::from("  ↓ marks a dependency listed later (mutual dependency)"),
        Line::from("  colour marks the change; there are no -/+ columns,"),
        Line::from("  and ╱╱╱ is a line the other side does not have"),
        Line::from("  s          toggle unified / split diff"),
        Line::from("  v          toggle reading plan / file view"),
        Line::from("  f          file list of the current view (enter jumps)"),
        Line::from("  space      toggle class reviewed"),
        Line::from("  c          add finding on current hunk"),
        Line::from("  dd         delete finding under cursor"),
        Line::from("  y          copy findings summary to clipboard"),
        Line::from("  q          quit (state is saved)"),
        Line::from(""),
        Line::from("press any key to close"),
    ])
    .block(Block::default().borders(Borders::ALL).title(" help "))
}
