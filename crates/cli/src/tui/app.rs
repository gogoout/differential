//! The reviewer's model, key handling and drawing. `handle_key` is a plain
//! method on the model returning effects — testable without a terminal.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use differential_engine::model::DiffView;
use differential_engine::review_state::{Anchor, Finding, FindingStatus, ReviewState};
use differential_schema as schema;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tui_textarea::TextArea;

use super::rows::{GroupContext, Row, RowFactory, RowKind, build_group_rows};
use super::theme::THEME;

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
    SaveState,
    SaveFindings,
    Yank(String),
}

pub struct GroupInfo {
    pub id: String,
    pub label: String,
    pub effort: schema::Effort,
    /// Class content keys of the group's classes (reviewed-mark keys).
    pub class_keys: Vec<String>,
    pub n_hunks: usize,
}

pub struct App {
    pub doc: schema::PlanDocument,
    pub view: DiffView,
    pub plan_hash: String,
    factory: RowFactory,

    pub groups: Vec<GroupInfo>,
    pub labels: HashMap<String, String>,
    /// hunk index -> class content key (reviewed-mark key).
    pub hunk_key: HashMap<usize, String>,

    pub state: ReviewState,
    pub findings: Vec<Finding>,

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
    pub fn new(
        doc: schema::PlanDocument,
        view: DiffView,
        plan_hash: String,
        factory: RowFactory,
        state: ReviewState,
        findings: Vec<Finding>,
    ) -> Self {
        let class_key: HashMap<&str, String> = doc
            .classes
            .iter()
            .map(|c| {
                let digests: Vec<String> = c
                    .hunk_ids
                    .iter()
                    .map(|hid| {
                        let idx: usize = hid[1..].parse().expect("h<N>");
                        doc.hunks[idx].digest.clone()
                    })
                    .collect();
                (
                    c.id.as_str(),
                    differential_engine::review_state::class_content_key(&digests),
                )
            })
            .collect();
        let mut hunk_key = HashMap::new();
        for c in &doc.classes {
            for hid in &c.hunk_ids {
                let idx: usize = hid[1..].parse().expect("h<N>");
                hunk_key.insert(idx, class_key[c.id.as_str()].clone());
            }
        }
        let empty = Vec::new();
        let schema_groups = doc.groups.as_ref().unwrap_or(&empty);
        let groups: Vec<GroupInfo> = schema_groups
            .iter()
            .map(|g| GroupInfo {
                id: g.id.clone(),
                label: g.label.clone(),
                effort: g.effort,
                class_keys: g
                    .class_ids
                    .iter()
                    .map(|c| class_key[c.as_str()].clone())
                    .collect(),
                n_hunks: g
                    .class_ids
                    .iter()
                    .map(|c| {
                        doc.classes
                            .iter()
                            .find(|cl| &cl.id == c)
                            .map_or(0, |cl| cl.hunk_ids.len())
                    })
                    .sum(),
            })
            .collect();
        let labels = schema_groups
            .iter()
            .map(|g| (g.id.clone(), g.label.clone()))
            .collect();

        // Resume position.
        let selected_group = state
            .cursor
            .as_ref()
            .and_then(|(gid, _)| groups.iter().position(|g| &g.id == gid))
            .unwrap_or(0);

        let mut app = App {
            doc,
            view,
            plan_hash,
            factory,
            groups,
            labels,
            hunk_key,
            state,
            findings,
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
        if let Some((_, row)) = app.state.cursor.clone() {
            app.cursor = row.min(app.rows.len().saturating_sub(1));
        }
        app
    }

    pub fn reviewed_hunks_of_selected(&self) -> HashSet<usize> {
        self.hunk_key
            .iter()
            .filter(|(_, key)| self.state.reviewed_classes.contains(*key))
            .map(|(hi, _)| *hi)
            .collect()
    }

    pub fn rebuild_rows(&mut self) {
        let Some(groups) = self.doc.groups.as_ref() else {
            self.rows = Vec::new();
            return;
        };
        if groups.is_empty() {
            self.rows = vec![Row {
                kind: RowKind::Blank,
                line: Line::from("nothing to review — empty diff"),
            }];
            return;
        }
        let g = &groups[self.selected_group.min(groups.len() - 1)];
        let reviewed = self.reviewed_hunks_of_selected();
        let ctx = GroupContext {
            doc: &self.doc,
            group: g,
            labels: &self.labels,
            findings: &self.findings,
            reviewed: &reviewed,
            fold_open: self.folds_open.contains(&g.id),
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
                        return self.add_finding(hunk, body);
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
            (KeyCode::Char('q'), _) => return vec![Effect::SaveState, Effect::Quit],
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
                let gid = self.groups[self.selected_group].id.clone();
                if !self.folds_open.insert(gid.clone()) {
                    self.folds_open.remove(&gid);
                }
                self.rebuild_rows();
            }
            (KeyCode::Char(' '), _) => return self.toggle_reviewed(),
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
                    return self.delete_finding_at_cursor();
                }
                self.pending_d = true;
            }
            (KeyCode::Char('y'), _) => {
                return vec![Effect::Yank(self.findings_summary())];
            }
            _ => {}
        }
        Vec::new()
    }

    fn toggle_reviewed(&mut self) -> Vec<Effect> {
        let Some(h) = self.current_hunk() else {
            return Vec::new();
        };
        let key = self.hunk_key[&h].clone();
        if !self.state.reviewed_classes.insert(key.clone()) {
            self.state.reviewed_classes.remove(&key);
        }
        self.state.cursor = Some((self.groups[self.selected_group].id.clone(), self.cursor));
        self.rebuild_rows();
        vec![Effect::SaveState]
    }

    fn add_finding(&mut self, hunk_idx: usize, body: String) -> Vec<Effect> {
        let hunk = &self.doc.hunks[hunk_idx];
        let side = if hunk.new_count > 0 { "new" } else { "old" };
        let line = if hunk.new_count > 0 {
            hunk.new_start.max(1)
        } else {
            hunk.old_start.max(1)
        };
        let vh = &self.view.hunks[hunk_idx];
        let line_text = vh
            .added
            .first()
            .or(vh.removed.first())
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .unwrap_or_default();
        let finding = Finding::new(
            body,
            self.plan_hash.clone(),
            Anchor {
                file: hunk.file.clone(),
                side: side.into(),
                line,
                hunk_digest: hunk.digest.clone(),
                line_text,
            },
        );
        self.findings.push(finding);
        self.status = "finding saved".into();
        self.rebuild_rows();
        vec![Effect::SaveFindings]
    }

    fn delete_finding_at_cursor(&mut self) -> Vec<Effect> {
        if let Some(RowKind::Finding(id, _)) = self.rows.get(self.cursor).map(|r| r.kind.clone()) {
            self.findings.retain(|f| f.id != id);
            self.status = "finding deleted".into();
            self.rebuild_rows();
            return vec![Effect::SaveFindings];
        }
        self.status = "dd works on a finding line".into();
        Vec::new()
    }

    /// Markdown summary of open findings, for pasting into an agent or PR.
    pub fn findings_summary(&self) -> String {
        let group_of_digest: HashMap<&str, &str> = self
            .doc
            .groups
            .iter()
            .flatten()
            .flat_map(|g| {
                g.class_ids.iter().flat_map(|cid| {
                    let class = self.doc.classes.iter().find(|c| &c.id == cid).unwrap();
                    class.hunk_ids.iter().map(|hid| {
                        let idx: usize = hid[1..].parse().unwrap();
                        (self.doc.hunks[idx].digest.as_str(), g.label.as_str())
                    })
                })
            })
            .collect();
        let mut out = String::new();
        for f in self
            .findings
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
            .constraints([Constraint::Length(34), Constraint::Min(0)])
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
                let done = g
                    .class_keys
                    .iter()
                    .all(|k| self.state.reviewed_classes.contains(k))
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
                    format!("{mark}{tier} {:>3}h  {}", g.n_hunks, g.label),
                    style,
                ))
            })
            .collect();
        let orphans = self
            .findings
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
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(inner_h)
            .map(|(i, r)| {
                let mut line = r.line.clone();
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
        let done = self.state.reviewed_classes.len().min(total);
        let open = self
            .findings
            .iter()
            .filter(|f| f.status == FindingStatus::Open)
            .count();
        let text = format!(
            " {done}/{total} classes reviewed · {open} finding(s) · {} · j/k J/K nav · space reviewed · c finding · z fold · y yank · ? help · q quit",
            self.status
        );
        frame.render_widget(
            Paragraph::new(text).style(Style::default().bg(THEME.status_bg)),
            area,
        );
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
