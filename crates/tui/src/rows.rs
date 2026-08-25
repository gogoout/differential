//! The single row builder (tuicr's lesson applied): the row-kind array IS the
//! output of the builder that renders the lines, so navigation and drawing can
//! never disagree about what a row is.
//!
//! Rows are built from the line ranges `window` plans, read straight out of
//! the base and head blobs. The reviewer used to diff and syntax-highlight
//! whole files and then search the result for each hunk; it now touches only
//! the lines it draws (ADR 0021).
//!
//! Row CONTENT still defers its columns to draw time — padding a background to
//! the pane edge is a width question, and row counts must never depend on
//! width or every resize would rebuild.

use std::collections::HashMap;
use std::ops::Range;

use differential_engine::gitio::Repo;
use differential_engine::plan::{
    Deferral, FileView, Fold, GroupView, HunkId, PlanIndex, ReviewView, reading_split,
};
use differential_engine::ports::ObjectReader;
use differential_engine::review_state::Finding;
use differential_engine::schema;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::{THEME, Theme, highlighter};
use super::vendor::LineOrigin;
use super::vendor::diff_algo::compute_side_by_side;
use super::vendor::diff_types::{ChangeType, DiffLine, InlineSegment, expand_tabs};
use super::vendor::syntax::HighlightedSpans;
use super::vendor::text_utils::split_pairs_at_ranges;
use super::window::{self, Expansion, Segment, Side};

const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    GroupHeader,
    /// A file header carrying its path (the file-list modal jumps to these).
    FileHeader(String),
    /// The `old` / `new` column labels above a file in split mode.
    ColumnHeader,
    /// Canonical hunk index, and whether this view lists the hunk. A foreign
    /// header is skipped by `n`/`N`: it is not on this group's reading list,
    /// even though `space` and `c` still act on it.
    HunkHeader {
        hunk: usize,
        foreign: bool,
    },
    /// The closing edge of a hunk's box.
    HunkFoot,
    /// A diff content row belonging to a hunk.
    Diff(usize),
    /// The edge of what is shown around a hunk: press `z` to pull in more.
    ///
    /// `crossing` distinguishes the two offers — more of the current gap, or
    /// the hunk beyond it once the gap is spent. It lives HERE rather than
    /// being read back off the rendered label, so the key handler and the
    /// renderer cannot disagree about what `z` does.
    ContextEdge {
        hunk: usize,
        side: Side,
        crossing: bool,
    },
    /// A finding attached to a hunk: (finding id, hunk index).
    Finding(String, usize),
    /// Collapsed remainder / noise: press z to unfold.
    Fold,
    Blank,
}

impl RowKind {
    pub fn selectable(&self) -> bool {
        matches!(
            self,
            RowKind::HunkHeader { .. }
                | RowKind::Diff(_)
                | RowKind::ContextEdge { .. }
                | RowKind::Finding(_, _)
                | RowKind::Fold
        )
    }

    /// The hunk a row acts on. Deliberately `None` for a context boundary:
    /// `space` and `c` should ask for a hunk rather than mark a class from a
    /// row that is only about how much of the file is visible.
    pub fn hunk(&self) -> Option<usize> {
        match self {
            RowKind::HunkHeader { hunk, .. } => Some(*hunk),
            RowKind::Diff(h) | RowKind::Finding(_, h) => Some(*h),
            _ => None,
        }
    }
}

/// Diff-pane layout. Split rows carry BOTH sides as style/text pairs; the
/// columns are composed at draw time from the pane width (row counts never
/// depend on width, so resizes never rebuild).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    Unified,
    Split,
}

/// Whose box a hunk sits in.
///
/// A foreign hunk was pulled in by expanding across it: it is real code the
/// reviewer asked to see, but it is not on this group's reading list, and a
/// dashed border is what says so at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxStyle {
    Own,
    Foreign,
}

impl BoxStyle {
    pub const fn horizontal(self) -> char {
        match self {
            BoxStyle::Own => '─',
            BoxStyle::Foreign => '╌',
        }
    }

    pub const fn vertical(self) -> char {
        match self {
            BoxStyle::Own => '│',
            BoxStyle::Foreign => '╎',
        }
    }
}

/// Which part of a hunk's box a row draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Top,
    Side,
    Bottom,
}

/// A row's part in the box drawn around a hunk's changed lines.
///
/// The box's sides ARE the diff pane's own border: they sit in the same column
/// rather than beside it, so the box costs no width and there are never two
/// parallel vertical lines a cell apart. That is also why the corners are
/// junctions (`├`, `┤`) — the pane's border carries on above and below them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub part: Part,
    pub box_style: BoxStyle,
    /// The hunk header band's colour, so the sides match the top rather than
    /// reading as a different thing that happens to touch it.
    pub style: Style,
}

impl Border {
    /// The glyph this row puts in each of the pane's border columns.
    pub fn glyphs(&self) -> (char, char) {
        match self.part {
            Part::Top => ('├', '┤'),
            Part::Bottom => ('├', '┤'),
            Part::Side => (self.box_style.vertical(), self.box_style.vertical()),
        }
    }
}

