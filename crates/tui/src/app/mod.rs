//! The reviewer's model, key handling and drawing. `handle_key` is a plain
//! method on the model returning effects — testable without a terminal.
//!
//! All review state (reviewed marks, findings, resume cursor) lives in the
//! engine's `ReviewSession`; this model holds presentation state only.
//!
//! This module holds the types and `App::new`. The four jobs `App` does are
//! one file each, because 3854 lines of them in one file meant a reader
//! looking for what a key does scrolled past everything that draws:
//!
//! - [`state`] — what the model recomputes when something changes: the tree,
//!   the rows, the cursor, the scroll, the layout toggles.
//! - [`keys`] — `handle_key` and `handle_paste`, the whole input surface.
//! - [`findings`] — the mutators that reach the session: reviewed marks,
//!   findings written, edited, deleted and jumped to.
//! - [`draw`] — every `draw_*`, and the row composition behind them.
//! - [`text`] — measuring and cutting text to a column budget. A leaf: it
//!   knows nothing about `App`, and both `keys` and `draw` read from it, which
//!   is what keeps a list's scroll height equal to its drawn height.
//! - [`help`] — one table of keys, read by the footer and by `?` (issue 30).
//! - [`forge`] — the forge's side: fetching review threads on a worker
//!   thread, resolving one, drafting a reply (ADR 0029).
//!
//! `App`'s inherent methods are split across those files, so a method that
//! was private to one file is `pub(super)` now. The scope is the same one it
//! always had — this module — and nothing new leaves it.

use std::collections::{HashMap, HashSet};

use differential_engine::FsReviewSession;
use differential_engine::config::ThemeName;
use differential_engine::review_state::{FindingStatus, Lines};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

use crate::rows::{
    DiffMode, GroupContext, RAIL, Row, RowContent, RowFactory, build_dir_rows, build_file_rows,
    build_group_rows, pill,
};
use crate::theme::Theme;
use crate::window::Expansion;

const SCROLL_MARGIN: usize = 3;

/// Presentation settings the application layer reads from config and hands to
/// the renderer. Not review state: nothing here is persisted in the sidecar.
#[derive(Debug, Clone)]
pub struct ReviewOptions {
    /// Context lines either side of a hunk before any expansion.
    pub context: usize,
    /// Lines one `z` on a context boundary row pulls in.
    pub context_step: usize,
    /// Which layout a review opens in when the reader has not chosen one.
    ///
    /// Resolved from config by the application layer, so the renderer takes a
    /// plain value and never learns the config's vocabulary.
    pub split_diff: bool,
    /// The range the reader typed, if they typed one — so the footer can name
    /// `dfr findings <range> --summary` when the clipboard is out of reach.
    ///
    /// Presentation, which is why it comes from the application layer rather
    /// than from the pipeline's result: the review's IDENTITY is a resolved
    /// sha plus a spec, and neither is what the reader would type back.
    pub range: Option<String>,
    /// Which palette to wear. A name, not a built [`Theme`]: building one
    /// parses the syntax set, and this struct is plain data the app layer
    /// fills in from config.
    pub theme: ThemeName,
}

