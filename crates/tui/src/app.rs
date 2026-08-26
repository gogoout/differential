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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use super::rows::{
    Border, DiffMode, Fill, GroupContext, Half, Row, RowContent, RowFactory, RowKind, RowsContext,
    build_dir_rows, build_file_rows, build_group_rows, pill,
};
use super::theme::{THEME, Theme};
use super::vendor::text_utils::truncate_or_pad_spans;
use super::window::{Expansion, Side};

const SCROLL_MARGIN: usize = 3;

/// How far a context boundary's rule reaches either side of its label.
///
/// A stub, not a line across the screen: the row is a note about what is
/// missing, and a full-width rule read as a chapter break in a file that has
/// not ended.
const RULE_ARM: usize = 10;

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

/// The reviewer's panes: a fixed-width plan pane, the detail, a status row.
pub struct Panes {
    pub body: Rect,
    pub plan: Rect,
    pub detail: Rect,
    pub status: Rect,
}

/// The one layout. `draw` places widgets with it and the event loop measures
/// with it, so the two can never disagree about how tall the detail pane is.
///
/// Focus does NOT enter into it. The overviews each focus brings up float over
/// a pane rather than splitting one, which is what lets the pane heights stay a
/// function of the terminal alone — and lets a key never change them.
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
        detail: panes[1],
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
    pub detail_rows: usize,
    pub plan_rows: usize,
}

impl Viewport {
    pub fn measure(area: Rect) -> Self {
        let panes = layout(area);
        Viewport {
            // Every pane is bordered.
            detail_rows: panes.detail.height.saturating_sub(2) as usize,
            plan_rows: panes.plan.height.saturating_sub(2) as usize,
        }
    }
}

