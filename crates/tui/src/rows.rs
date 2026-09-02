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
    Deferral, FileView, Fold, GroupView, HunkId, ReviewView, reading_split, role_name,
};
use differential_engine::ports::ObjectReader;
use differential_engine::review_state::Finding;
use differential_engine::schema;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;
use super::vendor::LineOrigin;
use super::vendor::diff_algo::compute_side_by_side;
use super::vendor::diff_types::{ChangeType, DiffLine, InlineSegment, expand_tabs};
use super::vendor::syntax::HighlightedSpans;
use super::vendor::text_utils::split_pairs_at_ranges;
use super::window::{self, Expansion, Segment, Side};

const TAB_WIDTH: usize = 4;

/// A finding's quoting rail. Shared with the renderer, which repeats it down
/// the continuation lines of a wrapped note.
pub const RAIL: char = '▍';

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

/// A row's edge marking which hunk it belongs to.
///
/// An EDGE, not a box: closing the top and bottom with horizontal rules cut
/// the file into slabs and broke the flow of reading down it. What a reviewer
/// needs is to see where a hunk begins and ends without the page being
/// chopped up, and a vertical run down one side says that on its own.
///
/// It sits IN the diff pane's own border column rather than beside it, so it
/// costs the content no width and there are never two vertical lines a cell
/// apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub box_style: BoxStyle,
    /// The hunk this edge belongs to — which one is lit is a cursor question,
    /// and the cursor moves without a rebuild, so drawing resolves it.
    pub hunk: usize,
    /// The colour it takes when it IS the one under the cursor. Every other
    /// edge is muted: a screenful of accents is a screenful of nothing.
    pub active_style: Style,
}