/// What a row's padding is filled with, out to the pane edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fill {
    /// The line's own background, so a change reads as a block rather than
    /// stopping at its last character.
    Bg(Style),
    /// This side has no line here at all. Hatched rather than blank, so an
    /// absent line is visibly absent instead of looking like empty code.
    Hatch,
    /// Rule out to the pane edge. What makes a row that is ABOUT the whole
    /// file — a boundary, a hunk header — read as spanning both columns
    /// instead of stopping mid-pane and leaving the split separator broken.
    ///
    /// `centered` puts the rule on both sides of the text. A boundary DIVIDES,
    /// so it reads best centred; a hunk header LABELS what follows it, and a
    /// label that drifts with the pane width is harder to scan down a column.
    Rule {
        style: Style,
        centered: bool,
        /// `─` normally, `╌` when the rule is a foreign hunk's top edge.
        glyph: char,
    },
}

/// One side of a diff row: a gutter whose first cell is reserved for the
/// cursor marker, the content, and what pads the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct Half {
    pub gutter: (Style, String),
    pub pairs: Vec<(Style, String)>,
    pub fill: Fill,
}

impl Half {
    /// The absent side of a split row.
    fn hatch() -> Self {
        Half {
            gutter: (Style::default(), String::new()),
            pairs: Vec::new(),
            fill: Fill::Hatch,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowContent {
    /// Headers, findings, fold and boundary rows — already a full line.
    Full(Line<'static>),
    /// One unified diff row, spanning the pane.
    Unified(Half),
    Split {
        old: Half,
        new: Half,
    },
}

pub struct Row {
    pub kind: RowKind,
    pub content: RowContent,
    pub border: Option<Border>,
}

impl Row {
    pub fn full(kind: RowKind, line: Line<'static>) -> Self {
        Row::banner(kind, line, Fill::Bg(Style::default()))
    }

    pub fn bordered(mut self, part: Part, box_style: BoxStyle, style: Style) -> Self {
        self.border = Some(Border {
            part,
            box_style,
            style,
        });
        self
    }

    /// A row that spans the whole diff pane.
    ///
    /// Built as a one-column row rather than a bare `Line` so it pads to the
    /// pane edge at draw time: a header or a boundary is about the whole file,
    /// and one that stopped at its last character left the split view's
    /// separator column with a hole in it. The leading cell is the cursor's,
    /// as on every other row.
    pub fn banner(kind: RowKind, line: Line<'static>, fill: Fill) -> Self {
        Row::banner_with(kind, line, fill, ' ', Style::default())
    }

    /// A banner whose reserved cell carries something other than a blank —
    /// a box edge continues its rule through it, so the corner joins up.
    pub fn banner_with(
        kind: RowKind,
        line: Line<'static>,
        fill: Fill,
        lead: char,
        lead_style: Style,
    ) -> Self {
        Row {
            kind,
            border: None,
            content: RowContent::Unified(Half {
                gutter: (lead_style, lead.to_string()),
                pairs: line
                    .spans
                    .into_iter()
                    .map(|s| (s.style, s.content.into_owned()))
                    .collect(),
                fill,
            }),
        }
    }
}

/// A file's two sides, as lines.
///
/// Normalised ONCE, so the diff and the highlight can no longer disagree about
/// a line's text — they used to, since the diff trimmed trailing whitespace
/// and the highlighter did not.
struct FileSource {
    old: Vec<String>,
    new: Vec<String>,
}

fn source_lines(blob: &str) -> Vec<String> {
    blob.lines()
        .map(|l| expand_tabs(l, TAB_WIDTH).trim_end().to_string())
        .collect()
}

pub struct RowFactory {
    repo: Repo,
    base: String,
    head: String,
    cache: HashMap<String, FileSource>,
    highlighted: usize,
}

impl RowFactory {
    pub fn new(repo: Repo, base: String, head: String) -> Self {
        RowFactory {
            repo,
            base,
            head,
            cache: HashMap::new(),
            highlighted: 0,
        }
    }

    /// Lines syntect has parsed for this factory's lifetime.
    ///
    /// The point of the windowed rebuild is that this stays proportional to
    /// what is drawn rather than to the size of the files touched, and that is
    /// a property worth asserting rather than timing.
    pub fn highlighted_lines(&self) -> usize {
        self.highlighted
    }

    /// Highlight the requested line ranges of one file, one forward pass per
    /// side. A method rather than a free function so reading the blobs first
    /// is not an ordering a caller can get wrong.
    fn highlight(
        &mut self,
        path: &str,
        old_want: &[Range<usize>],
        new_want: &[Range<usize>],
    ) -> (
        HashMap<usize, HighlightedSpans>,
        HashMap<usize, HighlightedSpans>,
    ) {
        self.source(path);
        let hl = highlighter();
        let p = std::path::Path::new(path);
        let src = &self.cache[path];
        let old = hl.highlight_ranges(p, &src.old, old_want);
        let new = hl.highlight_ranges(p, &src.new, new_want);
        let scanned = old.as_ref().map_or(0, |h| h.lines_scanned)
            + new.as_ref().map_or(0, |h| h.lines_scanned);
        self.highlighted += scanned;
        (
            old.map(|h| h.spans).unwrap_or_default(),
            new.map(|h| h.spans).unwrap_or_default(),
        )
    }

    fn source(&mut self, path: &str) -> &FileSource {
        if !self.cache.contains_key(path) {
            let read = |rev: &str| -> Vec<String> {
                let blob = self
                    .repo
                    .blob(rev, path.as_bytes())
                    .ok()
                    .flatten()
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                source_lines(&blob)
            };
            let (old, new) = (read(&self.base), read(&self.head));
            self.cache.insert(path.to_string(), FileSource { old, new });
        }
        &self.cache[path]
    }
}

/// What every hunk-level builder needs, independent of the left-pane view.
pub struct RowsContext<'a> {
    pub doc: &'a schema::PlanDocument,
    /// The projection: what resolves a file's full hunk list and a hunk's
    /// group.
    pub plan: &'a ReviewView,
    pub findings: &'a [Finding],
    /// Hunk indices whose class is marked reviewed.
    pub reviewed: &'a std::collections::HashSet<usize>,
    pub mode: DiffMode,
    /// Label each hunk with its group. The file view needs it, where a hunk's
    /// group membership is otherwise invisible; the group view's header
    /// already says it.
    pub show_group_labels: bool,
    /// Context lines either side of a hunk before any expansion.
    pub context: usize,
    /// Lines one `z` on a boundary row pulls in.
    pub context_step: usize,
    /// How far each hunk has been pulled open, by canonical index.
    pub expansion: &'a HashMap<usize, Expansion>,
}

/// The group view's extras on top of the shared core.
pub struct GroupContext<'a> {
    pub core: RowsContext<'a>,
    pub index: &'a PlanIndex<'a>,
    pub group: &'a schema::Group,
    /// The projected group, carrying resolved dependency edges and the
    /// back-fill flag the header renders.
    pub view: &'a GroupView,
    pub fold: Fold,
}

/// Build the right-pane rows for one group.
pub fn build_group_rows(factory: &mut RowFactory, ctx: &GroupContext) -> Vec<Row> {
    let mut rows = Vec::new();
    header_rows(ctx, &mut rows);

    let split = reading_split(ctx.index, ctx.group, ctx.fold);
    let shown: Vec<usize> = split.shown.iter().map(|h| h.index()).collect();
    hunk_list_rows(factory, &ctx.core, shown, &mut rows);

    // The fold line is presentation over the domain's reason for deferring.
    let what = match split.deferral {
        Deferral::None => return rows,
        Deferral::FoldedNoise => "folded generated hunks",
        Deferral::SkimRemainder => "remaining hunks, same shapes as the exemplars above",
    };
    rows.push(Row::full(
        RowKind::Fold,
        Line::from(Span::styled(
            format!(
                "  ── {} {what} — press z to unfold ──",
                split.deferred.len()
            ),
            Style::default().fg(THEME.noise_fg),
        )),
    ));
    rows
}

/// Shared hunk-list emitter: sort by (file, new_start), then render each file
/// once — header, then the blocks its shown hunks form. Both views feed
/// through here — one implementation, never two renderers to keep in sync.
fn hunk_list_rows(
    factory: &mut RowFactory,
    ctx: &RowsContext,
    mut hunks: Vec<usize>,
    rows: &mut Vec<Row>,
) {
    hunks.sort_by(|&a, &b| {
        let (ha, hb) = (&ctx.doc.hunks[a], &ctx.doc.hunks[b]);
        (ha.file.as_str(), ha.new_start).cmp(&(hb.file.as_str(), hb.new_start))
    });
    // A window's reach is bounded by the NEXT hunk of the file, shown or not,
    // so every file needs its full list — by path, once, rather than a scan
    // per file.
    let by_path: HashMap<&str, &FileView> = ctx
        .plan
        .files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();

    let mut i = 0;
    while i < hunks.len() {
        let path = ctx.doc.hunks[hunks[i]].file.as_str();
        let end = hunks[i..]
            .iter()
            .position(|&h| ctx.doc.hunks[h].file != path)
            .map_or(hunks.len(), |n| i + n);
        rows.push(file_header_row(ctx.doc, path));
        file_rows(
            factory,
            ctx,
            path,
            by_path.get(path).copied(),
            &hunks[i..end],
            rows,
        );
        i = end;
    }
}

/// Rows for a directory: every hunk beneath it, in file order. The shared
/// emitter already writes a file header on each file change.
pub fn build_dir_rows(factory: &mut RowFactory, ctx: &RowsContext, hunks: Vec<usize>) -> Vec<Row> {
    let mut rows = Vec::new();
    if hunks.is_empty() {
        rows.push(Row::full(
            RowKind::Blank,
            Line::from(Span::styled(
                "  (no text hunks under this directory)",
                Style::default().fg(THEME.noise_fg),
            )),
        ));
        return rows;
    }
    hunk_list_rows(factory, ctx, hunks, &mut rows);
    rows
}

/// Build the right-pane rows for one file (the flattened view): every hunk of
/// the file in position order, regardless of grouping; no fold logic — the
/// flat view's whole point is everything, in file order.
pub fn build_file_rows(
    factory: &mut RowFactory,
    ctx: &RowsContext,
    path: &str,
    hunks: Vec<usize>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if hunks.is_empty() {
        // Binary / submodule / mode-only changes carry no text hunks.
        rows.push(file_header_row(ctx.doc, path));
        rows.push(Row::full(
            RowKind::Blank,
            Line::from(Span::styled(
                "  (no text hunks — binary, submodule or mode-only change)",
                Style::default().fg(THEME.noise_fg),
            )),
        ));
        return rows;
    }
    hunk_list_rows(factory, ctx, hunks, &mut rows);
    rows
}

fn header_rows(ctx: &GroupContext, rows: &mut Vec<Row>) {
    let g = ctx.group;
    // The back-fill group is must-read for a different reason from an ordinary
    // focus group — the model never classified it at all — and the stack has
    // always said so. One source for the label, so both renderers agree.
    let tier = ctx.core.plan.tier_name(ctx.view);
    let role = Theme::role_suffix(g.role);
    rows.push(Row::full(
        RowKind::GroupHeader,
        Line::from(vec![
            Span::styled(
                format!("[{tier}] "),
                THEME.effort_style(g.effort).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                g.label.clone(),
                Style::default()
                    .fg(THEME.header_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(role.to_string(), Style::default().fg(THEME.gutter_fg)),
        ]),
    ));
    if !g.description.is_empty() {
        rows.push(Row::full(
            RowKind::GroupHeader,
            Line::from(Span::styled(
                format!("  {}", g.description),
                Style::default().fg(THEME.context_fg),
            )),
        ));
    }
    if !g.depends_on.is_empty() {
        // Edges arrive resolved: the projection already paired each id with
        // its label, so the renderer no longer carries a lookup table.
        let deps: Vec<String> = ctx
            .view
            .depends_on
            .iter()
            .map(|d| format!("{} ({})", d.id, d.label))
            .collect();
        rows.push(Row::full(
            RowKind::GroupHeader,
            Line::from(Span::styled(
                format!("  depends on: {}", deps.join(", ")),
                Style::default().fg(THEME.gutter_fg),
            )),
        ));
    }
    rows.push(Row::full(RowKind::Blank, Line::default()));
}

fn file_header_row(doc: &schema::PlanDocument, path: &str) -> Row {
    let entry = doc.files.iter().find(|f| f.path == path);
    let mut text = format!("▍{path}");
    if let Some(f) = entry {
        if let Some(old) = &f.old_path {
            let sim = f
                .rename_similarity
                .map(|s| format!(", {s}% similar"))
                .unwrap_or_default();
            text.push_str(&format!("  (renamed from {old}{sim})"));
        }
        if f.generated {
            text.push_str("  [generated]");
        }
    }
    Row::full(
        RowKind::FileHeader(path.to_string()),
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(THEME.header_fg)
                .add_modifier(Modifier::BOLD),
        )),
    )
}

/// The `old` / `new` labels over a split file. Built as a split row so the two
/// labels land on the columns through the same arithmetic that places the
/// content, at any pane width.
fn column_header_row() -> Row {
    let label = |text: &str| Half {
        gutter: (Style::default(), String::new()),
        pairs: vec![(
            Style::default()
                .fg(THEME.gutter_fg)
                .add_modifier(Modifier::BOLD),
            format!("  {text}"),
        )],
        fill: Fill::Bg(Style::default()),
    };
    Row {
        kind: RowKind::ColumnHeader,
        border: None,
        content: RowContent::Split {
            old: label("old"),
            new: label("new"),
        },
    }
}

/// Header row for one hunk, plus its findings.
/// A hunk's colour: green once its class is read, otherwise the skim tier's.
/// Shared by the band and the box sides so the two cannot drift apart.
fn hunk_style(ctx: &RowsContext, hi: usize) -> Style {
    if ctx.reviewed.contains(&hi) {
        Style::default().fg(THEME.reviewed_fg)
    } else {
        Style::default().fg(THEME.skim_fg)
    }
}

fn hunk_header_rows(ctx: &RowsContext, hi: usize, foreign: bool, rows: &mut Vec<Row>) {
    let hunk = &ctx.doc.hunks[hi];
    let reviewed = ctx.reviewed.contains(&hi);
    let check = if reviewed { " ✓ reviewed" } else { "" };
    // A foreign hunk ALWAYS names its group: that is the whole point of it
    // being on screen at all, and the dashed border says "not yours" without
    // saying whose.
    let group_label = if ctx.show_group_labels || foreign {
        ctx.plan
            .group_of_hunk(HunkId::from_index(hi))
            .map(|g| format!(" · {}", g.label))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let n_findings = ctx
        .findings
        .iter()
        .filter(|f| f.anchor.hunk_digest == hunk.digest)
        .count();
    let notes = if n_findings > 0 {
        format!("  ◆ {n_findings} finding(s)")
    } else {
        String::new()
    };
    let header_style = hunk_style(ctx, hi);
    // A band across the pane rather than a `@@ -a,b +c,d @@` line. Those
    // coordinates were the only way to know where you were when the gutter
    // showed one number; now every row carries both, so the header repeated
    // what was already on screen in a notation you had to decode. What it
    // uniquely says — the shape class, the size of the change, whether it is
    // read, what is filed against it — is what stays.
    let box_style = if foreign {
        BoxStyle::Foreign
    } else {
        BoxStyle::Own
    };
    let mut spans = vec![
        Span::styled(" ".to_string(), header_style),
        Span::styled(hunk.class.clone(), header_style),
        Span::styled(" · ".to_string(), Style::default().fg(THEME.gutter_fg)),
        Span::styled(
            format!("+{}", hunk.new_count),
            Style::default().fg(THEME.add_fg),
        ),
    ];
    if hunk.old_count > 0 {
        spans.push(Span::styled(" ".to_string(), header_style));
        spans.push(Span::styled(
            format!("−{}", hunk.old_count),
            Style::default().fg(THEME.del_fg),
        ));
    }
    spans.push(Span::styled(
        group_label,
        Style::default().fg(THEME.gutter_fg),
    ));
    spans.push(Span::styled(check.to_string(), header_style));
    spans.push(Span::styled(notes, Style::default().fg(THEME.finding_fg)));
    spans.push(Span::styled(" ".to_string(), header_style));
    rows.push(
        Row::banner_with(
            RowKind::HunkHeader { hunk: hi, foreign },
            Line::from(spans),
            Fill::Rule {
                style: header_style,
                centered: false,
                glyph: box_style.horizontal(),
            },
            box_style.horizontal(),
            header_style,
        )
        .bordered(Part::Top, box_style, header_style),
    );

    for f in ctx
        .findings
        .iter()
        .filter(|f| f.anchor.hunk_digest == hunk.digest)
    {
        let moved = if f.moved { " (moved)" } else { "" };
        rows.push(Row::full(
            RowKind::Finding(f.id.clone(), hi),
            Line::from(Span::styled(
                format!("  ◆ {}{moved}", f.body.lines().next().unwrap_or("")),
                Style::default().fg(THEME.finding_fg),
            )),
        ));
    }
}

/// One file's rows: the blocks its shown hunks form, each hunk's header sitting
/// directly on top of the lines it describes.
fn file_rows(
    factory: &mut RowFactory,
    ctx: &RowsContext,
    path: &str,
    view: Option<&FileView>,
    shown: &[usize],
    rows: &mut Vec<Row>,
) {
    // Binary / submodule files carry no reconstructable text rows.
    let file_entry = ctx.doc.files.iter().find(|f| f.path == path);
    if file_entry.is_some_and(|f| f.binary || f.submodule.is_some()) {
        for &hi in shown {
            hunk_header_rows(ctx, hi, false, rows);
            rows.push(Row::full(
                RowKind::Diff(hi),
                Line::from(Span::styled(
                    "  (binary or submodule change)",
                    Style::default().fg(THEME.noise_fg),
                )),
            ));
        }
        return;
    }

    if ctx.mode == DiffMode::Split {
        rows.push(column_header_row());
    }

    // Every hunk of the file in position order: the ones this view lists, and
    // the ones that merely bound how far a window may reach.
    // The projection always has the file; falling back to the shown set keeps
    // a stale index from silently dropping rows.
    let mut all: Vec<usize> = match view {
        Some(f) => f.hunks.iter().map(|h| h.index()).collect(),
        None => shown.to_vec(),
    };
    all.sort_by_key(|&h| ctx.doc.hunks[h].new_start);
    let candidates: Vec<window::Candidate> = all
        .iter()
        .map(|&h| window::Candidate {
            index: h,
            shown: shown.contains(&h),
            entry: &ctx.doc.hunks[h],
        })
        .collect();

    let (old_len, new_len) = {
        let src = factory.source(path);
        (src.old.len(), src.new.len())
    };
    let blocks = window::plan(&candidates, ctx.expansion, ctx.context, old_len, new_len);

    let (old_want, new_want) = wanted_lines(&blocks);
    let (old_hl, new_hl) = factory.highlight(path, &old_want, &new_want);
    let src = factory.source(path);

    for block in &blocks {
        if let Some(b) = &block.top {
            rows.push(boundary_row(ctx, b, ctx.context_step));
        }
        // A context row acts on the hunk it sits next to — the one below when
        // it leads a block, the one above otherwise — so `space` and `c` do
        // what a reviewer parked on context would expect, even in a merged
        // block spanning several hunks.
        let first_hunk = block.segments.iter().find_map(|s| match s {
            Segment::Change { hunk, .. } => Some(*hunk),
            _ => None,
        });
        let mut above: Option<usize> = None;
        for segment in &block.segments {
            match segment {
                Segment::Context {
                    old_from,
                    new_from,
                    len,
                } => {
                    let owner = above.or(first_hunk).unwrap_or(0);
                    for n in 0..*len {
                        let (o, w) = (old_from + n, new_from + n);
                        let text = src.new.get(w - 1).map(String::as_str).unwrap_or("");
                        let row = context_row(text, o, w, new_hl.get(&(w - 1)), ctx.mode);
                        rows.push(Row {
                            kind: RowKind::Diff(owner),
                            border: None,
                            content: row,
                        });
                    }
                }
                Segment::Change {
                    hunk,
                    foreign,
                    old,
                    new,
                } => {
                    above = Some(*hunk);
                    let box_style = if *foreign {
                        BoxStyle::Foreign
                    } else {
                        BoxStyle::Own
                    };
                    let header_style = hunk_style(ctx, *hunk);
                    hunk_header_rows(ctx, *hunk, *foreign, rows);
                    for content in change_rows(src, old, new, &old_hl, &new_hl, ctx.mode) {
                        rows.push(
                            Row {
                                kind: RowKind::Diff(*hunk),
                                border: None,
                                content,
                            }
                            .bordered(
                                Part::Side,
                                box_style,
                                header_style,
                            ),
                        );
                    }
                    // The box closes under the change; context flows outside
                    // it, so a merged block reads as boxes with file between.
                    rows.push(
                        Row::banner_with(
                            RowKind::HunkFoot,
                            Line::default(),
                            Fill::Rule {
                                style: header_style,
                                centered: false,
                                glyph: box_style.horizontal(),
                            },
                            box_style.horizontal(),
                            header_style,
                        )
                        .bordered(Part::Bottom, box_style, header_style),
                    );
                }
            }
        }
        if let Some(b) = &block.bottom {
            rows.push(boundary_row(ctx, b, ctx.context_step));
        }
        rows.push(Row::full(RowKind::Blank, Line::default()));
    }
}

fn boundary_row(ctx: &RowsContext, b: &window::Boundary, step: usize) -> Row {
    let arrow = match b.side {
        Side::Up => "↑",
        Side::Down => "↓",
    };
    let style = Style::default().fg(THEME.noise_fg);
    let label = match b.next {
        // The gap is exhausted and a hunk stands beyond it. Name it, so the
        // wall is visible and crossing is a deliberate press.
        Some(next) => {
            let class = &ctx.doc.hunks[next].class;
            let group = ctx
                .plan
                .group_of_hunk(HunkId::from_index(next))
                .map(|g| format!(" “{}”", g.label))
                .unwrap_or_default();
            format!(" {arrow} next: {class}{group} — z shows it ")
        }
        None => {
            let where_ = match b.side {
                Side::Up => "above",
                Side::Down => "below",
            };
            format!(
                " {arrow} {} more {where_} — z shows {} ",
                b.hidden,
                step.min(b.hidden)
            )
        }
    };
    // A rule to the pane edge: what is hidden is hidden from BOTH sides, so
    // the row saying so runs across both of them. The label itself is a BUTTON
    // on that rule, not more rule — it is the one thing on the row a reviewer
    // can act on, and as dim text it read as a divider meant to be ignored.
    let button = Style::default()
        .fg(THEME.button_fg)
        .bg(THEME.button_bg)
        .add_modifier(Modifier::BOLD);
    Row::banner(
        RowKind::ContextEdge {
            hunk: b.hunk,
            side: b.side,
            crossing: b.next.is_some(),
        },
        Line::from(Span::styled(format!(" {} ", label.trim()), button)),
        Fill::Rule {
            style,
            centered: true,
            glyph: '─',
        },
    )
}

/// Every line the blocks will draw, as sorted ranges per side.
fn wanted_lines(blocks: &[window::Block]) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut old_want: Vec<Range<usize>> = Vec::new();
    let mut new_want: Vec<Range<usize>> = Vec::new();
    for block in blocks {
        for segment in &block.segments {
            match segment {
                // A context stretch is identical on both sides, so only the
                // new side is ever highlighted for it.
                Segment::Context { new_from, len, .. } => {
                    new_want.push(new_from - 1..new_from - 1 + len);
                }
                Segment::Change { old, new, .. } => {
                    if !old.is_empty() {
                        old_want.push(old.start - 1..old.end - 1);
                    }
                    if !new.is_empty() {
                        new_want.push(new.start - 1..new.end - 1);
                    }
                }
            }
        }
    }
    old_want.sort_by_key(|r| r.start);
    new_want.sort_by_key(|r| r.start);
    (old_want, new_want)
}

/// One unchanged line: the same text on both sides, both numbers in the
/// gutter, no change colour.
fn context_row(
    text: &str,
    old_n: usize,
    new_n: usize,
    hl: Option<&HighlightedSpans>,
    mode: DiffMode,
) -> RowContent {
    let pairs = content_pairs(text, LineOrigin::Context, hl, None);
    match mode {
        DiffMode::Unified => RowContent::Unified(Half {
            gutter: gutter(&format!("{old_n:>4} {new_n:>4}"), LineOrigin::Context),
            pairs,
            fill: Fill::Bg(Style::default()),
        }),
        DiffMode::Split => RowContent::Split {
            old: Half {
                gutter: gutter(&format!("{old_n:>4}"), LineOrigin::Context),
                pairs: pairs.clone(),
                fill: Fill::Bg(Style::default()),
            },
            new: Half {
                gutter: gutter(&format!("{new_n:>4}"), LineOrigin::Context),
                pairs,
                fill: Fill::Bg(Style::default()),
            },
        },
    }
}

/// One hunk's changed lines: `similar` over just those lines on each side,
/// keeping the GitHub-style pairing and the word-level emphasis, with the
/// returned numbers rebased onto the file.
fn change_rows(
    src: &FileSource,
    old: &Range<usize>,
    new: &Range<usize>,
    old_hl: &HashMap<usize, HighlightedSpans>,
    new_hl: &HashMap<usize, HighlightedSpans>,
    mode: DiffMode,
) -> Vec<RowContent> {
    let slice = |lines: &[String], r: &Range<usize>| -> String {
        lines
            .get(r.start.saturating_sub(1)..r.end.saturating_sub(1).min(lines.len()))
            .unwrap_or_default()
            .join("\n")
    };
    let diff = compute_side_by_side(&slice(&src.old, old), &slice(&src.new, new), TAB_WIDTH);
    let mut out = Vec::new();
    for row in &diff {
        // `compute_side_by_side` numbers from 1 within the slice it was given.
        let old_n = row.old_line.as_ref().map(|(n, _)| n + old.start - 1);
        let new_n = row.new_line.as_ref().map(|(n, _)| n + new.start - 1);
        out.extend(render_change_row(row, old_n, new_n, old_hl, new_hl, mode));
    }
    out
}

/// One lumen row → row contents. Unified: one or two rows (Modified → the old
/// line then the new). Split: exactly one row carrying both sides.
fn render_change_row(
    row: &DiffLine,
    old_n: Option<usize>,
    new_n: Option<usize>,
    old_hl: &HashMap<usize, HighlightedSpans>,
    new_hl: &HashMap<usize, HighlightedSpans>,
    mode: DiffMode,
) -> Vec<RowContent> {
    let old_half = || {
        let (n, text) = (old_n?, row.old_line.as_ref()?.1.as_str());
        Some(half(
            n,
            text,
            LineOrigin::Deletion,
            old_hl.get(&(n - 1)),
            row.old_segments.as_deref(),
        ))
    };
    let new_half = || {
        let (n, text) = (new_n?, row.new_line.as_ref()?.1.as_str());
        Some(half(
            n,
            text,
            LineOrigin::Addition,
            new_hl.get(&(n - 1)),
            row.new_segments.as_deref(),
        ))
    };

    // `similar` can find an unchanged line INSIDE a hunk: git decided the
    // hunk's bounds with its own diff, and the two need not pair the same
    // lines. Such a row is context — rendering it as a deletion plus an
    // identical addition would invent a change git never reported.
    if matches!(row.change_type, ChangeType::Equal) {
        let text = row
            .new_line
            .as_ref()
            .or(row.old_line.as_ref())
            .map(|(_, t)| t.as_str())
            .unwrap_or("");
        let (o, w) = (old_n.unwrap_or(0), new_n.unwrap_or(0));
        return vec![context_row(
            text,
            o,
            w,
            new_hl.get(&w.saturating_sub(1)),
            mode,
        )];
    }

    if mode == DiffMode::Split {
        return vec![RowContent::Split {
            old: old_half().unwrap_or_else(Half::hatch),
            new: new_half().unwrap_or_else(Half::hatch),
        }];
    }
    // Unified: a Modified row is the removed line followed by the added one.
    let mut out = Vec::new();
    if !matches!(row.change_type, ChangeType::Insert)
        && let Some(h) = old_half()
    {
        out.push(RowContent::Unified(unify(h, old_n, None)));
    }
    if !matches!(row.change_type, ChangeType::Delete)
        && let Some(h) = new_half()
    {
        out.push(RowContent::Unified(unify(h, None, new_n)));
    }
    out
}

/// Widen a split half's gutter to the unified layout's two columns, in the
/// order they are drawn.
fn unify(mut h: Half, old_n: Option<usize>, new_n: Option<usize>) -> Half {
    let num = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
    h.gutter.1 = format!(" {:>4} {:>4} ", num(old_n), num(new_n));
    h
}

/// One side's content: the gutter block plus the line, on the change colour.
fn half(
    lineno: usize,
    text: &str,
    origin: LineOrigin,
    hl: Option<&HighlightedSpans>,
    segments: Option<&[InlineSegment]>,
) -> Half {
    Half {
        gutter: gutter(&format!("{lineno:>4}"), origin),
        pairs: content_pairs(text, origin, hl, segments),
        fill: Fill::Bg(match THEME.line_bg(origin) {
            Some(c) => Style::default().bg(c),
            None => Style::default(),
        }),
    }
}

/// The line-number cell.
///
/// A leading space is reserved for the cursor marker, so moving the cursor
/// never shifts the pane sideways. On a changed line the cell carries the
/// change colour as a solid block, deliberately stronger than the tint over
/// the code, which is what makes the gutter read as an edge.
fn gutter(text: &str, origin: LineOrigin) -> (Style, String) {
    let mut style = Style::default().fg(match origin {
        LineOrigin::Context => THEME.gutter_fg,
        _ => THEME.context_fg,
    });
    if let Some(bg) = THEME.gutter_bg(origin) {
        style = style.bg(bg);
    }
    (style, format!(" {text} "))
}

/// Syntax pairs for one line with the per-side background and word-level
/// emphasis applied.
fn content_pairs(
    text: &str,
    origin: LineOrigin,
    hl: Option<&HighlightedSpans>,
    segments: Option<&[InlineSegment]>,
) -> Vec<(Style, String)> {
    // Syntax spans for this line, else plain.
    let mut pairs: Vec<(Style, String)> = hl
        .cloned()
        .unwrap_or_else(|| vec![(Style::default().fg(THEME.context_fg), text.to_string())]);

    // Line background per side.
    pairs = highlighter().apply_diff_background(pairs, origin);

    // Word-level emphasis over the changed segments.
    if let Some(segs) = segments {
        let mut ranges = Vec::new();
        let mut off = 0usize;
        for s in segs {
            let end = off + s.text.len();
            if s.emphasized {
                ranges.push((off, end));
            }
            off = end;
        }
        if !ranges.is_empty() {
            pairs = split_pairs_at_ranges(
                &pairs,
                ranges,
                THEME.word_emphasis(matches!(origin, LineOrigin::Addition)),
            );
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git's `-U0` hunks contain only lines git itself considered changed, so
    /// this is defence rather than a common path — but the two diffs are
    /// computed over different inputs (git over the whole file, `similar` over
    /// the hunk's slice), and nothing forces them to agree. If one ever does
    /// come back Equal, it must render as context.
    #[test]
    fn an_equal_row_inside_a_hunk_renders_as_context_not_as_a_delete_and_an_add() {
        let row = DiffLine {
            old_line: Some((7, "same()".into())),
            new_line: Some((9, "same()".into())),
            change_type: ChangeType::Equal,
            old_segments: None,
            new_segments: None,
        };
        let (no_hl, mode) = (HashMap::new(), DiffMode::Unified);
        let out = render_change_row(&row, Some(7), Some(9), &no_hl, &no_hl, mode);

        assert_eq!(out.len(), 1, "one row, not a removal plus an addition");
        let RowContent::Unified(half) = &out[0] else {
            panic!("expected a unified row, got {:?}", out[0]);
        };
        assert!(
            matches!(half.fill, Fill::Bg(s) if s.bg.is_none()),
            "context carries no change colour: {:?}",
            half.fill
        );
        assert!(
            half.gutter.1.contains('7') && half.gutter.1.contains('9'),
            "both line numbers belong in a context gutter: {:?}",
            half.gutter.1
        );
        assert!(half.gutter.0.bg.is_none(), "no gutter block on context");

        // Split mode shows it once on each side, neither of them hatched.
        let out = render_change_row(&row, Some(7), Some(9), &no_hl, &no_hl, DiffMode::Split);
        let RowContent::Split { old, new } = &out[0] else {
            panic!("expected a split row");
        };
        assert!(matches!(old.fill, Fill::Bg(_)) && matches!(new.fill, Fill::Bg(_)));
    }

    /// A modification is two rows in unified mode, and the gutter reads
    /// old-then-new the way the columns are drawn.
    #[test]
    fn a_modified_row_is_the_removal_then_the_addition() {
        let row = DiffLine {
            old_line: Some((4, "was()".into())),
            new_line: Some((4, "now()".into())),
            change_type: ChangeType::Modified,
            old_segments: None,
            new_segments: None,
        };
        let no_hl = HashMap::new();
        let out = render_change_row(&row, Some(4), Some(4), &no_hl, &no_hl, DiffMode::Unified);
        assert_eq!(out.len(), 2);
        let text = |c: &RowContent| match c {
            RowContent::Unified(h) => (
                h.gutter.1.clone(),
                h.pairs.iter().map(|(_, t)| t.clone()).collect::<String>(),
            ),
            other => panic!("expected unified, got {other:?}"),
        };
        let (removed_gutter, removed) = text(&out[0]);
        let (added_gutter, added) = text(&out[1]);
        assert_eq!(removed, "was()");
        assert_eq!(added, "now()");
        // The removal fills the OLD column only, the addition the NEW column,
        // and both keep the leading cell the cursor marker lives in.
        assert_eq!(removed_gutter, format!(" {:>4} {:>4} ", "4", ""));
        assert_eq!(added_gutter, format!(" {:>4} {:>4} ", "", "4"));
        assert_eq!(removed_gutter.len(), added_gutter.len(), "columns align");
        assert!(removed_gutter.starts_with(' ') && added_gutter.starts_with(' '));
    }
}
