//! The single row builder (tuicr's lesson applied): the row-kind array IS the
//! output of the builder that renders the lines, so navigation and drawing can
//! never disagree about what a row is.

use std::collections::HashMap;

use differential_engine::gitio::Repo;
use differential_engine::review_state::Finding;
use differential_engine::schema;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::{THEME, highlighter};
use super::vendor::LineOrigin;
use super::vendor::diff_algo::compute_side_by_side;
use super::vendor::diff_types::{ChangeType, DiffLine, InlineSegment, expand_tabs};
use super::vendor::syntax::HighlightedLines;
use super::vendor::text_utils::split_pairs_at_ranges;

const CONTEXT: usize = 3;
const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    GroupHeader,
    /// A file header carrying its path (the file-list modal jumps to these).
    FileHeader(String),
    /// Canonical hunk index.
    HunkHeader(usize),
    /// A diff content row belonging to a hunk.
    Diff(usize),
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
            RowKind::HunkHeader(_) | RowKind::Diff(_) | RowKind::Finding(_, _) | RowKind::Fold
        )
    }

    pub fn hunk(&self) -> Option<usize> {
        match self {
            RowKind::HunkHeader(h) | RowKind::Diff(h) | RowKind::Finding(_, h) => Some(*h),
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

#[derive(Debug, Clone, PartialEq)]
pub enum RowContent {
    Full(Line<'static>),
    Split {
        old: Vec<(Style, String)>,
        new: Vec<(Style, String)>,
    },
}

pub struct Row {
    pub kind: RowKind,
    pub content: RowContent,
}

impl Row {
    pub fn full(kind: RowKind, line: Line<'static>) -> Self {
        Row {
            kind,
            content: RowContent::Full(line),
        }
    }
}

/// Per-file computed rows + pre-baked syntax highlighting, cached across
/// group switches.
pub struct FileRows {
    rows: Vec<DiffLine>,
    old_hl: Option<HighlightedLines>,
    new_hl: Option<HighlightedLines>,
}

pub struct RowFactory {
    repo: Repo,
    base: String,
    head: String,
    cache: HashMap<String, FileRows>,
}

impl RowFactory {
    pub fn new(repo: Repo, base: String, head: String) -> Self {
        RowFactory {
            repo,
            base,
            head,
            cache: HashMap::new(),
        }
    }

    fn file_rows(&mut self, path: &str) -> &FileRows {
        if !self.cache.contains_key(path) {
            let old = self
                .repo
                .blob(&self.base, path.as_bytes())
                .ok()
                .flatten()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let new = self
                .repo
                .blob(&self.head, path.as_bytes())
                .ok()
                .flatten()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let rows = compute_side_by_side(&old, &new, TAB_WIDTH);
            let hl = highlighter();
            let expand =
                |s: &str| -> Vec<String> { s.lines().map(|l| expand_tabs(l, TAB_WIDTH)).collect() };
            let p = std::path::Path::new(path);
            let old_hl = hl.highlight_file_lines(p, &expand(&old));
            let new_hl = hl.highlight_file_lines(p, &expand(&new));
            self.cache.insert(
                path.to_string(),
                FileRows {
                    rows,
                    old_hl,
                    new_hl,
                },
            );
        }
        &self.cache[path]
    }
}

/// What every hunk-level builder needs, independent of the left-pane view.
pub struct RowsContext<'a> {
    pub doc: &'a schema::PlanDocument,
    pub findings: &'a [Finding],
    /// Hunk indices whose class is marked reviewed.
    pub reviewed: &'a std::collections::HashSet<usize>,
    pub mode: DiffMode,
    /// Hunk index -> owning group label, shown on hunk headers in the file
    /// view (where group membership is otherwise invisible).
    pub hunk_labels: Option<&'a HashMap<usize, String>>,
}

/// The group view's extras on top of the shared core.
pub struct GroupContext<'a> {
    pub core: RowsContext<'a>,
    pub group: &'a schema::Group,
    /// Group id -> label, for rendering depends_on legibly.
    pub labels: &'a HashMap<String, String>,
    pub fold_open: bool,
}

/// Build the right-pane rows for one group.
pub fn build_group_rows(factory: &mut RowFactory, ctx: &GroupContext) -> Vec<Row> {
    let mut rows = Vec::new();
    header_rows(ctx, &mut rows);

    let class_by_id: HashMap<&str, &schema::ClassEntry> = ctx
        .core
        .doc
        .classes
        .iter()
        .map(|c| (c.id.as_str(), c))
        .collect();
    let hunk_idx = |hid: &str| -> usize { hid[1..].parse().expect("hunk ids are h<N>") };

    // Which hunks to show expanded vs behind the fold.
    let (shown, hidden): (Vec<usize>, Vec<usize>) = match ctx.group.effort {
        schema::Effort::Skim if !ctx.fold_open => {
            let ex: Vec<usize> = ctx
                .group
                .class_ids
                .iter()
                .map(|c| hunk_idx(&class_by_id[c.as_str()].exemplar))
                .collect();
            let rest: Vec<usize> = ctx
                .group
                .class_ids
                .iter()
                .flat_map(|c| {
                    let class = class_by_id[c.as_str()];
                    class
                        .hunk_ids
                        .iter()
                        .filter(|h| **h != class.exemplar)
                        .map(|h| hunk_idx(h))
                })
                .collect();
            (ex, rest)
        }
        schema::Effort::Noise if !ctx.fold_open => {
            let all: Vec<usize> = ctx
                .group
                .class_ids
                .iter()
                .flat_map(|c| class_by_id[c.as_str()].hunk_ids.iter().map(|h| hunk_idx(h)))
                .collect();
            (Vec::new(), all)
        }
        _ => {
            let all: Vec<usize> = ctx
                .group
                .class_ids
                .iter()
                .flat_map(|c| class_by_id[c.as_str()].hunk_ids.iter().map(|h| hunk_idx(h)))
                .collect();
            (all, Vec::new())
        }
    };

    hunk_list_rows(factory, &ctx.core, shown, &mut rows);

    if !hidden.is_empty() {
        let what = match ctx.group.effort {
            schema::Effort::Noise => "folded generated hunks",
            _ => "remaining hunks, same shapes as the exemplars above",
        };
        rows.push(Row::full(
            RowKind::Fold,
            Line::from(Span::styled(
                format!("  ── {} {what} — press z to unfold ──", hidden.len()),
                Style::default().fg(THEME.noise_fg),
            )),
        ));
    }
    rows
}

/// Shared hunk-list emitter: sort by (file, new_start), emit a file header on
/// every file change, then the hunk's rows. Both views feed through here —
/// one implementation, never two renderers to keep in sync.
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
    let mut current_file: Option<&str> = None;
    for hi in hunks {
        let hunk = &ctx.doc.hunks[hi];
        if current_file != Some(hunk.file.as_str()) {
            current_file = Some(hunk.file.as_str());
            rows.push(file_header_row(ctx.doc, &hunk.file));
        }
        hunk_rows(factory, ctx, hi, rows);
    }
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
    let tier = match g.effort {
        schema::Effort::Close => "close",
        schema::Effort::Skim => "skim",
        schema::Effort::Noise => "noise",
    };
    let role = g
        .role
        .map(|r| match r {
            schema::Role::Foundation => " · foundation",
            schema::Role::Consumer => " · consumer",
            schema::Role::Mechanical => " · mechanical",
            schema::Role::Noise => " · noise",
        })
        .unwrap_or("");
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
        let deps: Vec<String> = g
            .depends_on
            .iter()
            .map(|id| {
                ctx.labels
                    .get(id)
                    .map(|l| format!("{id} ({l})"))
                    .unwrap_or_else(|| id.clone())
            })
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

fn hunk_rows(factory: &mut RowFactory, ctx: &RowsContext, hi: usize, rows: &mut Vec<Row>) {
    let hunk = &ctx.doc.hunks[hi];
    let reviewed = ctx.reviewed.contains(&hi);
    let check = if reviewed { " ✓ reviewed" } else { "" };
    let group_label = ctx
        .hunk_labels
        .and_then(|m| m.get(&hi))
        .map(|l| format!("  · {l}"))
        .unwrap_or_default();
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
    let header_style = if reviewed {
        Style::default().fg(THEME.reviewed_fg)
    } else {
        Style::default().fg(THEME.skim_fg)
    };
    rows.push(Row::full(
        RowKind::HunkHeader(hi),
        Line::from(vec![
            Span::styled(
                format!(
                    "@@ -{},{} +{},{} @@  {}{group_label}{check}",
                    hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count, hunk.class
                ),
                header_style,
            ),
            Span::styled(notes, Style::default().fg(THEME.finding_fg)),
        ]),
    ));

    // Findings under the header.
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

    // Binary / submodule files carry no reconstructable text rows.
    let file_entry = ctx.doc.files.iter().find(|f| f.path == hunk.file);
    if file_entry.is_some_and(|f| f.binary || f.submodule.is_some()) {
        rows.push(Row::full(
            RowKind::Diff(hi),
            Line::from(Span::styled(
                "  (binary or submodule change)",
                Style::default().fg(THEME.noise_fg),
            )),
        ));
        return;
    }

    let file_rows = factory.file_rows(&hunk.file);
    let range = hunk_row_range(&file_rows.rows, hunk);
    let Some((start, end)) = range else {
        rows.push(Row::full(
            RowKind::Diff(hi),
            Line::from(Span::styled(
                "  (content unavailable)",
                Style::default().fg(THEME.noise_fg),
            )),
        ));
        return;
    };
    let from = start.saturating_sub(CONTEXT);
    let to = (end + CONTEXT + 1).min(file_rows.rows.len());
    for row in &file_rows.rows[from..to] {
        for content in render_diff_row(row, file_rows, ctx.mode) {
            rows.push(Row {
                kind: RowKind::Diff(hi),
                content,
            });
        }
    }
    rows.push(Row::full(RowKind::Blank, Line::default()));
}

/// Locate the row span covered by a canonical -U0 hunk.
fn hunk_row_range(rows: &[DiffLine], hunk: &schema::HunkEntry) -> Option<(usize, usize)> {
    let in_new = |n: usize| {
        hunk.new_count > 0
            && n >= hunk.new_start as usize
            && n < (hunk.new_start + hunk.new_count) as usize
    };
    let in_old = |n: usize| {
        hunk.old_count > 0
            && n >= hunk.old_start as usize
            && n < (hunk.old_start + hunk.old_count) as usize
    };
    let mut first = None;
    let mut last = None;
    for (i, r) in rows.iter().enumerate() {
        let hit = !matches!(r.change_type, ChangeType::Equal)
            && (r.new_line.as_ref().is_some_and(|(n, _)| in_new(*n))
                || r.old_line.as_ref().is_some_and(|(n, _)| in_old(*n)));
        if hit {
            if first.is_none() {
                first = Some(i);
            }
            last = Some(i);
        }
    }
    first.zip(last)
}

/// One lumen row → row contents. Unified: one or two full lines (Modified →
/// `-` then `+`). Split: exactly one row carrying both sides.
fn render_diff_row(row: &DiffLine, file: &FileRows, mode: DiffMode) -> Vec<RowContent> {
    if mode == DiffMode::Split {
        return vec![split_row(row, file)];
    }
    let mut out = Vec::new();
    match row.change_type {
        ChangeType::Equal => {
            if let Some((n, text)) = &row.new_line {
                let old_n = row.old_line.as_ref().map(|(o, _)| *o).unwrap_or(0);
                out.push(RowContent::Full(side_line(
                    Some(old_n),
                    Some(*n),
                    ' ',
                    text,
                    LineOrigin::Context,
                    file.new_hl.as_ref(),
                    *n,
                    None,
                )));
            }
        }
        ChangeType::Delete | ChangeType::Modified => {
            if let Some((n, text)) = &row.old_line {
                out.push(RowContent::Full(side_line(
                    Some(*n),
                    None,
                    '-',
                    text,
                    LineOrigin::Deletion,
                    file.old_hl.as_ref(),
                    *n,
                    row.old_segments.as_deref(),
                )));
            }
            if matches!(row.change_type, ChangeType::Modified)
                && let Some((n, text)) = &row.new_line
            {
                out.push(RowContent::Full(side_line(
                    None,
                    Some(*n),
                    '+',
                    text,
                    LineOrigin::Addition,
                    file.new_hl.as_ref(),
                    *n,
                    row.new_segments.as_deref(),
                )));
            }
        }
        ChangeType::Insert => {
            if let Some((n, text)) = &row.new_line {
                out.push(RowContent::Full(side_line(
                    None,
                    Some(*n),
                    '+',
                    text,
                    LineOrigin::Addition,
                    file.new_hl.as_ref(),
                    *n,
                    row.new_segments.as_deref(),
                )));
            }
        }
    }
    out
}

/// One lumen row → one split row: old on the left, new on the right, either
/// half blank when the row only exists on one side.
fn split_row(row: &DiffLine, file: &FileRows) -> RowContent {
    let old = row.old_line.as_ref().map(|(n, text)| {
        let (marker, origin, segments) = match row.change_type {
            ChangeType::Equal => (' ', LineOrigin::Context, None),
            _ => ('-', LineOrigin::Deletion, row.old_segments.as_deref()),
        };
        half_pairs(*n, marker, text, origin, file.old_hl.as_ref(), segments)
    });
    let new = row.new_line.as_ref().map(|(n, text)| {
        let (marker, origin, segments) = match row.change_type {
            ChangeType::Equal => (' ', LineOrigin::Context, None),
            _ => ('+', LineOrigin::Addition, row.new_segments.as_deref()),
        };
        half_pairs(*n, marker, text, origin, file.new_hl.as_ref(), segments)
    });
    RowContent::Split {
        old: old.unwrap_or_default(),
        new: new.unwrap_or_default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn side_line(
    old_n: Option<usize>,
    new_n: Option<usize>,
    marker: char,
    text: &str,
    origin: LineOrigin,
    hl: Option<&HighlightedLines>,
    lineno: usize,
    segments: Option<&[InlineSegment]>,
) -> Line<'static> {
    let gutter = format!(
        "{:>4} {:>4} {marker} ",
        old_n.map(|n| n.to_string()).unwrap_or_default(),
        new_n.map(|n| n.to_string()).unwrap_or_default(),
    );
    let pairs = content_pairs(text, origin, hl, lineno, segments);
    let mut spans = vec![Span::styled(gutter, Style::default().fg(THEME.gutter_fg))];
    spans.extend(pairs.into_iter().map(|(s, t)| Span::styled(t, s)));
    Line::from(spans)
}

/// One half of a split row: single-number gutter + the line's content pairs.
fn half_pairs(
    lineno: usize,
    marker: char,
    text: &str,
    origin: LineOrigin,
    hl: Option<&HighlightedLines>,
    segments: Option<&[InlineSegment]>,
) -> Vec<(Style, String)> {
    let mut pairs = vec![(
        Style::default().fg(THEME.gutter_fg),
        format!("{lineno:>4} {marker} "),
    )];
    pairs.extend(content_pairs(text, origin, hl, lineno, segments));
    pairs
}

/// Syntax pairs for one line with the per-side background and word-level
/// emphasis applied.
fn content_pairs(
    text: &str,
    origin: LineOrigin,
    hl: Option<&HighlightedLines>,
    lineno: usize,
    segments: Option<&[InlineSegment]>,
) -> Vec<(Style, String)> {
    // Syntax spans for this line, else plain.
    let mut pairs: Vec<(Style, String)> = hl
        .and_then(|lines| lines.get(lineno.saturating_sub(1)).cloned().flatten())
        .unwrap_or_else(|| vec![(Style::default().fg(THEME.context_fg), text.to_string())]);

    // Line background per side.
    pairs = highlighter_bg(pairs, origin);

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

fn highlighter_bg(pairs: Vec<(Style, String)>, origin: LineOrigin) -> Vec<(Style, String)> {
    super::theme::highlighter().apply_diff_background(pairs, origin)
}