impl Default for Viewport {
    /// Before the first measurement.
    fn default() -> Self {
        Viewport {
            detail_rows: 24,
            plan_rows: 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Groups,
    Detail,
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

/// One row of the group map — the document's file tree with everything the
/// selected group does not touch folded away.
///
/// A separate row type rather than a filtered `Vec<TreeEntry>`: the map folds
/// on a different question from the file view (does the GROUP touch this?
/// rather than did the reader press `z`?), and the two folds must not share
/// state — folding here would move the file view's cursor.
enum MapRow {
    /// A directory the group touches. Its children follow.
    Dir {
        depth: usize,
        name: String,
    },
    /// A directory the group does not touch, and everything under it. A chain
    /// of single-child directories is joined, so one row says `a/b/c/`.
    Folded {
        depth: usize,
        name: String,
        files: usize,
    },
    File {
        depth: usize,
        file_idx: usize,
    },
    /// A run of files the group does not touch, inside one it does.
    More {
        depth: usize,
        files: usize,
    },
}

impl MapRow {
    fn depth(&self) -> usize {
        match self {
            MapRow::Dir { depth, .. }
            | MapRow::Folded { depth, .. }
            | MapRow::File { depth, .. }
            | MapRow::More { depth, .. } => *depth,
        }
    }
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
    /// The overviews' inputs, computed when the rows are. Both used to be
    /// derived inside `draw`, which meant an O(hunks) scan with a string
    /// compare per hunk on EVERY frame — enough to make a large review feel
    /// stuck on each keypress.
    map_files: HashSet<usize>,
    listed_files: Vec<usize>,
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
            map_files: HashSet::new(),
            listed_files: Vec::new(),
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
        self.rebuild_overviews();
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
        let h = self.viewport.detail_rows.max(MIN_VIEWPORT);
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
    fn expand_at_cursor(&mut self) {
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
    fn file_list(&self) -> Vec<usize> {
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
    fn rebuild_overviews(&mut self) {
        self.listed_files = self.file_list();
        self.map_files = self.files_of_selected_group();
    }

    /// The row index of the file header the cursor is under.
    ///
    /// Walked backwards from the cursor, because a diff row does not name its
    /// file — the header above it does. Both the flat list's marker and the
    /// sticky header need this, so it is one function.
    fn file_header_above(&self, from: usize) -> Option<usize> {
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
        // No status line: the pane in front of the reader IS the answer, and a
        // message saying what they can see costs the footer its one slot for
        // things they cannot.
        self.status.clear();
    }

    /// Open or close the selected group's folded remainder — the skim group's
    /// hunks past its exemplars, or a noise group entire.
    fn toggle_group_fold(&mut self) {
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
                        self.focus = Focus::Detail;
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
                    Focus::Groups => Focus::Detail,
                    Focus::Detail => Focus::Groups,
                }
            }
            (KeyCode::Enter, _) if self.focus == Focus::Groups => {
                // Enter opens a directory rather than jumping to the diff.
                if !(self.view_mode == ViewMode::Files && self.toggle_dir()) {
                    self.focus = Focus::Detail;
                }
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => match self.focus {
                Focus::Groups => self.select_entry(self.selected_entry() + 1),
                Focus::Detail => self.move_cursor(1),
            },
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => match self.focus {
                Focus::Groups => self.select_entry(self.selected_entry().saturating_sub(1)),
                Focus::Detail => self.move_cursor(-1),
            },
            (KeyCode::Char('J'), _) | (KeyCode::Char('}'), _) => {
                self.select_entry(self.selected_entry() + 1)
            }
            (KeyCode::Char('K'), _) | (KeyCode::Char('{'), _) => {
                self.select_entry(self.selected_entry().saturating_sub(1))
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                let h = self.viewport.detail_rows.max(MIN_VIEWPORT) / 2;
                self.cursor = (self.cursor + h).min(self.rows.len().saturating_sub(1));
                self.cursor = self.next_selectable(self.cursor, -1).unwrap_or(self.cursor);
                self.follow_cursor();
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let h = self.viewport.detail_rows.max(MIN_VIEWPORT) / 2;
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
                    Some(RowKind::ContextEdge { .. })
                ) =>
            {
                self.expand_at_cursor();
            }
            (KeyCode::Char('z'), _) if self.view_mode == ViewMode::Files => {
                self.toggle_dir();
            }
            (KeyCode::Char('z'), _) => self.toggle_group_fold(),
            (KeyCode::Char('n'), KeyModifiers::NONE) => self.jump_hunk(1),
            (KeyCode::Char('N'), _) => self.jump_hunk(-1),
            (KeyCode::Char('s'), KeyModifiers::NONE) => self.toggle_split(),
            // One key for files, acting on the pane it is pressed in. In the
            // left pane that is which list of files you are reading — the
            // plan or the tree; in the diff pane it is which file you want to
            // be looking at. `v` used to switch the left pane from either
            // side, which meant a key in one pane silently rearranged the
            // other.
            (KeyCode::Char('f'), KeyModifiers::NONE) => match self.focus {
                Focus::Groups => self.toggle_file_view(),
                Focus::Detail => self.open_file_list(),
            },
            (KeyCode::Char(' '), _) => self.toggle_reviewed(),
            (KeyCode::Char('c'), KeyModifiers::NONE) => {
                if let Some(h) = self.current_hunk() {
                    let hunk = &self.session.doc().hunks[h];
                    // Name what is being annotated: findings anchor to a hunk,
                    // and a note whose subject you cannot see is a note you
                    // have to trust yourself to have written carefully.
                    let file = hunk.file.rsplit('/').next().unwrap_or(&hunk.file);
                    let lines = if hunk.new_count > 1 {
                        format!(
                            "L{}-{}",
                            hunk.new_start,
                            hunk.new_start + hunk.new_count - 1
                        )
                    } else {
                        format!("L{}", hunk.new_start)
                    };
                    let mut ta = TextArea::default();
                    ta.set_block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(THEME.header_fg))
                            .title(format!(" {file} · {lines} ")),
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
        self.draw_diff(frame, panes.detail);
        self.draw_status(frame, panes.status);

        // Each focus FLOATS a map of the other pane rather than replacing or
        // splitting one. The diff carries on underneath, so browsing the plan
        // still previews what entering it will show — and pane heights stay a
        // function of the terminal, never of a key.
        //
        // Only in the reading plan, though. The file view's left pane IS a file
        // tree: a floating map of one group would name a group nothing is
        // selecting, and a floating file list would be the pane behind it.
        if self.view_mode == ViewMode::Groups {
            match self.focus {
                Focus::Groups => self.draw_group_map(frame, panes.detail),
                Focus::Detail => self.draw_file_list(frame, panes.plan),
            }
        }

        match &self.mode {
            Mode::Editing(_, textarea) => {
                // A float over the diff, not a strip pinned to the bottom: a
                // finding is about the lines you can still see around it.
                let area = centered_rect(panes.body, panes.body.width * 3 / 5, 10);
                frame.render_widget(Clear, area);
                frame.render_widget(&**textarea, area);
                // The keys go INSIDE the box, on its last row, where a footer
                // belongs — the title says what you are annotating.
                let footer = Rect {
                    x: area.x + 1,
                    y: area.y + area.height.saturating_sub(2),
                    width: area.width.saturating_sub(2),
                    height: 1,
                };
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("  ctrl-s ", Style::default().fg(THEME.header_fg)),
                        Span::styled("save", Style::default().fg(THEME.context_fg)),
                        Span::styled("  │  ", Style::default().fg(THEME.gutter_fg)),
                        Span::styled("esc ", Style::default().fg(THEME.header_fg)),
                        Span::styled("cancel", Style::default().fg(THEME.context_fg)),
                        Span::styled("  │  ", Style::default().fg(THEME.gutter_fg)),
                        Span::styled("enter ", Style::default().fg(THEME.header_fg)),
                        Span::styled("newline", Style::default().fg(THEME.context_fg)),
                    ]))
                    .alignment(ratatui::layout::Alignment::Center),
                    footer,
                );
            }
            Mode::Help => {
                let area = centered_rect(panes.body, 62, 19);
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
                        // The counts say added and removed here too — they were
                        // one grey run, which is the one thing a file list is
                        // scanned for.
                        let on = |c| {
                            Style::default().fg(c).patch(
                                style
                                    .bg
                                    .map_or(Style::default(), |b| Style::default().bg(b)),
                            )
                        };
                        Line::from(vec![
                            Span::styled(format!("{mark} "), style),
                            Span::styled(format!("+{:<4}", e.adds), on(THEME.add_fg)),
                            Span::styled(format!("−{:<4} ", e.dels), on(THEME.del_fg)),
                            Span::styled(e.path.clone(), style),
                        ])
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
                let guides = tree_guides(&self.tree);
                (0..self.tree.len())
                    .map(|i| self.tree_lines(i, i == selected, &reviewed, &guides[i]))
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
        let block = pane(title, self.focus == Focus::Groups);
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
        //
        // The tick wears the arm the file tree's guides wear (`├─`, `└─`), so
        // it reaches the title it points at rather than stopping a cell short
        // — the two guides sit a pane apart and had no reason to differ.
        //
        // One colour, the pane's own border grey. The connector is chrome: it
        // says which rows are tied together, and the rows themselves say what
        // they are. Two accents in one column made the gutter compete with the
        // labels beside it.
        let head_glyph = match relation {
            Relation::Selected => "◆─",
            Relation::Dependency if idx == hi => "└─",
            Relation::Dependency => "├─",
            Relation::None if idx > lo && idx < hi => "│ ",
            Relation::None => "  ",
        };
        let head_style = Style::default().fg(THEME.gutter_fg);
        let tail_glyph = if idx >= lo && idx < hi { "│ " } else { "  " };
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
            Span::styled(head_glyph.to_string(), head_style),
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

        let mut counts = vec![
            Span::styled(tail_glyph.to_string(), Style::default().fg(THEME.gutter_fg)),
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
        ];
        // The ordering role is a fact about the group, like the class on a hunk
        // header — so it wears the same pill, in the muted colours, rather than
        // trailing off the line as dim text.
        if let Some(r) = g.role {
            let (fg, pill_bg) = THEME.pill();
            counts.push(Span::styled(" ", dim));
            counts.extend(
                pill(
                    vec![(fg, differential_engine::plan::role_name(r).to_string())],
                    pill_bg,
                )
                .into_iter()
                .map(|(st, t)| Span::styled(t, st)),
            );
        }
        lines.push(Line::from(counts));
        if !g.depends_on.is_empty() {
            // Every id reads the same. A dependency the ordering could not
            // honour used to wear a `↓` and a colour of its own, which put a
            // warning on the row for something the reader can do nothing about
            // — and the connector already shows it, by running DOWN from the
            // selected group instead of up.
            let mut spans = vec![
                Span::styled(tail_glyph.to_string(), Style::default().fg(THEME.gutter_fg)),
                Span::styled("   after: ".to_string(), dim),
            ];
            for d in &g.depends_on {
                spans.push(Span::styled(format!("{} ", d.id), dim));
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
        // Passed in, not computed here: this runs once per visible row, and
        // building the whole tree's connectors inside it would be quadratic on
        // every frame.
        guide: &str,
    ) -> Vec<Line<'static>> {
        let entry = &self.tree[row];
        let bg = |st: Style| {
            if selected {
                st.bg(THEME.selected_bg).add_modifier(Modifier::BOLD)
            } else {
                st
            }
        };
        let indent = guide;
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
                    Span::styled(format!("{mark}{indent}"), dim),
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

    /// The flat file list, floating over the foot of the plan pane: where you
    /// are, and how much is left.
    fn draw_file_list(&self, frame: &mut Frame, plan: Rect) {
        let files_len = self.listed_files.len();
        if files_len == 0 {
            return;
        }
        let h = (files_len as u16 + 2)
            .min(plan.height.saturating_sub(2))
            .max(3);
        let area = Rect {
            x: plan.x,
            y: plan.y + plan.height.saturating_sub(h),
            width: plan.width,
            height: h,
        };
        frame.render_widget(Clear, area);
        self.draw_file_list_in(frame, area);
    }

    fn draw_file_list_in(&self, frame: &mut Frame, area: Rect) {
        let reviewed = self.session.reviewed_hunks();
        let here = self.file_at_cursor();
        let files = &self.listed_files;
        let inner_w = area.width.saturating_sub(2) as usize;
        // Keep the current file in view; the list can outrun its pane.
        let h = area.height.saturating_sub(2) as usize;
        let at = here.and_then(|i| files.iter().position(|&f| f == i));
        let scroll = at.map_or(0, |n| n.saturating_sub(h.saturating_sub(1)));

        let mut lines: Vec<Line> = files
            .iter()
            .skip(scroll)
            .take(h)
            .map(|&i| {
                let f = &self.files()[i];
                let on = here == Some(i);
                let done =
                    !f.hunks.is_empty() && f.hunks.iter().all(|hk| reviewed.contains(&hk.index()));
                let base = Style::default().fg(if done {
                    THEME.reviewed_fg
                } else {
                    THEME.context_fg
                });
                let style = if on {
                    base.bg(THEME.selected_bg).add_modifier(Modifier::BOLD)
                } else {
                    base
                };
                let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                // No marker glyph: the row the reader is on is the one lit
                // edge to edge, which says it in the one place they are
                // already looking.
                let mut line = Line::from(vec![
                    Span::styled("  ".to_string(), style),
                    Span::styled(name.to_string(), style),
                ]);
                if on {
                    pad_to_width(&mut line, inner_w, THEME.selected_bg);
                }
                line
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no files)",
                Style::default().fg(THEME.gutter_fg),
            )));
        }
        let title = match at {
            Some(n) => format!(" file {} of {} ", n + 1, files.len()),
            None => format!(" {} files ", files.len()),
        };
        frame.render_widget(Paragraph::new(lines).block(pane(title, true)), area);
    }

    /// The right pane while the plan has focus: the whole document's file tree
    /// with the selected group's files lit, so what a group spans is one look
    /// rather than a walk through its hunks.
    ///
    /// Deliberately not interactive. It is a map; a second cursor in a second
    /// pane is a thing to explain and to get wrong.
    fn draw_group_map(&self, frame: &mut Frame, detail: Rect) {
        // Below the group's header block, so its full label and description —
        // which the 40-column plan pane truncates — stay readable, and the diff
        // carries on beneath the float.
        let header = self
            .rows
            .iter()
            .take_while(|r| matches!(r.kind, RowKind::GroupHeader | RowKind::Blank))
            .count()
            .min(6) as u16;
        let top = detail.y + 1 + header;
        let rows = self.map_rows();
        let area = Rect {
            x: detail.x + 1,
            y: top,
            width: detail.width.saturating_sub(2),
            height: detail
                .height
                .saturating_sub(header + 2)
                .min(rows.len() as u16 + 2)
                .max(3),
        };
        frame.render_widget(Clear, area);
        let inner_h = area.height.saturating_sub(2) as usize;
        let dim = Style::default().fg(THEME.gutter_fg);
        let guides = guides_for_depths(&rows.iter().map(MapRow::depth).collect::<Vec<_>>());
        let lines: Vec<Line> = rows
            .iter()
            .zip(&guides)
            .map(|(row, guide)| {
                let lead = Span::styled(format!("  {guide}"), dim);
                match row {
                    MapRow::Dir { name, .. } => Line::from(vec![
                        lead,
                        Span::styled(format!("{name}/"), Style::default().fg(THEME.context_fg)),
                    ]),
                    // A folded directory keeps the file view's own fold marker,
                    // and says how much it stands for — a row that hid six
                    // files without saying so would read as a directory the
                    // document happens to have nothing in.
                    MapRow::Folded { name, files, .. } => Line::from(vec![
                        lead,
                        Span::styled(format!("▸ {name}/"), dim),
                        Span::styled(
                            format!("  {files} file{}", if *files == 1 { "" } else { "s" }),
                            dim,
                        ),
                    ]),
                    MapRow::More { files, .. } => {
                        Line::from(vec![lead, Span::styled(format!("… {files} more"), dim)])
                    }
                    // Every file row IS one the group touches — the rest fold
                    // into a `…` row — so it is always lit, and the dot and
                    // the counts are unconditional.
                    MapRow::File { file_idx, .. } => {
                        let f = &self.files()[*file_idx];
                        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                        let style = Style::default()
                            .fg(THEME.context_fg)
                            .add_modifier(Modifier::BOLD);
                        // The marker sits WITH the name, not out in a column of
                        // its own — a dot at the far left of a deep tree points
                        // at nothing.
                        Line::from(vec![
                            lead,
                            Span::styled("● ".to_string(), Style::default().fg(THEME.header_fg)),
                            Span::styled(name.to_string(), style),
                            Span::styled("  ", style),
                            Span::styled(
                                format!("+{}", f.counts.adds),
                                Style::default().fg(THEME.add_fg),
                            ),
                            Span::styled(" ", style),
                            Span::styled(
                                format!("−{}", f.counts.dels),
                                Style::default().fg(THEME.del_fg),
                            ),
                        ])
                    }
                }
            })
            .collect();

        // Folding usually leaves the whole map on screen, so this is the rare
        // case: a group touching more files than the float is tall. Scroll to
        // the first one it touches, and no further.
        let first = rows
            .iter()
            .position(|r| matches!(r, MapRow::File { .. }))
            .unwrap_or(0);
        let scroll = first.saturating_sub(inner_h.saturating_sub(1));

        let title = match self.groups().get(self.selected_group) {
            Some(g) => format!(
                " files in {} · {} of {} ",
                g.id,
                self.map_files.len(),
                self.files().len()
            ),
            None => " files ".to_string(),
        };
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(scroll)
                    .take(inner_h)
                    .collect::<Vec<_>>(),
            )
            .block(pane(title, true)),
            area,
        );
    }