impl Border {
    /// The glyph this row puts in the pane's left border column.
    ///
    /// The match lived on `BoxStyle` behind a `vertical()` that this method
    /// was the only caller of, and this method has one caller of its own. Two
    /// names for one answer is one name too many.
    pub fn glyph(&self) -> char {
        match self.box_style {
            BoxStyle::Own => '│',
            BoxStyle::Foreign => '╎',
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
}

/// The line-number cell: its text, the block it wears, and the brighter block
/// it takes on the row the cursor is on.
///
/// Both styles travel with the row because whether a row is the cursor's is a
/// cursor question — the cursor moves without rebuilding rows, so the row
/// carries the colour it WOULD take and drawing chooses. Same reasoning as a
/// hunk's `Border`, one column over.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Gutter {
    pub text: String,
    pub style: Style,
    pub cursor: Style,
}

impl Gutter {
    /// A cell that takes the same style either way — a banner's, or a boundary
    /// band's, where the row is one tint the whole way across.
    fn flat(text: &str, style: Style) -> Self {
        Gutter {
            text: text.to_string(),
            style,
            cursor: style,
        }
    }
}

/// One side of a diff row: a line-number cell, the content, and what pads the
/// rest.
#[derive(Debug, Clone, PartialEq)]
pub struct Half {
    pub gutter: Gutter,
    pub pairs: Vec<(Style, String)>,
    pub fill: Fill,
}

impl Half {
    /// The absent side of a split row.
    ///
    /// Its line-number cell is blank but keeps the width the real one has, so
    /// the cursor block lands in the same column on both sides of a row that
    /// exists on only one of them.
    fn hatch(theme: &Theme) -> Self {
        Half {
            gutter: Gutter {
                text: format!(" {:4} ", ""),
                style: Style::default(),
                cursor: theme.gutter_cursor(None),
            },
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

/// Where a diff row sits in the file: which side, which line, and its text.
///
/// What a finding anchored to a LINE is built from. A `RowKind::Diff` carries
/// only its hunk, because that is what `space` and `n`/`N` act on; the line
/// numbers were computed and then formatted straight into a gutter string, so
/// nothing structured survived for `c` to read.
#[derive(Debug, Clone, PartialEq)]
pub struct LineRef {
    /// "old" | "new"
    pub side: &'static str,
    pub line: u32,
    /// The same row's number on the OTHER side, where the row shows one.
    ///
    /// A row that exists in both files has two numbers, and which of them a
    /// note anchors to depends on the layout it was written in — a
    /// modification is two rows in unified and one in split. Carrying both
    /// means a note written in either layout is placed in either.
    pub other: Option<(&'static str, u32)>,
    pub text: String,
}

impl LineRef {
    /// This row's number in one side's file, if it has one there.
    ///
    /// A row shows up to two numbers and belongs to up to two files. Which of
    /// them a reader means depends on what they are doing — a note anchors to
    /// one side, and a selection is a run in one side's numbering.
    pub fn line_on(&self, side: &str) -> Option<u32> {
        if self.side == side {
            Some(self.line)
        } else {
            self.other.filter(|(s, _)| *s == side).map(|(_, n)| n)
        }
    }

    /// Does a note anchored to `(side, line)` belong on this row?
    pub fn holds(&self, side: &str, line: u32) -> bool {
        self.line_on(side) == Some(line)
    }
}

pub struct Row {
    pub kind: RowKind,
    pub content: RowContent,
    pub border: Option<Border>,
    /// A glyph drawn in the pane's left border column, for rows that are a
    /// control rather than content.
    pub button: Option<&'static str>,
    /// The source line this row shows, for a finding anchored to it. `None` on
    /// every row that is not one line of a file.
    pub line: Option<LineRef>,
    /// What a hunk header keeps while the cursor is somewhere else.
    ///
    /// The marks — reviewed, and how many findings stand against the hunk —
    /// are FACTS about the hunk, and they are what a reader scans a file for.
    /// The rest of the pill describes it, and a column of descriptions down
    /// the page competes with the code it describes.
    pub idle: Vec<(Style, String)>,
    /// How to work this row, shown only while the cursor is ON it.
    ///
    /// A control that does not say how to work it is a label — but a screenful
    /// of bands each naming the same key is a wall the reader stops reading.
    /// The key belongs on the one row they can press it for. Whether that is
    /// this row is a cursor question, so the row carries the text and drawing
    /// chooses, exactly as it does for a hunk's accent.
    pub hint: Option<(Style, String)>,
}

impl Row {
    pub fn full(kind: RowKind, line: Line<'static>) -> Self {
        Row::banner(kind, line, Fill::Bg(Style::default()))
    }

    /// Drop the leading cell, so the row's band starts against the pane's own
    /// border.
    ///
    /// A hunk's pill caps the edge that runs down the hunk beneath it. A cell
    /// of gap between them read as two marks that happened to line up rather
    /// than as one mark and the run it opens.
    pub fn flush(mut self) -> Self {
        if let RowContent::Unified(half) = &mut self.content {
            half.gutter.text.clear();
        }
        self
    }

    /// Replace a banner's content with pairs built elsewhere — a pill, whose
    /// caps drawing has to find by position.
    pub fn with_pairs(mut self, pairs: Vec<(Style, String)>) -> Self {
        if let RowContent::Unified(half) = &mut self.content {
            half.pairs = pairs;
        }
        self
    }

    /// What survives on this row when the cursor is elsewhere.
    pub fn with_idle(mut self, pairs: Vec<(Style, String)>) -> Self {
        self.idle = pairs;
        self
    }

    /// Text the row adds while the cursor is on it — the key that works it.
    pub fn with_hint(mut self, style: Style, text: String) -> Self {
        self.hint = Some((style, text));
        self
    }

    /// A glyph for the pane's border column — how a boundary band says which
    /// way it opens, in the column a hunk's edge otherwise occupies.
    ///
    /// Tints the reserved cursor cell to match, since a band is one tinted row
    /// the whole way across and that cell is part of it.
    pub fn with_button(mut self, theme: &Theme, glyph: &'static str, tint: Style) -> Self {
        self.button = Some(glyph);
        if let RowContent::Unified(half) = &mut self.content {
            half.gutter.style = tint;
            // The band lightens under the cursor, and this cell is part of it.
            half.gutter.cursor = theme.lit_band(tint);
        }
        self
    }

    pub fn bordered(mut self, box_style: BoxStyle, hunk: usize, active: Style) -> Self {
        self.border = Some(Border {
            box_style,
            hunk,
            active_style: active,
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
        Row {
            kind,
            border: None,
            button: None,
            hint: None,
            idle: Vec::new(),
            line: None,
            content: RowContent::Unified(Half {
                gutter: Gutter::flat(" ", Style::default()),
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
    /// Highlighted lines kept between rebuilds, and which lines have been
    /// offered to syntect at all.
    ///
    /// ADR 0021 deliberately went without this: a rebuild costs O(what is
    /// drawn), which is cheap, so caching looked like complexity for its own
    /// sake. Measurement disagreed — moving between two groups re-highlighted
    /// ~2,600 lines each way on a line-heavy range, about half the cost of the
    /// switch, all of it work already done. The `scanned` marks are separate
    /// from the spans because a line syntect FAILS on has no spans and must not
    /// be retried forever.
    old_hl: Highlights,
    new_hl: Highlights,
}

#[derive(Default)]
struct Highlights {
    spans: HashMap<usize, HighlightedSpans>,
    scanned: Vec<bool>,
}

impl Highlights {
    /// The parts of `want` that have not been through syntect yet.
    ///
    /// Whole ranges, not individual lines: syntect's cost is a forward walk, so
    /// asking for a run with a hole in it is no cheaper than asking for the run.
    fn missing(&self, want: &[Range<usize>]) -> Vec<Range<usize>> {
        want.iter()
            .filter(|r| (r.start..r.end).any(|i| !self.scanned.get(i).copied().unwrap_or(false)))
            .cloned()
            .collect()
    }

    fn record(&mut self, want: &[Range<usize>], got: HashMap<usize, HighlightedSpans>, len: usize) {
        if self.scanned.len() < len {
            self.scanned.resize(len, false);
        }
        let end = self.scanned.len();
        for r in want {
            for i in r.start..r.end.min(end) {
                self.scanned[i] = true;
            }
        }
        self.spans.extend(got);
    }

    /// The spans for `want`, which `record` has already made complete.
    fn take(&self, want: &[Range<usize>]) -> HashMap<usize, HighlightedSpans> {
        want.iter()
            .flat_map(|r| r.start..r.end)
            .filter_map(|i| self.spans.get(&i).map(|s| (i, s.clone())))
            .collect()
    }
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
        theme: &Theme,
        path: &str,
        old_want: &[Range<usize>],
        new_want: &[Range<usize>],
    ) -> (
        HashMap<usize, HighlightedSpans>,
        HashMap<usize, HighlightedSpans>,
    ) {
        self.source(path);
        let hl = theme.highlighter();
        let p = std::path::Path::new(path);
        let src = self.cache.get_mut(path).expect("source() just inserted it");

        // Only what has not been through syntect before. Revisiting a group, or
        // rebuilding after a mark, then costs nothing.
        let mut scanned = 0;
        for (want, lines, into) in [
            (old_want, &src.old, &mut src.old_hl),
            (new_want, &src.new, &mut src.new_hl),
        ] {
            let missing = into.missing(want);
            if missing.is_empty() {
                continue;
            }
            if let Some(got) = hl.highlight_ranges(p, lines, &missing) {
                scanned += got.lines_scanned;
                into.record(&missing, got.spans, lines.len());
            } else {
                // No syntax for this file: mark it done so the probe is not
                // repeated on every rebuild.
                into.record(&missing, HashMap::new(), lines.len());
            }
        }
        self.highlighted += scanned;
        (src.old_hl.take(old_want), src.new_hl.take(new_want))
    }

    /// Read every file the rows are about to draw, in one `git` call.
    ///
    /// Reading a blob costs a process; doing it lazily per file meant two
    /// spawns per file, which was most of the wait the first time a group
    /// opened. The caller already knows the whole set, so it says so.
    fn prefetch(&mut self, paths: &[&str]) {
        let want: Vec<&str> = paths
            .iter()
            .copied()
            .filter(|p| !self.cache.contains_key(*p))
            .collect();
        if want.is_empty() {
            return;
        }
        // Both sides of every file, one list: old then new per path.
        let specs: Vec<(&str, &[u8])> = want
            .iter()
            .flat_map(|p| {
                [
                    (self.base.as_str(), p.as_bytes()),
                    (self.head.as_str(), p.as_bytes()),
                ]
            })
            .collect();
        let Ok(blobs) = self.repo.blobs(&specs) else {
            // A failed batch is not fatal: `source` still reads a file on its
            // own, and reports absence the way it always did.
            return;
        };
        for (path, sides) in want.iter().zip(blobs.chunks(2)) {
            let lines = |b: &Option<Vec<u8>>| match b {
                Some(bytes) => source_lines(&String::from_utf8_lossy(bytes)),
                None => Vec::new(),
            };
            self.cache.insert(
                (*path).to_string(),
                FileSource {
                    old: lines(&sides[0]),
                    new: lines(&sides[1]),
                    old_hl: Highlights::default(),
                    new_hl: Highlights::default(),
                },
            );
        }
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
            self.cache.insert(
                path.to_string(),
                FileSource {
                    old,
                    new,
                    old_hl: Highlights::default(),
                    new_hl: Highlights::default(),
                },
            );
        }
        &self.cache[path]
    }
}

/// What every hunk-level builder needs, independent of the left-pane view.
pub struct RowsContext<'a> {
    /// The palette every row is built with. Colours are baked into rows here,
    /// at BUILD time, not read at draw time — so this is where a theme has to
    /// reach, and changing one means a rebuild rather than a repaint.
    pub theme: &'a Theme,
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
    /// The projected group — and the ONLY handle on it. This carried three:
    /// the raw `schema::Group`, its projection, and a `PlanIndex` that existed
    /// to answer two questions the projection now answers itself.
    pub view: &'a GroupView,
    pub fold: Fold,
}

/// Build the right-pane rows for one group.
pub fn build_group_rows(factory: &mut RowFactory, ctx: &GroupContext) -> Vec<Row> {
    let mut rows = Vec::new();
    header_rows(ctx, &mut rows);

    let split = reading_split(ctx.core.plan, ctx.view, ctx.fold);
    let shown: Vec<usize> = split.shown.iter().map(|h| h.index()).collect();
    hunk_list_rows(factory, &ctx.core, shown, &mut rows);

    // The fold line is presentation over the domain's reason for deferring.
    let what = match split.deferral {
        Deferral::None => return rows,
        Deferral::FoldedNoise => "folded generated hunks",
        Deferral::SkimRemainder => "remaining hunks, same shapes as the exemplars above",
    };
    rows.push(
        Row::full(
            RowKind::Fold,
            Line::from(Span::styled(
                format!("  ── {} {what} ──", split.deferred.len()),
                Style::default().fg(ctx.core.theme.noise_fg),
            )),
        )
        .with_hint(
            Style::default().fg(ctx.core.theme.hint_cursor_fg),
            "  ·  z to show".to_string(),
        ),
    );
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
    // The same, over the document's own entries. The header row and the
    // binary/submodule test each found theirs by scanning `doc.files`, once
    // per file — so laying out N files cost N scans of N.
    let entry_by_path: HashMap<&str, &schema::FileEntry> =
        ctx.doc.files.iter().map(|f| (f.path.as_str(), f)).collect();

    // Every file at once, before any of them is drawn: one `git` call for the
    // group rather than two spawns per file.
    let mut paths: Vec<&str> = hunks
        .iter()
        .map(|&h| ctx.doc.hunks[h].file.as_str())
        .collect();
    // `dedup` removes CONSECUTIVE duplicates only, which is enough because the
    // caller hands hunks in file order — the same fact the loop below relies on
    // when it takes a file's run as one slice. If that order ever stops being
    // by file, this silently prefetches a file twice.
    paths.dedup();
    factory.prefetch(&paths);

    let mut i = 0;
    while i < hunks.len() {
        let path = ctx.doc.hunks[hunks[i]].file.as_str();
        let end = hunks[i..]
            .iter()
            .position(|&h| ctx.doc.hunks[h].file != path)
            .map_or(hunks.len(), |n| i + n);
        let entry = entry_by_path.get(path).copied();
        rows.push(file_header_row(ctx.theme, entry, path));
        file_rows(
            factory,
            ctx,
            path,
            entry,
            by_path.get(path).copied(),
            &hunks[i..end],
            rows,
        );
        i = end;
    }

    place_findings(ctx, rows);
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
                Style::default().fg(ctx.theme.noise_fg),
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
        // Binary / submodule / mode-only changes carry no text hunks. One
        // file, so one lookup — no index to build for it.
        let entry = ctx.doc.files.iter().find(|f| f.path == path);
        rows.push(file_header_row(ctx.theme, entry, path));
        rows.push(Row::full(
            RowKind::Blank,
            Line::from(Span::styled(
                "  (no text hunks — binary, submodule or mode-only change)",
                Style::default().fg(ctx.theme.noise_fg),
            )),
        ));
        return rows;
    }
    hunk_list_rows(factory, ctx, hunks, &mut rows);
    rows
}

fn header_rows(ctx: &GroupContext, rows: &mut Vec<Row>) {
    let g = ctx.view;
    // The back-fill group is must-read for a different reason from an ordinary
    // focus group — the model never classified it at all — and the stack has
    // always said so. One source for the label, so both renderers agree.
    let tier = ctx.core.plan.tier_name(ctx.view);
    let mut header = vec![
        Span::styled(
            format!("[{tier}] "),
            ctx.core
                .theme
                .effort_style(g.effort)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            g.label.clone(),
            Style::default()
                .fg(ctx.core.theme.header_fg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    // The same pill the plan pane gives the role. One fact, one rendering —
    // the grey suffix this replaces was the same `g.role` wearing a different
    // face on the other side of the screen.
    if let Some(r) = g.role {
        let (fg, bg) = ctx.core.theme.pill();
        header.push(Span::styled(" ".to_string(), Style::default()));
        header.extend(
            pill(vec![(fg, role_name(r).to_string())], bg)
                .into_iter()
                .map(|(st, t)| Span::styled(t, st)),
        );
    }
    rows.push(Row::full(RowKind::GroupHeader, Line::from(header)));
    if !g.description.is_empty() {
        rows.push(Row::full(
            RowKind::GroupHeader,
            Line::from(Span::styled(
                format!("  {}", g.description),
                Style::default().fg(ctx.core.theme.context_fg),
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
                Style::default().fg(ctx.core.theme.gutter_fg),
            )),
        ));
    }
    rows.push(Row::full(RowKind::Blank, Line::default()));
}

/// Takes the entry rather than finding it: the caller that draws many files
/// has an index, and the one that draws a single file has one lookup to do.
fn file_header_row(theme: &Theme, entry: Option<&schema::FileEntry>, path: &str) -> Row {
    let mut text = path.to_string();
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
                .fg(theme.header_fg)
                .add_modifier(Modifier::BOLD),
        )),
    )
}

/// The `old` / `new` labels over a split file. Built as a split row so the two
/// labels land on the columns through the same arithmetic that places the
/// content, at any pane width.
fn column_header_row(theme: &Theme) -> Row {
    let label = |text: &str| Half {
        gutter: Gutter::default(),
        pairs: vec![(
            Style::default()
                .fg(theme.gutter_fg)
                .add_modifier(Modifier::BOLD),
            format!("  {text}"),
        )],
        fill: Fill::Bg(Style::default()),
    };
    Row {
        kind: RowKind::ColumnHeader,
        border: None,
        button: None,
        hint: None,
        idle: Vec::new(),
        line: None,
        content: RowContent::Split {
            old: label("old"),
            new: label("new"),
        },
    }
}

/// Header row for one hunk, plus its findings.
/// A pill: one padded run of text on a filled block.
///
/// Square, not rounded — the half-circle caps that would round it are drawn at
/// inconsistent widths across terminals and fonts, and a pill that is a cell
/// wider in one terminal than another is worse than a pill with corners.
///
/// A hunk header's pill is built muted and RECOLOURED at draw time, because
/// whether it is the lit one is a cursor question and the cursor moves without
/// a rebuild. That recolouring rewrites the whole of a row's content, so a pill
/// must BE that content with nothing mixed in beside it.
pub fn pill(
    parts: Vec<(ratatui::style::Color, String)>,
    bg: ratatui::style::Color,
) -> Vec<(Style, String)> {
    let pad = || (Style::default().bg(bg), " ".to_string());
    let mut out = vec![pad()];
    out.extend(
        parts
            .into_iter()
            .map(|(fg, t)| (Style::default().fg(fg).bg(bg), t)),
    );
    out.push(pad());
    out
}

/// The colour a hunk's box takes when the cursor is in it.
///
/// Cyan is "here you are" everywhere else in this view — the pane title, the
/// cursor's bar — so it is what the hunk you are reading wears. A foreign
/// hunk wears the same cyan, muted: it is real code you asked to see, so it
/// belongs to the same family, but it is not on this reading list and a full
/// accent would say it was. Reviewed wins over both: that is the one fact a
/// reader wants at a glance on a hunk they have already been through.
fn hunk_accent(ctx: &RowsContext, hi: usize, foreign: bool) -> Style {
    let fg = match (foreign, ctx.reviewed.contains(&hi)) {
        (true, _) => ctx.theme.foreign_fg,
        (false, true) => ctx.theme.reviewed_fg,
        (false, false) => ctx.theme.header_fg,
    };
    Style::default().fg(fg)
}

fn hunk_header_rows(ctx: &RowsContext, hi: usize, foreign: bool, rows: &mut Vec<Row>) {
    let hunk = &ctx.doc.hunks[hi];
    let reviewed = ctx.reviewed.contains(&hi);
    // A foreign hunk ALWAYS names its group: that is the whole point of it
    // being on screen at all, and the dashed border says "not yours" without
    // saying whose.
    //
    // The id alone, not the label. The id is what the plan pane's rows and
    // their `after:` lines are keyed by, so it is what turns "some other
    // group" into a row you can go and look at — and the label is a sentence,
    // which made the header longer than the code beneath it.
    let group_id = (ctx.show_group_labels || foreign)
        .then(|| {
            ctx.plan
                .group_of_hunk(HunkId::from_index(hi))
                .map(|g| g.id.clone())
        })
        .flatten();
    let group_label = group_id
        .as_ref()
        .map(|id| format!(" · {id}"))
        .unwrap_or_default();
    let n_findings = ctx
        .findings
        .iter()
        .filter(|f| f.anchor.hunk_digest == hunk.digest)
        .count();
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
    // One palette. Whether the cursor is in this hunk is a cursor question, and
    // drawing answers it by lighting the pill's leading cell — not by re-inking
    // the pill, which would need a second ink for every span on it.
    // The SIZE first, then the class. How much changed is what a reader sizes
    // a hunk up by; the class is what they need only once they are in it, and
    // leading with it put a token they cannot read at a glance in front of two
    // numbers they can.
    let (fg, bg) = ctx.theme.pill();

    // The marks an idle header keeps: whose group the hunk is, `✓` for a class
    // already read, `◆ N` for what stands filed against it. A reader scans a
    // file for those; the class and the counts they ask a hunk for once they
    // are in it.
    //
    // The group id is in the IDLE list only. The active pill already names it,
    // right after the class, and appending the marks whole put it there a
    // second time two cells later — `· C158 · g11  g11`, which reads as two
    // facts about the hunk rather than one said twice.
    let mut marks: Vec<(ratatui::style::Color, String)> = Vec::new();
    let mark = |c, t: String, marks: &mut Vec<(ratatui::style::Color, String)>| {
        if !marks.is_empty() {
            marks.push((fg, " ".to_string()));
        }
        marks.push((c, t));
    };
    if reviewed {
        mark(ctx.theme.reviewed_fg, "✓".to_string(), &mut marks);
    }
    if n_findings > 0 {
        mark(ctx.theme.finding_fg, format!("◆ {n_findings}"), &mut marks);
    }

    let mut idle_marks: Vec<(ratatui::style::Color, String)> = Vec::new();
    if let Some(id) = &group_id {
        idle_marks.push((fg, id.clone()));
        if !marks.is_empty() {
            idle_marks.push((fg, " ".to_string()));
        }
    }
    idle_marks.extend(marks.iter().cloned());

    let mut parts = vec![(ctx.theme.add_fg, format!("+{}", hunk.new_count))];
    if hunk.old_count > 0 {
        parts.push((fg, " ".to_string()));
        parts.push((ctx.theme.del_fg, format!("−{}", hunk.old_count)));
    }
    parts.push((fg, format!(" · {}{group_label}", hunk.class)));
    if !marks.is_empty() {
        parts.push((fg, "  ".to_string()));
        parts.extend(marks.iter().cloned());
    }
    let idle = if idle_marks.is_empty() {
        Vec::new()
    } else {
        pill(idle_marks, bg)
    };
    rows.push(
        Row::banner(
            RowKind::HunkHeader { hunk: hi, foreign },
            Line::default(),
            Fill::Hatch,
        )
        .with_pairs(pill(parts, bg))
        .with_idle(idle)
        .flush()
        .bordered(box_style, hi, hunk_accent(ctx, hi, foreign)),
    );
}

/// One finding, as the rows that show it.
///
/// A quoted panel: every line of the note behind a muted rail, in muted
/// italics. It is prose the reviewer wrote about the code above it, so it has
/// to read as a different KIND of thing from the code without competing with
/// it — which a bright marker glyph on one truncated line did not.
///
/// Every line is a `Finding` row, so `dd` deletes the note from any of them
/// and the cursor never lands on a line that belongs to nothing.
fn finding_rows(theme: &Theme, f: &Finding, hunk: usize) -> Vec<Row> {
    let rail = Style::default().fg(theme.gutter_fg);
    let prose = Style::default()
        .fg(theme.hint_fg)
        .add_modifier(Modifier::ITALIC);
    let moved = if f.moved { " (moved)" } else { "" };
    let mut lines: Vec<String> = f.body.lines().map(str::to_string).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let last = lines.len() - 1;
    lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let text = if i == last {
                format!("{text}{moved}")
            } else {
                text
            };
            Row::full(
                RowKind::Finding(f.id.clone(), hunk),
                Line::from(vec![
                    Span::styled(format!("  {RAIL} "), rail),
                    Span::styled(text, prose),
                ]),
            )
        })
        .collect()
}

/// Put each finding under the line it annotates.
///
/// A finding anchors to a LINE, and a note under the line it is about is one
/// you read without holding a number in your head. Findings all sat under the
/// hunk header instead, because that was the only thing they could anchor to.
///
/// A finding whose line is not on screen still has to appear — the context
/// around it may still be folded, or a regeneration may have re-anchored it to
/// the hunk — so anything unplaced falls back to its hunk's header, where they
/// all used to be.
fn place_findings(ctx: &RowsContext, rows: &mut Vec<Row>) {
    if ctx.findings.is_empty() {
        return;
    }
    let mut left: Vec<&Finding> = ctx.findings.iter().collect();
    let mut out: Vec<Row> = Vec::with_capacity(rows.len() + left.len());
    let mut file = String::new();

    for row in std::mem::take(rows) {
        if let RowKind::FileHeader(path) = &row.kind {
            file.clone_from(path);
        }
        // Keyed on the file and the line, not on the hunk digest as well: a
        // context row's hunk is the one it sits NEXT to, which is a guess, and
        // a line is in exactly one place either way. `holds` takes both of a
        // row's numbers, so a note survives the layout it was written in.
        let here: Vec<&Finding> = match (&row.kind, &row.line) {
            (RowKind::Diff(_), Some(l)) => left
                .iter()
                .copied()
                .filter(|f| f.anchor.file == file && l.holds(&f.anchor.side, f.anchor.end_line))
                .collect(),
            _ => Vec::new(),
        };
        let hunk = row.kind.hunk().unwrap_or(0);
        out.push(row);
        for f in here {
            left.retain(|g| g.id != f.id);
            out.extend(finding_rows(ctx.theme, f, hunk));
        }
    }

    // Whatever found no line goes back under its hunk's header.
    if !left.is_empty() {
        let mut with_headers = Vec::with_capacity(out.len() + left.len());
        for row in out {
            let under: Vec<&Finding> = match &row.kind {
                RowKind::HunkHeader { hunk, .. } => {
                    let digest = &ctx.doc.hunks[*hunk].digest;
                    left.iter()
                        .copied()
                        .filter(|f| &f.anchor.hunk_digest == digest)
                        .collect()
                }
                _ => Vec::new(),
            };
            let hunk = row.kind.hunk().unwrap_or(0);
            with_headers.push(row);
            for f in under {
                with_headers.extend(finding_rows(ctx.theme, f, hunk));
            }
        }
        out = with_headers;
    }
    *rows = out;
}

/// One file's rows: the blocks its shown hunks form, each hunk's header sitting
/// directly on top of the lines it describes.
fn file_rows(
    factory: &mut RowFactory,
    ctx: &RowsContext,
    path: &str,
    file_entry: Option<&schema::FileEntry>,
    view: Option<&FileView>,
    shown: &[usize],
    rows: &mut Vec<Row>,
) {
    // Binary / submodule files carry no reconstructable text rows.
    if file_entry.is_some_and(|f| f.binary || f.submodule.is_some()) {
        for &hi in shown {
            hunk_header_rows(ctx, hi, false, rows);
            rows.push(Row::full(
                RowKind::Diff(hi),
                Line::from(Span::styled(
                    "  (binary or submodule change)",
                    Style::default().fg(ctx.theme.noise_fg),
                )),
            ));
        }
        return;
    }

    if ctx.mode == DiffMode::Split {
        rows.push(column_header_row(ctx.theme));
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
    let shown_set: std::collections::HashSet<usize> = shown.iter().copied().collect();
    let candidates: Vec<window::Candidate> = all
        .iter()
        .map(|&h| window::Candidate {
            index: h,
            shown: shown_set.contains(&h),
            entry: &ctx.doc.hunks[h],
        })
        .collect();

    let (old_len, new_len) = {
        let src = factory.source(path);
        (src.old.len(), src.new.len())
    };
    let blocks = window::plan(&candidates, ctx.expansion, ctx.context, old_len, new_len);

    let (old_want, new_want) = wanted_lines(&blocks);
    let (old_hl, new_hl) = factory.highlight(ctx.theme, path, &old_want, &new_want);
    let src = factory.source(path);

    // Set when the previous block's bottom row already spoke for the gap on its
    // own, so this block's top row would only repeat it.
    let mut spoken_for = false;
    for (n, block) in blocks.iter().enumerate() {
        if let Some(b) = &block.top
            && !spoken_for
        {
            rows.push(boundary_row(ctx, b, ctx.context_step, false));
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
                        let row =
                            context_row(ctx.theme, text, o, w, new_hl.get(&(w - 1)), ctx.mode);
                        rows.push(Row {
                            kind: RowKind::Diff(owner),
                            border: None,
                            button: None,
                            hint: None,
                            idle: Vec::new(),
                            // An unchanged line exists on both sides. A note
                            // written here is about the code as it will be, so
                            // it anchors NEW — but the row shows both numbers,
                            // so a note on either belongs on it.
                            line: Some(LineRef {
                                side: "new",
                                line: w as u32,
                                other: Some(("old", o as u32)),
                                text: text.to_string(),
                            }),
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
                    let accent = hunk_accent(ctx, *hunk, *foreign);
                    hunk_header_rows(ctx, *hunk, *foreign, rows);
                    for (content, line) in
                        change_rows(ctx.theme, src, old, new, &old_hl, &new_hl, ctx.mode)
                    {
                        rows.push(
                            Row {
                                kind: RowKind::Diff(*hunk),
                                border: None,
                                button: None,
                                hint: None,
                                idle: Vec::new(),
                                line,
                                content,
                            }
                            .bordered(box_style, *hunk, accent),
                        );
                    }
                }
            }
        }
        spoken_for = false;
        if let Some(b) = &block.bottom {
            // Two rows exist to offer a DIRECTION. When both ends would do the
            // same thing there is none to offer and the second row only repeats
            // the first, so one row speaks for both.
            //
            // Two ways that happens: one press closes the gap, or the gap is
            // spent at both ends and they name the same hunk beyond.
            spoken_for = blocks
                .get(n + 1)
                .and_then(|next| next.top.as_ref())
                .is_some_and(|t| match (b.next, t.next) {
                    (None, None) => b.hidden <= ctx.context_step,
                    (Some(x), Some(y)) => x == y,
                    _ => false,
                });
            rows.push(boundary_row(ctx, b, ctx.context_step, spoken_for));
        }
        // A blank separates a block from what follows — but NOT two boundary
        // rows, which describe one gap between two blocks and read as one band
        // when they touch.
        let joins_next =
            block.bottom.is_some() && blocks.get(n + 1).is_some_and(|next| next.top.is_some());
        if !joins_next {
            rows.push(Row::full(RowKind::Blank, Line::default()));
        }
    }
}

/// One boundary row. `both_ends` when this row stands for its own end AND the
/// one facing it, which is what turns the arrow into `↕`.
fn boundary_row(ctx: &RowsContext, b: &window::Boundary, step: usize, both_ends: bool) -> Row {
    let arrow = if both_ends {
        "↕"
    } else {
        match b.side {
            Side::Up => "↑",
            Side::Down => "↓",
        }
    };
    // The key goes on the band while the cursor is on it. A control that does
    // not say how to work it is a label — but every band saying the same key
    // at once is a wall, so the row carries the key and drawing shows it on
    // the one row the reader can press it for.
    let (label, hint) = match b.next {
        // The gap is exhausted and a hunk stands beyond it. Name it, so the
        // wall is visible and crossing is a deliberate press.
        Some(next) => {
            let class = &ctx.doc.hunks[next].class;
            let group = ctx
                .plan
                .group_of_hunk(HunkId::from_index(next))
                .map(|g| format!(" “{}”", g.label))
                .unwrap_or_default();
            (format!("next: {class}{group}"), "z shows it".to_string())
        }
        None => (
            format!("{} lines hidden", b.hidden),
            format!("z shows {}", step.min(b.hidden)),
        ),
    };
    // A band, not a rule: two of these sit adjacent where two blocks meet, and
    // a tinted row with the arrow in the border column reads as one seam in the
    // file rather than as two unrelated notices. `@@` is deliberately absent —
    // it is the notation the hunk headers dropped, and the gutters either side
    // already carry the numbers.
    Row::banner(
        RowKind::ContextEdge {
            hunk: b.hunk,
            side: b.side,
            crossing: b.next.is_some(),
        },
        Line::from(vec![
            Span::styled(" ".to_string(), Style::default().bg(ctx.theme.hint_bg)),
            Span::styled(
                label,
                Style::default().fg(ctx.theme.hint_fg).bg(ctx.theme.hint_bg),
            ),
        ]),
        Fill::Bg(Style::default().bg(ctx.theme.hint_bg)),
    )
    .with_button(ctx.theme, arrow, Style::default().bg(ctx.theme.hint_bg))
    .with_hint(
        Style::default()
            .fg(ctx.theme.hint_cursor_fg)
            .bg(ctx.theme.hint_cursor_bg)
            .add_modifier(Modifier::BOLD),
        format!("  ·  {hint}"),
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
    theme: &Theme,
    text: &str,
    old_n: usize,
    new_n: usize,
    hl: Option<&HighlightedSpans>,
    mode: DiffMode,
) -> RowContent {
    let pairs = content_pairs(theme, text, LineOrigin::Context, hl, None);
    match mode {
        DiffMode::Unified => RowContent::Unified(Half {
            gutter: gutter(
                theme,
                &format!("{old_n:>4} {new_n:>4}"),
                LineOrigin::Context,
            ),
            pairs,
            fill: Fill::Bg(Style::default()),
        }),
        DiffMode::Split => RowContent::Split {
            old: Half {
                gutter: gutter(theme, &format!("{old_n:>4}"), LineOrigin::Context),
                pairs: pairs.clone(),
                fill: Fill::Bg(Style::default()),
            },
            new: Half {
                gutter: gutter(theme, &format!("{new_n:>4}"), LineOrigin::Context),
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
    theme: &Theme,
    src: &FileSource,
    old: &Range<usize>,
    new: &Range<usize>,
    old_hl: &HashMap<usize, HighlightedSpans>,
    new_hl: &HashMap<usize, HighlightedSpans>,
    mode: DiffMode,
) -> Vec<(RowContent, Option<LineRef>)> {
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
        out.extend(render_change_row(
            theme, row, old_n, new_n, old_hl, new_hl, mode,
        ));
    }
    out
}

/// One lumen row → row contents. Unified: one or two rows (Modified → the old
/// line then the new). Split: exactly one row carrying both sides.
fn render_change_row(
    theme: &Theme,
    row: &DiffLine,
    old_n: Option<usize>,
    new_n: Option<usize>,
    old_hl: &HashMap<usize, HighlightedSpans>,
    new_hl: &HashMap<usize, HighlightedSpans>,
    mode: DiffMode,
) -> Vec<(RowContent, Option<LineRef>)> {
    let at = |side, n: Option<usize>, text: &str, other: Option<(&'static str, u32)>| {
        n.map(|n| LineRef {
            side,
            line: n as u32,
            other,
            text: text.to_string(),
        })
    };
    let other = |side, n: Option<usize>| n.map(|n| (side, n as u32));
    let old_half = || {
        let (n, text) = (old_n?, row.old_line.as_ref()?.1.as_str());
        Some(half(
            theme,
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
            theme,
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
        return vec![(
            context_row(theme, text, o, w, new_hl.get(&w.saturating_sub(1)), mode),
            at("new", new_n, text, other("old", old_n)).or_else(|| at("old", old_n, text, None)),
        )];
    }

    let old_text = row.old_line.as_ref().map(|(_, t)| t.as_str()).unwrap_or("");
    let new_text = row.new_line.as_ref().map(|(_, t)| t.as_str()).unwrap_or("");
    if mode == DiffMode::Split {
        // One row, two sides. It anchors to the new side where there is one:
        // a note on a modification is a note on what the change became.
        return vec![(
            RowContent::Split {
                old: old_half().unwrap_or_else(|| Half::hatch(theme)),
                new: new_half().unwrap_or_else(|| Half::hatch(theme)),
            },
            // One row, both sides. A note written on the removed half in the
            // unified layout is anchored OLD, and this is the row that holds
            // it here — without the other side it had nowhere to land, and
            // fell back to the hunk's header on every `s`.
            at("new", new_n, new_text, other("old", old_n))
                .or_else(|| at("old", old_n, old_text, None)),
        )];
    }
    // Unified: a Modified row is the removed line followed by the added one,
    // and each anchors to its own side.
    let mut out = Vec::new();
    if !matches!(row.change_type, ChangeType::Insert)
        && let Some(h) = old_half()
    {
        out.push((
            RowContent::Unified(unify(h, old_n, None)),
            at("old", old_n, old_text, None),
        ));
    }
    if !matches!(row.change_type, ChangeType::Delete)
        && let Some(h) = new_half()
    {
        out.push((
            RowContent::Unified(unify(h, None, new_n)),
            at("new", new_n, new_text, None),
        ));
    }
    out
}

/// Widen a split half's gutter to the unified layout's two columns, in the
/// order they are drawn.
fn unify(mut h: Half, old_n: Option<usize>, new_n: Option<usize>) -> Half {
    let num = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
    h.gutter.text = format!(" {:>4} {:>4} ", num(old_n), num(new_n));
    h
}

/// One side's content: the gutter block plus the line, on the change colour.
fn half(
    theme: &Theme,
    lineno: usize,
    text: &str,
    origin: LineOrigin,
    hl: Option<&HighlightedSpans>,
    segments: Option<&[InlineSegment]>,
) -> Half {
    Half {
        gutter: gutter(theme, &format!("{lineno:>4}"), origin),
        pairs: content_pairs(theme, text, origin, hl, segments),
        fill: Fill::Bg(match theme.line_bg(origin) {
            Some(c) => Style::default().bg(c),
            None => Style::default(),
        }),
    }
}

/// The line-number cell.
///
/// A space either side, so the number is a block rather than a run of digits
/// against the code. On a changed line that block carries the change colour,
/// deliberately stronger than the tint over the code, which is what makes the
/// gutter read as an edge — and brighter again on the cursor's row.
fn gutter(theme: &Theme, text: &str, origin: LineOrigin) -> Gutter {
    let mut style = Style::default().fg(match origin {
        LineOrigin::Context => theme.gutter_fg,
        _ => theme.context_fg,
    });
    if let Some(bg) = theme.gutter_bg(origin) {
        style = style.bg(bg);
    }
    Gutter {
        text: format!(" {text} "),
        style,
        cursor: theme.gutter_cursor(theme.gutter_bg(origin)),
    }
}

/// Syntax pairs for one line with the per-side background and word-level
/// emphasis applied.
fn content_pairs(
    theme: &Theme,
    text: &str,
    origin: LineOrigin,
    hl: Option<&HighlightedSpans>,
    segments: Option<&[InlineSegment]>,
) -> Vec<(Style, String)> {
    // Syntax spans for this line, else plain.
    let mut pairs: Vec<(Style, String)> = hl
        .cloned()
        .unwrap_or_else(|| vec![(Style::default().fg(theme.context_fg), text.to_string())]);

    // Line background per side.
    pairs = theme.highlighter().apply_diff_background(pairs, origin);

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
                theme.word_emphasis(matches!(origin, LineOrigin::Addition)),
            );
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any palette will do here: these tests are about which rows come out of
    /// a diff, not what colour they are.
    fn theme() -> Theme {
        Theme::named(differential_engine::config::ThemeName::Dark)
    }

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
        let out = render_change_row(&theme(), &row, Some(7), Some(9), &no_hl, &no_hl, mode);

        assert_eq!(out.len(), 1, "one row, not a removal plus an addition");
        let RowContent::Unified(half) = &out[0].0 else {
            panic!("expected a unified row, got {:?}", out[0]);
        };
        assert_eq!(
            out[0].1.as_ref().map(|l| (l.side, l.line)),
            Some(("new", 9)),
            "a context row anchors to its new-side line"
        );
        assert!(
            matches!(half.fill, Fill::Bg(s) if s.bg.is_none()),
            "context carries no change colour: {:?}",
            half.fill
        );
        assert!(
            half.gutter.text.contains('7') && half.gutter.text.contains('9'),
            "both line numbers belong in a context gutter: {:?}",
            half.gutter.text
        );
        assert!(half.gutter.style.bg.is_none(), "no gutter block on context");

        // Split mode shows it once on each side, neither of them hatched.
        let out = render_change_row(
            &theme(),
            &row,
            Some(7),
            Some(9),
            &no_hl,
            &no_hl,
            DiffMode::Split,
        );
        let RowContent::Split { old, new } = &out[0].0 else {
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
        let out = render_change_row(
            &theme(),
            &row,
            Some(4),
            Some(4),
            &no_hl,
            &no_hl,
            DiffMode::Unified,
        );
        assert_eq!(out.len(), 2);
        let text = |c: &RowContent| match c {
            RowContent::Unified(h) => (
                h.gutter.text.clone(),
                h.pairs.iter().map(|(_, t)| t.clone()).collect::<String>(),
            ),
            other => panic!("expected unified, got {other:?}"),
        };
        let (removed_gutter, removed) = text(&out[0].0);
        let (added_gutter, added) = text(&out[1].0);
        // Each half anchors to its own side, so a note on the removed line is
        // a note on the old file and one on the added line is on the new.
        assert_eq!(
            out[0].1.as_ref().map(|l| (l.side, l.line)),
            Some(("old", 4))
        );
        assert_eq!(
            out[1].1.as_ref().map(|l| (l.side, l.line)),
            Some(("new", 4))
        );
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
