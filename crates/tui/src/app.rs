//! The reviewer's model, key handling and drawing. `handle_key` is a plain
//! method on the model returning effects — testable without a terminal.
//!
//! All review state (reviewed marks, findings, resume cursor) lives in the
//! engine's `ReviewSession`; this model holds presentation state only.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use differential_engine::ReviewSession;
use differential_engine::review_state::FindingStatus;
use differential_engine::schema;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use super::rows::{
    DiffMode, GroupContext, Row, RowContent, RowFactory, RowKind, RowsContext, build_dir_rows,
    build_file_rows, build_group_rows,
};
use super::theme::THEME;
use super::vendor::text_utils::truncate_or_pad_spans;

const SCROLL_MARGIN: usize = 3;

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

pub struct GroupInfo {
    pub id: String,
    pub label: String,
    pub effort: schema::Effort,
    /// Class content keys of the group's classes (reviewed-mark keys).
    pub class_keys: Vec<String>,
    pub n_hunks: usize,
    /// Distinct paths touched (a rename counts as two: the canonical view is
    /// --no-renames). Binary/submodule changes carry no hunks and count 0.
    pub n_files: usize,
    /// Added / removed line totals over the group's hunks.
    pub adds: usize,
    pub dels: usize,
    /// Groups this one depends on: (id, appears later in the plan). A
    /// dependency listed later means the order could not honour that edge —
    /// the groups are mutually dependent and the toposort broke the cycle.
    pub after: Vec<(String, bool)>,
    pub role: Option<schema::Role>,
}

/// A plan row's relation to the selected group — what the gutter connector
/// draws. The plan is a DAG (a group can follow several others), not a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Selected,
    /// The selected group follows this one.
    Dependency,
    /// This one follows the selected group.
    Dependent,
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

pub struct FileInfo {
    pub path: String,
    /// Canonical hunk indices, position order.
    pub hunk_idxs: Vec<usize>,
    pub adds: usize,
    pub dels: usize,
}

pub struct App {
    pub session: ReviewSession,
    factory: RowFactory,

    pub groups: Vec<GroupInfo>,
    pub labels: HashMap<String, String>,
    /// Every file in the document (including zero-hunk binary/submodule
    /// changes the group view cannot surface), document order.
    pub files: Vec<FileInfo>,
    /// Hunk index -> owning group label, for file-view hunk headers.
    hunk_labels: HashMap<usize, String>,
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
    pub scroll: usize,
    pub group_scroll: usize,
    /// Group ids whose fold is open.
    pub folds_open: HashSet<String>,
    pub status: String,
    /// Diff-pane inner height, updated at draw time for paging math.
    pub viewport_hint: usize,
    pending_d: bool,
}