    /// File indices the selected group touches, via the projection rather than
    /// by re-deriving what belongs to a group here.
    fn files_of_selected_group(&self) -> HashSet<usize> {
        let plan = self.session.plan();
        let Some(g) = self.groups().get(self.selected_group) else {
            return HashSet::new();
        };
        let id = g.id.as_str();
        self.files()
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.hunks
                    .iter()
                    .any(|h| plan.group_of_hunk(*h).is_some_and(|owner| owner.id == id))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The group map's rows: the document's tree with everything the selected
    /// group does not touch folded away.
    ///
    /// The float has to fit its box, and a document of any size otherwise runs
    /// past the bottom of it. Folding on the group answers the question the
    /// map is asked — what does this group span — with the rest of the tree
    /// present as context rather than as rows.
    ///
    /// Reads `self.tree` and never writes it: the file view's left pane and
    /// its cursor are the same rows.
    fn map_rows(&self) -> Vec<MapRow> {
        let tree = &self.tree;
        let mine = &self.map_files;
        let n = tree.len();

        // A row is live if it IS a file the group touches, or holds one.
        let mut live = vec![false; n];
        for (i, e) in tree.iter().enumerate() {
            let TreeKind::File { file_idx } = &e.kind else {
                continue;
            };
            if !mine.contains(file_idx) {
                continue;
            }
            live[i] = true;
            // Light every ancestor: the rows above it with a smaller depth.
            let mut depth = e.depth;
            for j in (0..i).rev() {
                if tree[j].depth < depth {
                    live[j] = true;
                    depth = tree[j].depth;
                    if depth == 0 {
                        break;
                    }
                }
            }
        }

        // First row past a subtree: the next one at or above its own depth.
        let end_of = |i: usize| {
            tree[i + 1..]
                .iter()
                .position(|e| e.depth <= tree[i].depth)
                .map_or(n, |k| i + 1 + k)
        };
        let leaf = |i: usize| {
            let path = match &tree[i].kind {
                TreeKind::Dir { path } => path.as_str(),
                TreeKind::File { .. } => return String::new(),
            };
            path.rsplit('/').next().unwrap_or(path).to_string()
        };

        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            let depth = tree[i].depth;
            match &tree[i].kind {
                TreeKind::Dir { .. } if live[i] => {
                    out.push(MapRow::Dir {
                        depth,
                        name: leaf(i),
                    });
                    i += 1;
                }
                TreeKind::Dir { .. } => {
                    // Absorb a chain of single-child directories, so a deep
                    // path the group never enters costs one row, not four.
                    let end = end_of(i);
                    let mut name = leaf(i);
                    let mut cur = i;
                    loop {
                        let mut kids =
                            (cur + 1..end).filter(|&j| tree[j].depth == tree[cur].depth + 1);
                        let (Some(only), None) = (kids.next(), kids.next()) else {
                            break;
                        };
                        if !matches!(tree[only].kind, TreeKind::Dir { .. }) {
                            break;
                        }
                        name.push('/');
                        name.push_str(&leaf(only));
                        cur = only;
                    }
                    out.push(MapRow::Folded {
                        depth,
                        name,
                        files: self.files_of_tree_row(i).len(),
                    });
                    i = end;
                }
                TreeKind::File { file_idx } if mine.contains(file_idx) => {
                    out.push(MapRow::File {
                        depth,
                        file_idx: *file_idx,
                    });
                    i += 1;
                }
                TreeKind::File { .. } => {
                    // A run of files the group misses, side by side, is one row.
                    let mut j = i;
                    while j < n
                        && tree[j].depth == depth
                        && matches!(&tree[j].kind, TreeKind::File { file_idx }
                                    if !mine.contains(file_idx))
                    {
                        j += 1;
                    }
                    out.push(MapRow::More {
                        depth,
                        files: j - i,
                    });
                    i = j;
                }
            }
        }
        out
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        let inner_w = area.width.saturating_sub(2) as usize;
        // Which box is lit. Only one at a time: a screenful of accents is a
        // screenful of nothing, so every other box is muted to the gutter.
        let active = self.current_hunk();
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(inner_h)
            .map(|(i, r)| {
                let on = i == self.cursor && self.focus == Focus::Detail && r.kind.selectable();
                // A hunk's pill follows its edge, so the marker and the run
                // below it read as one thing — and which is lit is a cursor
                // question, decided here rather than when the row was built.
                let marker = match (r.border, &r.kind) {
                    (Some(b), RowKind::HunkHeader { .. }) if active == Some(b.hunk) => {
                        b.active_style.fg.map_or(Marker::Idle(&r.idle), Marker::Lit)
                    }
                    (_, RowKind::HunkHeader { .. }) => Marker::Idle(&r.idle),
                    _ => Marker::None,
                };
                // How to work this row, on the one row it can be worked from.
                let hint = on.then_some(r.hint.as_ref()).flatten();
                let mut line = compose_row(&r.content, inner_w, on, marker, hint);
                if on {
                    // Span backgrounds win over a line style, so this colours
                    // exactly the rows that have no change colour of their own
                    // — on the rest, the brightened gutter block carries it.
                    line = line.style(Style::default().bg(THEME.cursor_bg));
                }
                line
            })
            .collect();

        // Scrolled past a file's header, pin it to the top row. It costs a row
        // only while the filename would otherwise be off-screen, which is
        // exactly when a long file stops saying which file it is.
        let mut lines = lines;
        if let Some(header) = self
            .file_header_above(self.scroll)
            .filter(|&h| h < self.scroll)
            && let Some(first) = lines.first_mut()
        {
            *first = compose_row(
                &self.rows[header].content,
                inner_w,
                false,
                Marker::None,
                None,
            )
            .style(Style::default().bg(THEME.sticky_bg));
        }

        let block = pane(" detail ".to_string(), self.focus == Focus::Detail);
        frame.render_widget(Paragraph::new(lines).block(block), area);

        // A hunk's edge shares the pane's left border column rather than
        // sitting a cell inside it: no width lost, and no second vertical line
        // a cell away from the first. Drawn over the block, so it comes after.
        let buf = frame.buffer_mut();
        let on_cursor = |i: usize| i == self.cursor && self.focus == Focus::Detail;
        for (n, row) in self.rows.iter().skip(self.scroll).take(inner_h).enumerate() {
            let y = area.y + 1 + n as u16;
            // A control's button takes the same column a hunk's edge would, and
            // lightens with the band it belongs to.
            if let Some(glyph) = row.button {
                let band = Style::default().fg(THEME.hint_fg).bg(THEME.hint_bg);
                let cell = &mut buf[(area.x, y)];
                cell.set_symbol(glyph);
                cell.set_style(if on_cursor(self.scroll + n) {
                    THEME.lit_band(band)
                } else {
                    band
                });
                continue;
            }
            let Some(border) = row.border else { continue };
            let cell = &mut buf[(area.x, y)];
            cell.set_symbol(border.glyph().encode_utf8(&mut [0u8; 4]));
            cell.set_style(chrome(border, active));
        }

        // The cursor's bar, in the cell just inside the frame. The gutter block
        // says which LINE the cursor is on, but only a diff row has a gutter:
        // on a header, a fold or a boundary the cursor was a faint tint and
        // nothing else. The bar is on every selectable row, so the cursor is
        // one thing to look for rather than two.
        //
        // Keeps the cell's own background — over a lit gutter it stands on the
        // change colour rather than punching a hole in it.
        if self.focus == Focus::Detail
            && let Some(n) = self.cursor.checked_sub(self.scroll)
            && n < inner_h
            && self
                .rows
                .get(self.cursor)
                .is_some_and(|r| r.kind.selectable())
        {
            let cell = &mut buf[(area.x + 1, area.y + 1 + n as u16)];
            cell.set_symbol(CURSOR_BAR);
            cell.set_fg(THEME.header_fg);
            cell.modifier.insert(Modifier::BOLD);
        }
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

        let bar = Style::default().bg(THEME.status_bg);
        let (ink, fill) = THEME.pill();
        // Progress and findings are FACTS about the review, so they wear the
        // same pill a group's role and a hunk's class wear rather than trailing
        // off as a run of grey words. Each takes its own colour once it has
        // something to say: green when everything is read, magenta when
        // anything is filed.
        let tally = |lit: bool, accent: Color, text: String| {
            pill(vec![(if lit { accent } else { ink }, text)], fill)
                .into_iter()
                .map(|(st, t)| Span::styled(t, st))
        };
        let mut left = vec![Span::styled(" ", bar)];
        left.extend(tally(
            total > 0 && done == total,
            THEME.reviewed_fg,
            format!("{done}/{total} classes reviewed"),
        ));
        left.push(Span::styled(" ", bar));
        left.extend(tally(
            open > 0,
            THEME.finding_fg,
            format!("{open} finding{}", if open == 1 { "" } else { "s" }),
        ));
        if !self.status.is_empty() {
            left.push(Span::styled(
                format!("  {}", self.status),
                bar.fg(THEME.context_fg),
            ));
        }

        // Two keys, against the right edge. The rest moved to `?`, which is the
        // one place a full list belongs — a footer naming ten keys is a wall
        // the reader stops seeing, and it named them in a different order and a
        // different wording from the modal that also named them.
        let right = vec![
            Span::styled("? ", bar.fg(THEME.header_fg)),
            Span::styled("help", bar.fg(THEME.context_fg)),
            Span::styled("  ·  ", bar.fg(THEME.gutter_fg)),
            Span::styled("q ", bar.fg(THEME.header_fg)),
            Span::styled("quit", bar.fg(THEME.context_fg)),
            Span::styled(" ", bar),
        ];

        let used = |spans: &[Span]| -> usize {
            spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum()
        };
        let gap = (area.width as usize)
            .saturating_sub(used(&left) + used(&right))
            .max(1);
        let mut spans = left;
        spans.push(Span::styled(" ".repeat(gap), bar));
        spans.extend(right);
        frame.render_widget(Paragraph::new(Line::from(spans)).style(bar), area);
    }
}