impl Default for ReviewOptions {
    fn default() -> Self {
        ReviewOptions {
            context: 3,
            context_step: 10,
            // Matches `config::DiffLayout`'s default. The two are separate
            // because the renderer must not read config; a test asserts they
            // agree.
            split_diff: true,
            range: None,
            theme: ThemeName::default(),
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
/// Row BUILDING must still never depend on width. `RowContent::Split` defers
/// its columns to draw time precisely so a resize never rebuilds rows, and the
/// one width measured here does not change that: it is what a WRAPPED row is
/// composed at. Wrapping is a draw-time fact, and the scroll budget needs the
/// same fact, so both read one number measured in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub detail_rows: usize,
    /// The detail pane's CONTENT width — what a row wraps at, and therefore
    /// what its height is a function of.
    pub detail_cols: usize,
    pub plan_rows: usize,
    /// The body's FULL height, borders included — a modal floats over the
    /// body and draws its own box, so it needs the raw number the two panes
    /// have already subtracted their borders from.
    pub body_rows: usize,
    /// The whole screen the panes were laid out on. A mouse event names a
    /// cell, and which pane that cell is in is `layout(area)` — the same call
    /// `draw` makes, so a hit test and a frame cannot disagree.
    pub area: Rect,
}

impl Viewport {
    pub fn measure(area: Rect) -> Self {
        let panes = layout(area);
        Viewport {
            // Every pane is bordered.
            detail_rows: panes.detail.height.saturating_sub(2) as usize,
            detail_cols: panes.detail.width.saturating_sub(2) as usize,
            plan_rows: panes.plan.height.saturating_sub(2) as usize,
            body_rows: panes.body.height as usize,
            area,
        }
    }
}

impl Default for Viewport {
    /// Before the first measurement.
    fn default() -> Self {
        Viewport {
            detail_rows: 24,
            detail_cols: 78,
            plan_rows: 24,
            body_rows: 26,
            area: Rect::new(0, 0, 120, 27),
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
    /// Writing a finding.
    Editing {
        /// The canonical hunk it belongs to.
        hunk: usize,
        /// The lines the reviewer had picked when the box opened; `None`
        /// anchors the whole hunk.
        lines: Option<Lines>,
        /// The finding being rewritten. `None` files a new one.
        rewriting: Option<String>,
        /// The forge thread this answers. A reply is still a finding until a
        /// publish sends it (ADR 0029).
        reply_to: Option<String>,
        /// A comment of the reader's on the forge being rewritten. Saving
        /// sends the text there first.
        own: Option<differential_engine::forge::OwnComment>,
        editor: Box<TextArea<'static>>,
    },
    /// The keys of where the reader pressed `?`, carrying the mode they
    /// pressed it in: help opened over a modal has to give that modal back,
    /// not drop the reader into the review behind it.
    Help(Box<Mode>),
    /// Something the footer cannot hold: a forge's whole answer to a call
    /// that failed. Any key closes it.
    Notice {
        title: String,
        text: String,
    },
    /// File-list modal over the current rows: jump to a file header.
    FileList {
        entries: Vec<FileListEntry>,
        selected: usize,
        /// First visible entry. It had none, and clipped instead: a document
        /// with more files than the pane is tall simply never drew the rest,
        /// and the cursor walked off into rows that were not on screen.
        scroll: usize,
    },
    /// Every finding in the review, in one list.
    ///
    /// A note is written on a line and drawn under it, which is where it
    /// belongs while reading the code and no help at all in answering "what
    /// have I found". An ORPHANED note is worse off: it has no row anywhere,
    /// so this is the only place it can be read or deleted.
    Findings {
        entries: Vec<FindingEntry>,
        selected: usize,
        /// First visible entry. The file list has none and clips at the body
        /// height; a review has more notes than it has files.
        scroll: usize,
        /// `D` was pressed and the next key answers.
        confirming: bool,
    },
    /// `P` was pressed: what a publish would send and what it would leave,
    /// shown before anything leaves the machine. `y` sends (ADR 0029).
    Publish {
        plan: differential_engine::forge::PublishPlan,
    },
    /// `dd` on a comment of the reader's: the next key answers, and only
    /// `y` deletes it on the forge (ADR 0029).
    DeleteComment {
        own: differential_engine::forge::OwnComment,
    },
}

pub struct FindingEntry {
    pub id: String,
    /// `src/app.rs:1307`, or `src/app.rs:1307-1312` for a range.
    pub at: String,
    /// The note's first line.
    pub body: String,
    pub orphaned: bool,
    pub moved: bool,
    /// A forge thread rather than a note: `id` is the thread's (ADR 0029).
    pub thread: bool,
    /// A note the request already has, whose twin was not fetched (yet).
    pub published: bool,
    /// A thread the forge marks resolved.
    pub resolved: bool,
}

impl FindingEntry {
    /// Which of the list's three sections this sits in, in list order:
    /// notes, threads, orphaned.
    pub fn section(&self) -> u8 {
        match (self.orphaned, self.thread) {
            (true, _) => 2,
            (false, true) => 1,
            (false, false) => 0,
        }
    }
}

/// Where the rules between the list's sections fall: the index of the first
/// entry of each section after the first non-empty one. Drawn, not stored, so
/// they cost a row on screen and nothing in the model.
pub fn section_rules(entries: &[FindingEntry]) -> Vec<usize> {
    entries
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0].section() != w[1].section())
        .map(|(i, _)| i + 1)
        .collect()
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

/// An open symbol float: the token it lights, and the declaration it shows.
///
/// Everything here is resolved when `z` is pressed, because reading a blob and
/// running syntect both need `&mut self` and drawing is a pure function of the
/// model. Holding the CONTENT rather than the coordinates to fetch it is what
/// keeps that true.
pub struct Peek {
    /// The diff row this belongs to. Moving the cursor drops the whole thing —
    /// a highlight pointing at a row the cursor has left is worse than none.
    pub row: usize,
    /// Which resolvable symbol on that row, counting from 0, left to right.
    pub nth: usize,
    /// `name · file:line · C7 · g3`.
    pub title: String,
    /// The declaration, one entry per line: its number, and its styled text.
    pub body: Vec<crate::rows::SnippetLine>,
    /// Lines the cap cut, for the `… N more lines` row.
    pub more: usize,
}

/// A plan row's relation to the selected group — what the gutter connector
/// draws. The plan is a graph (a group can follow several others), not a tree, and not
/// acyclic either.
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
    pub(super) fn depth(&self) -> usize {
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
    /// Which files each tree row covers, by row. Rebuilt with `tree`, because
    /// it is a pure function of it and the file list.
    tree_files: Vec<Vec<usize>>,
    /// Directory paths currently collapsed.
    collapsed: HashSet<String>,

    pub focus: Focus,
    pub mode: Mode,
    pub view_mode: ViewMode,
    pub selected_group: usize,
    pub selected_file: usize,
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// The row a line selection is anchored at; the other end is the cursor.
    ///
    /// One field, not a mode: `j`/`k` go on moving the cursor and the
    /// selection is the span between the two ends, so `V` adds a state to
    /// the model without adding one to the key table.
    pub visual: Option<usize>,
    /// The open symbol float, and which symbol on its row it is showing.
    ///
    /// One field, not a mode — the same reason `visual` is one. `j`/`k` must go
    /// on moving the cursor, and a mode would add a state to the key table to
    /// say something the cursor already says.
    ///
    /// Transient, like `folds_open` and `expanded`: looking something up is a
    /// reading aid for this sitting, not a finding, so nothing here reaches the
    /// sidecar store.
    pub peek: Option<Peek>,
    scroll: usize,
    /// How far the diff pane's CONTENT is shifted left, in columns.
    ///
    /// Transient, like `folds_open` and `expanded`: where along a line the
    /// reader is looking is a reading position for this sitting, which is what
    /// `scroll` already is. `s` and `w` are recorded against a review because
    /// they are layout choices; a column is not one.
    hscroll: usize,
    group_scroll: usize,
    /// Group ids whose fold is open.
    pub folds_open: HashSet<String>,
    /// How far each hunk's context has been pulled open, by canonical index.
    ///
    /// Transient, like `folds_open`: how much of a file you are looking at is a
    /// reading aid for this sitting, not a finding, so nothing here reaches the
    /// sidecar store.
    expanded: HashMap<usize, Expansion>,
    /// Resolved threads the reader has opened with `z`. A resolved thread is
    /// collapsed to its header by default; the rest is settled reading, shown
    /// on demand. Transient, like `folds_open`.
    expanded_threads: HashSet<String>,
    opts: ReviewOptions,
    /// The palette, built once. Held rather than rebuilt per frame because
    /// building one parses the syntax set, and because rows bake their colours
    /// in at build time — `rebuild_rows` reads it as much as `draw` does.
    theme: Theme,
    pub status: String,
    /// The overviews' inputs, computed when the rows are. Both used to be
    /// derived inside `draw`, which meant an O(hunks) scan with a string
    /// compare per hunk on EVERY frame — enough to make a large review feel
    /// stuck on each keypress.
    /// Where each file sits in the document, by path. Built once: the
    /// document does not change while a session is open.
    file_index: HashMap<String, usize>,
    /// Hunk indices marked reviewed, in THIS document. Refreshed by
    /// `rebuild_rows`, which every path that changes a mark ends with.
    reviewed: HashSet<usize>,
    map_files: HashSet<usize>,
    /// The group map's rows, derived from `tree` and `map_files`.
    map_rows: Vec<MapRow>,
    listed_files: Vec<usize>,
    /// Measured geometry. An input to update, never a draw-time output.
    viewport: Viewport,
    pending_d: bool,
    /// The forge this review is of, when it is of a request (ADR 0029).
    forge: Option<forge::ForgeLink>,
    /// The one forge call that may be out. See `app::forge`.
    inflight: Option<forge::Inflight>,
}

impl App {
    /// Swap the palette. Rows bake their colours in at build time, so this
    /// rebuilds them rather than leaving the old ink on screen.
    ///
    /// Test-only: the float-ground assertion and the ignored `render_dump_themes`
    /// dump are the callers. A running reviewer picks its palette from config
    /// once, at startup, and never swaps it.
    #[doc(hidden)]
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.rebuild_rows();
    }

    pub fn new(
        session: FsReviewSession,
        factory: RowFactory,
        opts: ReviewOptions,
        theme: Theme,
    ) -> Self {
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
            theme,
            tree: Vec::new(),
            tree_files: Vec::new(),
            collapsed: HashSet::new(),
            focus: Focus::Groups,
            mode: Mode::Normal,
            view_mode,
            selected_group,
            selected_file: 0,
            rows: Vec::new(),
            cursor: 0,
            visual: None,
            peek: None,
            scroll: 0,
            hscroll: 0,
            group_scroll: 0,
            folds_open: HashSet::new(),
            expanded: HashMap::new(),
            expanded_threads: HashSet::new(),
            opts,
            status: String::new(),
            file_index: HashMap::new(),
            reviewed: HashSet::new(),
            map_files: HashSet::new(),
            map_rows: Vec::new(),
            listed_files: Vec::new(),
            viewport: Viewport::default(),
            pending_d: false,
            forge: None,
            inflight: None,
        };
        // The document is fixed for the session's life, so this is built once
        // rather than found by scanning the file list per row.
        app.file_index = app
            .files()
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.clone(), i))
            .collect();
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
}

mod draw;
mod findings;
mod forge;
mod help;
mod keys;
mod peek;
mod state;
mod text;

// The geometry a click is judged against, from the functions that draw it.
// Exposed so a test can aim a click at the box the draw will place, rather
// than at a number that was true of the box once.
pub use draw::{
    FRAME_ROWS, centered_x, composer_area, composer_footer, delete_comment_area,
    delete_comment_footer, file_list_modal_area, findings_modal_area, findings_question,
    footer_row, pane_inner, publish_area, publish_footer,
};
pub use help::{Act, Area, HelpSection};
pub use text::{Hint, Ink, hints_width};

pub use forge::ForgeLink;

/// What the footer says on a key aimed at someone else's comment.
pub(super) const NOT_YOURS: &str = "not your comment · r replies · x resolves";

/// The `s` a count takes, or not.
pub(super) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
