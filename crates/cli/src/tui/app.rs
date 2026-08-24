//! The reviewer's model, key handling and drawing. `handle_key` is a plain
//! method on the model returning effects — testable without a terminal.
//!
//! All review state (reviewed marks, findings, resume cursor) lives in the
//! engine's `ReviewSession`; this model holds presentation state only.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use differential_engine::ReviewSession;
use differential_engine::review_state::FindingStatus;
use differential_schema as schema;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tui_textarea::TextArea;

use super::rows::{DiffMode, GroupContext, Row, RowContent, RowFactory, RowKind, build_group_rows};
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
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Quit,
    Yank(String),
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
}

pub struct App {
    pub session: ReviewSession,
    factory: RowFactory,

    pub groups: Vec<GroupInfo>,
    pub labels: HashMap<String, String>,

    pub focus: Focus,
    pub mode: Mode,
    pub selected_group: usize,
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
        let groups: Vec<GroupInfo> = schema_groups
            .iter()
            .map(|g| {
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
        let labels = schema_groups
            .iter()
            .map(|g| (g.id.clone(), g.label.clone()))
            .collect();

        // Resume position.
        let (selected_group, resume_row) = match session.cursor() {
            Some((gid, row)) => (
                groups.iter().position(|g| &g.id == gid).unwrap_or(0),
                Some(*row),
            ),
            None => (0, None),
        };

        let mut app = App {
            session,
            factory,
            groups,
            labels,
            focus: Focus::Groups,
            mode: Mode::Normal,
            selected_group,
            rows: Vec::new(),
            cursor: 0,
            scroll: 0,
            group_scroll: 0,
            folds_open: HashSet::new(),
            status: String::new(),
            viewport_hint: 24,
            pending_d: false,
        };
        app.rebuild_rows();
        if let Some(row) = resume_row {
            app.cursor = row.min(app.rows.len().saturating_sub(1));
        }
        app
    }

    pub fn rebuild_rows(&mut self) {
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
        let reviewed = self.session.reviewed_hunks();
        let ctx = GroupContext {
            doc: self.session.doc(),
            group: g,
            labels: &self.labels,
            findings: self.session.findings(),
            reviewed: &reviewed,
            fold_open: self.folds_open.contains(&g.id),
            mode: self.diff_mode(),
        };
        self.rows = build_group_rows(&mut self.factory, &ctx);
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
    }

    fn select_group(&mut self, idx: usize) {
        if self.groups.is_empty() {
            return;
        }
        self.selected_group = idx.min(self.groups.len() - 1);
        self.cursor = 0;
        self.scroll = 0;
        self.rebuild_rows();
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
        let id = self
            .groups
            .get(self.selected_group)
            .map(|g| g.id.clone())
            .unwrap_or_default();
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
            (KeyCode::Enter, _) if self.focus == Focus::Groups => self.focus = Focus::Diff,
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => match self.focus {
                Focus::Groups => self.select_group(self.selected_group + 1),
                Focus::Diff => self.move_cursor(1),
            },
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => match self.focus {
                Focus::Groups => self.select_group(self.selected_group.saturating_sub(1)),
                Focus::Diff => self.move_cursor(-1),
            },
            (KeyCode::Char('J'), _) | (KeyCode::Char('}'), _) => {
                self.select_group(self.selected_group + 1)
            }
            (KeyCode::Char('K'), _) | (KeyCode::Char('{'), _) => {
                self.select_group(self.selected_group.saturating_sub(1))
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
            (KeyCode::Char('z'), _) => {
                if let Some(g) = self.groups.get(self.selected_group) {
                    let gid = g.id.clone();
                    if !self.folds_open.insert(gid.clone()) {
                        self.folds_open.remove(&gid);
                    }
                    self.rebuild_rows();
                }
            }
            (KeyCode::Char('s'), KeyModifiers::NONE) => self.toggle_split(),
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
                return vec![Effect::Yank(self.findings_summary())];
            }
            _ => {}
        }
        Vec::new()
    }

    fn toggle_reviewed(&mut self) {
        let Some(h) = self.current_hunk() else {
            return;
        };
        if let Err(e) = self.session.toggle_reviewed(h) {
            self.status = format!("save failed: {e:#}");
            return;
        }
        self.save_cursor();
        self.rebuild_rows();
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
                let area = centered_rect(outer[0], 60, 16);
                frame.render_widget(Clear, area);
                frame.render_widget(help_paragraph(), area);
            }
            Mode::Normal => {}
        }
    }

    fn draw_groups(&mut self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        if self.selected_group < self.group_scroll {
            self.group_scroll = self.selected_group;
        } else if self.selected_group >= self.group_scroll + inner_h {
            self.group_scroll = self.selected_group + 1 - inner_h;
        }
        let items: Vec<Line> = self
            .groups
            .iter()
            .enumerate()
            .skip(self.group_scroll)
            .take(inner_h)
            .map(|(i, g)| {
                let done = g.class_keys.iter().all(|k| self.session.is_reviewed(k))
                    && !g.class_keys.is_empty();
                let tier = match g.effort {
                    schema::Effort::Close => "C",
                    schema::Effort::Skim => "S",
                    schema::Effort::Noise => "N",
                };
                let mark = if done { "✓" } else { " " };
                let mut style = THEME.effort_style(g.effort);
                if i == self.selected_group {
                    style = style.bg(THEME.selected_bg).add_modifier(Modifier::BOLD);
                }
                Line::from(Span::styled(
                    format!(
                        "{mark}{tier} {:>2}f +{:<4}-{:<4} {}",
                        g.n_files, g.adds, g.dels, g.label
                    ),
                    style,
                ))
            })
            .collect();
        let orphans = self
            .session
            .findings()
            .iter()
            .filter(|f| f.status == FindingStatus::Orphaned)
            .count();
        let title = if orphans > 0 {
            format!(" reading plan · ⚠ {orphans} orphaned finding(s) ")
        } else {
            " reading plan ".to_string()
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
            " {done}/{total} classes reviewed · {open} finding(s) · {} · j/k J/K nav · space reviewed · c finding · s split · z fold · y yank · ? help · q quit",
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
        Line::from("  s          toggle unified / split diff"),
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