/// Render a row at the given pane width.
///
/// Every diff row pads HERE rather than at build time: a background that runs
/// to the pane edge is a width question, and row counts must stay independent
/// of width or each resize would rebuild them.
fn compose_row(
    content: &RowContent,
    width: usize,
    cursor: bool,
    marker: Marker<'_>,
    hint: Option<&(Style, String)>,
) -> Line<'static> {
    match content {
        RowContent::Full(line) => line.clone(),
        RowContent::Unified(half) => {
            // A hunk's pill stays in the muted palette whether the cursor is in
            // it or not. What changes is ONE cell: the pill's leading pad
            // becomes a bar in the hunk's own accent, so the marker and the
            // edge below it still read as one thing.
            //
            // Filling the whole pill said the same thing far more loudly — a
            // block of colour the eye went to before the code — and it forced
            // every ink on the pill to have a second, darker twin for the lit
            // background. One cell needs no twins.
            let repainted;
            let half = if !matches!(marker, Marker::None) || hint.is_some() {
                let mut pairs = half.pairs.clone();
                match marker {
                    // Nothing but the band. A pill on every header was a run of
                    // labels down the page competing with the code they label;
                    // the one worth reading is the hunk you are in, and moving
                    // into a hunk is what asks for it.
                    Marker::Idle(marks) => pairs = marks.to_vec(),
                    Marker::Lit(fg) if !pairs.is_empty() => {
                        pairs[0] = (
                            pairs[0].0.fg(fg).add_modifier(Modifier::BOLD),
                            PILL_BAR.to_string(),
                        );
                    }
                    _ => {}
                }
                // Straight after the label, not out at the pane's edge: the
                // reader's eye is on the words the row carries, and a key
                // parked a screen away from them is a key they have to go and
                // look for.
                if let Some((st, text)) = hint {
                    pairs.push((*st, text.clone()));
                }
                repainted = Half {
                    gutter: half.gutter.clone(),
                    pairs,
                    fill: half.fill,
                };
                &repainted
            } else {
                half
            };
            Line::from(compose_half(half, width, cursor))
        }
        RowContent::Split { old, new } => {
            let lw = width.saturating_sub(1) / 2;
            let rw = width.saturating_sub(1).saturating_sub(lw);
            // Both gutters light: a split row IS one row, and a cursor that
            // showed on one side only read as a cursor on that side's line.
            let mut spans = compose_half(old, lw, cursor);
            spans.push(Span::styled("│", Style::default().fg(THEME.gutter_fg)));
            spans.extend(compose_half(new, rw, cursor));
            Line::from(spans)
        }
    }
}