impl App {
    pub fn new(session: ReviewSession, factory: RowFactory) -> Self {
        let doc = session.doc();
        let class_by_id: HashMap<&str, &schema::ClassEntry> =
            doc.classes.iter().map(|c| (c.id.as_str(), c)).collect();
        let empty = Vec::new();
        let schema_groups = doc.groups.as_ref().unwrap_or(&empty);
        let labels_of: HashMap<String, String> = schema_groups
            .iter()
            .map(|g| (g.id.clone(), g.label.clone()))
            .collect();
        // Position in the plan, for spotting dependencies the order could not
        // satisfy (cycles).
        let rank_of: HashMap<String, usize> = schema_groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id.clone(), i))
            .collect();
        let groups: Vec<GroupInfo> = schema_groups
            .iter()
            .enumerate()
            .map(|(rank, g)| {
                let hunks: Vec<&schema::HunkEntry> = g
                    .class_ids
                    .iter()
                    .filter_map(|c| class_by_id.get(c.as_str()))
                    .flat_map(|cl| cl.hunk_ids.iter())
                    .map(|hid| {
                        let idx: usize = hid[1..].parse().expect("h<N>");
                        &doc.hunks[idx]
                    })
                    .collect();
                let files: std::collections::HashSet<&str> =
                    hunks.iter().map(|h| h.file.as_str()).collect();
                GroupInfo {
                    id: g.id.clone(),
                    label: g.label.clone(),
                    effort: g.effort,
                    after: g
                        .depends_on
                        .iter()
                        .map(|id| (id.clone(), rank_of.get(id).copied().unwrap_or(0) > rank))
                        .collect(),
                    role: g.role,
                    class_keys: g
                        .class_ids
                        .iter()
                        .map(|c| session.class_key(c).to_string())
                        .collect(),
                    n_hunks: hunks.len(),
                    n_files: files.len(),
                    adds: hunks.iter().map(|h| h.new_count as usize).sum(),
                    dels: hunks.iter().map(|h| h.old_count as usize).sum(),
                }
            })
            .collect();
        let labels = labels_of.clone();

        // Hunk -> owning group label (via the hunk's class).
        let group_of_class: HashMap<&str, &str> = schema_groups
            .iter()
            .flat_map(|g| g.class_ids.iter().map(|c| (c.as_str(), g.label.as_str())))
            .collect();
        let mut hunk_labels = HashMap::new();
        for c in &doc.classes {
            if let Some(label) = group_of_class.get(c.id.as_str()) {
                for hid in &c.hunk_ids {
                    let idx: usize = hid[1..].parse().expect("h<N>");
                    hunk_labels.insert(idx, label.to_string());
                }
            }
        }

        // Every file, document order — the flat view surfaces zero-hunk
        // (binary/submodule/mode-only) changes the group view cannot.
        let files: Vec<FileInfo> = doc
            .files
            .iter()
            .map(|f| {
                let hunk_idxs: Vec<usize> = f
                    .hunk_ids
                    .iter()
                    .map(|hid| hid[1..].parse().expect("h<N>"))
                    .collect();
                FileInfo {
                    path: f.path.clone(),
                    adds: hunk_idxs
                        .iter()
                        .map(|&i| doc.hunks[i].new_count as usize)
                        .sum(),
                    dels: hunk_idxs
                        .iter()
                        .map(|&i| doc.hunks[i].old_count as usize)
                        .sum(),
                    hunk_idxs,
                }
            })
            .collect();

        // Resume position: the cursor id is a group id in the semantic view,
        // a file path in the file view (session.file_view() disambiguates).
        let view_mode = if session.file_view() {
            ViewMode::Files
        } else {
            ViewMode::Groups
        };
        // The cursor id is a group id in the plan view, a path in the file
        // view; the tree row for a path is resolved after the tree is built.
        let resume_cursor: Option<(String, usize)> = session.cursor().cloned();
        let (selected_group, resume_row) = match (&resume_cursor, view_mode) {
            (Some((id, row)), ViewMode::Groups) => (
                groups.iter().position(|g| &g.id == id).unwrap_or(0),
                Some(*row),
            ),
            (Some((_, row)), ViewMode::Files) => (0, Some(*row)),
            (None, _) => (0, None),
        };
        let selected_file = 0;

        let mut app = App {
            session,
            factory,
            groups,
            labels,
            files,
            hunk_labels,
            tree: Vec::new(),
            collapsed: HashSet::new(),
            focus: Focus::Groups,
            mode: Mode::Normal,
            view_mode,
            selected_group,
            selected_file,
            rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            group_scroll: 0,
            folds_open: HashSet::new(),
            status: String::new(),
            viewport_hint: 24,
            pending_d: false,
        };
        app.rebuild_tree();
        // The persisted cursor names a path; reveal it in the tree.
        if app.view_mode == ViewMode::Files
            && let Some((id, _)) = resume_cursor.as_ref()
            && let Some(row) = app.reveal_path(id)
        {
            app.selected_file = row;
        }
        app.rebuild_rows();
        if let Some(row) = resume_row {
            app.cursor = row.min(app.rows.len().saturating_sub(1));
        }
        app
    }

    /// Rebuild the visible tree rows from the flat file list, honouring
    /// collapsed directories. Directory rows appear once, in path order.
    pub fn rebuild_tree(&mut self) {
        let mut paths: Vec<(usize, Vec<String>)> = self
            .files
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
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.path.starts_with(&prefix))
                    .map(|(i, _)| i)
                    .collect();
                // Path order, so the diff pane presents files in the order the
                // tree lists them rather than in document order.
                under.sort_by(|a, b| self.files[*a].path.cmp(&self.files[*b].path));
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
            Some(TreeKind::File { file_idx }) => Some(self.files[*file_idx].path.clone()),
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
            TreeKind::File { file_idx } => self.files[*file_idx].path == path,
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
                let g = &groups[self.selected_group.min(groups.len() - 1)];
                let ctx = GroupContext {
                    core: RowsContext {
                        doc: self.session.doc(),
                        findings: self.session.findings(),
                        reviewed: &reviewed,
                        mode: self.diff_mode(),
                        hunk_labels: None,
                    },
                    group: g,
                    labels: &self.labels,
                    fold_open: self.folds_open.contains(&g.id),
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
                    findings: self.session.findings(),
                    reviewed: &reviewed,
                    mode: self.diff_mode(),
                    hunk_labels: Some(&self.hunk_labels),
                };
                self.rows = match targets.as_slice() {
                    // A single file keeps its dedicated builder (it renders a
                    // placeholder for zero-hunk binary/submodule changes).
                    [only] => {
                        let f = &self.files[*only];
                        build_file_rows(&mut self.factory, &ctx, &f.path, f.hunk_idxs.clone())
                    }
                    // A directory: every hunk beneath it, file headers and all.
                    many => {
                        let hunks: Vec<usize> = many
                            .iter()
                            .flat_map(|i| self.files[*i].hunk_idxs.iter().copied())
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

    fn follow_cursor(&mut self) {
        // Viewport height is only known at draw time; use a conservative page
        // guess updated by draw().
        let h = self.viewport_hint.max(8);
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
                if self.groups.is_empty() {
                    return;
                }
                self.selected_group = idx.min(self.groups.len() - 1);
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
                let info = self.files.iter().find(|f| f.path == path);
                let (adds, dels, done) = info
                    .map(|f| {
                        (
                            f.adds,
                            f.dels,
                            !f.hunk_idxs.is_empty()
                                && f.hunk_idxs.iter().all(|h| reviewed.contains(h)),
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
                .groups
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
                let h = self.viewport_hint.max(8) / 2;
                self.cursor = (self.cursor + h).min(self.rows.len().saturating_sub(1));
                self.cursor = self.next_selectable(self.cursor, -1).unwrap_or(self.cursor);
                self.follow_cursor();
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let h = self.viewport_hint.max(8) / 2;
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
            (KeyCode::Char('z'), _) if self.view_mode == ViewMode::Files => {
                self.toggle_dir();
            }
            (KeyCode::Char('z'), _) => {
                if self.view_mode == ViewMode::Groups
                    && let Some(g) = self.groups.get(self.selected_group)
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
            ViewMode::Groups => match self.groups.get(self.selected_group) {
                Some(g) => g.class_keys.clone(),
                None => return Ok(()),
            },
            ViewMode::Files => self
                .files_of_tree_row(self.selected_file)
                .iter()
                .flat_map(|i| self.files[*i].hunk_idxs.iter())
                .map(|h| self.session.hunk_class_key(*h).to_string())
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
        let doc = self.session.doc();
        let group_of_digest: HashMap<&str, &str> = doc
            .groups
            .iter()
            .flatten()
            .flat_map(|g| {
                g.class_ids.iter().flat_map(|cid| {
                    let class = doc.classes.iter().find(|c| &c.id == cid).unwrap();
                    class.hunk_ids.iter().map(|hid| {
                        let idx: usize = hid[1..].parse().unwrap();
                        (doc.hunks[idx].digest.as_str(), g.label.as_str())
                    })
                })
            })
            .collect();
        let mut out = String::new();
        for f in self
            .session
            .findings()
            .iter()
            .filter(|f| f.status == FindingStatus::Open)
        {
            let label = group_of_digest
                .get(f.anchor.hunk_digest.as_str())
                .map(|l| format!(" ({l})"))
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

    pub fn draw(&mut self, frame: &mut Frame) {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(frame.area());
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(40), Constraint::Min(0)])
            .split(outer[0]);

        self.draw_groups(frame, panes[0]);
        self.draw_diff(frame, panes[1]);
        self.draw_status(frame, outer[1]);

        match &self.mode {
            Mode::Editing(_, textarea) => {
                let area = bottom_rect(outer[0], 8);
                frame.render_widget(Clear, area);
                frame.render_widget(&**textarea, area);
            }
            Mode::Help => {
                let area = centered_rect(outer[0], 60, 18);
                frame.render_widget(Clear, area);
                frame.render_widget(help_paragraph(), area);
            }
            Mode::FileList { entries, selected } => {
                let height = (entries.len() as u16 + 2).min(outer[0].height);
                let area = centered_rect(outer[0], 70, height);
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

    fn draw_groups(&mut self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        let selected = self.selected_entry();

        // Entries render as blocks of lines, so scrolling counts ROWS, not
        // entries; keep the whole selected block in view.
        let mut blocks: Vec<Vec<Line>> = match self.view_mode {
            ViewMode::Groups => (0..self.groups.len())
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
        let start_row: usize = blocks.iter().take(selected).map(Vec::len).sum();
        let end_row = start_row + blocks.get(selected).map_or(0, Vec::len);
        if start_row < self.group_scroll {
            self.group_scroll = start_row;
        } else if end_row > self.group_scroll + inner_h {
            self.group_scroll = end_row.saturating_sub(inner_h);
        }
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
        let selected = self.selected_group;
        if idx == selected {
            return Relation::Selected;
        }
        let Some(sel) = self.groups.get(selected) else {
            return Relation::None;
        };
        if sel.after.iter().any(|(id, _)| *id == self.groups[idx].id) {
            return Relation::Dependency;
        }
        if self.groups[idx].after.iter().any(|(id, _)| *id == sel.id) {
            return Relation::Dependent;
        }
        Relation::None
    }

    /// Rows spanned by the selected group's edges, so the connector can be
    /// drawn as one continuous line.
    fn edge_span(&self) -> (usize, usize) {
        let mut lo = self.selected_group;
        let mut hi = self.selected_group;
        for i in 0..self.groups.len() {
            if !matches!(self.relation_to_selected(i), Relation::None) {
                lo = lo.min(i);
                hi = hi.max(i);
            }
        }
        (lo, hi)
    }

    /// One group as 2–3 lines: title, counts, and what it follows.
    fn group_lines(&self, idx: usize, selected: bool) -> Vec<Line<'static>> {
        let g = &self.groups[idx];
        let relation = self.relation_to_selected(idx);
        let (lo, hi) = self.edge_span();
        // The connector: a line running between the selected group and every
        // group it links to, so the DAG is visible without reading ids.
        let (head_glyph, head_style) = match relation {
            Relation::Selected => ("◆", Style::default().fg(THEME.header_fg)),
            // Read before the selected group.
            Relation::Dependency => ("├", Style::default().fg(THEME.reviewed_fg)),
            // Reads after it.
            Relation::Dependent => ("├", Style::default().fg(THEME.skim_fg)),
            Relation::None if idx > lo && idx < hi => ("│", Style::default().fg(THEME.gutter_fg)),
            Relation::None => (" ", Style::default().fg(THEME.gutter_fg)),
        };
        let tail_glyph = if idx >= lo && idx < hi { "│" } else { " " };
        let done =
            g.class_keys.iter().all(|k| self.session.is_reviewed(k)) && !g.class_keys.is_empty();
        let tier = match g.effort {
            schema::Effort::Focus => "F",
            schema::Effort::Skim => "S",
            schema::Effort::Noise => "N",
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

        let role = match g.role {
            Some(schema::Role::Foundation) => " · foundation",
            Some(schema::Role::Consumer) => " · consumer",
            Some(schema::Role::Mechanical) => " · mechanical",
            Some(schema::Role::Noise) => " · noise",
            None => "",
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{tail_glyph} "),
                Style::default().fg(THEME.gutter_fg),
            ),
            Span::styled(format!("   {} files  ", g.n_files), dim),
            Span::styled(
                format!("+{}", g.adds),
                bg(Style::default().fg(THEME.add_fg)),
            ),
            Span::styled(" ", dim),
            Span::styled(
                format!("−{}", g.dels),
                bg(Style::default().fg(THEME.del_fg)),
            ),
            Span::styled(role.to_string(), dim),
        ]));
        if !g.after.is_empty() {
            // "↓" marks a dependency that appears LATER in the plan: the two
            // groups depend on each other, so no order can satisfy both.
            let mut spans = vec![
                Span::styled(
                    format!("{tail_glyph} "),
                    Style::default().fg(THEME.gutter_fg),
                ),
                Span::styled("   after: ".to_string(), dim),
            ];
            for (id, later) in &g.after {
                spans.push(Span::styled(
                    format!("{id}{} ", if *later { "↓" } else { "" }),
                    if *later {
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
            .map(|i| (self.files[*i].adds, self.files[*i].dels))
            .fold((0, 0), |(a, d), (x, y)| (a + x, d + y));
        let hunks: Vec<usize> = files
            .iter()
            .flat_map(|i| self.files[*i].hunk_idxs.iter().copied())
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
                let f = &self.files[*file_idx];
                let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
                let mut spans = vec![
                    Span::styled(format!("{mark}{indent}  "), dim),
                    Span::styled(name, name_style),
                    Span::styled("  ", dim),
                ];
                if f.hunk_idxs.is_empty() {
                    spans.push(Span::styled("(no text hunks)".to_string(), dim));
                } else {
                    spans.push(Span::styled(
                        format!("+{}", f.adds),
                        bg(Style::default().fg(THEME.add_fg)),
                    ));
                    spans.push(Span::styled(" ", dim));
                    spans.push(Span::styled(
                        format!("−{}", f.dels),
                        bg(Style::default().fg(THEME.del_fg)),
                    ));
                }
                vec![Line::from(spans)]
            }
        }
    }

    fn draw_diff(&mut self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        self.viewport_hint = inner_h;
        self.follow_cursor();
        let inner_w = area.width.saturating_sub(2) as usize;
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(inner_h)
            .map(|(i, r)| {
                let mut line = compose_row(&r.content, inner_w);
                if i == self.cursor && self.focus == Focus::Diff && r.kind.selectable() {
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

    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        let total: usize = self.groups.iter().map(|g| g.class_keys.len()).sum();
        let done = self.session.reviewed_count().min(total);
        let open = self
            .session
            .findings()
            .iter()
            .filter(|f| f.status == FindingStatus::Open)
            .count();
        let text = format!(
            " {done}/{total} classes reviewed · {open} finding(s) · {} · j/k J/K nav · n/N hunk · space reviewed · c finding · s split · v files · z fold · y copy summary · ? help · q quit",
            self.status
        );
        frame.render_widget(
            Paragraph::new(text).style(Style::default().bg(THEME.status_bg)),
            area,
        );
    }
}

/// Render a row at the given pane width. Split rows compose their two halves
/// here — width is a draw-time concern, so resizes never rebuild rows.
fn compose_row(content: &RowContent, width: usize) -> Line<'static> {
    match content {
        RowContent::Full(line) => line.clone(),
        RowContent::Split { old, new } => {
            let lw = width.saturating_sub(1) / 2;
            let rw = width.saturating_sub(1).saturating_sub(lw);
            let mut spans = truncate_or_pad_spans(old, lw, Style::default());
            spans.push(Span::styled("│", Style::default().fg(THEME.gutter_fg)));
            spans.extend(truncate_or_pad_spans(new, rw, Style::default()));
            Line::from(spans)
        }
    }
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
        Line::from("  z          unfold skim remainder / noise"),
        Line::from("  n/N        next / previous hunk"),
        Line::from(""),
        Line::from("  plan rows: <id> <tier> label · after: what it follows"),
        Line::from("  the line links the selected group to its neighbours;"),
        Line::from("  ↓ marks a dependency listed later (mutual dependency)"),
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