/// What colour a hunk's box and band take right now.
///
/// Deliberately not a flag on the row: the cursor moves without rebuilding
/// rows, so "is this the active hunk" cannot be decided when the row is built.
/// The row carries the colour it WOULD take, and drawing chooses.
fn chrome(border: Border, active: Option<usize>) -> Style {
    if active == Some(border.hunk) {
        border.active_style
    } else {
        Style::default().fg(THEME.gutter_fg)
    }
}

/// One side of a diff row at a known column width: the gutter, the content,
/// and padding out to the edge in whatever the row is filled with.
fn compose_half(half: &Half, width: usize, cursor: bool) -> Vec<Span<'static>> {
    let gutter = half.gutter.text.clone();
    let used = UnicodeWidthStr::width(gutter.as_str());
    let rest = width.saturating_sub(used);
    // The cursor IS the line-number block, brightened. There is no marker glyph
    // to make room for, so the cell never changes width and the pane never
    // shifts sideways as the cursor moves.
    let style = if cursor {
        half.gutter.cursor
    } else {
        half.gutter.style
    };

    let mut spans = Vec::new();
    if !gutter.is_empty() {
        spans.push(Span::styled(gutter, style));
    }
    // A boundary band carries its own colour the whole way across, so the row
    // tint that marks the cursor everywhere else never showed through it. One
    // pass re-inks the band and leaves change colours and syntax alone.
    let pairs: Vec<(Style, String)> = if cursor {
        half.pairs
            .iter()
            .map(|(st, t)| (THEME.lit_band(*st), t.clone()))
            .collect()
    } else {
        half.pairs.clone()
    };
    match half.fill {
        Fill::Bg(bg) => spans.extend(truncate_or_pad_spans(
            &pairs,
            rest,
            if cursor { THEME.lit_band(bg) } else { bg },
        )),
        // Hatched, not blank. On the absent side of a split row that says a
        // line does not exist here rather than that it is empty; on a hunk's
        // header it stops the pill's fill from reading as a bar that happens
        // to stop, and carries the band to the pane edge without a colour.
        Fill::Hatch => {
            let used: usize = pairs.iter().map(|(_, t)| t.width()).sum();
            if used >= rest {
                spans.extend(truncate_or_pad_spans(&pairs, rest, Style::default()));
            } else {
                spans.extend(pairs.iter().map(|(st, t)| Span::styled(t.clone(), *st)));
                spans.push(Span::styled(
                    "╱".repeat(rest - used),
                    Style::default().fg(THEME.hatch_fg),
                ));
            }
        }
        // A rule carries the row across the whole pane, separator column and
        // all — the row is about the file, not about one side of it.
        Fill::Rule(style) => {
            let used: usize = pairs.iter().map(|(_, t)| t.width()).sum();
            let ruled = used + 2 * RULE_ARM;
            if ruled >= rest {
                spans.extend(truncate_or_pad_spans(&pairs, rest, style));
            } else {
                // Dotted, and only a stub either side; the rest is left blank
                // so the row does not draw a line across the whole screen.
                let lead = (rest - ruled) / 2;
                let blank = |n: usize| Span::styled(" ".repeat(n), Style::default());
                let dots = Span::styled("┈".repeat(RULE_ARM), style);
                spans.push(blank(lead));
                spans.push(dots.clone());
                spans.extend(pairs.iter().map(|(s, t)| Span::styled(t.clone(), *s)));
                spans.push(dots);
                spans.push(blank(rest - ruled - lead));
            }
        }
    }
    spans
}

/// The connector prefix for each row of a tree, in order.
///
/// A tree drawn as bare indentation reads as a list that happens to be ragged;
/// the guides are what say which directory a file is under. Each row gets `│ `
/// for every ancestor that still has siblings below, then `└─` if it is the
/// last of its parent's children or `├─` if it is not.
fn tree_guides(tree: &[TreeEntry]) -> Vec<String> {
    guides_for_depths(&tree.iter().map(|e| e.depth).collect::<Vec<_>>())
}

/// The same connectors from depths alone, so a list that is not `TreeEntry`
/// rows — the group map's folded view — draws the identical guides.
fn guides_for_depths(depths: &[usize]) -> Vec<String> {
    // Whether a later row shares this row's depth before the tree pops out of
    // it — that is exactly "has a sibling below".
    let more_after: Vec<bool> = (0..depths.len())
        .map(|i| {
            depths[i + 1..]
                .iter()
                .take_while(|&&d| d >= depths[i])
                .any(|&d| d == depths[i])
        })
        .collect();

    let mut open: Vec<bool> = Vec::new();
    depths
        .iter()
        .enumerate()
        .map(|(i, &depth)| {
            open.truncate(depth);
            let mut prefix: String = open.iter().map(|&o| if o { "│ " } else { "  " }).collect();
            if depth > 0 || i + 1 < depths.len() {
                prefix.push_str(if more_after[i] { "├─" } else { "└─" });
            }
            open.push(more_after[i]);
            prefix
        })
        .collect()
}

/// The cursor's own cell, just inside the pane's frame.
///
/// A full-height bar rather than an arrow: it has to read at a glance against
/// a line of code, and against the change colour the gutter block beside it
/// already carries. In the pane title's cyan, which is the colour this view
/// uses for "here you are".
const CURSOR_BAR: &str = "▌";

/// The lit cell at the head of the hunk pill the cursor is in.
const PILL_BAR: &str = "▌";

/// What a hunk's header shows right now.
///
/// Decided at draw time, not when the row is built: whether the cursor is in a
/// hunk changes without rebuilding rows, and the header is the one row whose
/// CONTENT turns on it — idle, it is hatch and nothing else.
#[derive(Clone, Copy)]
enum Marker<'a> {
    /// Not a hunk header. Draw the row as it was built.
    None,
    /// A hunk header the cursor is not in: the band, carrying only the marks
    /// the row says survive it.
    Idle(&'a [(Style, String)]),
    /// A hunk header the cursor is in: the pill, its leading cell lit.
    Lit(Color),
}

/// A pane's frame: always the muted border, with the TITLE carrying focus.
///
/// A lit border draws a box around half the screen to say a thing about the
/// cursor, which is the smallest thing on it — and it competed with the hunk
/// edge, the one border in this view that means something. The title is where
/// a reader looks to know which pane they are in anyway.
fn pane(title: String, focused: bool) -> Block<'static> {
    let ink = if focused {
        THEME.header_fg
    } else {
        THEME.gutter_fg
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(THEME.gutter_fg))
        .title(Span::styled(
            title,
            Style::default().fg(ink).add_modifier(Modifier::BOLD),
        ))
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
    // Nothing but keys. Five lines of prose about the plan pane and the diff's
    // colours used to sit between `n/N` and `s`, splitting the table in half —
    // and a legend is not what anyone opens `?` to find.
    let key = Style::default().fg(THEME.header_fg);
    let text = Style::default().fg(THEME.context_fg);
    let dim = Style::default().fg(THEME.gutter_fg);

    let row = |k: &str, what: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<11}"), key),
            Span::styled(what.to_string(), text),
        ])
    };
    // No title inside the box: the border already carries one, and a name the
    // reader typed to get here is not what they opened `?` to read.
    let mut lines = vec![
        row("j/k", "move · in the plan pane, switch group"),
        row("J/K  { }", "previous / next group"),
        row("tab", "switch pane focus"),
        row("n/N", "next / previous hunk"),
        row("ctrl-d/u", "half page"),
        row("g/G", "top / bottom"),
        row("z", "boundary: show more, or cross into the hunk"),
        row("", "elsewhere: unfold skim remainder / noise"),
        row("s", "unified / split diff"),
        row("f", "plan pane: reading plan / file tree"),
        row("", "diff pane: file list (enter jumps)"),
        row("space", "mark the hunk's class reviewed"),
        row("c  ·  dd", "add finding · delete the one under the cursor"),
        row("y  ·  q", "copy findings · quit (state is saved)"),
        Line::from(""),
        Line::from(Span::styled("  press any key to close", dim)),
    ];
    lines.insert(0, Line::from(""));
    Paragraph::new(lines).block(pane(" help ".to_string(), true))
}
